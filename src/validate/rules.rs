//! The rule catalog and the check for each implemented rule.
//!
//! Semantics come from the Team Agents Kit rule definitions. Where the kit leaves a detail
//! unspecified, the rule does not check it (see `docs/rules.md`) rather than guessing.

use serde_json::Value;
use toml::de::{DeTable, DeValue};

use super::inputs::Input;
use super::{Ctx, Hit, Severity};
use crate::inventory::Resource;

pub enum Check {
    Run(fn(&Ctx, &mut Vec<Hit>)),
    /// Not implemented; the text says why and where it is tracked.
    Deferred(&'static str),
}

pub struct Rule {
    pub id: &'static str,
    pub severity: Severity,
    /// Part of `check --fast`: the kit's `--fast` rule list, minus rules that are Deferred.
    pub fast: bool,
    pub summary: &'static str,
    pub rationale: &'static str,
    pub fail: &'static str,
    pub pass: &'static str,
    pub fix: &'static str,
    pub check: Check,
}

pub fn find(id: &str) -> Option<&'static Rule> {
    CATALOG.iter().find(|rule| rule.id == id)
}

use Check::{Deferred, Run};
use Severity::{Error, Warning};

const TRACKED: &str = "not implemented yet; tracked for the full compatibility suite (#8)";
const NO_EXAMPLE: &str = "";

const fn deferred(
    id: &'static str,
    severity: Severity,
    summary: &'static str,
    why: &'static str,
) -> Rule {
    Rule {
        id,
        severity,
        fast: true,
        summary,
        rationale: why,
        fail: NO_EXAMPLE,
        pass: NO_EXAMPLE,
        fix: "",
        check: Deferred(why),
    }
}

/// Sorted by ID; `rules` prints it in this order.
pub static CATALOG: &[Rule] = &[
    Rule {
        id: "AG001",
        severity: Error,
        fast: true,
        summary: "An agent has name, description, model, effort and tools; name equals the file name.",
        rationale: "Every agent must be fully specified so its model, effort and tool access never depend on defaults. The name must match the file so the agent can be found by name.",
        fail: "agents/reviewer.md has `name: review` and no `tools`.",
        pass: "agents/reviewer.md has `name: reviewer` and all five keys.",
        fix: "Add the missing keys and set `name` to the file name without `.md`.",
        check: Run(ag001),
    },
    Rule {
        id: "AG002",
        severity: Error,
        fast: true,
        summary: "An agent's model is one of the allowed models.",
        rationale: "Only vetted models may be used. The allowed set defaults to haiku, sonnet and opus and is set in [policy.models.claude].models.",
        fail: "`model: gpt-4`",
        pass: "`model: sonnet`",
        fix: "Use an allowed model, or extend [policy.models.claude].models in agenvil.toml.",
        check: Run(ag002),
    },
    Rule {
        id: "AG003",
        severity: Error,
        fast: true,
        summary: "An agent's effort is one of the allowed efforts.",
        rationale: "Effort drives cost and latency. The allowed set defaults to low, medium, high and xhigh; `max` is excluded on purpose.",
        fail: "`effort: max`",
        pass: "`effort: high`",
        fix: "Use an allowed effort, or extend [policy.models.claude].efforts in agenvil.toml.",
        check: Run(ag003),
    },
    Rule {
        id: "AG004",
        severity: Error,
        fast: true,
        summary: "An agent's model and effort form an allowed pair.",
        rationale: "Not every effort suits every model. The pairs default to haiku+low, sonnet+medium, sonnet+high, opus+high and opus+xhigh and are set in [policy.models.claude].pairs.",
        fail: "`model: haiku` with `effort: xhigh`",
        pass: "`model: opus` with `effort: high`",
        fix: "Choose an allowed pair, or add the pair to [policy.models.claude].pairs.",
        check: Run(ag004),
    },
    Rule {
        id: "AG005",
        severity: Error,
        fast: true,
        summary: "An agent's tools is an explicit, non-empty list.",
        rationale: "Listing tools explicitly keeps agents at least privilege. Whether each name is a known tool is not checked: the kit does not define the list of known tools.",
        fail: "`tools: Read, Grep` (not a list) or `tools: []`",
        pass: "`tools: [Read, Grep]`",
        fix: "Write `tools` as a YAML list of the tool names the agent needs.",
        check: Run(ag005),
    },
    deferred(
        "AG006",
        Error,
        "Every skill in an agent's `skills:` exists.",
        TRACKED,
    ),
    deferred(
        "AG007",
        Error,
        "A skill in `skills:` is in the core or agent tier, never on-demand.",
        "not implemented: the kit defines no tier data yet and its mechanism is unverified; tracked for #8",
    ),
    Rule {
        id: "CF001",
        severity: Error,
        fast: true,
        summary: "config/defaults.toml is valid TOML with only known keys and valid values.",
        rationale: "Defaults are the only place for on/off switches, so a typo must not pass silently. Checked only when the file exists. This is the one rule that encodes the kit's own key list.",
        fail: "`[update]` with `auto_update = \"weekly\"`",
        pass: "`[update]` with `auto_update = \"daily\"`",
        fix: "Use only the documented sections and keys with values of the documented type.",
        check: Run(cf001),
    },
    deferred("CF002", Error, "No secret-like keys in config.", TRACKED),
    // The kit lists CF003 in its --fast set, but `--fast` must spawn no subprocess and the git
    // half needs one, so it is full-run only here (documented in docs/rules.md).
    Rule {
        fast: false,
        ..deferred(
            "CF003",
            Error,
            "A user config.toml is not in the repo tree or tracked by git.",
            "not implemented: the git part needs a subprocess, so it is never part of `check --fast`; tracked for #8",
        )
    },
    deferred(
        "CF004",
        Error,
        "connectors.toml and third-party.toml carry no `default` key.",
        TRACKED,
    ),
    deferred(
        "CN001",
        Error,
        "Each connector has the required fields.",
        TRACKED,
    ),
    deferred(
        "FM001",
        Error,
        "Frontmatter parses as YAML.",
        "not a rule here: malformed frontmatter is reported as an input problem (exit 4) on every run; its model and effort checks are AG002 and AG003",
    ),
    Rule {
        id: "HY001",
        severity: Error,
        fast: true,
        summary: "No personal paths or emails in agents, skills and rules.",
        rationale: "Shared source must not leak a user's machine or identity. Patterns are the regexes in [hygiene].personal_patterns (default `/Users/` and `~/.agents/`). Only agent, skill and rule files are scanned.",
        fail: "A line containing `/Users/alice/notes`",
        pass: "A line containing `~/notes`",
        fix: "Replace the personal path or address with a placeholder, or change the patterns in [hygiene].",
        check: Run(hy001),
    },
    Rule {
        id: "HY002",
        severity: Error,
        fast: true,
        summary: "Files in bin/ are executable and start with a shebang.",
        rationale: "Scripts that cannot run, or that run under the wrong interpreter, break installs. The executable bit is only checked on Unix.",
        fail: "bin/tool without `#!` on line 1, or with mode 0644",
        pass: "bin/tool with `#!/bin/sh` on line 1 and mode 0755",
        fix: "Add a shebang line and `chmod +x` the file.",
        check: Run(hy002),
    },
    deferred("HY003", Warning, "Symlinks in the repo.", TRACKED),
    deferred(
        "HY005",
        Warning,
        "CRLF line endings or a BOM in frontmatter files.",
        TRACKED,
    ),
    Rule {
        id: "PO001",
        severity: Error,
        fast: true,
        summary: "The model table in the policy file matches the agents' model and effort, both ways.",
        rationale: "The table between `<!-- kit:model-table -->` and `<!-- /kit:model-table -->` is generated from agent frontmatter; frontmatter wins when they disagree.",
        fail: "agents/a.md has `model: opus` but the table row says `sonnet`",
        pass: "Every agent has one row with its own model and effort, and no row names an unknown agent.",
        fix: "Regenerate or correct the table so each agent has exactly one matching row.",
        check: Run(po001),
    },
    Rule {
        id: "SK001",
        severity: Error,
        fast: true,
        summary: "A skill has a SKILL.md, its name equals the folder name and its description is not empty.",
        rationale: "Skills are found by folder and selected by description, so both must be present and consistent.",
        fail: "skills/review/ has no SKILL.md, or its `name` is `code-review`.",
        pass: "skills/review/SKILL.md has `name: review` and a description.",
        fix: "Add SKILL.md with `name` equal to the folder name and a non-empty `description`.",
        check: Run(sk001),
    },
    Rule {
        id: "SK002",
        severity: Warning,
        fast: false,
        summary: "A skill sets model and effort, or an explicit `inherit` for each.",
        rationale: "Skills should state their cost explicitly. Tier-based exemptions (core-tier, third-party skills) need data the kit does not define yet; use [[ignore]] for exceptions. Not part of --fast, like the kit's own fast list.",
        fail: "A skill with no `effort` key",
        pass: "A skill with `model: inherit` and `effort: inherit`",
        fix: "Add `model` and `effort`, or set each to `inherit`.",
        check: Run(sk002),
    },
    deferred(
        "SK003",
        Error,
        "Every skill has a tier and a pack.",
        TRACKED,
    ),
    deferred(
        "SK004",
        Error,
        "No two enabled packs provide the same skill name.",
        TRACKED,
    ),
    deferred(
        "SK005",
        Error,
        "The core tier's description size stays under the budget.",
        TRACKED,
    ),
    deferred(
        "SK006",
        Error,
        "Skill name and description follow the Agent Skills spec limits.",
        TRACKED,
    ),
    deferred(
        "ST001",
        Error,
        "team.d fragments are valid JSON with only known keys.",
        TRACKED,
    ),
    deferred(
        "ST002",
        Error,
        "Hook commands in team.d fragments exist in the repo.",
        TRACKED,
    ),
    deferred(
        "TL001",
        Error,
        "Each Brewfile equals the brew subset of tools.toml.",
        TRACKED,
    ),
    deferred(
        "TL002",
        Error,
        "Core and company tools have an install method or manual steps.",
        TRACKED,
    ),
    deferred(
        "TP001",
        Error,
        "Third-party entries are pinned and reviewed.",
        TRACKED,
    ),
    deferred(
        "WF001",
        Error,
        "Workflow scripts parse and set a model per stage.",
        TRACKED,
    ),
];

// -- helpers ---------------------------------------------------------------------------------

/// A hit with no source position (a missing key or file); positions are never made up.
fn file_hit(path: &str, message: String) -> Hit {
    Hit {
        path: path.to_owned(),
        line: None,
        column: None,
        message,
    }
}

fn line_hit(path: &str, line: usize, column: usize, message: String) -> Hit {
    Hit {
        path: path.to_owned(),
        line: Some(line),
        column: Some(column),
        message,
    }
}

/// A hit on the line where `key` is written in the resource's frontmatter, if it can be found.
fn key_hit(res: &Resource, key: &str, message: String) -> Hit {
    let Some(fm) = &res.frontmatter else {
        return file_hit(&res.path, message);
    };
    let text = res.source.strip_prefix('\u{feff}').unwrap_or(&res.source);
    let found = text
        .split('\n')
        .enumerate()
        .skip(fm.start_line)
        .take(fm.end_line.saturating_sub(fm.start_line + 1))
        .find(|(_, line)| {
            line.strip_prefix(key)
                .is_some_and(|rest| rest.trim_start().starts_with(':'))
        });
    match found {
        Some((index, _)) => line_hit(&res.path, index + 1, 1, message),
        None => file_hit(&res.path, message),
    }
}

/// The value of a frontmatter key; an explicit YAML `null` counts as unset.
fn value<'a>(res: &'a Resource, key: &str) -> Option<&'a Value> {
    res.frontmatter.as_ref()?.get(key).filter(|v| !v.is_null())
}

fn quoted(items: &[String]) -> String {
    items
        .iter()
        .map(|i| format!("`{i}`"))
        .collect::<Vec<_>>()
        .join(", ")
}

// -- agents ----------------------------------------------------------------------------------

fn ag001(ctx: &Ctx, out: &mut Vec<Hit>) {
    const REQUIRED: [&str; 5] = ["name", "description", "model", "effort", "tools"];
    for agent in &ctx.inventory.agents {
        if agent.frontmatter.is_none() {
            out.push(file_hit(
                &agent.path,
                "agent has no frontmatter; `name`, `description`, `model`, `effort` and `tools` are required".to_owned(),
            ));
            continue;
        }
        for key in REQUIRED {
            if value(agent, key).is_none() {
                out.push(file_hit(
                    &agent.path,
                    format!("missing required key `{key}`"),
                ));
            }
        }
        if let Some(name) = value(agent, "name")
            && name.as_str() != Some(agent.name.as_str())
        {
            out.push(key_hit(
                agent,
                "name",
                format!("`name` must equal the file name `{}`", agent.name),
            ));
        }
    }
}

fn allowed_value(ctx: &Ctx, out: &mut Vec<Hit>, key: &str, allowed: &[String]) {
    for agent in &ctx.inventory.agents {
        let Some(found) = value(agent, key) else {
            continue; // a missing key is AG001's finding
        };
        if !found
            .as_str()
            .is_some_and(|s| allowed.iter().any(|a| a == s))
        {
            out.push(key_hit(
                agent,
                key,
                format!(
                    "`{key}: {found}` is not allowed; allowed: {}",
                    quoted(allowed)
                ),
            ));
        }
    }
}

fn ag002(ctx: &Ctx, out: &mut Vec<Hit>) {
    allowed_value(ctx, out, "model", &ctx.config.policy.models.claude.models);
}

fn ag003(ctx: &Ctx, out: &mut Vec<Hit>) {
    allowed_value(ctx, out, "effort", &ctx.config.policy.models.claude.efforts);
}

fn ag004(ctx: &Ctx, out: &mut Vec<Hit>) {
    let pairs = &ctx.config.policy.models.claude.pairs;
    for agent in &ctx.inventory.agents {
        let (Some(model), Some(effort)) = (
            value(agent, "model").and_then(Value::as_str),
            value(agent, "effort").and_then(Value::as_str),
        ) else {
            continue; // missing or non-string values are AG001, AG002 and AG003
        };
        if !pairs.iter().any(|[m, e]| m == model && e == effort) {
            let list: Vec<_> = pairs.iter().map(|[m, e]| format!("{m}+{e}")).collect();
            out.push(key_hit(
                agent,
                "effort",
                format!(
                    "`model: {model}` with `effort: {effort}` is not an allowed pair; allowed: {}",
                    quoted(&list)
                ),
            ));
        }
    }
}

fn ag005(ctx: &Ctx, out: &mut Vec<Hit>) {
    for agent in &ctx.inventory.agents {
        let Some(tools) = value(agent, "tools") else {
            continue; // a missing key is AG001's finding
        };
        let problem = match tools.as_array() {
            None => Some("`tools` must be an explicit list, not a single value"),
            Some(list) if list.is_empty() => Some("`tools` must not be empty"),
            Some(list)
                if list
                    .iter()
                    .any(|t| t.as_str().is_none_or(|s| s.trim().is_empty())) =>
            {
                Some("every entry in `tools` must be a non-empty string")
            }
            Some(_) => None,
        };
        if let Some(message) = problem {
            out.push(key_hit(agent, "tools", message.to_owned()));
        }
    }
}

// -- skills ----------------------------------------------------------------------------------

fn sk001(ctx: &Ctx, out: &mut Vec<Hit>) {
    for dir in &ctx.missing_skill_files {
        out.push(file_hit(dir, "skill directory has no SKILL.md".to_owned()));
    }
    for skill in &ctx.inventory.skills {
        if skill.frontmatter.is_none() {
            out.push(file_hit(
                &skill.path,
                "skill has no frontmatter; `name` and `description` are required".to_owned(),
            ));
            continue;
        }
        match value(skill, "name") {
            None => out.push(file_hit(
                &skill.path,
                "missing required key `name`".to_owned(),
            )),
            Some(name) if name.as_str() != Some(skill.name.as_str()) => out.push(key_hit(
                skill,
                "name",
                format!("`name` must equal the folder name `{}`", skill.name),
            )),
            Some(_) => {}
        }
        match value(skill, "description") {
            None => out.push(file_hit(
                &skill.path,
                "missing required key `description`".to_owned(),
            )),
            Some(d) if d.as_str().is_none_or(|s| s.trim().is_empty()) => out.push(key_hit(
                skill,
                "description",
                "`description` must not be empty".to_owned(),
            )),
            Some(_) => {}
        }
    }
}

fn sk002(ctx: &Ctx, out: &mut Vec<Hit>) {
    for skill in &ctx.inventory.skills {
        for key in ["model", "effort"] {
            if value(skill, key).is_none() {
                out.push(file_hit(
                    &skill.path,
                    format!("`{key}` is not set; set it or write `{key}: inherit`"),
                ));
            }
        }
    }
}

// -- policy table ----------------------------------------------------------------------------

const TABLE_OPEN: &str = "<!-- kit:model-table -->";
const TABLE_CLOSE: &str = "<!-- /kit:model-table -->";

fn cells(line: &str) -> Vec<&str> {
    line.trim()
        .trim_start_matches('|')
        .trim_end_matches('|')
        .split('|')
        .map(str::trim)
        .collect()
}

fn is_separator(cells: &[&str]) -> bool {
    cells.iter().all(|c| {
        let c = c.trim_matches(':');
        !c.is_empty() && c.bytes().all(|b| b == b'-')
    })
}

/// Column of the first non-blank character, in characters.
fn first_column(line: &str) -> usize {
    line.chars().take_while(|c| c.is_whitespace()).count() + 1
}

fn po001(ctx: &Ctx, out: &mut Vec<Hit>) {
    let Input::Text(policy) = &ctx.policy else {
        return; // unreadable or missing-with-agents is reported as a problem by the runner
    };
    let path = ctx.config.paths.policy.to_slash();
    let policy = policy.strip_prefix('\u{feff}').unwrap_or(policy);
    let lines: Vec<&str> = policy
        .split('\n')
        .map(|l| l.trim_end_matches('\r'))
        .collect();
    let marker = |m: &str| lines.iter().position(|l| l.trim() == m);

    let Some(open) = marker(TABLE_OPEN) else {
        out.push(file_hit(
            &path,
            format!("no model table: `{TABLE_OPEN}` marker not found"),
        ));
        return;
    };
    let Some(close) = marker(TABLE_CLOSE).filter(|c| *c > open) else {
        out.push(line_hit(
            &path,
            open + 1,
            first_column(lines[open]),
            format!("model table is not closed with `{TABLE_CLOSE}`"),
        ));
        return;
    };

    // Lines that are not table rows (for example a "generated" comment) are ignored.
    let mut rows = (open + 1..close).filter(|i| lines[*i].trim_start().starts_with('|'));
    let at = |i: usize, message: String| line_hit(&path, i + 1, first_column(lines[i]), message);
    let Some(header) = rows.next() else {
        out.push(at(
            open,
            "model table is empty; expected the header `| agent | model | effort |`".to_owned(),
        ));
        return;
    };
    if cells(lines[header]) != ["agent", "model", "effort"] {
        out.push(at(
            header,
            "model table header must be `| agent | model | effort |`".to_owned(),
        ));
        return;
    }
    let separator_ok = rows.next().is_some_and(|i| is_separator(&cells(lines[i])));
    if !separator_ok {
        out.push(at(
            header,
            "model table needs a `|---|---|---|` separator row after the header".to_owned(),
        ));
        return;
    }

    let mut table: Vec<(String, &str, &str, usize)> = Vec::new();
    for i in rows {
        match cells(lines[i])[..] {
            [agent, model, effort] => table.push((agent.to_owned(), model, effort, i)),
            _ => out.push(at(
                i,
                "model table row must have exactly three columns: agent, model, effort".to_owned(),
            )),
        }
    }

    let agents_dir = ctx.config.paths.agents.to_slash();
    let unreadable = |name: &str| {
        let path = format!("{agents_dir}/{name}.md");
        ctx.inventory.problems.iter().any(|p| p.path == path)
    };
    for agent in &ctx.inventory.agents {
        let rows: Vec<_> = table.iter().filter(|r| r.0 == agent.name).collect();
        let Some(first) = rows.first() else {
            out.push(at(
                open,
                format!("agent `{}` has no row in the model table", agent.name),
            ));
            continue;
        };
        for dup in &rows[1..] {
            out.push(at(
                dup.3,
                format!(
                    "agent `{}` has more than one row in the model table",
                    agent.name
                ),
            ));
        }
        let model = value(agent, "model").and_then(Value::as_str);
        let effort = value(agent, "effort").and_then(Value::as_str);
        if let (Some(model), Some(effort)) = (model, effort)
            && (first.1 != model || first.2 != effort)
        {
            out.push(at(
                first.3,
                format!(
                    "row for `{}` says `{}` / `{}` but {} sets `{model}` / `{effort}`",
                    agent.name, first.1, first.2, agent.path
                ),
            ));
        }
    }
    for (name, _, _, i) in &table {
        let known = ctx.inventory.agents.iter().any(|a| &a.name == name) || unreadable(name);
        if !known {
            out.push(at(
                *i,
                format!("row names `{name}`, which is not an agent in {agents_dir}/"),
            ));
        }
    }
}

// -- config defaults -------------------------------------------------------------------------

const AUTO_UPDATE: [&str; 3] = ["daily", "always", "off"];
const CONNECTORS: [&str; 11] = [
    "datadog",
    "snowflake",
    "humio",
    "teleport",
    "github",
    "context7",
    "playwright",
    "mixpanel",
    "figma",
    "bruno",
    "onepassword",
];
const PACKS: [&str; 4] = ["superpowers", "ponytail", "aihero", "karpathy"];

#[derive(Clone, Copy)]
enum Kind {
    Bool,
    Port,
    Choice(&'static [&'static str]),
}

/// The kit's hand-written allow-list: (section, allowed keys with their kind).
fn allow_list(section: &str) -> Option<Vec<(&'static str, Kind)>> {
    let bools = |names: &[&'static str]| names.iter().map(|n| (*n, Kind::Bool)).collect();
    match section {
        "update" => Some(vec![("auto_update", Kind::Choice(&AUTO_UPDATE))]),
        "install" => Some(bools(&["tools"])),
        "hooks" => Some(bools(&["update", "delegate_nudge"])),
        "tools" => Some(bools(&["core", "company", "headroom"])),
        "headroom" => Some(vec![("port", Kind::Port)]),
        "connectors" => Some(bools(&CONNECTORS)),
        "packs" => Some(bools(&PACKS)),
        _ => None,
    }
}

fn cf001(ctx: &Ctx, out: &mut Vec<Hit>) {
    let Input::Text(text) = &ctx.defaults else {
        return; // absent: nothing to validate; unreadable: reported as a problem by the runner
    };
    let path = ctx.config.paths.defaults.to_slash();
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let at = |offset: usize, message: String| {
        let before = &text[..offset.min(text.len())];
        let line = before.matches('\n').count() + 1;
        let column = before.rsplit('\n').next().map_or(0, |l| l.chars().count()) + 1;
        line_hit(&path, line, column, message)
    };
    let table = match DeTable::parse(text) {
        Ok(table) => table,
        Err(error) => {
            let offset = error.span().map_or(0, |s| s.start);
            out.push(at(offset, format!("invalid TOML: {}", error.message())));
            return;
        }
    };
    for (section_key, section) in table.get_ref() {
        let section_name: &str = section_key.get_ref();
        let Some(allowed) = allow_list(section_name) else {
            out.push(at(
                section_key.span().start,
                format!("unknown section `{section_name}`"),
            ));
            continue;
        };
        let DeValue::Table(keys) = section.get_ref() else {
            out.push(at(
                section.span().start,
                format!("`{section_name}` must be a table"),
            ));
            continue;
        };
        for (key, val) in keys {
            let name: &str = key.get_ref();
            let Some((_, kind)) = allowed.iter().find(|(n, _)| *n == name) else {
                out.push(at(
                    key.span().start,
                    format!("unknown key `{section_name}.{name}`"),
                ));
                continue;
            };
            let ok = match (kind, val.get_ref()) {
                (Kind::Bool, DeValue::Boolean(_)) | (Kind::Port, DeValue::Integer(_)) => None,
                (Kind::Bool, _) => Some("must be true or false".to_owned()),
                (Kind::Port, _) => Some("must be an integer".to_owned()),
                (Kind::Choice(choices), DeValue::String(s)) if choices.contains(&&**s) => None,
                (Kind::Choice(choices), _) => {
                    Some(format!("must be one of {}", choices.join(", ")))
                }
            };
            if let Some(expected) = ok {
                out.push(at(
                    val.span().start,
                    format!("`{section_name}.{name}` {expected}"),
                ));
            }
        }
    }
}

// -- hygiene ---------------------------------------------------------------------------------

fn hy001(ctx: &Ctx, out: &mut Vec<Hit>) {
    let inventory = ctx.inventory;
    let files = inventory
        .agents
        .iter()
        .chain(&inventory.skills)
        .chain(&inventory.rules);
    for file in files {
        let text = file.source.strip_prefix('\u{feff}').unwrap_or(&file.source);
        for (index, line) in text.split('\n').enumerate() {
            let line = line.trim_end_matches('\r');
            // The earliest match on the line; one finding per line keeps the output readable.
            let first = ctx
                .patterns
                .iter()
                .filter_map(|re| re.find(line).map(|m| (m.start(), re)))
                .min_by_key(|(start, _)| *start);
            if let Some((start, re)) = first {
                out.push(line_hit(
                    &file.path,
                    index + 1,
                    line[..start].chars().count() + 1,
                    format!("matches personal pattern `{}`", re.as_str()),
                ));
            }
        }
    }
}

fn hy002(ctx: &Ctx, out: &mut Vec<Hit>) {
    for file in &ctx.bin {
        if file.executable == Some(false) {
            out.push(file_hit(&file.path, "file is not executable".to_owned()));
        }
        if !file.shebang {
            out.push(line_hit(
                &file.path,
                1,
                1,
                "file does not start with a shebang (`#!`)".to_owned(),
            ));
        }
    }
}
