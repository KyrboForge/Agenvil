use std::path::PathBuf;

use serde::Serialize;
use serde_json::Value;

use super::{
    ApplyError, Backup, EntryKind, Error, Inputs, Plan, Resolved, apply, backup, merge, read,
    settings_error,
};
use crate::install::Locations;

/// A managed record considered for retention during restore.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct RestoreRecord {
    pub kind: EntryKind,
    /// A JSON pointer for values, or the stable ID for hook groups.
    pub target: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub event: Option<String>,
}

/// Read-only preview. Apply backs up the current settings before replacing them.
#[derive(Debug, Serialize)]
pub struct RestorePlan {
    pub backup: Backup,
    pub current_sha256: String,
    pub retained: Vec<RestoreRecord>,
    pub dropped: Vec<RestoreRecord>,
    #[serde(skip)]
    pub(super) transaction: Plan,
}

/// List regular settings backups with matching digests without creating state.
pub fn restore_backups(locations: &Locations) -> Result<Vec<Backup>, Error> {
    let paths = Resolved::load(locations)?;
    let roots = paths.open()?;
    backup::list(roots.state.as_ref()).map_err(|err| {
        settings_error(
            &paths.local.join("state/backups/claude-settings"),
            err.to_string(),
        )
    })
}

/// Preview restoring a backup and keeping ownership only for records still present.
pub fn restore_plan(locations: &Locations, name: &str) -> Result<RestorePlan, ApplyError> {
    let invalid = || ApplyError::InvalidBackup(name.to_owned());
    if !backup::is_name(name) {
        return Err(invalid());
    }
    let inputs = Inputs::load(locations)?;
    let current = inputs
        .digest
        .as_ref()
        .filter(|_| inputs.digest == inputs.last_write)
        .ok_or_else(|| ApplyError::Changed {
            detail: "settings.json is not Agenvil's last write; reconcile it or restore manually"
                .to_owned(),
        })?
        .clone();
    let roots = inputs.paths.open().map_err(Error::from)?;
    let dir = roots
        .state
        .as_ref()
        .and_then(|state| backup::dir(state).ok().flatten())
        .ok_or_else(invalid)?;
    let (bytes, _) = read(Some(&dir), name).ok().flatten().ok_or_else(invalid)?;
    let backup = Backup::verified(name, &bytes).ok_or_else(invalid)?;
    let document = serde_json::from_slice::<Value>(&bytes)
        .ok()
        .filter(Value::is_object)
        .ok_or_else(invalid)?;
    let mut retained = Vec::new();
    let mut dropped = Vec::new();
    let mut report = |matches, record| {
        if matches {
            retained.push(record);
        } else {
            dropped.push(record);
        }
        matches
    };
    let values = inputs
        .values
        .iter()
        .filter(|record| {
            report(
                merge::matches_value(&document, record),
                RestoreRecord {
                    kind: EntryKind::Value,
                    target: record.pointer.clone(),
                    event: None,
                },
            )
        })
        .cloned()
        .collect();
    let hooks = inputs
        .hooks
        .iter()
        .filter(|record| {
            report(
                merge::matches_hook(&document, record),
                RestoreRecord {
                    kind: EntryKind::Hook,
                    target: record.id.clone(),
                    event: Some(record.event.clone()),
                },
            )
        })
        .cloned()
        .collect();
    let transaction = inputs.transaction(Vec::new(), values, hooks, Some(bytes))?;
    Ok(RestorePlan {
        backup,
        current_sha256: current,
        retained,
        dropped,
        transaction,
    })
}

/// Restore the planned backup and settle managed records through the settings journal.
pub fn restore_apply(plan: &RestorePlan) -> Result<Option<PathBuf>, ApplyError> {
    apply(&plan.transaction)
}

/// Restore a backup over Agenvil's last write and return the backup of the displaced settings.
pub fn restore(locations: &Locations, name: &str) -> Result<Option<PathBuf>, ApplyError> {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        restore_apply(&restore_plan(locations, name)?)
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = (locations, name);
        Err(ApplyError::Unsupported)
    }
}
