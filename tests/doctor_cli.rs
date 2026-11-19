#![cfg(any(target_os = "linux", target_os = "macos"))]

mod common;
#[path = "common/lifecycle.rs"]
mod fixture;

use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::os::unix::fs::symlink;

use predicates::prelude::*;
use serde_json::{Value, json};

use fixture::{Fixture, output_json, put};

fn checks<'a>(report: &'a Value, id: &str) -> impl Iterator<Item = &'a Value> {
    report["checks"]
        .as_array()
        .unwrap()
        .iter()
        .filter(move |check| check["id"].as_str() == Some(id))
}

fn expect_status(report: &Value, id: &str, status: &str) {
    assert!(
        checks(report, id).any(|check| check["status"] == status),
        "{report}"
    );
}

fn doctor(fixture: &Fixture, code: i32) -> Value {
    output_json(&fixture.run("doctor", &["--format", "json"]).code(code))
}

#[test]
fn clean_doctor_is_read_only_and_json_is_byte_stable() {
    let fixture = Fixture::new();
    put(
        &fixture.project().join("agenvil.toml"),
        "schema = 1\n[project]\nname = 'lifecycle-test'\n[targets.claude.settings]\nfragment = 'fragment.json'\n",
    );
    put(
        &fixture.project().join("fragment.json"),
        r#"{"schema":1,"values":{"/model":"opus"}}"#,
    );
    fixture.build();
    fixture.sync();
    let before = fixture.snapshot();

    let first = fixture.run("doctor", &["--format", "json"]).success();
    let second = fixture.run("doctor", &["--format", "json"]).success();
    assert_eq!(first.get_output().stdout, second.get_output().stdout);
    let report = output_json(&first);
    assert_eq!(report["schema"], 1);
    assert_eq!(report["status"], "healthy");
    for id in [
        "source.config",
        "build.staging",
        "links.ownership",
        "settings.consistency",
        "settings.source",
        "settings.desired",
        "operations.interrupted",
    ] {
        expect_status(&report, id, "healthy");
    }
    for id in ["prereq.claude_cli", "prereq.git"] {
        expect_status(&report, id, "missing");
        assert_eq!(checks(&report, id).next().unwrap()["required"], false);
    }
    for id in ["auth.claude", "integration.rtk", "integration.headroom"] {
        expect_status(&report, id, "unsupported");
    }
    assert_eq!(report["summary"]["healthy"], 7);
    assert_eq!(report["summary"]["missing"], 2);
    assert_eq!(report["summary"]["unsupported"], 3);
    fixture.run("doctor", &[]).success().stdout(
        predicate::str::contains("ok")
            .and(predicate::str::contains("MISS"))
            .and(predicate::str::contains("OFF")),
    );
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn doctor_detects_missing_and_broken_managed_links() {
    let fixture = Fixture::new();
    fixture.build();
    fixture.sync();
    let rules = fixture.claude.join("rules");
    fs::remove_file(rules.join("a.md")).unwrap();
    fs::remove_dir_all(fs::read_link(rules.join("b.md")).unwrap().parent().unwrap()).unwrap();
    let before = fixture.snapshot();

    let report = doctor(&fixture, 1);
    for destination in ["rules/a.md", "rules/b.md"] {
        assert!(
            checks(&report, "links.missing")
                .any(|check| { check["status"] == "missing" && check["subject"] == destination }),
            "{report}"
        );
    }
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn doctor_detects_a_dangling_private_link() {
    let fixture = Fixture::new();
    let private = fixture.local.join("rules/a.md");
    put(&private, "# Private");
    fixture.build();
    fixture.sync();
    fs::remove_file(private).unwrap();

    let report = doctor(&fixture, 1);
    assert!(checks(&report, "links.missing").any(|check| check["subject"] == "rules/a.md"));
}

#[test]
fn doctor_reports_user_owned_destination_and_corrupted_snapshot() {
    let fixture = Fixture::new();
    fixture.build();
    fixture.sync();
    let rules = fixture.claude.join("rules");
    fs::remove_file(rules.join("a.md")).unwrap();
    put(&rules.join("a.md"), "User rule");
    let snapshot = fs::read_link(rules.join("b.md")).unwrap();
    fs::set_permissions(&snapshot, fs::Permissions::from_mode(0o644)).unwrap();
    put(&snapshot, "Modified managed rule");
    put(&fixture.project().join("rules/c.md"), "# New rule");
    put(&rules.join("c.md"), "Independent user rule");
    fixture.build();

    let report = doctor(&fixture, 1);
    for (destination, status) in [
        ("rules/a.md", "user_action_required"),
        ("rules/b.md", "drifted"),
        ("rules/c.md", "user_action_required"),
    ] {
        assert!(
            checks(&report, "links.conflict")
                .any(|check| { check["status"] == status && check["subject"] == destination }),
            "{report}"
        );
    }
}

#[test]
fn doctor_reads_pending_ledger_even_with_invalid_config_and_settings() {
    let fixture = Fixture::new();
    fixture.build();
    fixture.sync();
    let ledger_path = fixture.local.join("state/ownership.json");
    let mut ledger: Value = serde_json::from_slice(&fs::read(&ledger_path).unwrap()).unwrap();
    ledger["harnesses"]["claude"]["user"]["entries"][0]["pending"] =
        json!({"operation": "remove", "previous_target": null});
    put(&ledger_path, ledger.to_string());
    put(&fixture.settings(), "{");
    put(&fixture.project().join("agenvil.toml"), "[broken");
    let before = fixture.snapshot();

    let report = doctor(&fixture, 1);
    expect_status(&report, "source.config", "user_action_required");
    expect_status(&report, "settings.consistency", "user_action_required");
    expect_status(&report, "operations.interrupted", "user_action_required");
    let interrupted = checks(&report, "operations.interrupted").next().unwrap();
    assert_eq!(interrupted["detail"], "Remove rules/a.md");
    assert_eq!(report["status"], "user_action_required");
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn doctor_checks_explicit_staging_and_detects_settings_drift() {
    let fixture = Fixture::new();
    let out = fixture.project().join("approved");
    let out = out.to_str().unwrap();
    fixture.run("build", &["--out", out]).success();
    fixture.run("sync", &["--out", out, "--apply"]).success();
    fixture.install_settings(&json!({"schema": 1, "values": {"/model": "opus"}}));
    put(&fixture.settings(), "{}");

    let report = output_json(
        &fixture
            .run("doctor", &["--out", out, "--format", "json"])
            .code(1),
    );
    expect_status(&report, "build.staging", "healthy");
    expect_status(&report, "settings.consistency", "drifted");
    expect_status(&doctor(&fixture, 1), "build.staging", "missing");

    put(&fixture.project().join("rules/a.md"), "# Updated rule");
    let report = output_json(
        &fixture
            .run("doctor", &["--out", out, "--format", "json"])
            .code(1),
    );
    expect_status(&report, "build.staging", "drifted");

    symlink(
        fixture.project().join("rules/a.md"),
        std::path::Path::new(out).join("extra"),
    )
    .unwrap();
    let report = output_json(
        &fixture
            .run("doctor", &["--out", out, "--format", "json"])
            .code(1),
    );
    expect_status(&report, "build.staging", "user_action_required");
}

#[test]
fn doctor_flags_duplicate_managed_hook_groups() {
    let fixture = Fixture::new();
    fixture.run("build", &[]).success();
    let group = json!({"matcher": "Bash", "hooks": [{"type": "command", "command": "guard.sh"}]});
    fixture.install_settings(&json!({
        "schema": 1,
        "hooks": [{"id": "guard", "event": "PreToolUse", "group": group}]
    }));
    let mut doc: Value =
        serde_json::from_slice(&std::fs::read(fixture.settings()).unwrap()).unwrap();
    let copy = doc["hooks"]["PreToolUse"][0].clone();
    doc["hooks"]["PreToolUse"]
        .as_array_mut()
        .unwrap()
        .push(copy);
    put(&fixture.settings(), doc.to_string());

    expect_status(
        &doctor(&fixture, 1),
        "settings.consistency",
        "user_action_required",
    );
}

#[test]
fn doctor_looks_up_executables_without_running_them() {
    let fixture = Fixture::new();
    fixture.build();
    fixture.sync();
    let bin = fixture.claude.parent().unwrap().join("bin");
    let claude = bin.join("claude");
    put(&claude, "#!/bin/sh\n: > \"$HOME/spawned\"\n");
    fs::set_permissions(&claude, fs::Permissions::from_mode(0o755)).unwrap();
    put(&bin.join("git"), "not executable");
    let before = fixture.snapshot();

    let report = doctor(&fixture, 0);
    expect_status(&report, "prereq.claude_cli", "healthy");
    expect_status(&report, "prereq.git", "missing");
    assert_eq!(fixture.snapshot(), before);
}
