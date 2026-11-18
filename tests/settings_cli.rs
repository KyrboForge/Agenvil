#![cfg(any(target_os = "linux", target_os = "macos"))]

mod common;

use std::fs;
use std::path::PathBuf;

use common::{agenvil_in, snapshot};
use predicates::prelude::*;
use serde_json::{Value, json};
use tempfile::TempDir;

struct Fixture {
    home: TempDir,
    fragment: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let home = tempfile::tempdir().unwrap();
        let fragment = home.path().join("fragment.json");
        let group =
            json!({"matcher": "Bash", "hooks": [{"type": "command", "command": "guard.sh"}]});
        let body = json!({
            "schema": 1,
            "values": {"/model": "opus"},
            "hooks": [{"id": "guard", "event": "PreToolUse", "group": group}]
        });
        fs::write(&fragment, body.to_string()).unwrap();
        Self { home, fragment }
    }

    fn settings(&self) -> PathBuf {
        self.home.path().join("claude-for-test/settings.json")
    }

    // No project: the command needs no agenvil.toml.
    fn run(&self, options: &[&str]) -> assert_cmd::assert::Assert {
        agenvil_in(self.home.path(), self.home.path())
            .env(
                "CLAUDE_CONFIG_DIR",
                self.home.path().join("claude-for-test"),
            )
            .args(["settings", "--target", "claude"])
            .args(options)
            .assert()
    }

    fn install(&self, options: &[&str]) -> assert_cmd::assert::Assert {
        let fragment = self.fragment.to_str().unwrap();
        self.run(&[&["--fragment", fragment], options].concat())
    }
}

#[test]
fn dry_run_prints_a_json_plan_and_touches_nothing() {
    let fixture = Fixture::new();
    let before = snapshot(fixture.home.path());
    let output = fixture
        .install(&["--format", "json"])
        .success()
        .get_output()
        .stdout
        .clone();
    let plan: Value = serde_json::from_slice(&output).unwrap();
    assert_eq!(plan["summary"]["add"], 2);
    assert_eq!(plan["operations"][0]["target"], "/model");
    assert_eq!(plan["files"]["settings"]["action"], "create");
    assert_eq!(snapshot(fixture.home.path()), before);
}

#[test]
fn apply_installs_then_remove_uninstalls() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.home.path().join("claude-for-test")).unwrap();
    fs::write(fixture.settings(), r#"{"theme":"dark"}"#).unwrap();
    fixture
        .install(&["--apply"])
        .success()
        .stdout(predicate::str::contains("2 add"))
        .stdout(predicate::str::contains("Backup "));
    let doc: Value = serde_json::from_slice(&fs::read(fixture.settings()).unwrap()).unwrap();
    assert_eq!(doc["model"], "opus");
    assert_eq!(
        doc["hooks"]["PreToolUse"][0]["hooks"][0]["command"],
        "guard.sh"
    );
    fixture
        .install(&["--apply"])
        .success()
        .stdout(predicate::str::contains("2 unchanged"));
    fixture
        .run(&["--remove", "--apply"])
        .success()
        .stdout(predicate::str::contains("2 remove"));
    let doc: Value = serde_json::from_slice(&fs::read(fixture.settings()).unwrap()).unwrap();
    assert_eq!(doc, json!({"theme": "dark"}));
}

#[test]
fn conflicts_exit_one_and_leave_the_file() {
    let fixture = Fixture::new();
    fs::create_dir(fixture.home.path().join("claude-for-test")).unwrap();
    fs::write(fixture.settings(), r#"{"model":"haiku"}"#).unwrap();
    fixture
        .install(&["--apply"])
        .code(1)
        .stdout(predicate::str::contains("Conflict Value /model"))
        .stderr(predicate::str::contains("1 conflict(s)"));
    assert_eq!(
        fs::read(fixture.settings()).unwrap(),
        br#"{"model":"haiku"}"#
    );
}

#[test]
fn rejected_fragments_and_flags_are_usage_errors() {
    let fixture = Fixture::new();
    fs::write(
        &fixture.fragment,
        r#"{"schema":1,"values":{"/env/TOKEN":"x"}}"#,
    )
    .unwrap();
    fixture.install(&["--apply"]).code(2);
    assert!(!fixture.settings().exists());
    fixture.run(&[]).code(2);
    fixture.run(&["--remove", "--apply", "--dry-run"]).code(2);
}
