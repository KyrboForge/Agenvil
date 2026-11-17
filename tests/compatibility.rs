//! The fixture suite and the guards that keep `docs/compatibility.md` honest.
//!
//! Green here means: Agenvil's implemented rules behave as the fixtures say. It does NOT mean
//! legacy parity; no legacy oracle output is committed, and the matrix says which rules are
//! deferred or blocked.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::fs;
use std::path::Path;

use common::fixtures::{Case, Finding, TESTS, blessing, cases, run};
use common::{agenvil_in, rules};

const PARITY: [&str; 5] = [
    "verified",
    "approved deviation",
    "deferred",
    "unspecified",
    "blocked",
];

fn manifest(path: &str) -> String {
    let path = Path::new(env!("CARGO_MANIFEST_DIR")).join(path);
    fs::read_to_string(&path).unwrap_or_else(|e| panic!("{}: {e}", path.display()))
}

#[test]
fn fixtures_behave() {
    for case in cases().iter().filter(|c| c.applies()) {
        let json = run(case, "json");
        assert_eq!(json.exit, case.expect.exit, "{}: exit status", case.name);

        // `check` prints the findings even when the verdict is incomplete.
        // Usage and config errors (exit 2) print nothing on stdout.
        let doc: serde_json::Value = if json.exit == 2 {
            assert!(json.stdout.is_empty(), "{}: stdout on exit 2", case.name);
            serde_json::json!({ "findings": [] })
        } else {
            serde_json::from_slice(&json.stdout)
                .unwrap_or_else(|e| panic!("{}: stdout is not JSON: {e}", case.name))
        };
        let actual: Vec<_> = doc["findings"]
            .as_array()
            .unwrap()
            .iter()
            .map(|f| Finding {
                id: f["id"].as_str().unwrap().into(),
                severity: f["severity"].as_str().unwrap().into(),
                path: f["path"].as_str().unwrap().into(),
                line: f["line"].as_u64().map(|n| n as usize),
                column: f["column"].as_u64().map(|n| n as usize),
            })
            .collect();
        assert_eq!(actual, case.expect.findings, "{}: findings", case.name);

        for needle in &case.expect.stderr_contains {
            assert!(
                json.stderr.contains(needle),
                "{}: stderr lacks `{needle}`:\n{}",
                case.name,
                json.stderr
            );
        }
        for needle in &case.expect.stderr_excludes {
            assert!(
                !json.stderr.contains(needle),
                "{}: stderr has `{needle}`:\n{}",
                case.name,
                json.stderr
            );
        }

        // The format changes the rendering, never the verdict.
        for format in ["human", "github"] {
            assert_eq!(
                run(case, format).exit,
                case.expect.exit,
                "{}: exit status with --format {format}",
                case.name
            );
        }
    }
}

/// Rule ID -> fixtures that expect a finding from it.
fn failing(cases: &[Case]) -> BTreeMap<String, BTreeSet<String>> {
    let mut map: BTreeMap<String, BTreeSet<String>> = BTreeMap::new();
    for case in cases {
        for f in &case.expect.findings {
            map.entry(f.id.clone())
                .or_default()
                .insert(case.name.clone());
        }
    }
    map
}

#[test]
fn every_implemented_rule_has_a_failing_fixture() {
    let failing = failing(&cases());
    for rule in rules().iter().filter(|r| r.status == "implemented") {
        assert!(
            failing.contains_key(&rule.id),
            "{} is implemented but no fixture expects it to fire",
            rule.id
        );
    }
}

#[test]
fn default_check_notes_deferred_rules_and_named_ones_fail_closed() {
    let tmp = tempfile::tempdir().unwrap();
    let (project, home) = (tmp.path().join("p"), tmp.path().join("h"));
    fs::create_dir_all(&project).unwrap();
    fs::create_dir(&home).unwrap();
    fs::write(
        project.join("agenvil.toml"),
        "schema = 1\n[project]\nname = \"demo\"\n",
    )
    .unwrap();
    // A default run covers the implemented, applicable rules; the rest are reported, not passed.
    let out = agenvil_in(&project, &home).arg("check").output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "deferred rules are not a failure"
    );
    let stderr = String::from_utf8(out.stderr).unwrap();
    assert!(stderr.contains("not implemented yet") && stderr.contains("AG006"));
    // Selecting one explicitly leaves no verdict.
    let out = agenvil_in(&project, &home)
        .args(["check", "--only", "AG006"])
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(4),
        "a pass must not hide a named deferred rule"
    );
}

struct Row {
    cells: Vec<String>,
}

/// The rows of the matrix in `docs/compatibility.md`: `| ID | ... |` with a rule ID first.
fn matrix() -> BTreeMap<String, Row> {
    let mut map = BTreeMap::new();
    for line in manifest("docs/compatibility.md").lines() {
        let cells: Vec<String> = line
            .trim()
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim().to_owned())
            .collect();
        let id = cells[0].clone();
        let is_id = id.len() == 5
            && id[..2].chars().all(|c| c.is_ascii_uppercase())
            && id[2..].chars().all(|c| c.is_ascii_digit());
        if line.trim_start().starts_with('|') && is_id {
            assert!(
                map.insert(id.clone(), Row { cells }).is_none(),
                "{id} twice"
            );
        }
    }
    map
}

#[test]
fn matrix_covers_the_registry() {
    let matrix = matrix();
    let rules = rules();
    let ids: BTreeSet<_> = rules.iter().map(|r| r.id.clone()).collect();
    assert_eq!(
        matrix.keys().cloned().collect::<BTreeSet<_>>(),
        ids,
        "docs/compatibility.md must have exactly one row per registry rule"
    );
    let failing = failing(&cases());
    // Columns: ID | Description | Legacy | Implementation | Exit | Fast | Fixtures | Parity | Follow-up
    for rule in &rules {
        let row = &matrix[&rule.id].cells;
        assert_eq!(row.len(), 9, "{}: expected 9 columns", rule.id);
        assert!(
            row[1].contains(&rule.severity),
            "{}: severity {}",
            rule.id,
            rule.severity
        );
        assert_eq!(row[3], rule.status, "{}: implementation status", rule.id);
        assert_eq!(row[5], rule.fast, "{}: fast-mode column", rule.id);
        let fixtures = if failing.contains_key(&rule.id) {
            "failing"
        } else {
            "none"
        };
        assert_eq!(row[6], fixtures, "{}: fixture column", rule.id);
        let parity = row[7].as_str();
        assert!(
            PARITY.contains(&parity),
            "{}: unknown parity `{parity}`",
            rule.id
        );
        // A deferred rule is never a passing check, and an implemented one is not "deferred".
        assert_eq!(
            parity == "deferred",
            rule.status == "deferred",
            "{}: parity",
            rule.id
        );
        // `verified` needs committed oracle output.
        assert!(
            parity != "verified" || Path::new(TESTS).join("oracle").is_dir(),
            "{}: `verified` needs committed legacy oracle results under tests/oracle/",
            rule.id
        );
    }
}

fn coverage() -> String {
    let cases = cases();
    let failing = failing(&cases);
    let goldens: BTreeMap<String, usize> = cases.iter().fold(BTreeMap::new(), |mut m, c| {
        for f in c
            .expect
            .findings
            .iter()
            .map(|f| &f.id)
            .collect::<BTreeSet<_>>()
        {
            if !c.expect.golden.is_empty() {
                *m.entry(f.clone()).or_default() += 1;
            }
        }
        m
    });
    let matrix = matrix();
    let count = |exit: i32| cases.iter().filter(|c| c.expect.exit == exit).count();
    let rules = rules();
    let implemented = rules.iter().filter(|r| r.status == "implemented").count();
    let parity = |p: &str| matrix.values().filter(|r| r.cells[7] == p).count();

    let mut out = String::from(
        "# Fixture coverage\n\n\
         Generated by `AGENVIL_BLESS=1 cargo test --test compatibility`; checked for currency in CI.\n\
         The fixtures and goldens are Agenvil's own regression baseline. **None is legacy shell output**;\n\
         see [`docs/compatibility.md`](../../docs/compatibility.md) for parity.\n\n",
    );
    out += &format!(
        "| Measure | Count |\n|---|---|\n\
         | Registry rules | {} |\n| Implemented | {implemented} |\n| Deferred | {} |\n\
         | Fixtures | {} |\n| Pass (exit 0) | {} |\n| Policy violations (exit 1) | {} |\n\
         | Usage or config errors (exit 2) | {} |\n| Incomplete verdict (exit 4) | {} |\n\
         | Fixtures with golden snapshots | {} |\n\
         | Parity verified against the shell | {} |\n| Parity approved deviation | {} |\n\
         | Parity blocked (no oracle) | {} |\n| Parity deferred (rule not implemented) | {} |\n\
         | Parity unspecified | {} |\n\n",
        rules.len(),
        rules.len() - implemented,
        cases.len(),
        count(0),
        count(1),
        count(2),
        count(4),
        cases.iter().filter(|c| !c.expect.golden.is_empty()).count(),
        parity("verified"),
        parity("approved deviation"),
        parity("blocked"),
        parity("deferred"),
        parity("unspecified"),
    );
    out += "## Per rule\n\n| Rule | Status | Failing fixtures | With golden | Parity |\n|---|---|---|---|---|\n";
    for r in &rules {
        out += &format!(
            "| {} | {} | {} | {} | {} |\n",
            r.id,
            r.status,
            failing.get(&r.id).map_or(0, |s| s.len()),
            goldens.get(&r.id).copied().unwrap_or(0),
            matrix[&r.id].cells[7],
        );
    }
    out +=
        "\n## Fixtures\n\n| Fixture | Exit | Rules | Golden | Platform |\n|---|---|---|---|---|\n";
    for c in &cases {
        let ids: BTreeSet<_> = c.expect.findings.iter().map(|f| f.id.as_str()).collect();
        out += &format!(
            "| {} | {} | {} | {} | {} |\n",
            c.name,
            c.expect.exit,
            ids.into_iter().collect::<Vec<_>>().join(", "),
            c.expect.golden.join(", "),
            c.expect.platform.as_deref().unwrap_or("all"),
        );
    }
    out
}

#[test]
fn coverage_report_is_current() {
    let path = Path::new(TESTS).join("golden/coverage.md");
    let current = coverage();
    if blessing() {
        fs::create_dir_all(path.parent().unwrap()).unwrap();
        fs::write(&path, current).unwrap();
    } else {
        let committed = fs::read_to_string(&path).unwrap_or_default();
        assert!(
            committed == current,
            "tests/golden/coverage.md is out of date; run `AGENVIL_BLESS=1 cargo test --test compatibility` and review the diff"
        );
    }
}
