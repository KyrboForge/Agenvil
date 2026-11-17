//! The policy validator: a static rule catalog run over the inventory and the typed config.
//!
//! Rules are plain functions in [`rules::CATALOG`]; there is no plugin runtime. A rule only reports
//! what it found ([`Hit`]); severity, ignores and ordering are applied here so every rule behaves
//! the same way. Rules whose kit semantics are not implemented yet stay in the catalog as
//! `Deferred` and are never counted as passing.

mod inputs;
pub mod rules;

use std::path::Path;

use regex::{Regex, RegexBuilder};
use serde::Serialize;

use crate::config::{Config, Level};
use crate::diagnostics::{Problem, ProblemKind};
use crate::inventory::Inventory;
use inputs::{BinFile, Input};
pub use rules::{CATALOG, Check, Rule, find};

/// Size limit for `POLICY.md` and `defaults.toml`, like `agenvil.toml`.
const MAX_INPUT_BYTES: u64 = 1024 * 1024;
/// Per-pattern compiled size limit for `[hygiene] personal_patterns` (config is untrusted).
const REGEX_SIZE_LIMIT: usize = 1 << 20;
const MAX_BIN_ENTRIES: usize = 10_000;

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Severity {
    Error,
    Warning,
}

impl Severity {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Error => "error",
            Self::Warning => "warning",
        }
    }
}

/// One policy violation. Field order is the JSON key order of the output contract.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Finding {
    pub id: &'static str,
    pub severity: Severity,
    /// Relative to the project root, `/`-separated.
    pub path: String,
    /// 1-based. `null` when the violation has no source position (a missing key, a missing file).
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub message: String,
    pub fix: &'static str,
}

/// What a rule reports; the runner adds id, severity and the fix hint.
#[derive(Debug)]
pub struct Hit {
    pub path: String,
    pub line: Option<usize>,
    pub column: Option<usize>,
    pub message: String,
}

/// Everything a rule may look at.
pub struct Ctx<'a> {
    pub config: &'a Config,
    pub inventory: &'a Inventory,
    pub policy: Input,
    pub defaults: Input,
    pub bin: Vec<BinFile>,
    pub patterns: Vec<Regex>,
    /// Skill directories discovery found without a SKILL.md (SK001 reports them).
    pub missing_skill_files: Vec<String>,
}

/// Bad rule selection or policy settings. Always exit 2, with every error listed at once.
#[derive(Debug, thiserror::Error)]
#[error("{}", .0.join("\n"))]
pub struct SettingsError(pub Vec<String>);

pub struct Options<'a> {
    pub only: &'a [String],
    pub skip: &'a [String],
    pub fast: bool,
}

/// The rules to run, resolved and validated.
pub struct Plan {
    pub rules: Vec<(&'static Rule, Severity)>,
    /// Selected rules without an implementation. Any of them makes the verdict incomplete, so
    /// they are never mistaken for passing.
    pub unavailable: Vec<&'static str>,
    /// Rules without an implementation that nothing selected. They stay visible but do not make
    /// the verdict incomplete: a default run covers the implemented, applicable rules.
    pub deferred: Vec<&'static str>,
}

impl Plan {
    pub fn new(config: &Config, options: &Options) -> Result<Self, SettingsError> {
        let mut errors = Vec::new();
        let only: Vec<_> = options
            .only
            .iter()
            .filter_map(|id| known(&mut errors, "--only", id))
            .collect();
        let skip: Vec<_> = options
            .skip
            .iter()
            .filter_map(|id| known(&mut errors, "--skip", id))
            .collect();
        for id in config.rules.keys() {
            known(&mut errors, "agenvil.toml [rules]", id);
        }
        for ignore in &config.ignore {
            known(&mut errors, "agenvil.toml [[ignore]]", &ignore.rule);
            if ignore.reason.trim().is_empty() {
                errors.push(format!(
                    "agenvil.toml [[ignore]] for `{}` {}: `reason` must not be empty",
                    ignore.rule,
                    ignore.path.to_slash()
                ));
            }
        }
        for pattern in config
            .hygiene
            .personal_patterns
            .as_deref()
            .unwrap_or_default()
        {
            if let Err(message) = compile(pattern) {
                errors.push(format!(
                    "agenvil.toml [hygiene] personal_patterns: `{pattern}`: {message}"
                ));
            }
        }
        // `--skip` wins over `--only`, so a skipped rule is never held to `--only` requirements.
        let kept = || only.iter().filter(|r| !skip.iter().any(|s| s.id == r.id));
        if options.fast {
            for rule in kept() {
                if !rule.fast {
                    errors.push(format!(
                        "--only: {} is not part of --fast; run without --fast",
                        rule.id
                    ));
                }
            }
        }
        for rule in kept() {
            if let Some(missing) = unconfigured(config, rule.id) {
                errors.push(format!(
                    "--only: {} needs {missing}, which agenvil.toml does not set",
                    rule.id
                ));
            }
        }
        if !errors.is_empty() {
            return Err(SettingsError(errors));
        }

        let mut plan = Self {
            rules: Vec::new(),
            unavailable: Vec::new(),
            deferred: Vec::new(),
        };
        for rule in CATALOG {
            let named = only.iter().any(|r| r.id == rule.id);
            if (!only.is_empty() && !named) || skip.iter().any(|r| r.id == rule.id) {
                continue;
            }
            if options.fast && !rule.fast {
                continue;
            }
            let level = config.rules.get(rule.id).copied();
            // Naming a rule on the command line beats `off` in the config file.
            if level == Some(Level::Off) && !named {
                continue;
            }
            let severity = match level {
                Some(Level::Error) => Severity::Error,
                Some(Level::Warning) => Severity::Warning,
                Some(Level::Off) | None => rule.severity,
            };
            // Selected: named, or given a level in [rules]. Anything else is the default set.
            let selected = named || level.is_some();
            if !selected && (unconfigured(config, rule.id).is_some() || opt_in(config, rule.id)) {
                continue;
            }
            match rule.check {
                Check::Run(_) => plan.rules.push((rule, severity)),
                Check::Deferred(_) if selected => plan.unavailable.push(rule.id),
                Check::Deferred(_) => plan.deferred.push(rule.id),
            }
        }
        Ok(plan)
    }
}

/// What a rule needs from `agenvil.toml` that is not set, if anything. Such a rule has nothing
/// to check and is left out of a default run.
fn unconfigured(config: &Config, id: &str) -> Option<&'static str> {
    match id {
        "PO001" => config.paths.policy.is_none().then_some("[paths] policy"),
        "CF001" => config
            .paths
            .defaults
            .is_none()
            .then_some("[paths] defaults"),
        "HY002" => config.paths.bin.is_none().then_some("[paths] bin"),
        "HY001" => config
            .hygiene
            .personal_patterns
            .as_deref()
            .is_none_or(<[String]>::is_empty)
            .then_some("[hygiene] personal_patterns"),
        _ => None,
    }
}

/// Rules that run only when asked for: `--only`, a `[rules]` entry or the TAK profile.
fn opt_in(config: &Config, id: &str) -> bool {
    id == "SK002" && !config.tak()
}

/// Looks up a rule ID from `origin`, recording an actionable error when it does not exist.
fn known(errors: &mut Vec<String>, origin: &str, id: &str) -> Option<&'static Rule> {
    let rule = find(id);
    if rule.is_none() {
        errors.push(format!(
            "{origin}: unknown rule ID `{id}` (run `agenvil rules` to list the IDs)"
        ));
    }
    rule
}

fn compile(pattern: &str) -> Result<Regex, String> {
    if pattern.is_empty() {
        return Err("an empty pattern would match every line".to_owned());
    }
    RegexBuilder::new(pattern)
        .size_limit(REGEX_SIZE_LIMIT)
        .build()
        .map_err(|e| e.to_string())
}

#[derive(Debug)]
pub struct Outcome {
    pub findings: Vec<Finding>,
    /// Inputs a selected rule needed but could not read. The verdict is incomplete.
    pub problems: Vec<Problem>,
}

/// Runs the planned rules. Read-only: it only reads files under `root` and spawns nothing.
pub fn run(root: &Path, config: &Config, inventory: &Inventory, plan: &Plan) -> Outcome {
    let wants = |id: &str| plan.rules.iter().any(|(rule, _)| rule.id == id);
    // Discovery problems make the verdict incomplete, except a missing SKILL.md when SK001 runs:
    // that is SK001's own finding.
    let (missing, mut problems): (Vec<Problem>, Vec<Problem>) = inventory
        .problems
        .iter()
        .cloned()
        .partition(|p| wants("SK001") && p.kind == ProblemKind::MissingSkillFile);

    let policy = if let (true, Some(path)) = (wants("PO001"), &config.paths.policy) {
        let input = inputs::read_text(root, path, MAX_INPUT_BYTES);
        match input {
            Input::Missing if !inventory.agents.is_empty() => Input::Problem(Problem {
                path: path.to_slash(),
                line: None,
                kind: ProblemKind::Missing,
                message: "PO001 needs the policy file to compare with the agents; create it or skip PO001".to_owned(),
            }),
            other => other,
        }
    } else {
        Input::Missing
    };
    let defaults = match (wants("CF001"), &config.paths.defaults) {
        (true, Some(path)) => inputs::read_text(root, path, MAX_INPUT_BYTES),
        _ => Input::Missing,
    };
    let bin = match (wants("HY002"), &config.paths.bin) {
        (true, Some(path)) => {
            let (files, found) = inputs::bin_files(root, path, MAX_BIN_ENTRIES);
            problems.extend(found);
            files
        }
        _ => Vec::new(),
    };
    // Validated by `Plan::new`.
    let patterns = config
        .hygiene
        .personal_patterns
        .as_deref()
        .unwrap_or_default()
        .iter()
        .filter_map(|p| compile(p).ok())
        .collect();

    let ctx = Ctx {
        config,
        inventory,
        policy,
        defaults,
        bin,
        patterns,
        missing_skill_files: missing.into_iter().map(|p| p.path).collect(),
    };
    for input in [&ctx.policy, &ctx.defaults] {
        if let Input::Problem(problem) = input {
            problems.push(problem.clone());
        }
    }

    let mut findings = Vec::new();
    for (rule, severity) in &plan.rules {
        let Check::Run(check) = rule.check else {
            continue;
        };
        let mut hits = Vec::new();
        check(&ctx, &mut hits);
        findings.extend(hits.into_iter().map(|hit| Finding {
            id: rule.id,
            severity: *severity,
            path: hit.path,
            line: hit.line,
            column: hit.column,
            message: hit.message,
            fix: rule.fix,
        }));
    }
    findings.retain(|f| {
        !config
            .ignore
            .iter()
            .any(|i| i.rule == f.id && i.path.to_slash() == f.path)
    });
    findings.sort_by(|a, b| {
        (&a.path, a.line, a.column, a.id, &a.message)
            .cmp(&(&b.path, b.line, b.column, b.id, &b.message))
    });
    findings.dedup();
    problems.sort();
    problems.dedup();
    Outcome { findings, problems }
}
