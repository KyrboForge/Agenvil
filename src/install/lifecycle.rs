//! Compose Claude lifecycle operations from the install and settings engines.
//!
//! Sync reconciles staged links and kit settings.

use std::path::{Path, PathBuf};

use serde::Serialize;

use super::{
    ApplyError, ApplyReport, Locations, Operation, OperationKind, Plan, PlanError, settings,
};
use crate::config::Config;
use crate::diagnostics::Exit;

pub use super::ledger::Intent as LinkIntent;
pub use super::settings::{
    Backup, RestorePlan, RestoreRecord, apply as settings_apply, plan as settings_plan,
    restore_apply, restore_backups, restore_plan,
};
pub use super::{apply as sync_apply, plan as sync_plan};

/// Read the optional fragment from the checkout, even when config was loaded via --config.
pub fn source_settings(
    root: &Path,
    config: &Config,
) -> Result<Option<settings::Fragment>, settings::Error> {
    config
        .targets
        .claude
        .settings
        .as_ref()
        .map(|source| settings::Fragment::read_source(root, &source.fragment))
        .transpose()
}

#[derive(Debug, Serialize)]
pub struct SyncPlan {
    #[serde(flatten)]
    pub links: Plan,
    pub settings: settings::Plan,
}

impl SyncPlan {
    pub fn with_settings(
        links: Plan,
        locations: &Locations,
        fragment: Option<&settings::Fragment>,
    ) -> Result<Self, settings::Error> {
        let mut settings = settings_plan(locations, fragment)?;
        if let Some(fragment) = fragment {
            settings.include_hook_commands(fragment);
        }
        Ok(Self { links, settings })
    }

    pub fn check_settings_conflicts(&self) -> Result<(), settings::ApplyError> {
        if self.settings.has_conflicts() {
            return Err(settings::ApplyError::Conflicts {
                count: self.settings.summary.conflict,
            });
        }
        Ok(())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum SyncApplyError {
    #[error(transparent)]
    Links(#[from] ApplyError),
    #[error(transparent)]
    SettingsConflict(#[from] settings::ApplyError),
    #[error("links applied; settings failed; re-run sync to retry: {source}")]
    Settings {
        source: settings::ApplyError,
        links: Box<ApplyReport>,
    },
}

impl SyncApplyError {
    pub fn exit(&self) -> Exit {
        match self {
            Self::Links(error) => error.exit(),
            Self::SettingsConflict(error) | Self::Settings { source: error, .. } => error.exit(),
        }
    }

    pub fn link_report(&self) -> Option<&ApplyReport> {
        match self {
            Self::Links(error) => error.report(),
            Self::SettingsConflict(_) => None,
            Self::Settings { links, .. } => Some(links),
        }
    }
}

pub fn sync_with_settings_apply(plan: &SyncPlan) -> Result<ApplyReport, SyncApplyError> {
    plan.check_settings_conflicts()?;
    let links = sync_apply(&plan.links)?;
    if let Err(source) = settings_apply(&plan.settings) {
        return Err(SyncApplyError::Settings {
            source,
            links: Box::new(links),
        });
    }
    Ok(links)
}

/// Read-only plan for detaching the recorded Claude user install.
#[derive(Debug, Serialize)]
pub struct UninstallPlan {
    links: Plan,
    settings: settings::Plan,
}

impl UninstallPlan {
    pub fn links(&self) -> &Plan {
        &self.links
    }

    pub fn settings(&self) -> &settings::Plan {
        &self.settings
    }

    pub fn has_conflicts(&self) -> bool {
        self.links.has_conflicts() || self.settings.has_conflicts()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum UninstallPlanError {
    #[error(transparent)]
    Links(#[from] PlanError),
    #[error(transparent)]
    Settings(#[from] settings::Error),
}

impl UninstallPlanError {
    pub fn exit(&self) -> Exit {
        match self {
            Self::Links(error) => error.exit(),
            Self::Settings(error) => error.exit(),
        }
    }
}

/// Plan from the ownership and settings records, with no checkout or staging required.
pub fn uninstall_plan(locations: &Locations) -> Result<UninstallPlan, UninstallPlanError> {
    Ok(UninstallPlan {
        links: super::plan::uninstall_plan(locations)?,
        settings: settings_plan(locations, None)?,
    })
}

#[derive(Debug, Serialize)]
pub struct UninstallReport {
    pub links: ApplyReport,
    pub link_conflicts: Vec<Operation>,
    pub settings_backup: Option<PathBuf>,
    pub settings_conflicts: Vec<settings::Operation>,
}

impl UninstallReport {
    pub fn has_conflicts(&self) -> bool {
        !self.link_conflicts.is_empty() || !self.settings_conflicts.is_empty()
    }
}

#[derive(Debug, thiserror::Error)]
pub enum UninstallApplyError {
    #[error(transparent)]
    Links(#[from] ApplyError),
    #[error("links processed; settings detach failed: {source}")]
    Settings {
        #[source]
        source: settings::ApplyError,
        links: Box<ApplyReport>,
    },
}

impl UninstallApplyError {
    pub fn exit(&self) -> Exit {
        match self {
            Self::Links(error) => error.exit(),
            Self::Settings { source, .. } => source.exit(),
        }
    }
}

/// Remove proven links first, then detach settings when the settings plan is conflict-free.
pub fn uninstall_apply(plan: &UninstallPlan) -> Result<UninstallReport, UninstallApplyError> {
    let links = super::apply::apply_reconciled(&plan.links)?;
    let settings_backup = match settings_apply(&plan.settings) {
        Ok(backup) => backup,
        Err(settings::ApplyError::Conflicts { .. }) => None,
        Err(source) => {
            return Err(UninstallApplyError::Settings {
                source,
                links: Box::new(links),
            });
        }
    };
    Ok(UninstallReport {
        links,
        link_conflicts: plan
            .links
            .operations()
            .iter()
            .filter(|op| op.operation == OperationKind::Conflict)
            .cloned()
            .collect(),
        settings_backup,
        settings_conflicts: plan
            .settings
            .operations
            .iter()
            .filter(|op| op.operation == settings::OperationKind::Conflict)
            .cloned()
            .collect(),
    })
}

#[derive(Debug, Serialize)]
pub struct PendingLink {
    pub destination: String,
    pub operation: LinkIntent,
}

#[derive(Debug, Serialize)]
pub struct PendingOperations {
    pub links: Vec<PendingLink>,
    pub settings: bool,
}

impl PendingOperations {
    pub fn is_empty(&self) -> bool {
        self.links.is_empty() && !self.settings
    }
}

/// Inspect recorded intents without locking or inspecting live link and settings contents.
pub fn pending_operations(locations: &Locations) -> Result<PendingOperations, UninstallPlanError> {
    let paths = super::actual::Resolved::load(locations)?;
    let roots = paths.open()?;
    let ledger = super::ledger::Ledger::read(roots.state.as_ref(), &paths.local, &paths.claude)
        .map_err(PlanError::from)?;
    Ok(PendingOperations {
        links: ledger
            .entries()
            .filter_map(|entry| {
                let pending = entry.pending.as_ref()?;
                Some(PendingLink {
                    destination: entry.destination.clone(),
                    operation: pending.operation.clone(),
                })
            })
            .collect(),
        settings: settings::has_pending(&paths, roots.state.as_ref())?,
    })
}
