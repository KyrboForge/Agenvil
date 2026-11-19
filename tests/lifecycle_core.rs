#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::fs;
use std::os::unix::fs::symlink;
use std::path::{Path, PathBuf};

use agenvil::build;
use agenvil::config::Config;
use agenvil::install::{LedgerState, Locations, lifecycle::*, settings};
use agenvil::inventory::Inventory;
use agenvil::manifest::sha256;
use serde_json::{Value, json};

const CONFIG: &str = "schema = 1\n[project]\nname = \"demo\"\n";

struct Fixture {
    temp: tempfile::TempDir,
    locations: Locations,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let home = temp.path().join("home");
        Self {
            locations: Locations {
                claude_root: home.join("custom-claude"),
                local_root: home.join(".agents.local"),
            },
            temp,
        }
    }

    fn sync(&self) {
        let project = self.temp.path().join("project");
        let staging = self.temp.path().join("approved");
        put(&project.join("agenvil.toml"), CONFIG);
        put(&project.join("rules/a.md"), "# Rule A");
        put(&project.join("rules/b.md"), "# Rule B");
        let config = Config::load(&project).unwrap();
        let inventory = Inventory::build(&project, &config, &build::limits()).unwrap();
        build::claude(&project, &config, &inventory, &staging).unwrap();
        sync_apply(&sync_plan(&project, &config, &staging, &self.locations).unwrap()).unwrap();
    }

    fn write_settings(&self, contents: &Value) {
        put(&self.settings_path(), contents.to_string());
    }

    fn settings_path(&self) -> PathBuf {
        self.locations.claude_root.join("settings.json")
    }

    fn install_settings(&self, contents: &Value) -> Option<PathBuf> {
        let path = self.temp.path().join("fragment.json");
        put(&path, contents.to_string());
        let fragment = settings::Fragment::read(&path).unwrap();
        settings_apply(&settings_plan(&self.locations, Some(&fragment)).unwrap()).unwrap()
    }

    fn ledger(&self) -> Value {
        read_json(&self.locations.local_root.join("state/ownership.json"))
    }
}

fn put(path: &Path, contents: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, contents).unwrap();
}

fn read_json(path: &Path) -> Value {
    serde_json::from_slice(&fs::read(path).unwrap()).unwrap()
}

#[test]
fn uninstall_detaches_links_and_settings_and_is_repeatable() {
    let fixture = Fixture::new();
    let private = fixture.locations.local_root.join("rules/a.md");
    put(&private, "# Private A");
    let rules = fixture.locations.claude_root.join("rules");
    put(&rules.join("user.md"), "# User");
    let user = json!({"unmanaged": true, "hooks": {"PreToolUse": [{
        "matcher": "Read", "hooks": [{"type": "command", "command": "mine"}]
    }]}});
    fixture.write_settings(&user);
    fixture.sync();
    fixture.install_settings(&json!({
        "schema": 1, "values": {"/model": "opus"}, "hooks": [{
            "id": "guard", "event": "PreToolUse",
            "group": {"matcher": "Bash", "hooks": [{"type": "command", "command": "guard"}]}
        }]
    }));
    let snapshot = fs::read_link(rules.join("b.md")).unwrap();
    fs::remove_dir_all(fixture.temp.path().join("project")).unwrap();
    fs::remove_dir_all(fixture.temp.path().join("approved")).unwrap();

    let plan = uninstall_plan(&fixture.locations).unwrap();
    assert_eq!(plan.links().summary().remove, 2);
    assert!(!plan.has_conflicts());
    uninstall_apply(&plan).unwrap();
    for name in ["a.md", "b.md"] {
        assert!(fs::symlink_metadata(rules.join(name)).is_err());
    }
    assert_eq!(fs::read_to_string(rules.join("user.md")).unwrap(), "# User");
    assert_eq!(fs::read_to_string(&private).unwrap(), "# Private A");
    assert_eq!(fs::read_to_string(snapshot).unwrap(), "# Rule B");
    assert_eq!(read_json(&fixture.settings_path()), user);
    let ledger = fixture.ledger();
    assert!(ledger["harnesses"]["claude"]["user"].is_null());

    let again = uninstall_plan(&fixture.locations).unwrap();
    assert!(again.links().operations().is_empty());
    assert!(again.settings().operations.is_empty());
    let report = uninstall_apply(&again).unwrap();
    assert_eq!(report.links.ledger, LedgerState::Unchanged);
    assert!(report.settings_backup.is_none());
    assert_eq!(fixture.ledger(), ledger);
    fixture.sync();
    assert!(rules.join("b.md").is_symlink());
}

#[test]
fn uninstall_preserves_replacements_and_reports_conflicts() {
    let fixture = Fixture::new();
    fixture.sync();
    let replaced = fixture.locations.claude_root.join("rules/a.md");
    let untouched = fixture.locations.claude_root.join("rules/b.md");
    let target = fs::read_link(&replaced).unwrap();
    fs::rename(&replaced, replaced.with_file_name("saved-a.md")).unwrap();
    symlink(&target, &replaced).unwrap();

    let plan = uninstall_plan(&fixture.locations).unwrap();
    assert_eq!(plan.links().summary().conflict, 1);
    let report = uninstall_apply(&plan).unwrap();
    assert!(report.has_conflicts());
    assert_eq!(report.link_conflicts[0].destination, "rules/a.md");
    assert_eq!(fs::read_link(&replaced).unwrap(), target);
    assert!(fs::symlink_metadata(untouched).is_err());
}

#[test]
fn settings_conflict_does_not_block_link_removal() {
    let fixture = Fixture::new();
    fixture.sync();
    fixture.install_settings(&json!({"schema": 1, "values": {"/model": "opus"}}));
    let edited = json!({"model": "mine"});
    fixture.write_settings(&edited);
    let plan = uninstall_plan(&fixture.locations).unwrap();
    let report = uninstall_apply(&plan).unwrap();
    assert!(report.has_conflicts());
    assert_eq!(
        report.settings_conflicts[0].reason,
        settings::Reason::UserEdited
    );
    assert_eq!(read_json(&fixture.settings_path()), edited);
    assert!(fs::symlink_metadata(fixture.locations.claude_root.join("rules/a.md")).is_err());
}

#[test]
fn restore_reports_and_settles_records_and_refuses_concurrent_edits() {
    let fixture = Fixture::new();
    fixture.write_settings(&json!({"unmanaged": true}));
    fixture.install_settings(&json!({"schema": 1, "values": {"/model": "opus", "/keep": true}}));
    let fragment = json!({"schema": 1, "values": {"/model": "sonnet", "/keep": true}});
    let backup = fixture.install_settings(&fragment).unwrap();
    let name = backup.file_name().unwrap().to_str().unwrap();
    let original = fs::read(&backup).unwrap();
    let before = fs::read(fixture.settings_path()).unwrap();
    let backups = restore_backups(&fixture.locations).unwrap();
    let listed = backups.iter().find(|item| item.name == name).unwrap();
    assert_eq!(listed.size, original.len() as u64);
    assert_eq!(listed.sha256, sha256(&original));
    let plan = restore_plan(&fixture.locations, name).unwrap();
    assert_eq!(plan.current_sha256, sha256(&before));
    assert_eq!(plan.backup, *listed);
    assert_eq!(plan.retained[0].target, "/keep");
    assert_eq!(plan.dropped[0].target, "/model");
    assert_eq!(fs::read(fixture.settings_path()).unwrap(), before);
    assert_eq!(restore_backups(&fixture.locations).unwrap(), backups);

    let state_path = fixture.locations.local_root.join("state/managed.json");
    let original_state = fs::read(&state_path).unwrap();
    let mut reformatted_state = original_state.clone();
    reformatted_state.push(b' ');
    fs::write(&state_path, reformatted_state).unwrap();
    assert!(matches!(
        restore_apply(&plan),
        Err(settings::ApplyError::Changed { .. })
    ));
    assert_eq!(fs::read(fixture.settings_path()).unwrap(), before);
    assert_eq!(restore_backups(&fixture.locations).unwrap(), backups);
    fs::write(&state_path, original_state).unwrap();

    let plan = restore_plan(&fixture.locations, name).unwrap();
    let prior = restore_apply(&plan).unwrap().unwrap();
    assert_eq!(fs::read(prior).unwrap(), before);
    assert_eq!(fs::read(fixture.settings_path()).unwrap(), original);
    let state = read_json(&state_path);
    assert_eq!(
        state["harnesses"]["claude"]["user"]["last_write"],
        sha256(&original)
    );
    let detach = settings_plan(&fixture.locations, None).unwrap();
    assert!(!detach.has_conflicts());
    assert_eq!(detach.operations.len(), 1);
    assert_eq!(detach.operations[0].target, "/keep");

    let fresh = restore_plan(&fixture.locations, name).unwrap();
    fixture.write_settings(&json!({"model": "user change"}));
    assert!(matches!(
        restore_apply(&fresh),
        Err(settings::ApplyError::Changed { .. })
    ));
    assert!(matches!(
        restore_plan(&fixture.locations, name),
        Err(settings::ApplyError::Changed { .. })
    ));
    assert_eq!(
        read_json(&fixture.settings_path()),
        json!({"model": "user change"})
    );
}

#[test]
fn dry_runs_on_fresh_locations_create_nothing() {
    let fixture = Fixture::new();
    assert!(!uninstall_plan(&fixture.locations).unwrap().has_conflicts());
    assert!(restore_backups(&fixture.locations).unwrap().is_empty());
    assert!(restore_plan(&fixture.locations, "00000000000000000001-abcdef01.json").is_err());
    assert!(pending_operations(&fixture.locations).unwrap().is_empty());
    assert_eq!(fs::read_dir(fixture.temp.path()).unwrap().count(), 0);
}

#[test]
fn uninstall_apply_on_fresh_locations_creates_nothing() {
    let fixture = Fixture::new();
    let plan = uninstall_plan(&fixture.locations).unwrap();
    let report = uninstall_apply(&plan).unwrap();
    assert_eq!(report.links.ledger, LedgerState::Unchanged);
    assert_eq!(fs::read_dir(fixture.temp.path()).unwrap().count(), 0);
}
