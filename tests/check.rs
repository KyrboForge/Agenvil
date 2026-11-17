//! `agenvil check`, `rules` and `explain` against fixture projects in temporary directories.
//! Nothing here reads the real HOME, the network or this repository.

use std::fs;

use predicates::prelude::*;
use serde_json::{Value, json};
use tempfile::TempDir;

mod common;
use common::{agenvil, deferred, snapshot};

const CONFIG: &str = "schema = 1\n[project]\nname = \"demo\"\n[compat]\nprofile = \"tak\"\n";
const AGENT: &str = "---\nname: reviewer\ndescription: Reviews code\nmodel: opus\neffort: high\ntools: [Read, Grep]\n---\nBody\n";
const SKILL: &str =
    "---\nname: tdd\ndescription: Test first\nmodel: inherit\neffort: inherit\n---\nBody\n";
const POLICY: &str = "# Policy\n<!-- kit:model-table -->\n| agent | model | effort |\n|---|---|---|\n| reviewer | opus | high |\n<!-- /kit:model-table -->\n";
const DEFAULTS: &str =
    "[update]\nauto_update = \"daily\"\n[hooks]\nupdate = true\n[headroom]\nport = 8787\n";

fn write(dir: &TempDir, path: &str, content: impl AsRef<[u8]>) {
    let full = dir.path().join(path);
    fs::create_dir_all(full.parent().unwrap()).unwrap();
    fs::write(full, content).unwrap();
}

/// A project that passes every implemented rule.
fn clean() -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    write(&dir, "agenvil.toml", CONFIG);
    write(&dir, "agents/reviewer.md", AGENT);
    write(&dir, "skills/tdd/SKILL.md", SKILL);
    write(&dir, "POLICY.md", POLICY);
    write(&dir, "config/defaults.toml", DEFAULTS);
    write(&dir, "bin/tool", "#!/bin/sh\necho hi\n");
    make_executable(&dir, "bin/tool");
    dir
}

#[cfg(unix)]
fn make_executable(dir: &TempDir, path: &str) {
    use std::os::unix::fs::PermissionsExt;
    let full = dir.path().join(path);
    fs::set_permissions(full, fs::Permissions::from_mode(0o755)).unwrap();
}

#[cfg(not(unix))]
fn make_executable(_: &TempDir, _: &str) {}

/// Runs `check --format json` with `args` over the implemented rules (deferred rules are skipped
/// unless `args` has `--only`), returning the exit code and parsed stdout.
fn check(dir: &TempDir, args: &[&str]) -> (i32, Value) {
    let mut cmd = agenvil(dir);
    cmd.args(["check", "--format", "json"]).args(args);
    if !args.contains(&"--only") {
        cmd.args(["--skip", &deferred()]);
    }
    let output = cmd.output().unwrap();
    let code = output.status.code().unwrap();
    let json = serde_json::from_slice(&output.stdout).unwrap_or_else(|e| {
        panic!(
            "stdout is not JSON ({e}): {}",
            String::from_utf8_lossy(&output.stdout)
        )
    });
    (code, json)
}

/// `(id, path, line)` of every finding, in output order.
fn found(json: &Value) -> Vec<(String, String, Option<u64>)> {
    json["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| {
            (
                f["id"].as_str().unwrap().to_owned(),
                f["path"].as_str().unwrap().to_owned(),
                f["line"].as_u64(),
            )
        })
        .collect()
}

fn ids(json: &Value) -> Vec<String> {
    found(json).into_iter().map(|f| f.0).collect()
}

#[track_caller]
fn assert_finds(dir: &TempDir, id: &str, path: &str, line: Option<u64>) {
    let (code, json) = check(dir, &["--only", id]);
    assert_eq!(code, 1, "{json}");
    assert_eq!(
        found(&json),
        [(id.to_owned(), path.to_owned(), line)],
        "{json}"
    );
}

#[test]
fn clean_project_passes_with_the_documented_json() {
    let dir = clean();
    let (code, json) = check(&dir, &[]);
    assert_eq!(code, 0, "{json}");
    assert_eq!(
        json,
        json!({"schema": 1, "findings": [], "summary": {"errors": 0, "warnings": 0}})
    );
    agenvil(&dir)
        .args(["check", "--skip", &deferred()])
        .assert()
        .code(0)
        .stdout(predicate::str::contains("0 error(s), 0 warning(s)"));
}

#[test]
fn default_check_runs_implemented_rules_and_only_notes_deferred_ones() {
    let dir = clean();
    agenvil(&dir)
        .arg("check")
        .assert()
        .code(0)
        .stdout(predicate::str::contains("0 error(s), 0 warning(s)"))
        .stderr(
            predicate::str::contains("19 rule(s) are not implemented yet and were not run: AG006")
                .and(predicate::str::contains("SK003")),
        );
    // `--quiet` must not hide the note.
    agenvil(&dir)
        .args(["check", "--quiet"])
        .assert()
        .code(0)
        .stderr(predicate::str::contains("not implemented yet"));
    // Selecting a deferred rule, on the command line or in [rules], leaves no verdict.
    agenvil(&dir)
        .args(["check", "--only", "AG006"])
        .assert()
        .code(4)
        .stderr(predicate::str::contains("cannot verify AG006"));
    write(
        &dir,
        "agenvil.toml",
        format!("{CONFIG}[rules]\nAG006 = \"error\"\n"),
    );
    agenvil(&dir)
        .arg("check")
        .assert()
        .code(4)
        .stderr(predicate::str::contains("cannot verify AG006"));
    // `off` is not a selection.
    write(
        &dir,
        "agenvil.toml",
        format!("{CONFIG}[rules]\nAG006 = \"off\"\n"),
    );
    agenvil(&dir)
        .arg("check")
        .assert()
        .code(0)
        .stderr(predicate::str::contains("AG006").not());
}

#[test]
fn empty_project_passes() {
    let dir = tempfile::tempdir().unwrap();
    write(&dir, "agenvil.toml", CONFIG);
    assert_eq!(check(&dir, &[]).0, 0);
}

// -- AG001..AG005 ----------------------------------------------------------------------------

#[test]
fn ag001_requires_every_key_and_a_matching_name() {
    let dir = clean();
    write(
        &dir,
        "agents/reviewer.md",
        "---\nname: other\nmodel: opus\neffort: high\n---\n",
    );
    let (code, json) = check(&dir, &["--only", "AG001"]);
    assert_eq!(code, 1);
    let messages: Vec<_> = json["findings"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["message"].as_str().unwrap().to_owned())
        .collect();
    assert_eq!(messages.len(), 3, "{messages:?}");
    assert!(messages.iter().any(|m| m.contains("`description`")));
    assert!(messages.iter().any(|m| m.contains("`tools`")));
    // The wrong name is located on its own line; missing keys have no position.
    let located: Vec<_> = json["findings"]
        .as_array()
        .unwrap()
        .iter()
        .filter(|f| !f["line"].is_null())
        .collect();
    assert_eq!(
        (
            located.len(),
            located[0]["line"].as_u64(),
            located[0]["column"].as_u64()
        ),
        (1, Some(2), Some(1))
    );
}

#[test]
fn ag001_name_and_description_must_be_non_empty_strings() {
    let dir = tempfile::tempdir().unwrap();
    write(
        &dir,
        "agenvil.toml",
        "schema = 1\n[project]\nname = \"demo\"\n",
    );
    for (field, line) in [
        ("description: \"\"", Some(3)),
        ("description: \"  \"", Some(3)),
        ("description: 123", Some(3)),
        ("description: null", None),
    ] {
        write(
            &dir,
            "agents/reviewer.md",
            format!("---\nname: reviewer\n{field}\n---\nBody\n"),
        );
        let (code, json) = check(&dir, &["--only", "AG001"]);
        assert_eq!(code, 1, "{field}: {json}");
        assert_eq!(
            found(&json),
            [("AG001".to_owned(), "agents/reviewer.md".to_owned(), line)],
            "{field}: {json}"
        );
    }
    write(
        &dir,
        "agents/reviewer.md",
        "---\nname: 7\ndescription: Reviews\n---\nBody\n",
    );
    assert_finds(&dir, "AG001", "agents/reviewer.md", Some(2));
}

#[test]
fn null_model_and_effort_are_inherited_even_with_allowlists() {
    let dir = tempfile::tempdir().unwrap();
    write(
        &dir,
        "agenvil.toml",
        "schema = 1\n[project]\nname = \"demo\"\n[policy.models.claude]\nmodels = [\"sonnet\"]\nefforts = [\"medium\"]\n",
    );
    write(
        &dir,
        "agents/reviewer.md",
        "---\nname: reviewer\ndescription: Reviews\nmodel: null\neffort:\n---\nBody\n",
    );
    let (code, json) = check(&dir, &["--only", "AG001,AG002,AG003,AG004"]);
    assert_eq!((code, ids(&json)), (0, Vec::<String>::new()), "{json}");
}

#[test]
fn skip_wins_over_only_for_rule_prerequisites() {
    let dir = tempfile::tempdir().unwrap();
    write(
        &dir,
        "agenvil.toml",
        "schema = 1\n[project]\nname = \"demo\"\n",
    );
    write(
        &dir,
        "agents/reviewer.md",
        "---\nname: reviewer\ndescription: Reviews\n---\nBody\n",
    );
    for skipped in ["PO001", "CF001", "HY001", "HY002"] {
        let only = format!("AG001,{skipped}");
        let (code, json) = check(&dir, &["--only", &only, "--skip", skipped]);
        assert_eq!(code, 0, "{skipped}: {json}");
    }
}

#[test]
fn ag001_agent_without_frontmatter() {
    let dir = clean();
    write(&dir, "agents/reviewer.md", "just text\n");
    assert_finds(&dir, "AG001", "agents/reviewer.md", None);
}

#[test]
fn ag002_rejects_models_outside_the_allowed_set() {
    let dir = clean();
    write(
        &dir,
        "agents/reviewer.md",
        AGENT.replace("model: opus", "model: gpt-4"),
    );
    assert_finds(&dir, "AG002", "agents/reviewer.md", Some(4));
}

#[test]
fn ag003_rejects_efforts_outside_the_allowed_set() {
    let dir = clean();
    write(
        &dir,
        "agents/reviewer.md",
        AGENT.replace("effort: high", "effort: max"),
    );
    assert_finds(&dir, "AG003", "agents/reviewer.md", Some(5));
}

#[test]
fn ag004_checks_model_effort_pairs() {
    let dir = clean();
    for (model, effort, ok) in [
        ("haiku", "low", true),
        ("sonnet", "medium", true),
        ("sonnet", "high", true),
        ("opus", "high", true),
        ("opus", "xhigh", true),
        ("haiku", "xhigh", false),
        ("opus", "low", false),
        ("sonnet", "xhigh", false),
    ] {
        let agent = AGENT
            .replace("model: opus", &format!("model: {model}"))
            .replace("effort: high", &format!("effort: {effort}"));
        write(&dir, "agents/reviewer.md", agent);
        let (code, json) = check(&dir, &["--only", "AG004"]);
        assert_eq!(code, i32::from(!ok), "{model}+{effort}: {json}");
    }
}

#[test]
fn ag004_pairs_come_from_config() {
    let dir = clean();
    write(
        &dir,
        "agents/reviewer.md",
        AGENT.replace("effort: high", "effort: low"),
    );
    assert_eq!(check(&dir, &["--only", "AG004"]).0, 1);
    write(
        &dir,
        "agenvil.toml",
        format!(
            "{CONFIG}[policy.models.claude]\npairs = [[\"opus\", \"low\"]]\nefforts = [\"low\", \"high\"]\n"
        ),
    );
    assert_eq!(check(&dir, &["--only", "AG004"]).0, 0);
}

#[test]
fn ag005_requires_a_non_empty_list() {
    let dir = clean();
    for tools in ["Read, Grep", "[]", "[Read, \"\"]", "{a: 1}"] {
        write(
            &dir,
            "agents/reviewer.md",
            AGENT.replace("[Read, Grep]", tools),
        );
        assert_finds(&dir, "AG005", "agents/reviewer.md", Some(6));
    }
}

#[test]
fn model_policy_is_configurable() {
    let dir = clean();
    write(&dir, "POLICY.md", POLICY.replace("| opus |", "| sonnet |"));
    write(
        &dir,
        "agents/reviewer.md",
        AGENT.replace("model: opus", "model: sonnet"),
    );
    write(
        &dir,
        "agenvil.toml",
        format!("{CONFIG}[policy.models.claude]\nmodels = [\"opus\"]\n"),
    );
    assert_eq!(check(&dir, &["--only", "AG002"]).0, 1);
    write(
        &dir,
        "agenvil.toml",
        format!(
            "{CONFIG}[policy.models.claude]\nmodels = [\"sonnet\"]\npairs = [[\"sonnet\", \"high\"]]\n"
        ),
    );
    assert_eq!(check(&dir, &[]).0, 0);
}

#[test]
fn an_allowlist_never_makes_a_non_native_model_acceptable() {
    let dir = clean();
    write(
        &dir,
        "agents/reviewer.md",
        AGENT.replace("model: opus", "model: custom"),
    );
    write(
        &dir,
        "agenvil.toml",
        format!(
            "{CONFIG}[policy.models.claude]\nmodels = [\"custom\"]\npairs = [[\"custom\", \"high\"]]\n"
        ),
    );
    let (code, json) = check(&dir, &["--only", "AG002"]);
    assert_eq!(code, 1);
    assert!(
        json["findings"][0]["message"]
            .as_str()
            .unwrap()
            .contains("custom")
    );
}

// -- SK001, SK002 ----------------------------------------------------------------------------

#[test]
fn sk001_name_description_and_missing_file() {
    let dir = clean();
    write(
        &dir,
        "skills/tdd/SKILL.md",
        "---\nname: other\ndescription: \"  \"\n---\n",
    );
    let (code, json) = check(&dir, &["--only", "SK001"]);
    assert_eq!(code, 1);
    assert_eq!(
        found(&json),
        [
            (
                "SK001".to_owned(),
                "skills/tdd/SKILL.md".to_owned(),
                Some(2)
            ),
            (
                "SK001".to_owned(),
                "skills/tdd/SKILL.md".to_owned(),
                Some(3)
            ),
        ]
    );

    let dir = clean();
    fs::create_dir(dir.path().join("skills/empty")).unwrap();
    // A missing SKILL.md is the rule's finding, not an unreadable input.
    assert_finds(&dir, "SK001", "skills/empty", None);
    // With SK001 off it is an input problem again.
    agenvil(&dir)
        .args(["check", "--skip", "SK001"])
        .assert()
        .code(4);
}

#[test]
fn sk001_missing_key_and_no_frontmatter() {
    let dir = clean();
    write(
        &dir,
        "skills/tdd/SKILL.md",
        "---\nmodel: inherit\neffort: inherit\n---\n",
    );
    let (_, json) = check(&dir, &["--only", "SK001"]);
    assert_eq!(found(&json).len(), 2);
    write(&dir, "skills/tdd/SKILL.md", "plain\n");
    assert_finds(&dir, "SK001", "skills/tdd/SKILL.md", None);
}

#[test]
fn sk002_is_a_warning_and_needs_fail_on_warning() {
    let dir = clean();
    write(
        &dir,
        "skills/tdd/SKILL.md",
        "---\nname: tdd\ndescription: d\nmodel: sonnet\n---\n",
    );
    let (code, json) = check(&dir, &["--only", "SK002"]);
    assert_eq!(code, 0, "warnings alone do not fail by default");
    assert_eq!(json["summary"], json!({"errors": 0, "warnings": 1}));
    assert_eq!(json["findings"][0]["severity"], "warning");
    assert_eq!(
        check(&dir, &["--only", "SK002", "--fail-on", "warning"]).0,
        1
    );
}

// -- PO001 -----------------------------------------------------------------------------------

#[test]
fn po001_flags_mismatch_missing_unknown_and_duplicate_rows() {
    let dir = clean();
    write(
        &dir,
        "POLICY.md",
        POLICY.replace("| opus | high |", "| sonnet | high |"),
    );
    assert_finds(&dir, "PO001", "POLICY.md", Some(5));

    write(
        &dir,
        "POLICY.md",
        POLICY.replace("| reviewer | opus | high |\n", ""),
    );
    assert_finds(&dir, "PO001", "POLICY.md", Some(2));

    write(
        &dir,
        "POLICY.md",
        POLICY.replace("<!-- /kit", "| ghost | opus | high |\n<!-- /kit"),
    );
    assert_finds(&dir, "PO001", "POLICY.md", Some(6));

    write(
        &dir,
        "POLICY.md",
        POLICY.replace("<!-- /kit", "| reviewer | opus | high |\n<!-- /kit"),
    );
    assert_finds(&dir, "PO001", "POLICY.md", Some(6));
}

#[test]
fn po001_flags_a_broken_table() {
    let dir = clean();
    for (policy, line) in [
        ("# no table\n", None),
        (
            "<!-- kit:model-table -->\n| agent | model | effort |\n|---|---|---|\n",
            Some(1),
        ),
        (
            "<!-- kit:model-table -->\n| name | model | effort |\n|---|---|---|\n<!-- /kit:model-table -->\n",
            Some(2),
        ),
        (
            "<!-- kit:model-table -->\n| agent | model | effort |\n| reviewer | opus | high |\n<!-- /kit:model-table -->\n",
            Some(2),
        ),
        (
            "<!-- kit:model-table -->\n| agent | model | effort |\n|---|---|---|\n| reviewer | opus |\n<!-- /kit:model-table -->\n",
            Some(4),
        ),
    ] {
        write(&dir, "POLICY.md", policy);
        let (code, json) = check(&dir, &["--only", "PO001"]);
        assert_eq!(code, 1, "{policy:?}: {json}");
        // Follow-on findings (e.g. the agent now lacking a row) may precede the broken line.
        assert!(
            found(&json).iter().any(|f| f.2 == line),
            "{policy:?}: {json}"
        );
    }
}

#[test]
fn po001_separator_needs_one_cell_per_column() {
    let dir = clean();
    for (separator, ok) in [
        ("|---|", false),
        ("|---|---|", false),
        ("|:---|---:|:-:|", true),
        ("|---|---|---|---|", false),
    ] {
        write(
            &dir,
            "POLICY.md",
            POLICY.replace("|---|---|---|", separator),
        );
        let (code, json) = check(&dir, &["--only", "PO001"]);
        assert_eq!(code, if ok { 0 } else { 1 }, "{separator}: {json}");
        if !ok {
            // Reported at the header the separator belongs to.
            assert!(
                found(&json).iter().any(|f| f.2 == Some(3)),
                "{separator}: {json}"
            );
        }
    }
}

#[test]
fn po001_accepts_crlf_and_a_bom() {
    let dir = clean();
    write(
        &dir,
        "POLICY.md",
        format!("\u{feff}{}", POLICY.replace('\n', "\r\n")),
    );
    assert_eq!(check(&dir, &["--only", "PO001"]).0, 0);
}

#[test]
fn po001_missing_policy_with_agents_cannot_be_verified() {
    let dir = clean();
    fs::remove_file(dir.path().join("POLICY.md")).unwrap();
    agenvil(&dir)
        .args(["check", "--only", "PO001"])
        .assert()
        .code(4)
        .stderr(predicate::str::contains("POLICY.md").and(predicate::str::contains("missing")));
    // Skipping the rule is the documented way out; so is having no agents at all.
    assert_eq!(check(&dir, &["--skip", "PO001"]).0, 0);
    fs::remove_file(dir.path().join("agents/reviewer.md")).unwrap();
    agenvil(&dir)
        .args(["check", "--only", "PO001"])
        .assert()
        .code(0);
}

// -- CF001 -----------------------------------------------------------------------------------

#[test]
fn cf001_validates_defaults_toml() {
    let dir = clean();
    for (defaults, line, column) in [
        ("[update]\nauto_update = \"weekly\"\n", 2, 15),
        ("[bogus]\nx = 1\n", 1, 2),
        ("[hooks]\nupdte = true\n", 2, 1),
        ("[hooks]\nupdate = \"yes\"\n", 2, 10),
        ("[headroom]\nport = \"80\"\n", 2, 8),
        ("update = 1\n", 1, 10),
        ("[hooks\n", 1, 7),
    ] {
        write(&dir, "config/defaults.toml", defaults);
        let (code, json) = check(&dir, &["--only", "CF001"]);
        assert_eq!(code, 1, "{defaults:?}: {json}");
        let f = &json["findings"][0];
        assert_eq!(f["path"], "config/defaults.toml");
        assert_eq!(
            (f["line"].as_u64(), f["column"].as_u64()),
            (Some(line), Some(column)),
            "{defaults:?}: {json}"
        );
    }
}

#[test]
fn cf001_absent_defaults_is_not_an_error() {
    let dir = clean();
    fs::remove_file(dir.path().join("config/defaults.toml")).unwrap();
    assert_eq!(check(&dir, &["--only", "CF001"]).0, 0);
}

// -- HY001, HY002 ----------------------------------------------------------------------------

#[test]
fn hy001_finds_personal_paths_in_agents_skills_and_rules() {
    let dir = clean();
    write(
        &dir,
        "agents/reviewer.md",
        format!("{AGENT}see /Users/alice/x\n"),
    );
    write(
        &dir,
        "skills/tdd/SKILL.md",
        format!("{SKILL}~/.agents/foo\n"),
    );
    write(&dir, "rules/style.md", "ok /Users/bob\n");
    let (code, json) = check(&dir, &["--only", "HY001"]);
    assert_eq!(code, 1);
    assert_eq!(
        found(&json),
        [
            ("HY001".to_owned(), "agents/reviewer.md".to_owned(), Some(9)),
            ("HY001".to_owned(), "rules/style.md".to_owned(), Some(1)),
            (
                "HY001".to_owned(),
                "skills/tdd/SKILL.md".to_owned(),
                Some(8)
            ),
        ]
    );
    assert_eq!(json["findings"][1]["column"], 4);
}

#[test]
fn hy001_patterns_are_configurable_regexes() {
    let dir = clean();
    write(
        &dir,
        "rules/style.md",
        "mail me at a@corp.example\n/Users/x\n",
    );
    write(
        &dir,
        "agenvil.toml",
        format!("{CONFIG}[hygiene]\npersonal_patterns = ['@corp\\.example$']\n"),
    );
    let (code, json) = check(&dir, &["--only", "HY001"]);
    assert_eq!(code, 1);
    assert_eq!(
        found(&json),
        [("HY001".to_owned(), "rules/style.md".to_owned(), Some(1))]
    );
}

#[cfg(unix)]
#[test]
fn hy002_requires_shebang_and_executable_bit() {
    let dir = clean();
    write(&dir, "bin/noshebang", "echo\n");
    make_executable(&dir, "bin/noshebang");
    assert_finds(&dir, "HY002", "bin/noshebang", Some(1));

    let dir = clean();
    write(&dir, "bin/plain", "#!/bin/sh\n");
    assert_finds(&dir, "HY002", "bin/plain", None);
}

#[cfg(unix)]
#[test]
fn symlinks_in_inputs_are_problems_not_findings() {
    let dir = clean();
    std::os::unix::fs::symlink("tool", dir.path().join("bin/link")).unwrap();
    agenvil(&dir)
        .args(["check", "--only", "HY002"])
        .assert()
        .code(4)
        .stderr(predicate::str::contains("bin/link").and(predicate::str::contains("symlink")));
    std::os::unix::fs::symlink("agents", dir.path().join("POLICY2.md")).unwrap();
    fs::remove_file(dir.path().join("POLICY.md")).unwrap();
    std::os::unix::fs::symlink("agents/reviewer.md", dir.path().join("POLICY.md")).unwrap();
    agenvil(&dir)
        .args(["check", "--only", "PO001"])
        .assert()
        .code(4)
        .stderr(predicate::str::contains("POLICY.md"));
}

// -- selection, severity, thresholds ----------------------------------------------------------

fn broken() -> TempDir {
    let dir = clean();
    write(
        &dir,
        "agents/reviewer.md",
        AGENT
            .replace("model: opus", "model: gpt")
            .replace("effort: high", "effort: max"),
    );
    dir
}

#[test]
fn only_and_skip_select_rules() {
    let dir = broken();
    assert_eq!(
        ids(&check(&dir, &[]).1),
        ["PO001", "AG002", "AG003", "AG004"]
    );
    assert_eq!(
        ids(&check(&dir, &["--only", "AG002,AG003"]).1),
        ["AG002", "AG003"]
    );
    assert_eq!(
        ids(&check(&dir, &["--only", "AG002", "--only", "AG003"]).1),
        ["AG002", "AG003"]
    );
    assert_eq!(
        ids(&check(&dir, &["--skip", "AG002,AG004,PO001"]).1),
        ["AG003"]
    );
    // skip wins over only
    let (code, json) = check(&dir, &["--only", "AG002", "--skip", "AG002"]);
    assert_eq!((code, ids(&json).len()), (0, 0));
}

#[test]
fn findings_are_ordered_by_path_then_position_and_are_deterministic() {
    let dir = broken();
    write(&dir, "agents/a.md", "---\nname: a\n---\n");
    let first = check(&dir, &[]);
    let second = check(&dir, &[]);
    assert_eq!(first, second);
    let order: Vec<_> = found(&first.1).into_iter().map(|f| (f.1, f.2)).collect();
    let mut sorted = order.clone();
    sorted.sort();
    assert_eq!(order, sorted);
    // Bytewise: uppercase sorts before lowercase.
    assert_eq!(order[0].0, "POLICY.md");
}

#[test]
fn severity_overrides_and_off() {
    let dir = broken();
    write(
        &dir,
        "agenvil.toml",
        format!("{CONFIG}[rules]\nAG002 = \"warning\"\nAG003 = \"off\"\n"),
    );
    let (code, json) = check(&dir, &[]);
    assert_eq!(code, 1);
    assert_eq!(json["summary"], json!({"errors": 2, "warnings": 1}));
    assert_eq!(ids(&json), ["PO001", "AG002", "AG004"]);
    // Turning the errors into warnings makes the default threshold pass.
    write(
        &dir,
        "agenvil.toml",
        format!(
            "{CONFIG}[rules]\nAG002 = \"warning\"\nAG003 = \"off\"\nAG004 = \"warning\"\nPO001 = \"off\"\n"
        ),
    );
    assert_eq!(check(&dir, &[]).0, 0);
    assert_eq!(check(&dir, &["--fail-on", "warning"]).0, 1);
    // Naming an `off` rule on the command line runs it.
    assert_eq!(ids(&check(&dir, &["--only", "AG003"]).1), ["AG003"]);
}

#[test]
fn ignore_entries_need_a_reason_and_suppress_one_rule_in_one_file() {
    let dir = broken();
    write(
        &dir,
        "agenvil.toml",
        format!(
            "{CONFIG}[[ignore]]\nrule = \"AG002\"\npath = \"agents/reviewer.md\"\nreason = \"pinned vendor model\"\n"
        ),
    );
    assert_eq!(ids(&check(&dir, &[]).1), ["PO001", "AG003", "AG004"]);

    write(
        &dir,
        "agenvil.toml",
        format!(
            "{CONFIG}[[ignore]]\nrule = \"AG002\"\npath = \"agents/reviewer.md\"\nreason = \" \"\n"
        ),
    );
    agenvil(&dir)
        .arg("check")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("`reason` must not be empty"));
}

// -- configuration and usage errors ----------------------------------------------------------

#[test]
fn invalid_rule_ids_exit_2_with_every_error_listed() {
    let dir = clean();
    agenvil(&dir)
        .args(["check", "--only", "AG001,NOPE", "--skip", "ZZ999"])
        .assert()
        .code(2)
        .stdout("")
        .stderr(
            predicate::str::contains("--only: unknown rule ID `NOPE`")
                .and(predicate::str::contains("--skip: unknown rule ID `ZZ999`"))
                .and(predicate::str::contains("agenvil rules")),
        );
    // Rule IDs are exact: no case folding, no prefixes.
    for id in ["ag001", "AG00", "AG"] {
        agenvil(&dir).args(["check", "--only", id]).assert().code(2);
    }
}

#[test]
fn invalid_policy_config_exits_2() {
    let dir = clean();
    for (extra, message) in [
        (
            "[rules]\nNOPE1 = \"error\"\n",
            "agenvil.toml [rules]: unknown rule ID `NOPE1`",
        ),
        ("[rules]\nAG001 = \"fatal\"\n", "unknown variant `fatal`"),
        (
            "[[ignore]]\nrule = \"NOPE1\"\npath = \"a.md\"\nreason = \"r\"\n",
            "[[ignore]]: unknown rule ID `NOPE1`",
        ),
        (
            "[[ignore]]\nrule = \"AG001\"\npath = \"../a.md\"\nreason = \"r\"\n",
            "stay inside",
        ),
        ("[[ignore]]\nrule = \"AG001\"\npath = \"a.md\"\n", "reason"),
        (
            "[hygiene]\npersonal_patterns = [\"(\"]\n",
            "personal_patterns",
        ),
        ("[hygiene]\npersonal_patterns = [\"\"]\n", "empty pattern"),
        (
            "[policy.models.claude]\nbogus = 1\n",
            "unknown field `bogus`",
        ),
        ("[policy.models.claude]\npairs = [[\"opus\"]]\n", "pairs"),
        ("[paths]\npolicy = \"/etc/passwd\"\n", "relative"),
    ] {
        write(&dir, "agenvil.toml", format!("{CONFIG}{extra}"));
        agenvil(&dir)
            .arg("check")
            .assert()
            .code(2)
            .stdout("")
            .stderr(predicate::str::contains(message));
    }
}

#[test]
fn bad_flag_values_exit_2() {
    let dir = clean();
    for args in [
        &["check", "--fail-on", "info"][..],
        &["check", "--format", "xml"],
        &["check", "--only"],
    ] {
        agenvil(&dir).args(args).assert().code(2);
    }
}

#[test]
fn config_path_override() {
    let dir = clean();
    let other = tempfile::tempdir().unwrap();
    write(
        &other,
        "custom.toml",
        format!("{CONFIG}[rules]\nAG002 = \"off\"\n"),
    );
    // Defaults to <root>/agenvil.toml; --config replaces it while --root keeps the project.
    agenvil(&other)
        .arg("--root")
        .arg(dir.path())
        .arg("--config")
        .arg(other.path().join("custom.toml"))
        .args(["check", "--skip", &deferred()])
        .assert()
        .code(0);
    agenvil(&other)
        .arg("--root")
        .arg(dir.path())
        .args(["--config", "missing.toml", "check"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("missing.toml"));
}

// -- incomplete inventories ------------------------------------------------------------------

#[test]
fn malformed_frontmatter_is_exit_4_not_a_policy_finding() {
    let dir = clean();
    write(&dir, "agents/broken.md", "---\nname: [unclosed\n---\n");
    let output = agenvil(&dir)
        .args(["check", "--format", "json"])
        .output()
        .unwrap();
    assert_eq!(output.status.code(), Some(4));
    let json: Value = serde_json::from_slice(&output.stdout).unwrap();
    assert_eq!(
        json["findings"],
        json!([]),
        "a parse failure is not a finding"
    );
    let stderr = String::from_utf8(output.stderr).unwrap();
    assert!(
        stderr.contains("agents/broken.md") && stderr.contains("frontmatter"),
        "{stderr}"
    );
}

#[test]
fn problems_take_precedence_over_findings_but_findings_are_still_printed() {
    let dir = broken();
    write(&dir, "agents/broken.md", "---\nname: [unclosed\n---\n");
    let (code, json) = check(&dir, &[]);
    assert_eq!(code, 4);
    assert!(!ids(&json).is_empty());
}

#[test]
fn oversized_and_invalid_inputs_are_problems() {
    let dir = clean();
    write(&dir, "POLICY.md", vec![b'a'; 2 * 1024 * 1024]);
    agenvil(&dir)
        .args(["check", "--only", "PO001"])
        .assert()
        .code(4)
        .stderr(predicate::str::contains("POLICY.md").and(predicate::str::contains("too-large")));
    write(&dir, "POLICY.md", [0xff, 0xfe, 0x00]);
    agenvil(&dir)
        .args(["check", "--only", "PO001"])
        .assert()
        .code(4)
        .stderr(predicate::str::contains("invalid-utf8"));
    write(&dir, "config/defaults.toml", [0xff, 0xff]);
    agenvil(&dir)
        .args(["check", "--only", "CF001"])
        .assert()
        .code(4)
        .stderr(predicate::str::contains("config/defaults.toml"));
}

#[test]
fn deferred_rules_are_never_reported_as_passing() {
    let dir = clean();
    // Named on the command line: no verdict is possible.
    for id in ["AG006", "CF003", "SK003"] {
        agenvil(&dir)
            .args(["check", "--only", id])
            .assert()
            .code(4)
            .stderr(predicate::str::contains(id).and(predicate::str::contains("not implemented")));
    }
    // Part of a normal run: reported as not run, never counted as passing, and not a failure.
    agenvil(&dir)
        .arg("check")
        .assert()
        .code(0)
        .stderr(predicate::str::contains("AG006").and(predicate::str::contains("SK003")));
}

// -- --fast ----------------------------------------------------------------------------------

#[test]
fn fast_excludes_sk002_and_cf003_and_rejects_naming_them() {
    let dir = clean();
    write(
        &dir,
        "skills/tdd/SKILL.md",
        "---\nname: tdd\ndescription: d\n---\n",
    );
    assert_eq!(check(&dir, &["--fast", "--fail-on", "warning"]).0, 0);
    assert_eq!(check(&dir, &["--fail-on", "warning"]).0, 1);
    for id in ["SK002", "CF003"] {
        agenvil(&dir)
            .args(["check", "--fast", "--only", id])
            .assert()
            .code(2)
            .stderr(predicate::str::contains(format!(
                "{id} is not part of --fast"
            )));
    }
    agenvil(&dir)
        .args(["check", "--fast"])
        .assert()
        .code(0)
        .stderr(predicate::str::contains("CF002").and(predicate::str::contains("CF003").not()));
}

#[test]
fn check_is_read_only_offline_and_does_not_run_repo_scripts() {
    let dir = clean();
    // A script that would leave evidence if anything ever executed it.
    write(&dir, "bin/evil", "#!/bin/sh\ntouch executed\n");
    make_executable(&dir, "bin/evil");
    write(&dir, ".git/hooks/pre-commit", "#!/bin/sh\ntouch executed\n");
    let before = snapshot(dir.path());
    let deferred = deferred();
    for args in [
        &["check", "--fast"][..],
        &["check"],
        &["check", "--format", "github"],
    ] {
        agenvil(&dir)
            // A proxy that would fail loudly if a network call were attempted.
            .env("HTTPS_PROXY", "http://127.0.0.1:1")
            .env("HTTP_PROXY", "http://127.0.0.1:1")
            .args(args)
            .args(["--skip", &deferred])
            .assert()
            .code(0);
    }
    assert_eq!(snapshot(dir.path()), before);
    assert!(!dir.path().join("executed").exists());
}

// -- output formats --------------------------------------------------------------------------

#[test]
fn github_format_emits_annotations() {
    let dir = broken();
    let out = agenvil(&dir)
        .args(["check", "--format", "github", "--only", "AG002,PO001"])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let out = String::from_utf8(out).unwrap();
    let lines: Vec<_> = out.lines().collect();
    assert_eq!(lines.len(), 2, "{out}");
    assert!(
        lines[0].starts_with("::error file=POLICY.md,line=5,col=1,title=PO001::"),
        "{out}"
    );
    assert!(
        lines[1].starts_with("::error file=agents/reviewer.md,line=4,col=1,title=AG002::"),
        "{out}"
    );
    // Warnings use the warning command.
    write(
        &dir,
        "agenvil.toml",
        format!("{CONFIG}[rules]\nAG002 = \"warning\"\n"),
    );
    agenvil(&dir)
        .args(["check", "--format", "github", "--only", "AG002"])
        .assert()
        .code(0)
        .stdout(predicate::str::starts_with(
            "::warning file=agents/reviewer.md",
        ));
}

#[test]
fn human_format_and_quiet() {
    let dir = broken();
    let loud = agenvil(&dir)
        .args(["check", "--only", "AG002"])
        .assert()
        .code(1)
        .get_output()
        .stdout
        .clone();
    let loud = String::from_utf8(loud).unwrap();
    assert!(
        loud.starts_with("agents/reviewer.md:4:1: error[AG002] `model: \"gpt\"` is not allowed"),
        "{loud}"
    );
    assert!(
        loud.contains("    fix: ") && loud.ends_with("1 error(s), 0 warning(s)\n"),
        "{loud}"
    );

    let quiet = agenvil(&dir)
        .args(["check", "--only", "AG002", "--quiet"])
        .assert()
        .code(1)
        .stderr("error: 1 finding(s) at or above the `--fail-on` level\n")
        .get_output()
        .stdout
        .clone();
    let quiet = String::from_utf8(quiet).unwrap();
    assert_eq!(quiet.lines().count(), 1, "{quiet}");
    agenvil(clean().path())
        .args(["check", "--quiet", "--skip", &deferred()])
        .assert()
        .code(0)
        .stdout("")
        .stderr("");
}

#[test]
fn output_has_no_machine_specific_data() {
    let dir = broken();
    let output = agenvil(&dir)
        .args(["check", "--format", "json"])
        .output()
        .unwrap();
    let text = String::from_utf8(output.stdout).unwrap();
    let root = dir.path().to_string_lossy().into_owned();
    assert!(!text.contains(&root), "{text}");
    for f in serde_json::from_str::<Value>(&text).unwrap()["findings"]
        .as_array()
        .unwrap()
    {
        assert!(!f["path"].as_str().unwrap().contains('\\'), "{text}");
    }
}

// -- rules and explain -----------------------------------------------------------------------

#[test]
fn rules_lists_every_rule_with_status_and_explain_covers_each() {
    let dir = tempfile::tempdir().unwrap();
    let out = agenvil(&dir)
        .arg("rules")
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    let table = String::from_utf8(out).unwrap();
    let implemented = [
        "AG001", "AG002", "AG003", "AG004", "AG005", "SK001", "SK002", "CF001", "PO001", "HY001",
        "HY002",
    ];
    for id in implemented {
        let row = table
            .lines()
            .find(|l| l.starts_with(id))
            .unwrap_or_else(|| panic!("{id} missing:\n{table}"));
        assert!(row.contains("implemented"), "{row}");
    }
    let deferred = [
        "AG006", "AG007", "CF002", "CF003", "CF004", "SK003", "ST001", "TL001",
    ];
    for id in deferred {
        let row = table
            .lines()
            .find(|l| l.starts_with(id))
            .unwrap_or_else(|| panic!("{id} missing:\n{table}"));
        assert!(row.contains("deferred"), "{row}");
    }
    assert!(
        table
            .lines()
            .find(|l| l.starts_with("SK002"))
            .unwrap()
            .contains("warning")
    );
    for id in table
        .lines()
        .skip(1)
        .map(|l| l.split_whitespace().next().unwrap())
    {
        agenvil(&dir)
            .args(["explain", id])
            .assert()
            .code(0)
            .stdout(predicate::str::contains(id));
    }
    agenvil(&dir)
        .args(["explain", "CF003"])
        .assert()
        .code(0)
        .stdout(
            predicate::str::contains("Not checked")
                .and(predicate::str::contains("In `check --fast`: no")),
        );
}
