//! Ownership-aware merging of reviewed settings and hooks into the user's Claude `settings.json`.

mod apply;
mod backup;
mod fragment;
mod merge;
mod restore;
mod state;
#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;

pub use apply::{ApplyError, Step, apply};
pub use backup::Backup;
pub use fragment::Fragment;
pub use restore::{
    RestorePlan, RestoreRecord, restore, restore_apply, restore_backups, restore_plan,
};

use std::ffi::OsStr;
use std::fs::Metadata;
use std::io::{self, Read};
use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

use super::actual::{Resolved, Roots};
use super::{Locations, Ownership, PlanError};
use crate::manifest;
use crate::stage::{PathKind, pinned::Root};
use state::{HookRecord, Pending, State, User, ValueRecord};

pub(super) const SETTINGS: &str = "settings.json";
/// Bounds every read (the fragment, `settings.json` and `managed.json`) and so every write.
const MAX_BYTES: u64 = 8 * 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("invalid settings fragment {} ({detail}); nothing was changed", path.display())]
    Fragment { path: PathBuf, detail: String },
    #[error(transparent)]
    Location(#[from] PlanError),
    #[error("cannot merge into {} ({detail}); fix it by hand, Agenvil leaves it untouched", path.display())]
    Settings { path: PathBuf, detail: String },
    #[error("invalid managed settings state {} ({detail}); inspect or restore it before retrying", path.display())]
    State { path: PathBuf, detail: String },
}

impl Error {
    pub fn exit(&self) -> crate::diagnostics::Exit {
        match self {
            Self::Location(error) => error.exit(),
            _ => crate::diagnostics::Exit::Usage,
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Add,
    Change,
    Remove,
    Noop,
    Conflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum EntryKind {
    Value,
    Hook,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Reason {
    /// Not in the file yet.
    Missing,
    /// Already the managed value.
    Current,
    /// The user already has the desired value; it is kept on removal.
    Adopted,
    /// The managed value or group changed in the fragment.
    Changed,
    /// Recorded, but no longer in the fragment.
    Obsolete,
    /// Recorded, but no longer in the file.
    Gone,
    /// A different value Agenvil never wrote.
    UserValue,
    /// The user edited a value Agenvil wrote.
    UserEdited,
    /// The user deleted a value or hook group Agenvil recorded; `--remove` drops the record.
    UserRemoved,
    /// A parent on the path is not an object, or a hook event is not an array.
    TypeClash,
    /// A managed hook group whose commands are still there was edited.
    HookModified,
    /// The user already has a group running the same commands.
    HookDuplicate,
}

impl std::fmt::Display for Reason {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let text = serde_json::to_value(self).expect("unit variant");
        f.write_str(text.as_str().expect("string variant"))
    }
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Operation {
    pub operation: OperationKind,
    pub kind: EntryKind,
    /// A JSON pointer for values, the stable ID for hooks.
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
    pub ownership: Ownership,
    pub reason: Reason,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum FileAction {
    Create,
    Modify,
    Unchanged,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct FileChange {
    pub path: PathBuf,
    pub action: FileAction,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Files {
    pub settings: FileChange,
    pub state: FileChange,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub add: usize,
    pub change: usize,
    pub remove: usize,
    pub noop: usize,
    pub conflict: usize,
}

#[cfg(unix)]
type Identity = (u64, u64, u64, i64, i64);
#[cfg(not(unix))]
type Identity = ();

/// What planning read from a file, so apply can refuse a file that changed since.
#[derive(Clone, Debug, PartialEq, Eq)]
struct Seen {
    digest: String,
    mode: u32,
    identity: Identity,
}

#[cfg_attr(not(any(target_os = "linux", target_os = "macos")), allow(dead_code))]
/// A pure merge plan: planning never creates directories, takes locks or writes.
#[derive(Serialize)]
pub struct Plan {
    pub root: PathBuf,
    pub operations: Vec<Operation>,
    pub files: Files,
    pub summary: Summary,
    #[serde(skip)]
    locations: Locations,
    #[serde(skip)]
    paths: Resolved,
    #[serde(skip)]
    settings: Option<(Vec<u8>, Seen)>,
    #[serde(skip)]
    settings_after: Option<Vec<u8>>,
    #[serde(skip)]
    state_digest: Option<String>,
    #[serde(skip)]
    state_before: Option<Vec<u8>>,
    #[serde(skip)]
    state_after: Option<Vec<u8>>,
    #[serde(skip)]
    journal: Option<Vec<u8>>,
}

impl std::fmt::Debug for Plan {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Plan")
            .field("root", &self.root)
            .field("operations", &self.operations)
            .field("files", &self.files)
            .finish_non_exhaustive()
    }
}

impl Plan {
    pub fn has_conflicts(&self) -> bool {
        self.summary.conflict != 0
    }
}

/// Plans merging `desired` into user's settings; `None` removes everything recorded.
pub fn plan(locations: &Locations, desired: Option<&Fragment>) -> Result<Plan, Error> {
    let inputs = Inputs::load(locations)?;
    let empty = Fragment::default();
    let outcome = merge::merge(
        &inputs.document,
        &inputs.values,
        &inputs.hooks,
        desired.unwrap_or(&empty),
    );
    let settings_after = (outcome.document != inputs.document).then(|| {
        let mut bytes = serde_json::to_vec_pretty(&outcome.document).expect("JSON values encode");
        bytes.push(b'\n');
        bytes
    });
    inputs.transaction(
        outcome.operations,
        outcome.values,
        outcome.hooks,
        settings_after,
    )
}

struct Inputs {
    locations: Locations,
    paths: Resolved,
    settings: Option<(Vec<u8>, Seen)>,
    document: Value,
    state_bytes: Option<Vec<u8>>,
    recorded: State,
    values: Vec<ValueRecord>,
    hooks: Vec<HookRecord>,
    last_write: Option<String>,
    digest: Option<String>,
}

impl Inputs {
    fn load(locations: &Locations) -> Result<Self, Error> {
        let paths = Resolved::load(locations)?;
        let Roots { claude, state, .. } = paths.open()?;
        let settings_path = paths.claude.join(SETTINGS);
        let state_path = paths.local.join("state").join(state::FILE);
        let unusable = |detail| settings_error(&settings_path, detail);
        let settings = read(claude.as_ref(), SETTINGS)
            .map_err(|err| unusable(err.to_string()))?
            .map(|(bytes, metadata)| {
                let seen = seen(&bytes, &metadata);
                (bytes, seen)
            });
        let document = match &settings {
            None => Value::Object(Default::default()),
            Some((bytes, _)) => match serde_json::from_slice(bytes) {
                Ok(value @ Value::Object(_)) => value,
                Ok(_) => return Err(unusable("the top level is not a JSON object".to_owned())),
                Err(err) => return Err(unusable(format!("invalid JSON: {err}"))),
            },
        };
        let state_bytes = read(state.as_ref(), state::FILE)
            .map_err(|err| Error::State {
                path: state_path.clone(),
                detail: err.to_string(),
            })?
            .map(|(bytes, _)| bytes);
        let recorded = State::parse(state_bytes.as_deref(), &state_path, &paths.claude)?;

        let digest = settings.as_ref().map(|(_, seen)| seen.digest.clone());
        let (values, hooks, last_write) = match &recorded.user {
            None => (Vec::new(), Vec::new(), None),
            Some(User {
                pending: Some(pending),
                ..
            }) if digest.as_deref() == Some(pending.after.as_str()) => (
                pending.values.clone(),
                pending.hooks.clone(),
                Some(pending.after.clone()),
            ),
            Some(user) if user.pending.as_ref().is_none_or(|p| p.before == digest) => (
                user.values.clone(),
                user.hooks.clone(),
                user.last_write.clone(),
            ),
            Some(_) => {
                return Err(Error::State {
                    path: state_path,
                    detail: format!(
                        "an interrupted apply left {} matching neither its old nor its new content",
                        settings_path.display()
                    ),
                });
            }
        };

        Ok(Self {
            locations: locations.clone(),
            paths,
            settings,
            document,
            state_bytes,
            recorded,
            values,
            hooks,
            last_write,
            digest,
        })
    }

    fn transaction(
        self,
        operations: Vec<Operation>,
        new_values: Vec<ValueRecord>,
        new_hooks: Vec<HookRecord>,
        settings_after: Option<Vec<u8>>,
    ) -> Result<Plan, Error> {
        let Self {
            locations,
            paths,
            settings,
            state_bytes,
            recorded,
            values,
            hooks,
            last_write,
            digest,
            ..
        } = self;
        let settings_path = paths.claude.join(SETTINGS);
        let state_path = paths.local.join("state").join(state::FILE);
        let written = match &settings_after {
            Some(bytes) => Some(manifest::sha256(bytes)),
            None => last_write.clone(),
        };
        let user = |values, hooks, last_write, pending| User {
            root: paths.claude.clone(),
            values,
            hooks,
            last_write,
            pending,
        };
        let after = (!new_values.is_empty() || !new_hooks.is_empty() || written.is_some())
            .then(|| user(new_values.clone(), new_hooks.clone(), written, None));
        let encode = |user: Option<&User>| {
            recorded.encoded(user).map_err(|err| Error::State {
                path: state_path.clone(),
                detail: err.to_string(),
            })
        };
        // The old records stay authoritative until the new settings are visible.
        let journal = settings_after
            .as_ref()
            .map(|bytes| {
                encode(Some(&user(
                    values,
                    hooks,
                    last_write.clone(),
                    Some(Pending {
                        before: digest,
                        after: manifest::sha256(bytes),
                        values: new_values,
                        hooks: new_hooks,
                    }),
                )))
            })
            .transpose()?;
        // A journal is always settled, even when the final records equal the old ones.
        let state_after = (after != recorded.user || journal.is_some())
            .then(|| encode(after.as_ref()))
            .transpose()?;

        // What is written must stay readable by the next run.
        let oversized =
            |bytes: &Option<Vec<u8>>| bytes.as_ref().is_some_and(|b| b.len() as u64 > MAX_BYTES);
        let too_large =
            "the result would exceed the 8 MiB size limit; nothing was changed".to_owned();
        if oversized(&settings_after) {
            return Err(settings_error(&settings_path, too_large));
        }
        if oversized(&state_after) || oversized(&journal) {
            return Err(Error::Settings {
                path: state_path,
                detail: too_large,
            });
        }

        let action = |before: bool, after: bool| match (before, after) {
            (_, false) => FileAction::Unchanged,
            (false, true) => FileAction::Create,
            (true, true) => FileAction::Modify,
        };
        let mut summary = Summary::default();
        for op in &operations {
            *match op.operation {
                OperationKind::Add => &mut summary.add,
                OperationKind::Change => &mut summary.change,
                OperationKind::Remove => &mut summary.remove,
                OperationKind::Noop => &mut summary.noop,
                OperationKind::Conflict => &mut summary.conflict,
            } += 1;
        }
        Ok(Plan {
            root: paths.claude.clone(),
            operations,
            files: Files {
                settings: FileChange {
                    path: settings_path,
                    action: action(settings.is_some(), settings_after.is_some()),
                },
                state: FileChange {
                    path: state_path,
                    action: action(state_bytes.is_some(), state_after.is_some()),
                },
            },
            summary,
            locations,
            paths,
            settings,
            settings_after,
            state_digest: state_bytes.as_deref().map(manifest::sha256),
            state_before: state_bytes,
            state_after,
            journal,
        })
    }
}

fn settings_error(path: &std::path::Path, detail: String) -> Error {
    Error::Settings {
        path: path.to_owned(),
        detail,
    }
}

/// Report a pending journal without reading or interpreting `settings.json`.
pub(super) fn has_pending(paths: &Resolved, root: Option<&Root>) -> Result<bool, Error> {
    let path = paths.local.join("state").join(state::FILE);
    let bytes = read(root, state::FILE)
        .map_err(|err| Error::State {
            path: path.clone(),
            detail: err.to_string(),
        })?
        .map(|(bytes, _)| bytes);
    let state = State::parse(bytes.as_deref(), &path, &paths.claude)?;
    Ok(state.user.is_some_and(|user| user.pending.is_some()))
}

/// A bounded read of a regular file directly in `dir`; symlinks and special files are refused.
fn read(dir: Option<&Root>, name: &str) -> io::Result<Option<(Vec<u8>, Metadata)>> {
    let Some(dir) = dir else { return Ok(None) };
    match dir.kind(OsStr::new(name))? {
        None => return Ok(None),
        Some(PathKind::File) => {}
        Some(_) => {
            return Err(io::Error::other(
                "not a regular file; symlinks and special files are refused",
            ));
        }
    }
    let file = dir
        .open_file(name)?
        .ok_or_else(|| io::Error::other("it changed while opening it"))?;
    let metadata = file.metadata()?;
    let mut bytes = Vec::new();
    file.take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_BYTES {
        return Err(io::Error::other("it exceeds the 8 MiB size limit"));
    }
    Ok(Some((bytes, metadata)))
}

fn seen(bytes: &[u8], metadata: &Metadata) -> Seen {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        Seen {
            digest: manifest::sha256(bytes),
            mode: metadata.mode() & 0o7777,
            identity: (
                metadata.dev(),
                metadata.ino(),
                metadata.size(),
                metadata.mtime(),
                metadata.mtime_nsec(),
            ),
        }
    }
    #[cfg(not(unix))]
    {
        let _ = metadata;
        Seen {
            digest: manifest::sha256(bytes),
            mode: 0,
            identity: (),
        }
    }
}
