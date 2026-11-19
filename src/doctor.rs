//! Read-only diagnosis of a Claude user installation.

use std::env;
use std::fs;
use std::io;
use std::path::Path;

use serde::Serialize;

use crate::config::{Config, ConfigError};
use crate::install::lifecycle;
use crate::install::settings;
use crate::install::{
    ConflictReason, Locations, Observed, Operation, Ownership, Plan, PlanError, Reason,
    ReconcileReason,
};
use crate::stage;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Status {
    Healthy,
    Missing,
    AuthenticationRequired,
    UserActionRequired,
    Drifted,
    Unsupported,
}

impl Status {
    pub fn label(self) -> &'static str {
        match self {
            Self::Healthy => "ok",
            Self::Missing => "MISS",
            Self::AuthenticationRequired => "LOGIN",
            Self::UserActionRequired => "CHECK",
            Self::Drifted => "DRIFT",
            Self::Unsupported => "OFF",
        }
    }

    fn priority(self, required: bool) -> u8 {
        match self {
            Self::Healthy | Self::Unsupported => 0,
            Self::Missing => u8::from(required),
            Self::Drifted => 2,
            Self::AuthenticationRequired => 3,
            Self::UserActionRequired => 4,
        }
    }
}

#[derive(Debug, Serialize)]
pub struct Check {
    pub id: &'static str,
    pub status: Status,
    pub summary: String,
    pub required: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub remediation: Option<&'static str>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub subject: Option<String>,
}

impl Check {
    fn new(id: &'static str, status: Status, summary: impl Into<String>) -> Self {
        Self {
            id,
            status,
            summary: summary.into(),
            required: true,
            detail: None,
            remediation: None,
            subject: None,
        }
    }

    fn detail(mut self, detail: impl Into<String>) -> Self {
        self.detail = Some(detail.into());
        self
    }

    fn remediation(mut self, remediation: &'static str) -> Self {
        self.remediation = Some(remediation);
        self
    }

    fn subject(mut self, subject: impl Into<String>) -> Self {
        self.subject = Some(subject.into());
        self
    }
}

#[derive(Debug, Default, Serialize)]
pub struct Summary {
    pub healthy: usize,
    pub missing: usize,
    pub authentication_required: usize,
    pub user_action_required: usize,
    pub drifted: usize,
    pub unsupported: usize,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub schema: u32,
    pub status: Status,
    pub checks: Vec<Check>,
    pub summary: Summary,
}

impl Report {
    fn new(checks: Vec<Check>) -> Self {
        let mut summary = Summary::default();
        let mut status = Status::Healthy;
        let mut highest = 0;
        for check in &checks {
            *match check.status {
                Status::Healthy => &mut summary.healthy,
                Status::Missing => &mut summary.missing,
                Status::AuthenticationRequired => &mut summary.authentication_required,
                Status::UserActionRequired => &mut summary.user_action_required,
                Status::Drifted => &mut summary.drifted,
                Status::Unsupported => &mut summary.unsupported,
            } += 1;
            let priority = check.status.priority(check.required);
            if priority > highest {
                highest = priority;
                status = check.status;
            }
        }
        Self {
            schema: 1,
            status,
            checks,
            summary,
        }
    }

    pub fn findings(&self) -> usize {
        self.checks
            .iter()
            .filter(|check| check.status.priority(check.required) > 0)
            .count()
    }
}

/// Compose this with the lifecycle facade for preflight or post-install checks.
/// A bad source config still permits independent checks of local state.
pub fn diagnose(
    root: &Path,
    config_path: Option<&Path>,
    out: &Path,
    locations: &Locations,
) -> Report {
    let default_config = root.join(crate::config::FILE_NAME);
    let config_path = config_path.unwrap_or(&default_config);
    let config = Config::load_file(config_path);
    let source = match &config {
        Ok(_) => Check::new(
            "source.config",
            Status::Healthy,
            "Source configuration is valid",
        ),
        Err(ConfigError::Read { source, .. }) if source.kind() == io::ErrorKind::NotFound => {
            Check::new(
                "source.config",
                Status::Missing,
                "Source configuration is missing",
            )
            .remediation("Select a project with agenvil.toml using --root or --config.")
        }
        Err(error) => Check::new(
            "source.config",
            Status::UserActionRequired,
            "Cannot load source configuration",
        )
        .detail(error.to_string())
        .remediation("Fix the reported configuration error and re-run doctor."),
    };
    let mut checks = vec![source.subject(config_path.display().to_string())];

    // Sync planning runs the same staging check as build --check before it opens local state.
    let link_plan = config
        .as_ref()
        .ok()
        .map(|config| lifecycle::sync_plan(root, config, out, locations));
    checks.push(staging_check(out, link_plan.as_ref()));
    match link_plan {
        Some(Ok(plan)) => check_links(&plan, &mut checks),
        Some(Err(error)) if !staging_error(&error) => checks.push(
            Check::new(
                "links.ownership",
                Status::UserActionRequired,
                "Cannot inspect link ownership",
            )
            .detail(error.to_string())
            .remediation(
                "Review the recorded kit and installation paths before re-running doctor.",
            ),
        ),
        _ => checks.push(Check::new(
            "links.ownership",
            Status::Unsupported,
            "Link check requires approved staging",
        )),
    }

    check_settings(root, config.as_ref().ok(), locations, &mut checks);
    checks.push(check_operations(locations));
    checks.push(prerequisite("prereq.claude_cli", "claude"));
    checks.push(prerequisite("prereq.git", "git"));

    let mut auth = Check::new("auth.claude", Status::Unsupported, "Claude authentication")
        .detail("not checked")
        .remediation("Run claude to sign in if needed.");
    auth.required = false;
    checks.push(auth);
    for (id, name) in [
        ("integration.rtk", "RTK"),
        ("integration.headroom", "Headroom"),
    ] {
        let mut check =
            Check::new(id, Status::Unsupported, name).detail("adapter not implemented yet");
        check.required = false;
        checks.push(check);
    }
    Report::new(checks)
}

fn staging_error(error: &PlanError) -> bool {
    matches!(
        error,
        PlanError::Staging { .. }
            | PlanError::Build(_)
            | PlanError::Inventory(_)
            | PlanError::Resource(_)
    )
}

fn staging_check(out: &Path, plan: Option<&Result<Plan, PlanError>>) -> Check {
    let id = "build.staging";
    let check = match plan {
        None => Check::new(
            id,
            Status::Unsupported,
            "Staging check requires valid source configuration",
        ),
        Some(Err(PlanError::Staging { findings, .. })) => {
            let (status, summary) = if stage::is_unsafe(findings) {
                (
                    Status::UserActionRequired,
                    "Staging contains unsafe entries",
                )
            } else if findings
                .iter()
                .any(|f| f.kind == stage::Kind::Missing && f.path == ".")
            {
                (Status::Missing, "Staging directory is missing")
            } else {
                (Status::Drifted, "Staging differs from sources")
            };
            Check::new(id, status, summary)
                .detail(stage::report(out, findings).trim_end())
                .remediation("Review the staging path and run agenvil build --target claude.")
        }
        Some(Err(error)) if staging_error(error) => {
            Check::new(id, Status::UserActionRequired, "Cannot verify staging")
                .detail(error.to_string())
                .remediation("Resolve the reported issue before rebuilding staging.")
        }
        Some(_) => Check::new(id, Status::Healthy, "Staging matches sources"),
    };
    check.subject(out.display().to_string())
}

fn check_links(plan: &Plan, checks: &mut Vec<Check>) {
    checks.push(
        Check::new(
            "links.ownership",
            Status::Healthy,
            "Compared link ownership with Claude configuration",
        )
        .subject(plan.root().display().to_string()),
    );
    for operation in plan.operations() {
        if let Some(check) = link_finding(operation) {
            checks.push(check);
        }
    }
}

fn link_finding(operation: &Operation) -> Option<Check> {
    use ConflictReason as Conflict;
    use ReconcileReason as Reconcile;

    let (id, status, summary, remediation) = match operation.reason {
        Reason::Reconcile(
            Reconcile::Missing | Reconcile::ManagedLinkRemoved | Reconcile::RestoreSnapshot,
        )
        | Reason::Conflict(Conflict::BrokenManagedTarget) => (
            "links.missing",
            Status::Missing,
            "Link or its managed target is missing",
            "Review the plan and re-run sync --apply to repair the link.",
        ),
        Reason::Reconcile(Reconcile::Changed | Reconcile::Obsolete)
            if missing_target(operation) =>
        {
            (
                "links.missing",
                Status::Missing,
                "Managed link target is missing",
                "Review the plan and re-run sync --apply to reconcile the link.",
            )
        }
        Reason::Conflict(Conflict::CorruptSnapshot) => (
            "links.conflict",
            Status::Drifted,
            "Managed snapshot has changed",
            "Inspect and preserve the changed snapshot before repairing the installation.",
        ),
        Reason::Conflict(_) => (
            "links.conflict",
            Status::UserActionRequired,
            "Link conflict needs review",
            "Review the destination and resolve the conflict before applying sync.",
        ),
        Reason::Reconcile(Reconcile::Changed | Reconcile::Obsolete) => (
            "links.ownership",
            Status::Drifted,
            "Managed link differs from the desired install",
            "Review the plan and re-run sync --apply.",
        ),
        Reason::Reconcile(Reconcile::Current | Reconcile::Pending) => return None,
    };
    Some(
        Check::new(id, status, summary)
            .detail(operation.reason.to_string())
            .remediation(remediation)
            .subject(&operation.destination),
    )
}

// Team snapshot verification is part of sync planning. This also catches a
// recorded private source removed since sync, without opening its contents.
fn missing_target(operation: &Operation) -> bool {
    operation.ownership == Ownership::Managed
        && matches!(&operation.observed, Observed::Symlink { target }
            if fs::metadata(target).is_err_and(|e| e.kind() == io::ErrorKind::NotFound))
}

fn check_settings(
    root: &Path,
    config: Option<&Config>,
    locations: &Locations,
    checks: &mut Vec<Check>,
) {
    let source = config.map(|config| lifecycle::source_settings(root, config));
    let desired = source.as_ref().and_then(|source| source.as_ref().ok());
    let plan = lifecycle::settings_plan(locations, desired.and_then(Option::as_ref));
    let source_usable = desired.is_some();
    checks.push(check_settings_consistency(&plan));
    if let Some(source) = source {
        let id = "settings.source";
        let check = match source {
            Ok(Some(_)) => Check::new(id, Status::Healthy, "Kit settings fragment valid"),
            Ok(None) => Check::new(id, Status::Healthy, "Kit declares no settings fragment"),
            Err(error) => {
                let (status, summary) = if matches!(error, settings::Error::MissingFragment { .. })
                {
                    (Status::Missing, "Kit settings fragment missing")
                } else {
                    (
                        Status::UserActionRequired,
                        "Kit settings fragment invalid or unsafe",
                    )
                };
                Check::new(id, status, summary)
                    .detail(error.to_string())
                    .remediation("Fix the declared fragment before re-running sync.")
            }
        };
        checks.push(check);
        if source_usable {
            checks.push(check_settings_desired(&plan));
        }
    }
}

fn check_settings_consistency(plan: &Result<settings::Plan, settings::Error>) -> Check {
    use settings::Reason::*;
    let id = "settings.consistency";
    let plan = match plan {
        Ok(plan) => plan,
        Err(error) => {
            return Check::new(
                id,
                Status::UserActionRequired,
                "Cannot inspect managed settings",
            )
            .detail(error.to_string())
            .remediation("Review the reported settings or managed state file.");
        }
    };

    let mut status = Status::Healthy;
    let mut details = Vec::new();
    for op in &plan.operations {
        let finding = match op.reason {
            TypeClash | HookDuplicate => Status::UserActionRequired,
            UserEdited | UserRemoved | Gone | HookModified => Status::Drifted,
            _ => continue,
        };
        if finding.priority(true) > status.priority(true) {
            status = finding;
        }
        details.push(setting_detail(op));
    }
    let subject = plan.files.settings.path.display().to_string();
    if details.is_empty() {
        Check::new(id, status, "Managed settings and hooks are consistent").subject(subject)
    } else {
        Check::new(
            id,
            status,
            "Managed settings or hooks differ from recorded values",
        )
        .detail(details.join("\n"))
        .remediation("Review the listed values and hooks before applying settings changes.")
        .subject(subject)
    }
}

fn check_settings_desired(plan: &Result<settings::Plan, settings::Error>) -> Check {
    use settings::{FileAction, OperationKind};
    let id = "settings.desired";
    let plan = match plan {
        Ok(plan) => plan,
        Err(error) => {
            return Check::new(
                id,
                Status::UserActionRequired,
                "Cannot compare settings with the kit",
            )
            .detail(error.to_string());
        }
    };
    let remediation = "Preview sync and review the settings operations.";
    if plan.has_conflicts() {
        Check::new(
            id,
            Status::UserActionRequired,
            "Settings conflicts block sync",
        )
        .detail(
            plan.operations
                .iter()
                .filter(|op| op.operation == OperationKind::Conflict)
                .map(setting_detail)
                .collect::<Vec<_>>()
                .join("\n"),
        )
        .remediation(remediation)
    } else if plan.files.settings.action != FileAction::Unchanged
        || plan.files.state.action != FileAction::Unchanged
        || plan
            .operations
            .iter()
            .any(|op| op.operation != OperationKind::Noop)
    {
        Check::new(id, Status::Drifted, "Kit settings need sync").remediation(remediation)
    } else {
        Check::new(id, Status::Healthy, "Settings match the kit")
    }
}

fn setting_detail(op: &settings::Operation) -> String {
    match &op.event {
        Some(event) => format!("{event}/{} ({})", op.target, op.reason),
        None => format!("{} ({})", op.target, op.reason),
    }
}

fn check_operations(locations: &Locations) -> Check {
    let id = "operations.interrupted";
    match lifecycle::pending_operations(locations) {
        Ok(pending) if pending.is_empty() => {
            Check::new(id, Status::Healthy, "No interrupted operations")
        }
        Ok(pending) => {
            let mut details: Vec<_> = pending
                .links
                .iter()
                .map(|link| format!("{:?} {}", link.operation, link.destination))
                .collect();
            if pending.settings {
                details.push("Settings journal is pending".to_owned());
            }
            Check::new(
                id,
                Status::UserActionRequired,
                "Interrupted operations need recovery",
            )
            .detail(details.join("\n"))
            .remediation("re-run sync/uninstall/restore --apply")
        }
        Err(error) => Check::new(
            id,
            Status::UserActionRequired,
            "Cannot inspect operation journals",
        )
        .detail(error.to_string())
        .remediation("Review the reported state file or installation path."),
    }
}

fn prerequisite(id: &'static str, name: &str) -> Check {
    let found = env::var_os("PATH")
        .is_some_and(|path| env::split_paths(&path).any(|dir| executable_in(&dir, name)));
    let (status, summary) = if found {
        (Status::Healthy, format!("{name} is on PATH"))
    } else {
        (Status::Missing, format!("{name} is missing from PATH"))
    };
    let mut check = Check::new(id, status, summary);
    check.required = false;
    check
}

fn executable_in(dir: &Path, name: &str) -> bool {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::metadata(dir.join(name))
            .is_ok_and(|meta| meta.is_file() && meta.permissions().mode() & 0o111 != 0)
    }
    #[cfg(windows)]
    {
        ["exe", "com", "cmd", "bat"]
            .iter()
            .any(|ext| dir.join(name).with_extension(ext).is_file())
    }
    #[cfg(not(any(unix, windows)))]
    {
        dir.join(name).is_file()
    }
}
