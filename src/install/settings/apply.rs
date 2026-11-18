use std::io;
use std::path::PathBuf;

use serde::Serialize;

use super::{Error, Plan};
use crate::install::Locations;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Step {
    Lock,
    Backup,
    Journal,
    Settings,
    Commit,
}

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error("the plan has {count} conflict(s); resolve them and re-plan")]
    Conflicts { count: usize },
    #[error("managed settings are unsupported on this platform")]
    Unsupported,
    #[error("the settings changed since planning ({detail}); re-plan before applying")]
    Changed { detail: String },
    #[error("ownership.lock is busy; retry after the other run finishes")]
    Locked,
    #[error("`{0}` is not a settings backup Agenvil wrote")]
    InvalidBackup(String),
    #[error(transparent)]
    Plan(#[from] Error),
    #[error("settings apply failed during {step:?}: {source}; {outcome}")]
    Failed {
        step: Step,
        #[source]
        source: io::Error,
        outcome: String,
    },
}

impl ApplyError {
    pub fn exit(&self) -> crate::diagnostics::Exit {
        use crate::diagnostics::Exit;
        match self {
            Self::Conflicts { .. } | Self::Changed { .. } | Self::Locked => Exit::Findings,
            Self::Plan(error) => error.exit(),
            _ => Exit::Usage,
        }
    }
}

/// Applies a conflict-free plan after rechecking what it read; returns the backup, if one was made.
pub fn apply(plan: &Plan) -> Result<Option<PathBuf>, ApplyError> {
    if plan.has_conflicts() {
        return Err(ApplyError::Conflicts {
            count: plan.summary.conflict,
        });
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        native::apply_with(plan, &mut |_| Ok(()))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        Err(ApplyError::Unsupported)
    }
}

/// Restores backup `name` over `settings.json`, only while that file is still Agenvil's last
/// write. The current content is backed up first; `managed.json` is left as is.
pub fn restore(locations: &Locations, name: &str) -> Result<Option<PathBuf>, ApplyError> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        native::restore(locations, name)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (locations, name);
        Err(ApplyError::Unsupported)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) mod native {
    use std::io::{self, Read};
    use std::path::PathBuf;

    use rustix::fs::RenameFlags;

    use super::{ApplyError, Step};
    use crate::install::Locations;
    use crate::install::actual::Resolved;
    use crate::install::fsops::{Fs, rename_new, rename_with, temp_name, write_file};
    use crate::install::settings::{MAX_BYTES, Plan, SETTINGS, backup, plan, read, seen, state};
    use crate::manifest;
    use crate::stage::pinned::Root;

    const UNCHANGED: &str = "settings.json is unchanged";
    const JOURNALED: &str = "settings.json is unchanged; the next plan settles managed.json";
    const WRITTEN: &str = "settings.json has the new content but was not fully synced; the next plan settles managed.json";
    const COMMITTED: &str = "settings.json and managed.json were written but not fully synced";

    /// A failed publication, and whether the new generation became visible before it failed.
    struct PublishError {
        visible: bool,
        source: io::Error,
    }

    impl From<io::Error> for PublishError {
        fn from(source: io::Error) -> Self {
            Self {
                visible: false,
                source,
            }
        }
    }

    /// `inject` runs at each step before its publication and again once it is visible.
    pub(in crate::install::settings) fn apply_with(
        plan: &Plan,
        inject: &mut dyn FnMut(Step) -> io::Result<()>,
    ) -> Result<Option<PathBuf>, ApplyError> {
        if plan.settings_after.is_none() && plan.state_after.is_none() {
            return Ok(None);
        }
        let fail = |step, outcome: &str| {
            let outcome = outcome.to_owned();
            move |source| ApplyError::Failed {
                step,
                source,
                outcome,
            }
        };
        let failed = |step, source, outcome: &str| ApplyError::Failed {
            step,
            source,
            outcome: outcome.to_owned(),
        };
        let mut quiet = |_| Ok(());
        let mut fs = Fs(&mut quiet);
        inject(Step::Lock).map_err(fail(Step::Lock, UNCHANGED))?;
        let state = fs
            .path(&plan.paths.local.join("state"))
            .map_err(fail(Step::Lock, UNCHANGED))?;
        let _lock = match fs.lock(&state) {
            Ok(lock) => lock,
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Err(ApplyError::Locked),
            Err(err) => return Err(fail(Step::Lock, UNCHANGED)(err)),
        };
        let claude = preflight(plan, &state)?;

        let Some(after) = &plan.settings_after else {
            // Only the records changed, so there is nothing to journal.
            let bytes = plan.state_after.as_deref().expect("checked above");
            let at = Step::Commit;
            publish(
                &state,
                state::FILE,
                plan.state_digest.as_deref(),
                bytes,
                0o600,
                &mut || inject(at),
            )
            .map_err(|err| {
                let outcome = if err.visible { COMMITTED } else { UNCHANGED };
                failed(at, err.source, outcome)
            })?;
            return Ok(None);
        };
        let backup = match &plan.settings {
            Some((bytes, _)) => {
                inject(Step::Backup).map_err(fail(Step::Backup, UNCHANGED))?;
                let name =
                    backup::save(&mut fs, &state, bytes).map_err(fail(Step::Backup, UNCHANGED))?;
                Some(backup::path(&plan.paths.local, &name))
            }
            None => None,
        };
        let mut state_digest = plan.state_digest.clone();
        if let Some(journal) = &plan.journal {
            let at = Step::Journal;
            publish(
                &state,
                state::FILE,
                state_digest.as_deref(),
                journal,
                0o600,
                &mut || inject(at),
            )
            .map_err(|err| {
                let outcome = if err.visible { JOURNALED } else { UNCHANGED };
                failed(at, err.source, outcome)
            })?;
            state_digest = Some(manifest::sha256(journal));
        }
        let before = plan
            .settings
            .as_ref()
            .map(|(bytes, seen)| (bytes, seen.digest.as_str()));
        let mode = plan.settings.as_ref().map_or(0o600, |(_, seen)| seen.mode);
        let claude = match claude {
            Some(claude) => claude,
            None => fs
                .path(&plan.paths.claude)
                .map_err(fail(Step::Settings, JOURNALED))?,
        };
        let published = publish(
            &claude,
            SETTINGS,
            before.map(|(_, digest)| digest),
            after,
            mode,
            &mut || inject(Step::Settings),
        );
        match published {
            // The new settings are visible: the journal stays so the next plan settles from it.
            Err(err) if err.visible => return Err(failed(Step::Settings, err.source, WRITTEN)),
            Err(err) => {
                // The journal never took effect; put the previous records back so they stay valid.
                let journal = state_digest.as_deref();
                let undone = match &plan.state_before {
                    Some(old) => publish(&state, state::FILE, journal, old, 0o600, &mut || Ok(()))
                        .or_else(|err| if err.visible { Ok(()) } else { Err(err.source) }),
                    None => state
                        .remove(state::FILE, |file| {
                            Ok(Some(digest(file)?) == journal.map(str::to_owned))
                        })
                        .map(|_| ()),
                };
                let outcome = if plan.journal.is_none() || undone.is_ok() {
                    UNCHANGED
                } else {
                    JOURNALED
                };
                return Err(failed(Step::Settings, err.source, outcome));
            }
            Ok(()) => {}
        }

        if let Some(bytes) = &plan.state_after {
            let committed = publish(
                &state,
                state::FILE,
                state_digest.as_deref(),
                bytes,
                0o600,
                &mut || inject(Step::Commit),
            );
            match committed {
                // The records are settled, so settings.json must not be rolled back under them.
                Err(err) if err.visible => return Err(failed(Step::Commit, err.source, COMMITTED)),
                Err(err) => {
                    let written = manifest::sha256(after);
                    // Undo only our own write; a newer user edit is never replaced.
                    let rollback = match before {
                        Some((old, _)) => {
                            publish(&claude, SETTINGS, Some(&written), old, mode, &mut || Ok(()))
                                .or_else(|err| if err.visible { Ok(()) } else { Err(err.source) })
                        }
                        None => unpublish(&claude, SETTINGS, &written),
                    };
                    let outcome = match rollback {
                        Ok(()) => {
                            "settings.json was rolled back; the next plan settles managed.json"
                                .to_owned()
                        }
                        Err(why) => format!(
                            "settings.json was not rolled back ({why}); the next plan settles managed.json"
                        ),
                    };
                    return Err(ApplyError::Failed {
                        step: Step::Commit,
                        source: err.source,
                        outcome,
                    });
                }
                Ok(()) => {}
            }
        }
        Ok(backup)
    }

    pub(super) fn restore(
        locations: &Locations,
        name: &str,
    ) -> Result<Option<PathBuf>, ApplyError> {
        // A removal plan reads and checks everything a restore needs; only its writes are replaced.
        let mut plan = plan(locations, None)?;
        let invalid = || ApplyError::InvalidBackup(name.to_owned());
        if !backup::is_name(name) {
            return Err(invalid());
        }
        let current = plan.settings.as_ref().map(|(_, seen)| seen.digest.clone());
        if current.is_none() || current != plan.last_write {
            return Err(ApplyError::Changed {
                detail: "settings.json is not Agenvil's last write".to_owned(),
            });
        }
        let state = Root::open(&plan.paths.local.join("state"))
            .map_err(|_| invalid())?
            .ok_or_else(invalid)?;
        let bytes = backup::dir(&state)
            .ok()
            .flatten()
            .and_then(|dir| read(Some(&dir), name).ok().flatten())
            .ok_or_else(invalid)?
            .0;
        plan.settings_after = Some(bytes);
        plan.state_after = None;
        plan.journal = None;
        apply_with(&plan, &mut |_| Ok(()))
    }

    /// Rechecks under the lock that roots, `settings.json` and `managed.json` are what was planned.
    fn preflight(plan: &Plan, state: &Root) -> Result<Option<Root>, ApplyError> {
        let changed = |detail: &str| ApplyError::Changed {
            detail: detail.to_owned(),
        };
        if Resolved::load(&plan.locations).ok().as_ref() != Some(&plan.paths) {
            return Err(changed("Claude config or local root changed"));
        }
        let roots = plan
            .paths
            .open()
            .map_err(|_| changed("Claude config or local root changed"))?;
        if !roots
            .state
            .as_ref()
            .is_some_and(|planned| same_dir(planned, state).unwrap_or(false))
        {
            return Err(changed("state directory replaced"));
        }
        let settings = read(roots.claude.as_ref(), SETTINGS)
            .map_err(|_| changed("settings.json is unreadable"))?
            .map(|(bytes, metadata)| seen(&bytes, &metadata));
        if settings.as_ref() != plan.settings.as_ref().map(|(_, seen)| seen) {
            return Err(changed("settings.json differs"));
        }
        let records = read(Some(state), state::FILE)
            .map_err(|_| changed("managed.json is unreadable"))?
            .map(|(bytes, _)| manifest::sha256(&bytes));
        if records != plan.state_digest {
            return Err(changed("managed.json differs"));
        }
        Ok(roots.claude)
    }

    fn same_dir(left: &Root, right: &Root) -> io::Result<bool> {
        let (a, b) = (rustix::fs::fstat(left)?, rustix::fs::fstat(right)?);
        Ok((a.st_dev, a.st_ino) == (b.st_dev, b.st_ino))
    }

    fn digest(mut file: &std::fs::File) -> io::Result<String> {
        let mut bytes = Vec::new();
        file.by_ref().take(MAX_BYTES + 1).read_to_end(&mut bytes)?;
        Ok(manifest::sha256(&bytes))
    }

    fn named(dir: &Root, name: &str) -> io::Result<Option<String>> {
        Ok(read(Some(dir), name)?.map(|(bytes, _)| manifest::sha256(&bytes)))
    }

    /// Publishes `bytes` as `name` atomically: no-replace when it is new, an exchange otherwise.
    /// A displaced generation that is not `expected` belongs to a concurrent writer: it is put
    /// back, or kept beside `name`, but never deleted. `inject` is a fault point before the
    /// publication and once it is visible.
    fn publish(
        dir: &Root,
        name: &str,
        expected: Option<&str>,
        bytes: &[u8],
        mode: u32,
        inject: &mut dyn FnMut() -> io::Result<()>,
    ) -> Result<(), PublishError> {
        if named(dir, name)?.as_deref() != expected {
            return Err(io::Error::other(format!("{name} changed before publication")).into());
        }
        let temp = temp_name("settings");
        write_file(dir, &temp, bytes, mode)?;
        let published = inject().and_then(|()| match expected {
            None => rename_new(dir, &temp, name),
            Some(_) => rename_with(dir, &temp, name, RenameFlags::EXCHANGE),
        });
        if let Err(err) = published {
            dir.remove(&temp, |_| Ok(true))?;
            return Err(err.into());
        }
        let visible = |source| PublishError {
            visible: true,
            source,
        };
        // Reported only after the displaced generation is checked, like a failed sync.
        let injected = inject();
        if let Some(expected) = expected {
            // Anything unreadable, a symlink or a special file fails the check too.
            if named(dir, &temp).ok().flatten().as_deref() != Some(expected) {
                return Err(recover(dir, name, &temp, &manifest::sha256(bytes)));
            }
            dir.remove(&temp, |file| Ok(digest(file)? == expected))
                .map_err(visible)?;
        }
        injected.map_err(visible)?;
        rustix::fs::fsync(dir).map_err(|err| visible(err.into()))?;
        Ok(())
    }

    /// Undoes an exchange that displaced a concurrent generation now held at `temp`. Best effort:
    /// pathname races remain, but only `ours` is ever deleted.
    fn recover(dir: &Root, name: &str, temp: &str, ours: &str) -> PublishError {
        let error = |visible, detail: String| PublishError {
            visible,
            source: io::Error::other(format!("{name} changed during publication; {detail}")),
        };
        let kept = format!("the displaced content was kept as {temp} beside {name}");
        // A newer writer already replaced ours, so swapping back would hide that generation.
        if named(dir, name).ok().flatten().as_deref() != Some(ours) {
            return error(false, kept);
        }
        if let Err(err) = rename_with(dir, temp, name, RenameFlags::EXCHANGE) {
            return error(true, format!("{kept} ({err})"));
        }
        // `temp` holds ours unless yet another writer got in between; that one is kept.
        match dir.remove(temp, |file| Ok(digest(file)? == ours)) {
            Ok(true) => error(false, "the concurrent content was put back".to_owned()),
            _ => error(
                false,
                format!("the concurrent content was put back and {temp} beside it was kept"),
            ),
        }
    }

    /// Removes a `settings.json` Agenvil created without unlinking the live name: it is first
    /// renamed aside, deleted only if it is still `written`, and put back otherwise.
    fn unpublish(dir: &Root, name: &str, written: &str) -> io::Result<()> {
        let aside = temp_name("settings");
        rename_new(dir, name, &aside)?;
        if matches!(
            dir.remove(&aside, |file| Ok(digest(file)? == written)),
            Ok(true)
        ) {
            return rustix::fs::fsync(dir).map_err(Into::into);
        }
        match rename_new(dir, &aside, name) {
            Ok(()) => Err(io::Error::other(format!(
                "{name} changed; it was left in place"
            ))),
            Err(err) => Err(io::Error::other(format!(
                "{name} changed and could not be put back ({err}); it was kept as {aside} beside it"
            ))),
        }
    }
}
