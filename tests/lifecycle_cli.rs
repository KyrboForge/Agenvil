#![cfg(any(target_os = "linux", target_os = "macos"))]

mod common;
#[path = "common/lifecycle.rs"]
mod fixture;

use std::fs;

use predicates::prelude::*;
use serde_json::{Value, json};

use fixture::{Fixture, output_json, put};

#[test]
fn uninstall_dry_run_then_apply_preserves_unmanaged_content() {
    let fixture = Fixture::new();
    let private = fixture.local.join("rules/a.md");
    put(&private, "# Private rule");
    fixture.build();
    fixture.sync();
    let user_rule = fixture.claude.join("rules/user.md");
    put(&user_rule, "# User rule");
    let user = json!({"hooks": {"PreToolUse": [
        {"matcher": "Bash", "hooks": [{"type": "command", "command": "user.sh"}]}
    ]}});
    put(&fixture.settings(), user.to_string());
    fixture.install_settings(&json!({
        "schema": 1,
        "values": {"/model": "opus"},
        "hooks": [{"id": "guard", "event": "PreToolUse", "group": {
            "matcher": "Bash", "hooks": [{"type": "command", "command": "guard.sh"}]
        }}]
    }));
    fs::remove_file(fixture.project().join("agenvil.toml")).unwrap();
    fs::remove_dir_all(fixture.project().join(".generated")).unwrap();
    let before = fixture.snapshot();

    let dry = fixture.run("uninstall", &["--format", "json"]).success();
    let explicit = fixture
        .run("uninstall", &["--format", "json", "--dry-run"])
        .success();
    assert_eq!(dry.get_output().stdout, explicit.get_output().stdout);
    let plan = output_json(&dry);
    assert_eq!(plan["links"]["summary"]["remove"], 2);
    assert_eq!(plan["settings"]["summary"]["remove"], 2);
    assert_eq!(fixture.snapshot(), before);

    let applied = output_json(
        &fixture
            .run("uninstall", &["--format", "json", "--apply"])
            .success(),
    );
    assert_eq!(applied["links"]["ledger"], "committed");
    assert_eq!(applied["link_conflicts"], json!([]));
    assert_eq!(applied["settings_conflicts"], json!([]));
    assert!(applied["settings_backup"].is_string());
    for name in ["a.md", "b.md"] {
        assert!(!fixture.claude.join("rules").join(name).is_symlink());
    }
    assert_eq!(fs::read_to_string(user_rule).unwrap(), "# User rule");
    assert_eq!(fs::read_to_string(private).unwrap(), "# Private rule");
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(fixture.settings()).unwrap()).unwrap(),
        user
    );

    let detached = fixture.snapshot();
    fixture
        .run("uninstall", &["--apply"])
        .success()
        .stdout(predicate::str::contains("nothing to uninstall"));
    assert_eq!(fixture.snapshot(), detached);
}

#[test]
fn uninstall_conflicts_preserve_user_content_and_detach_proven_links() {
    let fixture = Fixture::new();
    fixture.build();
    fixture.sync();
    fixture.install_settings(&json!({"schema": 1, "values": {"/model": "opus"}}));
    let first = fixture.claude.join("rules/a.md");
    fs::remove_file(&first).unwrap();
    put(&first, "User rule");
    put(&fixture.settings(), r#"{"model":"user"}"#);
    let before = fixture.snapshot();

    let dry = output_json(&fixture.run("uninstall", &["--format", "json"]).code(1));
    assert_eq!(dry["links"]["summary"]["conflict"], 1);
    assert_eq!(dry["settings"]["summary"]["conflict"], 1);
    assert_eq!(fixture.snapshot(), before);

    let applied = output_json(
        &fixture
            .run("uninstall", &["--format", "json", "--apply"])
            .code(1),
    );
    assert_eq!(applied["link_conflicts"][0]["destination"], "rules/a.md");
    assert_eq!(applied["settings_conflicts"][0]["reason"], "user_edited");
    assert_eq!(fs::read_to_string(first).unwrap(), "User rule");
    assert_eq!(
        fs::read_to_string(fixture.settings()).unwrap(),
        r#"{"model":"user"}"#
    );
    assert!(!fixture.claude.join("rules/b.md").is_symlink());
}

#[test]
fn restore_lists_previews_and_settles_managed_records() {
    let fixture = Fixture::new();
    put(&fixture.settings(), r#"{"theme":"dark"}"#);
    fixture
        .install_settings(&json!({"schema": 1, "values": {"/model": "opus", "/language": "en"}}));
    let previous = fs::read(fixture.settings()).unwrap();
    fixture
        .install_settings(&json!({"schema": 1, "values": {"/model": "sonnet", "/language": "en"}}));
    let current = fs::read(fixture.settings()).unwrap();
    fs::remove_file(fixture.project().join("agenvil.toml")).unwrap();
    let before = fixture.snapshot();

    let list = output_json(
        &fixture
            .run("restore", &["--list", "--format", "json"])
            .success(),
    );
    assert_eq!(list.as_array().unwrap().len(), 2);
    let backup = &list[0];
    let name = backup["name"].as_str().unwrap();
    assert!(
        backup["timestamp_utc_ns"].as_str().unwrap()
            > list[1]["timestamp_utc_ns"].as_str().unwrap()
    );
    assert_eq!(backup["size"], previous.len());
    assert_eq!(backup["sha256"], agenvil::manifest::sha256(&previous));
    let plan = output_json(
        &fixture
            .run("restore", &["--backup", name, "--format", "json"])
            .success(),
    );
    assert_eq!(&plan["backup"], backup);
    assert_eq!(plan["current_sha256"], agenvil::manifest::sha256(&current));
    assert_eq!(plan["retained"][0]["target"], "/language");
    assert_eq!(plan["dropped"][0]["target"], "/model");
    fixture
        .run("restore", &["--backup", name, "--dry-run"])
        .success()
        .stdout(
            predicate::str::contains("Would restore")
                .and(predicate::str::contains("backs up current settings")),
        );
    assert_eq!(fixture.snapshot(), before);

    let applied = output_json(
        &fixture
            .run(
                "restore",
                &["--backup", name, "--apply", "--format", "json"],
            )
            .success(),
    );
    assert_eq!(applied["backup"], plan["backup"]);
    assert_eq!(
        fs::read(applied["settings_backup"].as_str().unwrap()).unwrap(),
        current
    );
    assert_eq!(fs::read(fixture.settings()).unwrap(), previous);
    let plan = output_json(
        &fixture
            .run("settings", &["--remove", "--format", "json"])
            .success(),
    );
    assert_eq!(plan["summary"]["conflict"], 0);
    assert_eq!(plan["operations"][0]["target"], "/language");
}

#[test]
fn restore_omits_and_refuses_corrupted_backups() {
    let fixture = Fixture::new();
    put(&fixture.settings(), "{}");
    fixture.install_settings(&json!({"schema": 1, "values": {"/model": "opus"}}));
    let list = output_json(
        &fixture
            .run("restore", &["--list", "--format", "json"])
            .success(),
    );
    let name = list[0]["name"].as_str().unwrap();
    put(
        &fixture
            .local
            .join("state/backups/claude-settings")
            .join(name),
        r#"{"modified":true}"#,
    );
    let before = fixture.snapshot();

    for args in [vec!["--backup", name], vec!["--backup", name, "--apply"]] {
        fixture
            .run("restore", &args)
            .code(2)
            .stderr(predicate::str::contains("digest"));
    }
    let list = output_json(
        &fixture
            .run("restore", &["--list", "--format", "json"])
            .success(),
    );
    assert_eq!(list, json!([]));
    assert_eq!(fixture.snapshot(), before);
}

#[test]
fn uninstall_allows_sync_at_a_new_claude_root() {
    let mut fixture = Fixture::new();
    fixture.build();
    fixture.sync();
    fixture.run("uninstall", &["--apply"]).success();

    fixture.claude = fixture.claude.with_file_name("another-claude-root");
    fixture.sync();
    assert!(fixture.claude.join("rules/a.md").is_symlink());
}

#[test]
fn uninstall_drops_the_last_missing_link_and_allows_a_different_kit() {
    let fixture = Fixture::new();
    fs::remove_file(fixture.project().join("rules/b.md")).unwrap();
    fixture.build();
    fixture.sync();
    let link = fixture.claude.join("rules/a.md");
    assert!(link.is_symlink());
    fs::remove_file(&link).unwrap();

    let before = fixture.snapshot();
    let dry = output_json(&fixture.run("uninstall", &["--format", "json"]).success());
    assert_eq!(fixture.snapshot(), before);

    let applied = output_json(
        &fixture
            .run("uninstall", &["--apply", "--format", "json"])
            .success(),
    );
    let ledger = fixture.local.join("state/ownership.json");
    let state: Value = serde_json::from_slice(&fs::read(ledger).unwrap()).unwrap();
    assert!(state["harnesses"]["claude"]["user"].is_null(), "{state}");
    assert_eq!(applied["links"]["ledger"], "committed");
    assert_eq!(dry["links"]["summary"]["remove"], 1);
    assert_eq!(dry["links"]["operations"][0]["destination"], "rules/a.md");
    assert!(!link.exists());

    let detached = fixture.snapshot();
    fixture
        .run("uninstall", &["--apply"])
        .success()
        .stdout(predicate::str::contains("nothing to uninstall"));
    assert_eq!(fixture.snapshot(), detached);

    put(
        &fixture.project().join("agenvil.toml"),
        "schema = 1\n[project]\nname = 'another-kit'\n",
    );
    fixture.build();
    fixture.sync();
    assert!(link.is_symlink());
}

#[test]
fn uninstall_cleans_only_its_retired_links() {
    let fixture = Fixture::new();
    fixture.build();
    fixture.sync();
    let rules = fixture.claude.join("rules");
    let lookalike = rules.join(".tmp-agenvil-retired-link-0-0-0");
    let target = fs::read_link(rules.join("a.md")).unwrap();
    std::os::unix::fs::symlink(&target, &lookalike).unwrap();

    fixture.run("uninstall", &["--apply"]).success();

    let paths: Vec<_> = fs::read_dir(rules)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect();
    assert_eq!(paths, std::slice::from_ref(&lookalike));
    assert_eq!(fs::read_link(lookalike).unwrap(), target);
}

#[test]
fn restore_rejects_invalid_unknown_or_unsafe_restore_without_writing() {
    let fixture = Fixture::new();
    put(&fixture.settings(), "{}");
    fixture.install_settings(&json!({"schema": 1, "values": {"/model": "opus"}}));
    let list = output_json(
        &fixture
            .run("restore", &["--list", "--format", "json"])
            .success(),
    );
    let name = list[0]["name"].as_str().unwrap();
    let before = fixture.snapshot();

    for invalid in ["../settings.json", "00000000000000000001-abcdef01.json"] {
        fixture
            .run("restore", &["--backup", invalid])
            .code(2)
            .stderr(predicate::str::contains(invalid));
    }
    for args in [
        vec![],
        vec!["--list", "--apply"],
        vec!["--list", "--dry-run"],
        vec!["--list", "--backup", name],
        vec!["--backup", name, "--apply", "--dry-run"],
    ] {
        fixture.run("restore", &args).code(2);
    }
    fixture.run("uninstall", &["--apply", "--dry-run"]).code(2);
    assert_eq!(fixture.snapshot(), before);

    put(&fixture.settings(), r#"{"model":"user"}"#);
    let edited = fixture.snapshot();
    fixture
        .run("restore", &["--backup", name, "--apply"])
        .code(1)
        .stderr(predicate::str::contains("last write"));
    assert_eq!(fixture.snapshot(), edited);
}

#[test]
fn repeated_sync_apply_keeps_the_same_state_and_private_link() {
    let fixture = Fixture::new();
    let private = fixture.local.join("rules/a.md");
    put(&private, "# Private rule");
    fixture.build();
    fixture.sync();
    let before = fixture.snapshot();

    let plan = output_json(
        &fixture
            .run("sync", &["--apply", "--format", "json"])
            .success(),
    );
    assert_eq!(plan["summary"]["noop"], 2);
    assert_eq!(
        fs::read_link(fixture.claude.join("rules/a.md")).unwrap(),
        private.canonicalize().unwrap()
    );
    assert_eq!(fixture.snapshot(), before);
}
