use std::collections::{BTreeMap, BTreeSet};
use std::ffi::OsStr;
use std::io;
use std::path::{Component, Path, PathBuf};

use super::desired::{Desired, resource_digest};
#[cfg(unix)]
use super::ledger::NodeId;
use super::ledger::{Entry, Ledger, target_digest};
use super::plan::{Ignored, IgnoredReason, Observed};
use super::{Key, Kind, Locations, PlanError};
use crate::{
    build, manifest,
    stage::{PathKind, pinned::Root},
};

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Resolved {
    pub claude: PathBuf,
    pub local: PathBuf,
}

impl Resolved {
    pub fn load(locations: &Locations) -> Result<Self, PlanError> {
        let claude = resolve_claude(absolute(&locations.claude_root)?)?;
        let local = absolute(&locations.local_root)?;
        Ok(Self { claude, local })
    }

    pub fn open(&self) -> Result<Roots, PlanError> {
        let (local, local_ids, local_suffix) = existing_prefix(&self.local)?;
        let (claude, claude_ids, claude_suffix) = existing_prefix(&self.claude)?;
        if overlaps(&self.claude, &self.local)
            || (local_suffix.is_empty() && claude_ids.contains(local_ids.last().unwrap()))
            || (claude_suffix.is_empty() && local_ids.contains(claude_ids.last().unwrap()))
            || (local_ids.last() == claude_ids.last()
                && local_suffix.iter().zip(&claude_suffix).all(|(a, b)| a == b))
        {
            return Err(unsafe_path(
                &self.claude,
                "Claude config and local roots must be disjoint",
            ));
        }
        let local = local_suffix.is_empty().then_some(local);
        let claude = claude_suffix.is_empty().then_some(claude);
        let state = child(local.as_ref(), "state")
            .map_err(|err| PlanError::inspect(&self.local.join("state"), err))?;
        Ok(Roots {
            claude,
            local,
            state,
        })
    }
}

#[cfg(unix)]
type DirectoryId = NodeId;
#[cfg(windows)]
type DirectoryId = PathBuf;

fn existing_prefix(path: &Path) -> Result<(Root, Vec<DirectoryId>, Vec<String>), PlanError> {
    let base = path.ancestors().last().expect("absolute path has a root");
    let inspect = |err| PlanError::inspect(path, err);
    let mut root = Root::open_checked(base)
        .map_err(inspect)?
        .ok_or_else(|| inspect(io::ErrorKind::NotFound.into()))?;
    let mut ids = vec![directory_identity(&root).map_err(inspect)?];
    let mut missing = Vec::new();
    for part in path
        .strip_prefix(base)
        .expect("path includes its root")
        .iter()
    {
        if missing.is_empty()
            && let Some(child) = root.child(part).map_err(inspect)?
        {
            ids.push(directory_identity(&child).map_err(inspect)?);
            root = child;
        } else {
            // With no directory entry to compare, conservatively treat case variants as aliases.
            missing.push(part.to_string_lossy().to_lowercase());
        }
    }
    Ok((root, ids, missing))
}

#[cfg(unix)]
fn directory_identity(dir: &Root) -> io::Result<NodeId> {
    Ok(stat_identity(&rustix::fs::fstat(dir)?))
}

#[cfg(windows)]
fn directory_identity(dir: &Root) -> io::Result<DirectoryId> {
    dir.canonical_path()
}

#[cfg(unix)]
#[allow(clippy::unnecessary_cast)] // stat field widths differ across Unix platforms.
pub(super) fn stat_identity(stat: &rustix::fs::Stat) -> NodeId {
    (stat.st_dev as u64, stat.st_ino as u64)
}

#[cfg(unix)]
pub(super) fn node_identity(parent: &Root, name: &str) -> io::Result<Option<NodeId>> {
    use rustix::{
        fs::{AtFlags, statat},
        io::Errno,
    };
    match statat(parent, name, AtFlags::SYMLINK_NOFOLLOW) {
        Ok(stat) => Ok(Some(stat_identity(&stat))),
        Err(Errno::NOENT) => Ok(None),
        Err(err) => Err(err.into()),
    }
}

pub(super) fn overlaps(a: &Path, b: &Path) -> bool {
    a.starts_with(b) || b.starts_with(a)
}

pub(super) struct Roots {
    pub claude: Option<Root>,
    pub local: Option<Root>,
    pub state: Option<Root>,
}

fn unsafe_path(path: &Path, reason: &'static str) -> PlanError {
    PlanError::UnsafePath {
        path: path.to_owned(),
        reason,
    }
}

fn absolute(path: &Path) -> Result<PathBuf, PlanError> {
    if path.as_os_str().is_empty() || path.components().any(|c| c == Component::ParentDir) {
        return Err(unsafe_path(
            path,
            "expected a nonempty path without parent traversal",
        ));
    }
    let abs = std::path::absolute(path).map_err(|err| PlanError::inspect(path, err))?;
    // macOS exposes these system directories through fixed aliases.
    #[cfg(target_os = "macos")]
    let abs = system_alias(abs).map_err(|err| PlanError::inspect(path, err))?;
    if abs.to_str().is_none() {
        return Err(unsafe_path(path, "paths must be UTF-8 to record ownership"));
    }
    Ok(abs.components().collect())
}

#[cfg(target_os = "macos")]
fn system_alias(abs: PathBuf) -> io::Result<PathBuf> {
    for name in ["tmp", "var", "etc"] {
        let alias = Path::new("/").join(name);
        if let Ok(tail) = abs.strip_prefix(&alias) {
            let target = Path::new("/private").join(name);
            let root = Root::open_checked(Path::new("/"))?.expect("filesystem root exists");
            if root.kind(OsStr::new(name))? == Some(PathKind::Symlink) {
                let link = root.link(OsStr::new(name))?;
                if link == target || Path::new("/").join(link) == target {
                    return Ok(target.join(tail));
                }
            }
        }
    }
    Ok(abs)
}

fn resolve_claude(path: PathBuf) -> Result<PathBuf, PlanError> {
    let (Some(parent), Some(name)) = (path.parent(), path.file_name()) else {
        return Err(unsafe_path(&path, "invalid Claude config root"));
    };
    let Some(root) = Root::open_checked(parent).map_err(|err| PlanError::inspect(parent, err))?
    else {
        return Ok(path);
    };
    match root
        .kind(name)
        .map_err(|err| PlanError::inspect(&path, err))?
    {
        None | Some(PathKind::Directory) => Ok(path),
        Some(PathKind::Symlink) => {
            let resolved =
                std::fs::canonicalize(&path).map_err(|err| PlanError::inspect(&path, err))?;
            absolute(&resolved)
        }
        _ => Err(unsafe_path(&path, "Claude config root must be a directory")),
    }
}

pub(super) fn child(root: Option<&Root>, name: &str) -> io::Result<Option<Root>> {
    root.map(|root| root.child(OsStr::new(name)))
        .transpose()
        .map(Option::flatten)
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Actual {
    pub private: BTreeSet<Key>,
    pub ignored: Vec<Ignored>,
    pub nodes: BTreeMap<Key, Observed>,
    pub aliases: BTreeSet<Key>,
    #[cfg(unix)]
    pub node_ids: BTreeMap<Key, NodeId>,
    pub snapshots: BTreeMap<(Key, String), SnapshotCheck>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum SnapshotCheck {
    Missing,
    Verified,
    Corrupt,
}

impl Actual {
    pub fn inspect(
        paths: &Resolved,
        roots: &Roots,
        desired: &Desired,
        ledger: &Ledger,
    ) -> Result<Self, PlanError> {
        let (private, ignored) = private_layer(roots.local.as_ref())
            .map_err(|err| PlanError::inspect(&paths.local, err))?;
        let entries: BTreeMap<_, _> = ledger.entries().map(|entry| (entry.key(), entry)).collect();
        let keys: BTreeSet<_> = desired
            .resources
            .keys()
            .chain(private.iter())
            .chain(entries.keys())
            .collect();
        let mut nodes = BTreeMap::new();
        let mut aliases = BTreeSet::new();
        #[cfg(unix)]
        let mut node_ids = BTreeMap::new();
        let mut snapshots = BTreeMap::new();
        for kind in Kind::ALL {
            let used = keys.iter().copied().filter(|key| key.kind == kind);
            let parent = child(roots.claude.as_ref(), kind.directory());
            #[cfg(unix)]
            let mut by_identity = BTreeMap::<NodeId, Vec<Key>>::new();
            #[cfg(windows)]
            let mut by_spelling = BTreeMap::<String, Vec<Key>>::new();
            for key in used {
                let observed = match &parent {
                    Ok(parent) => observe(parent.as_ref(), &key.leaf()),
                    Err(err) if err.kind() == io::ErrorKind::NotADirectory => {
                        Ok(Observed::UnsafeAncestor)
                    }
                    Err(err) => Err(io::Error::from(err.kind())),
                }
                .map_err(|err| PlanError::inspect(&paths.claude.join(key.destination()), err))?;
                if let Ok(Some(parent)) = &parent {
                    if let Some(entry) = entries.get(key) {
                        check_quarantines(parent, &paths.claude, entry)?;
                    }
                    #[cfg(unix)]
                    if matches!(observed, Observed::Symlink { .. }) {
                        let inspect =
                            |err| PlanError::inspect(&paths.claude.join(key.destination()), err);
                        let id = node_identity(parent, &key.leaf())
                            .map_err(inspect)?
                            .ok_or_else(|| inspect(io::Error::other("destination disappeared")))?;
                        node_ids.insert(key.clone(), id);
                        by_identity.entry(id).or_default().push(key.clone());
                    }
                    #[cfg(windows)]
                    if matches!(observed, Observed::Symlink { .. }) {
                        by_spelling
                            .entry(key.name.to_lowercase())
                            .or_default()
                            .push(key.clone());
                    }
                }
                if let Some(resource) = desired.resources.get(key)
                    && !private.contains(key)
                {
                    snapshots.insert(
                        (key.clone(), resource.digest.clone()),
                        SnapshotCheck::Missing,
                    );
                }
                if let Observed::Symlink { target } = &observed
                    && entries.get(key).is_some_and(|entry| entry.owns(target))
                    && let Some(digest) = target_digest(key, &paths.local, target)
                {
                    snapshots.insert((key.clone(), digest.to_owned()), SnapshotCheck::Missing);
                }
                nodes.insert(key.clone(), observed);
            }
            #[cfg(unix)]
            let matching = by_identity.into_values();
            #[cfg(windows)]
            let matching = by_spelling.into_values();
            let mut matching = matching.filter(|keys| keys.len() > 1).peekable();
            if let Ok(Some(parent)) = &parent
                && matching.peek().is_some()
            {
                let names = parent
                    .names(manifest::MAX_ENTRIES)
                    .map_err(|err| PlanError::inspect(&paths.claude.join(kind.directory()), err))?;
                for group in matching {
                    if group
                        .iter()
                        .any(|key| names.binary_search(&key.leaf().into()).is_err())
                    {
                        aliases.extend(group);
                    }
                }
            }
        }
        for ((key, digest), check) in &mut snapshots {
            *check = inspect_snapshot(roots.state.as_ref(), key, digest)
                .map_err(|err| PlanError::inspect(&key.snapshot_path(&paths.local, digest), err))?;
        }
        Ok(Self {
            private,
            ignored,
            nodes,
            aliases,
            #[cfg(unix)]
            node_ids,
            snapshots,
        })
    }
}

pub(super) fn check_quarantines(
    parent: &Root,
    root: &Path,
    entry: &Entry,
) -> Result<(), PlanError> {
    if let Some(pending) = &entry.pending {
        for item in &pending.quarantines {
            let quarantine = root.join(entry.kind.directory()).join(&item.name);
            let verified = verified_quarantine(parent, item);
            if !matches!(verified, Ok(true)) {
                return Err(PlanError::Quarantine {
                    quarantine,
                    destination: root.join(&entry.destination),
                    source: verified.err(),
                });
            }
        }
    }
    Ok(())
}

fn verified_quarantine(parent: &Root, item: &super::ledger::Quarantine) -> io::Result<bool> {
    match observe(Some(parent), &item.name)? {
        Observed::Absent => Ok(true),
        #[cfg(unix)]
        Observed::Symlink { target } if super::same_target(&target, &item.target) => {
            Ok(item.identity.is_some() && node_identity(parent, &item.name)? == item.identity)
        }
        _ => Ok(false),
    }
}

pub(super) fn observe(parent: Option<&Root>, leaf: &str) -> io::Result<Observed> {
    let Some(parent) = parent else {
        return Ok(Observed::Absent);
    };
    let name = OsStr::new(leaf);
    Ok(match parent.kind(name)? {
        None => Observed::Absent,
        Some(PathKind::File) => Observed::File,
        Some(PathKind::Directory) => Observed::Directory,
        Some(PathKind::Symlink) => Observed::Symlink {
            target: parent.link(name)?,
        },
        Some(PathKind::Other) => Observed::Other,
    })
}

pub(super) fn private_layer(local: Option<&Root>) -> io::Result<(BTreeSet<Key>, Vec<Ignored>)> {
    let mut resources = BTreeSet::new();
    let mut ignored = Vec::new();
    for kind in Kind::ALL {
        let directory = match child(local, kind.directory()) {
            Ok(Some(directory)) => directory,
            Ok(None) => continue,
            Err(err) if err.kind() == io::ErrorKind::NotADirectory => {
                ignored.push(Ignored {
                    kind,
                    path: kind.directory().to_owned(),
                    reason: IgnoredReason::WrongType,
                });
                continue;
            }
            Err(err) => return Err(err),
        };
        for name in directory.names(manifest::MAX_ENTRIES)? {
            let reason = match name
                .to_str()
                .and_then(|name| Key::from_filename(kind, name))
            {
                None => IgnoredReason::InvalidName,
                Some(key) => match (kind, directory.kind(&name)?) {
                    (Kind::Agent | Kind::Rule, Some(PathKind::File)) => {
                        resources.insert(key);
                        continue;
                    }
                    (Kind::Skill, Some(PathKind::Directory)) => match directory.child(&name) {
                        Ok(Some(skill))
                            if skill.kind(OsStr::new("SKILL.md"))? == Some(PathKind::File) =>
                        {
                            resources.insert(key);
                            continue;
                        }
                        Ok(_) => IgnoredReason::MissingSkillFile,
                        Err(err) if err.kind() == io::ErrorKind::NotADirectory => {
                            IgnoredReason::WrongType
                        }
                        Err(err) => return Err(err),
                    },
                    (_, _) => IgnoredReason::WrongType,
                },
            };
            ignored.push(Ignored {
                kind,
                path: format!("{}/{}", kind.directory(), name.to_string_lossy()),
                reason,
            });
        }
    }
    Ok((resources, ignored))
}

pub(super) fn inspect_snapshot(
    state: Option<&Root>,
    key: &Key,
    digest: &str,
) -> io::Result<SnapshotCheck> {
    use SnapshotCheck::{Corrupt, Missing, Verified};
    let inspect = || -> io::Result<SnapshotCheck> {
        let Some(snapshots) = child(state, "snapshots")? else {
            return Ok(Missing);
        };
        if !safe_snapshot_directory(&snapshots)? {
            return Ok(Corrupt);
        }
        let Some(bucket) = child(Some(&snapshots), digest)? else {
            return Ok(Missing);
        };
        let leaf = key.leaf();
        if !safe_snapshot_directory(&bucket)? || bucket.names(2)? != [OsStr::new(&leaf)] {
            return Ok(Corrupt);
        }
        let mut scan = SnapshotScan {
            files: BTreeMap::new(),
            entries: 0,
            bytes: 0,
            limits: build::limits(),
        };
        let valid = match key.kind {
            Kind::Skill => match child(Some(&bucket), &leaf)? {
                Some(skill) => scan.walk(&skill, "", 0)? && scan.files.contains_key("SKILL.md"),
                None => false,
            },
            Kind::Agent | Kind::Rule => scan.file(&bucket, &leaf, leaf.clone())?,
        };
        if !valid
            || resource_digest(
                key,
                scan.files
                    .into_iter()
                    .map(|(name, (sha, exec))| (name, sha, exec)),
            ) != digest
        {
            return Ok(Corrupt);
        }
        Ok(Verified)
    };
    match inspect() {
        Err(err)
            if matches!(
                err.kind(),
                io::ErrorKind::NotADirectory | io::ErrorKind::Other
            ) =>
        {
            Ok(Corrupt)
        }
        result => result,
    }
}

fn safe_snapshot_directory(dir: &Root) -> io::Result<bool> {
    #[cfg(unix)]
    {
        Ok(rustix::fs::fstat(dir)?.st_mode & 0o022 == 0)
    }
    #[cfg(windows)]
    {
        let _ = dir;
        Ok(true)
    }
}

struct SnapshotScan {
    files: BTreeMap<String, (String, bool)>,
    entries: usize,
    bytes: u64,
    limits: crate::discovery::Limits,
}

impl SnapshotScan {
    fn walk(&mut self, dir: &Root, prefix: &str, depth: usize) -> io::Result<bool> {
        if depth > 24 || !safe_snapshot_directory(dir)? {
            return Ok(false);
        }
        let names = dir.names(self.limits.max_entries.saturating_sub(self.entries))?;
        if names.is_empty() {
            return Ok(false);
        }
        self.entries += names.len();
        for name in names {
            let Some(name) = name.to_str() else {
                return Ok(false);
            };
            let path = format!("{prefix}{name}");
            if manifest::path_flaw(&path).is_some() {
                return Ok(false);
            }
            let valid = match dir.kind(OsStr::new(name))? {
                Some(PathKind::Directory) => match child(Some(dir), name)? {
                    Some(child) => self.walk(&child, &format!("{path}/"), depth + 1)?,
                    None => false,
                },
                Some(PathKind::File) => self.file(dir, name, path)?,
                _ => false,
            };
            if !valid {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn file(&mut self, dir: &Root, name: &str, path: String) -> io::Result<bool> {
        let limit = self
            .limits
            .max_file_bytes
            .min(self.limits.max_total_bytes.saturating_sub(self.bytes));
        let Some((bytes, mode)) = dir.read_mode(name, limit + 1)? else {
            return Ok(false);
        };
        if bytes.len() as u64 > limit || (cfg!(unix) && !matches!(mode, 0o444 | 0o555)) {
            return Ok(false);
        }
        self.bytes += bytes.len() as u64;
        self.files
            .insert(path, (manifest::sha256(&bytes), mode == 0o555));
        Ok(true)
    }
}
