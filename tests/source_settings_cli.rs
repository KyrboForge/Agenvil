#![cfg(any(target_os = "linux", target_os = "macos"))]

mod common;
#[path = "common/lifecycle.rs"]
mod fixture;

use std::fs;
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::time::SystemTime;

use predicates::prelude::*;
use serde_json::{Value, json};

use fixture::{Fixture, output_json, put};

const CONFIG: &str = "schema = 1\n[project]\nname = 'lifecycle-test'\n";
const FRAGMENT: &str = "config/claude-settings.json";
const COMMAND: &str = "printf harmless > hook-ran";

fn declare(fixture: &Fixture, path: &str) {
    put(
        &fixture.project().join("agenvil.toml"),
        format!("{CONFIG}\n[targets.claude.settings]\nfragment = '{path}'\n"),
    );
}

fn group(command: &str) -> Value {
    json!({"matcher": "Bash", "hooks": [{"type": "command", "command": command}]})
}

fn write_fragment(fixture: &Fixture, model: &str, command: Option<&str>) {
    let hooks: Vec<_> = command
        .map(|command| json!({"id": "rtk-pretooluse", "event": "PreToolUse", "group": group(command)}))
        .into_iter()
        .collect();
    put(
        &fixture.project().join(FRAGMENT),
        json!({"schema": 1, "values": {"/model": model}, "hooks": hooks}).to_string(),
    );
    declare(fixture, FRAGMENT);
}

fn state(fixture: &Fixture) -> PathBuf {
    fixture.local.join("state/managed.json")
}

fn read_json(path: PathBuf) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

fn mtimes(fixture: &Fixture) -> Vec<(PathBuf, SystemTime)> {
    let home = fixture.local.parent().unwrap();
    fixture
        .snapshot()
        .into_iter()
        .map(|(path, _)| {
            let time = fs::symlink_metadata(home.join(&path))
                .unwrap()
                .modified()
                .unwrap();
            (path, time)
        })
        .collect()
}

fn doctor_status(report: &Value, id: &str, expected: &str) {
    assert!(
        report["checks"]
            .as_array()
            .unwrap()
            .iter()
            .any(|check| check["id"] == id && check["status"] == expected),
        "{id} should be {expected}: {report}"
    );
}

#[test]
fn sync_without_declaration_leaves_unmanaged_settings_alone() {
    let fixture = Fixture::new();
    put(&fixture.settings(), r#"{"model":"user"}"#);
    fixture.build();
    fixture.sync();
    assert_eq!(
        fs::read(fixture.settings()).unwrap(),
        br#"{"model":"user"}"#
    );
    assert!(!state(&fixture).exists());
}

#[test]
fn source_sync_previews_hook_commands_and_applies_idempotently() {
    let fixture = Fixture::new();
    write_fragment(&fixture, "opus", Some(COMMAND));
    fixture.build();
    assert!(!fixture.project().join("hook-ran").exists());
    let project = common::snapshot(fixture.project());
    let before = fixture.snapshot();

    fixture.run("sync", &[]).success().stdout(
        predicate::str::contains("Create rules/a.md")
            .and(predicate::str::contains("rtk-pretooluse"))
            .and(predicate::str::contains("PreToolUse"))
            .and(predicate::str::contains(COMMAND)),
    );
    let plan = output_json(&fixture.run("sync", &["--format", "json"]).success());
    assert_eq!(plan["operations"][0]["operation"], "create");
    let hook = &plan["settings"]["operations"][1];
    assert_eq!(hook["operation"], "add");
    assert_eq!(hook["event"], "PreToolUse");
    assert_eq!(hook["commands"], json!([COMMAND]));
    assert_eq!(fixture.snapshot(), before);

    let applied = output_json(
        &fixture
            .run("sync", &["--apply", "--format", "json"])
            .success(),
    );
    assert_eq!(applied, plan);
    assert!(fixture.claude.join("rules/a.md").is_symlink());
    assert!(!fixture.claude.join("hook-ran").exists());
    assert_eq!(common::snapshot(fixture.project()), project);
    let doc = read_json(fixture.settings());
    assert_eq!(doc["model"], "opus");
    assert_eq!(doc["hooks"]["PreToolUse"], json!([group(COMMAND)]));

    let before = fixture.snapshot();
    let times = mtimes(&fixture);
    let plan = output_json(
        &fixture
            .run("sync", &["--format", "json", "--apply"])
            .success(),
    );
    assert_eq!(plan["summary"]["noop"], 2);
    assert_eq!(plan["settings"]["summary"]["noop"], 2);
    assert_eq!(fixture.snapshot(), before);
    assert_eq!(mtimes(&fixture), times);
}

#[test]
fn source_sync_updates_and_removes_owned_entries_preserving_user_hook_order() {
    let fixture = Fixture::new();
    write_fragment(&fixture, "opus", Some(COMMAND));
    fixture.build();
    let first = group("user-first");
    let last = group("user-last");
    put(
        &fixture.settings(),
        json!({"user": true, "hooks": {"PreToolUse": [first]}}).to_string(),
    );
    fixture.sync();

    let mut doc = read_json(fixture.settings());
    doc["hooks"]["PreToolUse"]
        .as_array_mut()
        .unwrap()
        .push(last.clone());
    put(&fixture.settings(), doc.to_string());
    write_fragment(&fixture, "sonnet", Some("printf updated"));
    let plan = output_json(
        &fixture
            .run("sync", &["--format", "json", "--apply"])
            .success(),
    );
    let hook = &plan["settings"]["operations"][1];
    assert_eq!(hook["operation"], "change");
    assert_eq!(hook["event"], "PreToolUse");
    assert_eq!(hook["commands"], json!(["printf updated"]));
    assert_eq!(
        read_json(fixture.settings()),
        json!({
            "model": "sonnet", "user": true,
            "hooks": {"PreToolUse": [first, group("printf updated"), last]}
        })
    );

    write_fragment(&fixture, "sonnet", None);
    fixture.sync();
    assert_eq!(
        read_json(fixture.settings())["hooks"]["PreToolUse"],
        json!([first, last])
    );
    write_fragment(&fixture, "sonnet", Some(COMMAND));
    fixture.sync();
    put(&fixture.project().join("agenvil.toml"), CONFIG);
    fixture.sync();
    assert_eq!(
        read_json(fixture.settings()),
        json!({
            "user": true, "hooks": {"PreToolUse": [first, last]}
        })
    );
    let owner = &read_json(state(&fixture))["harnesses"]["claude"]["user"];
    assert_eq!(owner["values"], json!([]));
    assert_eq!(owner["hooks"], json!([]));
}

#[test]
fn source_conflicts_block_even_new_links_and_preserve_managed_user_edits() {
    let fixture = Fixture::new();
    write_fragment(&fixture, "opus", Some(COMMAND));
    fixture.build();
    put(&fixture.settings(), "{");
    let before = fixture.snapshot();
    fixture.run("sync", &["--apply"]).code(2);
    assert_eq!(fixture.snapshot(), before);

    put(&fixture.settings(), r#"{"model":"user"}"#);
    let before = fixture.snapshot();
    let plan = output_json(
        &fixture
            .run("sync", &["--apply", "--format", "json"])
            .code(1),
    );
    assert_eq!(plan["settings"]["operations"][0]["reason"], "user_value");
    assert!(!fixture.claude.join("rules/a.md").exists());
    assert_eq!(fixture.snapshot(), before);

    fs::remove_file(fixture.settings()).unwrap();
    fixture.sync();
    put(&fixture.project().join("rules/c.md"), "# Rule C\n");
    fixture.build();
    let mut doc = read_json(fixture.settings());
    doc["model"] = json!("user edit");
    doc["hooks"]["PreToolUse"][0]["matcher"] = json!("User edit");
    put(&fixture.settings(), doc.to_string());
    let before = fixture.snapshot();
    let plan = output_json(
        &fixture
            .run("sync", &["--apply", "--format", "json"])
            .code(1),
    );
    assert_eq!(plan["settings"]["operations"][0]["reason"], "user_edited");
    assert_eq!(plan["settings"]["operations"][1]["reason"], "hook_modified");
    assert!(!fixture.claude.join("rules/c.md").exists());
    assert_eq!(fixture.snapshot(), before);
    let report = output_json(&fixture.run("doctor", &["--format", "json"]).code(1));
    doctor_status(&report, "settings.consistency", "drifted");
    doctor_status(&report, "settings.desired", "user_action_required");
}

#[test]
fn source_fragment_rejects_invalid_missing_and_unsafe_paths_before_apply() {
    let fixture = Fixture::new();
    fixture.build();
    let before = fixture.snapshot();

    declare(&fixture, FRAGMENT);
    let run = |message: &str| {
        fixture
            .run("sync", &["--apply"])
            .code(2)
            .stderr(predicate::str::contains(message));
        assert_eq!(fixture.snapshot(), before);
    };
    run("missing settings fragment");
    let path = fixture.project().join(FRAGMENT);
    put(&path, "{");
    run("invalid settings fragment");
    put(&path, r#"{"schema":1,"values":{"/env/SECRET":"value"}}"#);
    run("/env/SECRET");
    put(&path, vec![b' '; 8 * 1024 * 1024 + 1]);
    run("exceeds");
    fs::remove_file(&path).unwrap();
    fs::create_dir(&path).unwrap();
    run("regular file");
    fs::remove_dir(&path).unwrap();
    let other = tempfile::NamedTempFile::new().unwrap();
    symlink(other.path(), &path).unwrap();
    run("symlink");
    declare(&fixture, "../x");
    run("relative");
    declare(&fixture, other.path().to_str().unwrap());
    run("relative");
    symlink(
        other.path().parent().unwrap(),
        fixture.project().join("linked"),
    )
    .unwrap();
    declare(&fixture, "linked/anything.json");
    run("symlink");
}

#[test]
fn settings_failure_after_links_is_reported_and_sync_retries() {
    let fixture = Fixture::new();
    write_fragment(&fixture, "opus", Some(COMMAND));
    fixture.build();
    put(&fixture.settings(), r#"{"user":true}"#);
    let blocker = fixture.local.join("state/backups/claude-settings");
    put(&blocker, "block the settings backup");
    fixture.run("sync", &["--apply"]).failure().stderr(
        predicate::str::contains("links applied; settings failed")
            .and(predicate::str::contains("re-run sync")),
    );
    assert!(fixture.claude.join("rules/a.md").is_symlink());
    assert_eq!(read_json(fixture.settings()), json!({"user":true}));

    fs::remove_file(blocker).unwrap();
    fixture.sync();
    assert_eq!(read_json(fixture.settings())["model"], "opus");
}

#[test]
fn doctor_compares_desired_settings_and_reports_pending_recovery_read_only() {
    let fixture = Fixture::new();
    fixture.build();
    fixture.sync();
    write_fragment(&fixture, "opus", Some(COMMAND));

    let diagnose = |code| {
        let before = fixture.snapshot();
        let times = mtimes(&fixture);
        let report = output_json(&fixture.run("doctor", &["--format", "json"]).code(code));
        assert_eq!(fixture.snapshot(), before);
        assert_eq!(mtimes(&fixture), times);
        report
    };
    let report = diagnose(1);
    doctor_status(&report, "settings.source", "healthy");
    doctor_status(&report, "settings.desired", "drifted");
    fixture.sync();
    doctor_status(&diagnose(0), "settings.desired", "healthy");

    let bytes = fs::read(fixture.settings()).unwrap();
    let mut managed = read_json(state(&fixture));
    let owner = &mut managed["harnesses"]["claude"]["user"];
    owner["pending"] = json!({
        "before": null,
        "after": agenvil::manifest::sha256(&bytes),
        "values": owner["values"],
        "hooks": owner["hooks"],
    });
    put(&state(&fixture), managed.to_string());
    doctor_status(
        &diagnose(1),
        "operations.interrupted",
        "user_action_required",
    );
    fixture.sync();
    assert!(read_json(state(&fixture))["harnesses"]["claude"]["user"]["pending"].is_null());
    assert_eq!(fs::read(fixture.settings()).unwrap(), bytes);

    let mut doc = read_json(fixture.settings());
    doc["hooks"]["PreToolUse"] = json!([]);
    put(&fixture.settings(), doc.to_string());
    let report = diagnose(1);
    doctor_status(&report, "settings.consistency", "drifted");
    doctor_status(&report, "settings.desired", "user_action_required");

    let path = fixture.project().join(FRAGMENT);
    fs::remove_file(&path).unwrap();
    doctor_status(&diagnose(1), "settings.source", "missing");
    put(&path, "{");
    doctor_status(&diagnose(1), "settings.source", "user_action_required");
    fs::remove_file(&path).unwrap();
    symlink(fixture.settings(), path).unwrap();
    doctor_status(&diagnose(1), "settings.source", "user_action_required");
}

#[test]
fn standalone_fragment_remains_available_and_sync_reconciles_its_records() {
    let fixture = Fixture::new();
    fixture.build();
    fixture.install_settings(&json!({"schema": 1, "values": {"/model": "opus"}}));
    assert_eq!(read_json(fixture.settings())["model"], "opus");
    fixture.sync();
    assert_eq!(read_json(fixture.settings()), json!({}));
}
