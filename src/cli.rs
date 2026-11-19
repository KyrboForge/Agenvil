//! Command line interface.

use std::path::{Path, PathBuf};

use clap::{Parser, Subcommand, ValueEnum};

use crate::build;
use crate::config::{Config, ConfigError};
use crate::diagnostics::Exit;
use crate::discovery::{Limits, OverlapError};
use crate::doctor;
use crate::install;
use crate::install::lifecycle;
use crate::inventory::Inventory;
use crate::report;
use crate::stage;
use crate::validate::{self, Check, Options, Plan, SettingsError, Severity};

#[derive(Debug, Parser)]
#[command(name = "agenvil", version, about)]
pub struct Cli {
    /// Project root containing agenvil.toml [default: current directory]
    #[arg(long, global = true, value_name = "DIR")]
    pub root: Option<PathBuf>,

    /// Configuration file [default: <root>/agenvil.toml]
    #[arg(long, global = true, value_name = "FILE")]
    pub config: Option<PathBuf>,

    #[command(subcommand)]
    pub command: Command,
}

#[derive(Debug, Subcommand)]
pub enum Command {
    /// Validate the source tree (read-only, offline)
    Check {
        /// Run only the rules that need no subprocess and no slow work
        #[arg(long)]
        fast: bool,
        #[arg(long, value_enum, default_value_t = CheckFormat::Human)]
        format: CheckFormat,
        /// Run only these rule IDs (comma-separated)
        #[arg(long, value_delimiter = ',', value_name = "IDS")]
        only: Vec<String>,
        /// Do not run these rule IDs (comma-separated)
        #[arg(long, value_delimiter = ',', value_name = "IDS")]
        skip: Vec<String>,
        /// Lowest severity that makes the exit code 1
        #[arg(long, value_enum, default_value_t = FailOn::Error)]
        fail_on: FailOn,
        /// Human format: print findings only, without fix hints or the summary
        #[arg(long)]
        quiet: bool,
    },
    /// List the discovered source resources (read-only, offline)
    Inventory {
        #[arg(long, value_enum, default_value_t = Format::Json)]
        format: Format,
    },
    /// List the validation rules (read-only, needs no config)
    Rules,
    /// Explain one rule ID (read-only, needs no config)
    Explain {
        /// Stable rule ID, for example AG001
        id: String,
    },
    /// Build a staging tree for one target; never installs or touches ~/.claude (mutating)
    Build {
        /// Target to build for
        #[arg(long, value_enum)]
        target: Target,
        /// Output directory, replaced only if empty or written by an earlier build
        /// [default: <root>/.generated/claude]
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
        /// Compare the staging tree with what a build would write; fully read-only
        #[arg(long, conflicts_with_all = ["clean", "apply"])]
        check: bool,
        /// Remove files an earlier build wrote that are no longer generated; dry run unless --apply
        #[arg(long)]
        clean: bool,
        /// With --clean: really delete the listed files
        #[arg(long, requires = "clean")]
        apply: bool,
    },
    /// Plan or apply managed Claude resource links and settings
    Sync {
        #[arg(long, value_enum)]
        target: Target,
        /// Approved staging directory [default: <root>/.generated/claude]
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
        /// Apply the plan to the user configuration
        #[arg(long)]
        apply: bool,
        /// Print the plan without making changes (the default)
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
        #[arg(long, value_enum, default_value_t = SyncFormat::Human)]
        format: SyncFormat,
    },
    /// Plan or apply managed Claude settings values and hooks (needs no config)
    Settings {
        #[arg(long, value_enum)]
        target: Target,
        /// Settings fragment JSON to install
        #[arg(
            long,
            value_name = "FILE",
            required_unless_present = "remove",
            conflicts_with = "remove"
        )]
        fragment: Option<PathBuf>,
        /// Remove everything Agenvil recorded as managed
        #[arg(long)]
        remove: bool,
        /// Apply the plan to the user settings
        #[arg(long)]
        apply: bool,
        /// Print the plan without making changes (the default)
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
        #[arg(long, value_enum, default_value_t = SyncFormat::Human)]
        format: SyncFormat,
    },
    /// Diagnose the Claude installation and its source and staging trees (read-only)
    Doctor {
        #[arg(long, value_enum)]
        target: Target,
        /// Approved staging directory [default: <root>/.generated/claude]
        #[arg(long, value_name = "DIR")]
        out: Option<PathBuf>,
        #[arg(long, value_enum, default_value_t = SyncFormat::Human)]
        format: SyncFormat,
    },
    /// Detach managed Claude links and settings (needs no config)
    Uninstall {
        #[arg(long, value_enum)]
        target: Target,
        /// Apply the plan to user configuration
        #[arg(long)]
        apply: bool,
        /// Print the plan without making changes (the default)
        #[arg(long, conflicts_with = "apply")]
        dry_run: bool,
        #[arg(long, value_enum, default_value_t = SyncFormat::Human)]
        format: SyncFormat,
    },
    /// List or restore managed Claude settings backups (needs no config)
    Restore {
        #[arg(long, value_enum)]
        target: Target,
        /// List settings backups, newest first
        #[arg(
            long,
            required_unless_present = "backup",
            conflicts_with_all = ["backup", "apply", "dry_run"]
        )]
        list: bool,
        /// Name of the settings backup to restore
        #[arg(long, value_name = "NAME")]
        backup: Option<String>,
        /// Restore the selected backup
        #[arg(long, requires = "backup")]
        apply: bool,
        /// Preview the restore without making changes (the default)
        #[arg(long, requires = "backup", conflicts_with = "apply")]
        dry_run: bool,
        #[arg(long, value_enum, default_value_t = SyncFormat::Human)]
        format: SyncFormat,
    },
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Target {
    Claude,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum Format {
    Json,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum SyncFormat {
    Human,
    Json,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum CheckFormat {
    Human,
    Json,
    Github,
}

#[derive(Clone, Copy, Debug, ValueEnum)]
pub enum FailOn {
    Error,
    Warning,
}

impl FailOn {
    fn reached_by(self, severity: Severity) -> bool {
        match self {
            Self::Error => severity == Severity::Error,
            Self::Warning => true,
        }
    }
}

#[derive(Debug, thiserror::Error)]
enum Error {
    #[error(transparent)]
    Config(#[from] ConfigError),
    #[error(transparent)]
    Build(#[from] build::Error),
    #[error(transparent)]
    Overlap(#[from] OverlapError),
    #[error(transparent)]
    Settings(#[from] SettingsError),
    #[error("unknown rule ID `{0}` (run `agenvil rules` to list the IDs)")]
    UnknownRule(String),
    #[error("could not serialize the output: {0}")]
    Serialize(#[from] serde_json::Error),
    #[error(transparent)]
    SyncPlan(#[from] install::PlanError),
    #[error(transparent)]
    SyncApply(#[from] lifecycle::SyncApplyError),
    #[error(transparent)]
    ManagedSettings(#[from] install::settings::Error),
    #[error(transparent)]
    ManagedApply(#[from] install::settings::ApplyError),
    #[error(transparent)]
    UninstallPlan(#[from] lifecycle::UninstallPlanError),
    #[error(transparent)]
    UninstallApply(#[from] lifecycle::UninstallApplyError),
    #[error("{0} finding(s) reported")]
    LifecycleFindings(usize),
    #[error("could not determine your home directory for sync")]
    NoHome,
    #[error("the sync plan has {0} unsafe destination(s); check the directory paths")]
    UnsafeSync(usize),
    /// Output was printed, but problems mean it is incomplete.
    #[error("{0} problem(s) found; the result above is incomplete")]
    Incomplete(usize),
    /// Selected rules that have no implementation.
    #[error(
        "cannot verify {}: selected but not implemented, so the result is incomplete; drop them \
         from `--only`, or set them to \"off\" under [rules] in agenvil.toml",
        .0.join(", ")
    )]
    Unavailable(Vec<&'static str>),
    /// The staging tree differs from the sources; the report was printed.
    #[error("the staging tree is out of date: {0} finding(s)")]
    Drift(usize),
    /// The staging tree holds a symlink, special file or escaping path; the report was printed.
    #[error("the staging tree is not safe to trust or touch: {0} finding(s)")]
    UnsafeTree(usize),
    /// Findings at or above the `--fail-on` threshold; already printed.
    #[error("{0} finding(s) at or above the `--fail-on` level")]
    Findings(usize),
}

impl Error {
    fn exit(&self) -> Exit {
        match self {
            Self::Config(_)
            | Self::Overlap(_)
            | Self::Settings(_)
            | Self::UnknownRule(_)
            | Self::UnsafeTree(_)
            | Self::NoHome
            | Self::UnsafeSync(_) => Exit::Usage,
            Self::Incomplete(_) | Self::Unavailable(_) => Exit::Unverifiable,
            Self::Build(error) => error.exit(),
            Self::SyncPlan(error) => error.exit(),
            Self::SyncApply(error) => error.exit(),
            Self::ManagedSettings(error) => error.exit(),
            Self::ManagedApply(error) => error.exit(),
            Self::UninstallPlan(error) => error.exit(),
            Self::UninstallApply(error) => error.exit(),
            Self::LifecycleFindings(_) => Exit::Findings,
            Self::Findings(_) | Self::Drift(_) => Exit::Findings,
            Self::Serialize(_) => Exit::Internal,
        }
    }
}

/// Runs the command, reporting errors on stderr.
pub fn run(cli: &Cli) -> Exit {
    match execute(cli) {
        Ok(()) => Exit::Success,
        Err(error) => {
            eprintln!("error: {error}");
            error.exit()
        }
    }
}

fn execute(cli: &Cli) -> Result<(), Error> {
    let root = cli.root.as_deref().unwrap_or(Path::new("."));
    match &cli.command {
        // These describe the built-in catalog, so they work without a project.
        Command::Rules => {
            print!("{}", rules_table());
            return Ok(());
        }
        Command::Explain { id } => {
            let rule = validate::find(id).ok_or_else(|| Error::UnknownRule(id.clone()))?;
            print!("{}", explain(rule));
            return Ok(());
        }
        Command::Settings {
            target: Target::Claude,
            fragment,
            apply,
            format,
            ..
        } => return settings(fragment.as_deref(), *apply, *format),
        Command::Doctor {
            target: Target::Claude,
            out,
            format,
        } => {
            let out = out.clone().unwrap_or_else(|| root.join(build::DEFAULT_OUT));
            return diagnose(root, cli.config.as_deref(), &out, *format);
        }
        Command::Uninstall {
            target: Target::Claude,
            apply,
            format,
            ..
        } => return uninstall(*apply, *format),
        Command::Restore {
            target: Target::Claude,
            backup,
            apply,
            format,
            ..
        } => return restore(backup.as_deref(), *apply, *format),
        _ => {}
    }
    // Load first so a broken config is never mistaken for a clean or merely unimplemented run.
    let config = match &cli.config {
        Some(path) => Config::load_file(path)?,
        None => Config::load(root)?,
    };
    match &cli.command {
        Command::Inventory {
            format: Format::Json,
        } => {
            let inventory = Inventory::build(root, &config, &Limits::default())?;
            print!("{}", inventory.to_json()?);
            // Problems are untrusted-input failures, not policy findings, so this is "no verdict".
            match inventory.problems.len() {
                0 => Ok(()),
                n => Err(Error::Incomplete(n)),
            }
        }
        Command::Check {
            fast,
            format,
            only,
            skip,
            fail_on,
            quiet,
        } => {
            let options = Options {
                only,
                skip,
                fast: *fast,
            };
            let plan = Plan::new(&config, &options)?;
            let inventory = Inventory::build(root, &config, &Limits::default())?;
            let mut outcome = validate::run(root, &config, &inventory, &plan);
            if let Some(source) = &config.paths.instructions {
                match build::instructions(root, &config, source.as_path(), &build::limits()) {
                    Ok(_) => {}
                    Err(Ok(issue)) => return Err(build::Error::Invalid(vec![issue]).into()),
                    Err(Err(problem)) => {
                        outcome.problems.push(problem);
                        outcome.problems.sort();
                    }
                }
            }
            let findings = &outcome.findings;
            match format {
                CheckFormat::Human => print!("{}", report::human(findings, *quiet)),
                CheckFormat::Json => print!("{}", report::json(findings)?),
                CheckFormat::Github => print!("{}", report::github(findings)),
            }
            eprint!("{}", report::problems(&outcome.problems));
            if !plan.deferred.is_empty() {
                // Not run and not a failure: say so rather than let silence read as "passed".
                eprintln!(
                    "note: {} rule(s) are not implemented yet and were not run: {} \
                     (see `agenvil rules`)",
                    plan.deferred.len(),
                    plan.deferred.join(", ")
                );
            }
            // No verdict beats any finding: an incomplete run must not look like a clean or a
            // merely failing one.
            if !plan.unavailable.is_empty() {
                return Err(Error::Unavailable(plan.unavailable));
            }
            if !outcome.problems.is_empty() {
                return Err(Error::Incomplete(outcome.problems.len()));
            }
            match findings
                .iter()
                .filter(|f| fail_on.reached_by(f.severity))
                .count()
            {
                0 => Ok(()),
                n => Err(Error::Findings(n)),
            }
        }
        Command::Build {
            target: Target::Claude,
            out,
            check,
            clean,
            apply,
        } => {
            lifecycle::source_settings(root, &config)?;
            let inventory = Inventory::build(root, &config, &build::limits())?;
            let out = out.clone().unwrap_or_else(|| root.join(build::DEFAULT_OUT));
            if *check || *clean {
                // The same plan a build writes, held in memory; nothing below creates anything.
                let expected = build::expected(root, &config, &inventory)?;
                return if *check {
                    check_tree(root, &config, &out, &expected)
                } else {
                    clean_tree(root, &config, &out, &expected, *apply)
                };
            }
            let built = build::claude(root, &config, &inventory, &out)?;
            println!(
                "built {} agent(s), {} skill(s) and {} rule(s){} as {} file(s) in {}\n\
                 staging only: nothing was installed; copy or link the tree yourself",
                built.agents,
                built.skills,
                built.rules,
                if built.instructions {
                    " plus CLAUDE.md"
                } else {
                    ""
                },
                built.files,
                built.out.display()
            );
            Ok(())
        }
        Command::Sync {
            target: Target::Claude,
            out,
            apply,
            format,
            ..
        } => {
            let out = out.clone().unwrap_or_else(|| root.join(build::DEFAULT_OUT));
            sync(root, &config, &out, *apply, *format)
        }
        Command::Rules
        | Command::Explain { .. }
        | Command::Settings { .. }
        | Command::Doctor { .. }
        | Command::Uninstall { .. }
        | Command::Restore { .. } => unreachable!("handled before the config is loaded"),
    }
}

fn diagnose(
    root: &Path,
    config: Option<&Path>,
    out: &Path,
    format: SyncFormat,
) -> Result<(), Error> {
    let report = doctor::diagnose(root, config, out, &locations()?);
    match format {
        SyncFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        SyncFormat::Human => {
            for check in &report.checks {
                print!("{:<5} {} {}", check.status.label(), check.id, check.summary);
                if let Some(subject) = &check.subject {
                    print!(" ({subject})");
                }
                println!();
                if let Some(detail) = &check.detail {
                    for line in detail.lines() {
                        println!("      {line}");
                    }
                }
                if let Some(remediation) = check.remediation {
                    println!("      {remediation}");
                }
            }
            println!(
                "{} overall, {} healthy, {} missing, {} login, {} check, {} drifted, {} unsupported",
                report.status.label(),
                report.summary.healthy,
                report.summary.missing,
                report.summary.authentication_required,
                report.summary.user_action_required,
                report.summary.drifted,
                report.summary.unsupported,
            );
        }
    }
    lifecycle_verdict(report.findings())
}

fn uninstall(apply: bool, format: SyncFormat) -> Result<(), Error> {
    let plan = lifecycle::uninstall_plan(&locations()?)?;
    if matches!(format, SyncFormat::Human) {
        if plan.links().operations().is_empty() && plan.settings().operations.is_empty() {
            println!("nothing to uninstall");
        } else {
            println!(
                "Uninstall plan for Claude config at {}",
                plan.links().root().display()
            );
            for op in plan.links().operations() {
                println!("{:?} link {} ({})", op.operation, op.destination, op.reason);
            }
            for op in &plan.settings().operations {
                print!("{:?} {:?} {}", op.operation, op.kind, op.target);
                if let Some(event) = &op.event {
                    print!(" [{event}]");
                }
                println!(" ({})", op.reason);
            }
        }
    }
    if !apply {
        if matches!(format, SyncFormat::Json) {
            println!("{}", serde_json::to_string_pretty(&plan)?);
        }
        return lifecycle_verdict(
            plan.links().summary().conflict + plan.settings().summary.conflict,
        );
    }

    let report = match lifecycle::uninstall_apply(&plan) {
        Ok(report) => report,
        Err(error) => {
            let links = match &error {
                lifecycle::UninstallApplyError::Links(err) => err.report(),
                lifecycle::UninstallApplyError::Settings { links, .. } => Some(links.as_ref()),
            };
            if let Some(links) = links {
                eprintln!("{}", serde_json::to_string_pretty(links)?);
            }
            return Err(error.into());
        }
    };
    match format {
        SyncFormat::Json => println!("{}", serde_json::to_string_pretty(&report)?),
        SyncFormat::Human => {
            if report.links.ledger != install::LedgerState::Unchanged {
                println!("Ledger {:?}", report.links.ledger);
            }
            if let Some(backup) = &report.settings_backup {
                println!("Backup {}", backup.display());
            }
            if !report.settings_conflicts.is_empty() {
                println!("Settings changes skipped because of conflicts");
            }
        }
    }
    lifecycle_verdict(report.link_conflicts.len() + report.settings_conflicts.len())
}

fn restore(backup: Option<&str>, apply: bool, format: SyncFormat) -> Result<(), Error> {
    let locations = locations()?;
    let Some(name) = backup else {
        let mut backups = lifecycle::restore_backups(&locations)?;
        backups.sort_unstable_by(|a, b| b.name.cmp(&a.name));
        match format {
            SyncFormat::Json => println!("{}", serde_json::to_string_pretty(&backups)?),
            SyncFormat::Human if backups.is_empty() => println!("No settings backups"),
            SyncFormat::Human => {
                for backup in &backups {
                    println!(
                        "{}  {} UTC Unix ns  {} bytes  SHA-256 {}",
                        backup.name, backup.timestamp_utc_ns, backup.size, backup.sha256
                    );
                }
            }
        }
        return Ok(());
    };

    let plan = lifecycle::restore_plan(&locations, name)?;
    let settings_backup = if apply {
        lifecycle::restore_apply(&plan)?
    } else {
        None
    };
    match format {
        SyncFormat::Json if apply => {
            #[derive(serde::Serialize)]
            struct RestoreReport<'a> {
                #[serde(flatten)]
                plan: &'a lifecycle::RestorePlan,
                settings_backup: Option<PathBuf>,
            }
            let report = RestoreReport {
                plan: &plan,
                settings_backup,
            };
            println!("{}", serde_json::to_string_pretty(&report)?);
        }
        SyncFormat::Json => println!("{}", serde_json::to_string_pretty(&plan)?),
        SyncFormat::Human => {
            let action = if apply { "Restored" } else { "Would restore" };
            println!(
                "{action} {} (SHA-256 {} -> {})",
                plan.backup.name, plan.current_sha256, plan.backup.sha256
            );
            for (action, records) in [("Keep", &plan.retained), ("Drop", &plan.dropped)] {
                for record in records {
                    print!("{action} {:?} {}", record.kind, record.target);
                    if let Some(event) = &record.event {
                        print!(" [{event}]");
                    }
                    println!();
                }
            }
            if !apply {
                println!("Apply backs up current settings before restoring.");
            }
            if let Some(backup) = settings_backup {
                println!("Backup {}", backup.display());
            }
        }
    }
    Ok(())
}

fn lifecycle_verdict(findings: usize) -> Result<(), Error> {
    if findings == 0 {
        Ok(())
    } else {
        Err(Error::LifecycleFindings(findings))
    }
}

fn sync(
    root: &Path,
    config: &Config,
    out: &Path,
    apply: bool,
    format: SyncFormat,
) -> Result<(), Error> {
    let fragment = lifecycle::source_settings(root, config)?;
    let locations = locations()?;
    let links = lifecycle::sync_plan(root, config, out, &locations).inspect_err(|error| {
        if let install::PlanError::Staging { out, findings } = error {
            eprint!("{}", stage::report(out, findings));
        }
    })?;
    let plan = lifecycle::SyncPlan::with_settings(links, &locations, fragment.as_ref())?;
    let links = &plan.links;
    match format {
        SyncFormat::Json => println!("{}", serde_json::to_string_pretty(&plan)?),
        SyncFormat::Human => {
            println!("Claude config at {}", links.root().display());
            for op in links.operations() {
                print!("{:?} {}", op.operation, op.destination);
                if let Some(target) = &op.link_target {
                    print!(" -> {}", target.display());
                }
                println!(" ({})", op.reason);
            }
            for snapshot in links.snapshots() {
                if snapshot.status != install::SnapshotStatus::Verified {
                    println!("{:?} snapshot {}", snapshot.status, snapshot.path.display());
                }
            }
            for entry in links.ignored() {
                println!("Ignored private {} ({:?})", entry.path, entry.reason);
            }
            let counts = links.summary();
            println!(
                "{} create, {} update, {} remove, {} unchanged, {} conflict",
                counts.create, counts.update, counts.remove, counts.noop, counts.conflict
            );
            println!("Claude settings");
            print_settings_plan(&plan.settings);
        }
    }
    plan.check_settings_conflicts()?;
    let unsafe_count = links
        .operations()
        .iter()
        .filter(|op| {
            op.reason == install::Reason::Conflict(install::ConflictReason::UnsafeAncestor)
        })
        .count();
    if unsafe_count != 0 {
        return Err(Error::UnsafeSync(unsafe_count));
    }
    if links.has_conflicts() {
        return Err(
            lifecycle::SyncApplyError::Links(install::ApplyError::Conflicts {
                count: links.summary().conflict,
            })
            .into(),
        );
    }
    if apply {
        match lifecycle::sync_with_settings_apply(&plan) {
            Ok(report) if matches!(format, SyncFormat::Human) => {
                println!("Ledger {:?}", report.ledger)
            }
            Ok(_) => {}
            Err(error) => {
                if let Some(report) = error.link_report() {
                    eprintln!("{}", serde_json::to_string_pretty(report)?);
                }
                return Err(error.into());
            }
        }
    }
    Ok(())
}

/// The user's Claude root (`CLAUDE_CONFIG_DIR`, else `~/.claude`) and `~/.agents.local`.
fn locations() -> Result<install::Locations, Error> {
    let home = std::env::home_dir().ok_or(Error::NoHome)?;
    Ok(install::Locations {
        claude_root: std::env::var_os("CLAUDE_CONFIG_DIR")
            .filter(|path| !path.is_empty())
            .map_or_else(|| home.join(".claude"), PathBuf::from),
        local_root: home.join(".agents.local"),
    })
}

/// Plans a fragment install, or removal when there is no fragment; applies it only with `--apply`.
fn settings(fragment: Option<&Path>, apply: bool, format: SyncFormat) -> Result<(), Error> {
    use install::settings;
    let fragment = fragment.map(settings::Fragment::read).transpose()?;
    let plan = lifecycle::settings_plan(&locations()?, fragment.as_ref())?;
    match format {
        SyncFormat::Json => println!("{}", serde_json::to_string_pretty(&plan)?),
        SyncFormat::Human => {
            println!("Claude config at {}", plan.root.display());
            print_settings_plan(&plan);
        }
    }
    if plan.has_conflicts() {
        return Err(settings::ApplyError::Conflicts {
            count: plan.summary.conflict,
        }
        .into());
    }
    if apply {
        let backup = lifecycle::settings_apply(&plan)?;
        if let (Some(backup), SyncFormat::Human) = (backup, format) {
            println!("Backup {}", backup.display());
        }
    }
    Ok(())
}

fn print_settings_plan(plan: &install::settings::Plan) {
    for op in &plan.operations {
        print!("{:?} {:?} {}", op.operation, op.kind, op.target);
        if let Some(event) = &op.event {
            print!(" [{event}]");
        }
        println!(" ({:?}, {})", op.ownership, op.reason);
        if let Some(commands) = &op.commands {
            for command in commands {
                println!("  command {command:?}");
            }
        }
    }
    for file in [&plan.files.settings, &plan.files.state] {
        println!("{:?} {}", file.action, file.path.display());
    }
    let counts = &plan.summary;
    println!(
        "{} add, {} change, {} remove, {} unchanged, {} conflict",
        counts.add, counts.change, counts.remove, counts.noop, counts.conflict
    );
}

/// Prints the findings, then picks the exit: an unsafe tree is a usage error (2), any other
/// finding is drift (1).
fn verdict(out: &Path, findings: &[stage::Finding]) -> Result<(), Error> {
    print!("{}", stage::report(out, findings));
    match findings.len() {
        0 => Ok(()),
        n if stage::is_unsafe(findings) => Err(Error::UnsafeTree(n)),
        n => Err(Error::Drift(n)),
    }
}

fn check_tree(
    root: &Path,
    config: &Config,
    out: &Path,
    expected: &[build::Entry],
) -> Result<(), Error> {
    verdict(out, &stage::check(root, config, out, expected)?)
}

fn clean_tree(
    root: &Path,
    config: &Config,
    out: &Path,
    expected: &[build::Entry],
    apply: bool,
) -> Result<(), Error> {
    match stage::clean(root, config, out, expected, apply)? {
        Ok(cleanup) => {
            print!("{}", stage::cleanup_report(out, &cleanup, apply));
            Ok(())
        }
        Err(blockers) => {
            print!("{}", stage::report(out, &blockers));
            eprintln!("cleanup blocked: nothing was deleted");
            verdict_of_blockers(&blockers)
        }
    }
}

fn verdict_of_blockers(blockers: &[stage::Finding]) -> Result<(), Error> {
    if stage::is_unsafe(blockers) {
        Err(Error::UnsafeTree(blockers.len()))
    } else {
        Err(Error::Drift(blockers.len()))
    }
}

fn status(rule: &validate::Rule) -> &'static str {
    match rule.check {
        Check::Run(_) => "implemented",
        Check::Deferred(_) => "deferred",
    }
}

fn rules_table() -> String {
    let mut out = String::from("ID     SEVERITY  FAST  STATUS       SUMMARY\n");
    for rule in validate::CATALOG {
        out.push_str(&format!(
            "{:<6} {:<9} {:<5} {:<12} {}\n",
            rule.id,
            rule.severity.as_str(),
            if rule.fast { "yes" } else { "no" },
            status(rule),
            rule.summary
        ));
    }
    out
}

fn explain(rule: &validate::Rule) -> String {
    let mut out = format!(
        "{}  {}  ({})\n\n{}\n",
        rule.id,
        rule.severity.as_str(),
        status(rule),
        rule.summary
    );
    if let Check::Deferred(why) = rule.check {
        out.push_str(&format!("\nNot checked: {why}\n"));
    }
    out.push_str(&format!("\nWhy: {}\n", rule.rationale));
    if !rule.fail.is_empty() {
        out.push_str(&format!("\nFails: {}\nPasses: {}\n", rule.fail, rule.pass));
    }
    if !rule.fix.is_empty() {
        out.push_str(&format!("\nFix: {}\n", rule.fix));
    }
    out.push_str(&format!(
        "\nIn `check --fast`: {}\n",
        if rule.fast { "yes" } else { "no" }
    ));
    out
}

#[cfg(test)]
mod tests {
    use clap::CommandFactory;

    use super::*;

    #[test]
    fn cli_definition_is_valid() {
        Cli::command().debug_assert();
    }

    #[test]
    fn config_errors_exit_usage_and_selected_deferred_rules_exit_unverifiable() {
        let dir = tempfile::tempdir().unwrap();
        let cli = Cli::parse_from(["agenvil", "--root", dir.path().to_str().unwrap(), "check"]);
        assert_eq!(run(&cli), Exit::Usage);

        std::fs::write(
            dir.path().join("agenvil.toml"),
            "schema = 1\n[project]\nname = \"x\"\n",
        )
        .unwrap();
        // The default run covers the implemented rules; deferred ones are only reported.
        assert_eq!(run(&cli), Exit::Success);
        // Naming a deferred rule makes the verdict incomplete.
        let only = Cli::parse_from([
            "agenvil",
            "--root",
            dir.path().to_str().unwrap(),
            "check",
            "--only",
            "AG006",
        ]);
        assert_eq!(run(&only), Exit::Unverifiable);
    }
}
