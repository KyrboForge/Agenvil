use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use tempfile::TempDir;

use super::apply::native::apply_with;
use super::*;

struct Home {
    dir: TempDir,
    locations: Locations,
}

impl Home {
    fn new() -> Self {
        let dir = tempfile::tempdir().unwrap();
        let locations = Locations {
            claude_root: dir.path().join(".claude"),
            local_root: dir.path().join(".agents.local"),
        };
        Self { dir, locations }
    }

    fn settings(&self) -> PathBuf {
        self.locations.claude_root.join(SETTINGS)
    }

    fn write(&self, text: &str) {
        fs::create_dir_all(&self.locations.claude_root).unwrap();
        fs::write(self.settings(), text).unwrap();
    }

    fn bytes(&self) -> Vec<u8> {
        fs::read(self.settings()).unwrap()
    }

    fn json(&self) -> Value {
        serde_json::from_slice(&self.bytes()).unwrap()
    }

    fn plan(&self, fragment: Option<&Fragment>) -> Plan {
        plan(&self.locations, fragment).unwrap()
    }

    fn install(&self, fragment: &Fragment) -> Plan {
        let plan = self.plan(Some(fragment));
        apply(&plan).unwrap();
        plan
    }

    fn backups(&self) -> Vec<String> {
        let dir = self
            .locations
            .local_root
            .join("state/backups/claude-settings");
        let mut names: Vec<_> = fs::read_dir(dir)
            .map(|entries| {
                entries
                    .map(|e| e.unwrap().file_name().into_string().unwrap())
                    .collect()
            })
            .unwrap_or_default();
        names.sort();
        names
    }

    fn state(&self) -> Option<Vec<u8>> {
        fs::read(self.locations.local_root.join("state").join(state::FILE)).ok()
    }
}

fn fragment(value: Value) -> Fragment {
    Fragment::parse(value.to_string().as_bytes()).unwrap()
}

fn group(command: &str) -> Value {
    json!({"matcher": "Bash", "hooks": [{"type": "command", "command": command}]})
}

fn hook(id: &str, event: &str, command: &str) -> Value {
    json!({"id": id, "event": event, "group": group(command)})
}

fn ops(plan: &Plan) -> Vec<(OperationKind, &str, Reason)> {
    plan.operations
        .iter()
        .map(|op| (op.operation, op.target.as_str(), op.reason))
        .collect()
}

fn tree(path: &Path) -> Vec<(PathBuf, Option<Vec<u8>>)> {
    let mut out = Vec::new();
    for entry in fs::read_dir(path).unwrap() {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.push((path.clone(), None));
            out.extend(tree(&path));
        } else {
            out.push((path.clone(), Some(fs::read(&path).unwrap())));
        }
    }
    out.sort();
    out
}

fn basic() -> Fragment {
    fragment(json!({
        "schema": 1,
        "values": {"/model": "opus", "/permissions/defaultMode": "plan"},
        "hooks": [hook("guard", "PreToolUse", "guard.sh"), hook("done", "Stop", "done.sh")]
    }))
}

#[test]
fn fresh_install_creates_a_private_file_and_records_ownership() {
    let home = Home::new();
    let plan = home.plan(Some(&basic()));
    assert_eq!(plan.files.settings.action, FileAction::Create);
    assert_eq!(plan.files.state.action, FileAction::Create);
    assert_eq!(plan.summary.add, 4);
    assert_eq!(apply(&plan).unwrap(), None, "nothing existed to back up");
    assert_eq!(
        home.json(),
        json!({
            "model": "opus",
            "permissions": {"defaultMode": "plan"},
            "hooks": {"PreToolUse": [group("guard.sh")], "Stop": [group("done.sh")]}
        })
    );
    let mode = fs::metadata(home.settings()).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o600);
    let state: Value = serde_json::from_slice(&home.state().unwrap()).unwrap();
    let user = &state["harnesses"]["claude"]["user"];
    assert_eq!(user["values"][1]["created"], json!(["/permissions"]));
    assert_eq!(user["hooks"][0]["id"], "guard");
    assert!(home.backups().is_empty());
}

#[test]
fn user_keys_and_hooks_keep_their_order_and_managed_groups_append() {
    let home = Home::new();
    home.write(
        r#"{"zeta":1,"alpha":{"y":2,"x":3},"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"mine"}]}]},"omega":[3,1]}"#,
    );
    home.install(&basic());
    let doc = home.json();
    let keys: Vec<_> = doc.as_object().unwrap().keys().cloned().collect();
    assert_eq!(
        keys,
        ["zeta", "alpha", "hooks", "omega", "model", "permissions"]
    );
    let inner: Vec<_> = doc["alpha"].as_object().unwrap().keys().cloned().collect();
    assert_eq!(inner, ["y", "x"]);
    assert_eq!(doc["hooks"]["PreToolUse"][0]["hooks"][0]["command"], "mine");
    assert_eq!(doc["hooks"]["PreToolUse"][1], group("guard.sh"));
    assert_eq!(doc["hooks"]["Stop"], json!([group("done.sh")]));
    assert!(home.bytes().ends_with(b"}\n"));
    assert_eq!(home.backups().len(), 1);
}

#[test]
fn repeated_and_reformatted_installs_are_noops() {
    let home = Home::new();
    home.write(r#"{"user": true}"#);
    home.install(&basic());
    let (bytes, state) = (home.bytes(), home.state());
    let mtime = fs::metadata(home.settings()).unwrap().modified().unwrap();
    let plan = home.plan(Some(&basic()));
    assert!(
        plan.operations
            .iter()
            .all(|op| op.operation == OperationKind::Noop)
    );
    assert_eq!(plan.files.settings.action, FileAction::Unchanged);
    assert_eq!(plan.files.state.action, FileAction::Unchanged);
    assert_eq!(apply(&plan).unwrap(), None);
    assert_eq!(home.bytes(), bytes);
    assert_eq!(home.state(), state);
    assert_eq!(
        fs::metadata(home.settings()).unwrap().modified().unwrap(),
        mtime
    );
    assert_eq!(home.backups().len(), 1);

    // The same values, compact and reordered: still nothing to write.
    let compact = serde_json::to_string(&home.json()["hooks"]).unwrap();
    let reordered = format!(
        r#"{{"permissions":{{"defaultMode":"plan"}},"hooks":{compact},"model":"opus","user":true}}"#
    );
    home.write(&reordered);
    let plan = home.plan(Some(&basic()));
    assert_eq!(plan.files.settings.action, FileAction::Unchanged);
    assert_eq!(plan.files.state.action, FileAction::Unchanged);
    apply(&plan).unwrap();
    assert_eq!(home.bytes(), reordered.as_bytes());
}

#[test]
fn updates_replace_values_and_hook_groups_in_place() {
    let home = Home::new();
    home.install(&basic());
    let mut doc = home.json();
    doc["hooks"]["PreToolUse"]
        .as_array_mut()
        .unwrap()
        .push(group("mine"));
    home.write(&doc.to_string());
    let next = fragment(json!({
        "schema": 1,
        "values": {"/model": "sonnet", "/permissions/defaultMode": "plan"},
        "hooks": [hook("guard", "PreToolUse", "guard-v2.sh"), hook("done", "Stop", "done.sh")]
    }));
    let plan = home.install(&next);
    assert_eq!(
        ops(&plan),
        [
            (OperationKind::Change, "/model", Reason::Changed),
            (
                OperationKind::Noop,
                "/permissions/defaultMode",
                Reason::Current
            ),
            (OperationKind::Change, "guard", Reason::Changed),
            (OperationKind::Noop, "done", Reason::Current),
        ]
    );
    let doc = home.json();
    assert_eq!(doc["model"], "sonnet");
    assert_eq!(
        doc["hooks"]["PreToolUse"],
        json!([group("guard-v2.sh"), group("mine")])
    );
    // A hook moved to another event leaves its old event pruned.
    let moved =
        fragment(json!({"schema": 1, "hooks": [hook("guard", "PostToolUse", "guard-v2.sh")]}));
    home.install(&moved);
    let doc = home.json();
    assert_eq!(doc["hooks"]["PreToolUse"], json!([group("mine")]));
    assert_eq!(doc["hooks"]["PostToolUse"], json!([group("guard-v2.sh")]));
    assert!(doc["hooks"].get("Stop").is_none());
    assert!(doc.get("permissions").is_none());
}

#[test]
fn user_edits_after_install_are_conflicts_and_never_overwritten() {
    let home = Home::new();
    home.install(&basic());
    let mut doc = home.json();
    doc["model"] = json!("haiku");
    doc["hooks"]["PreToolUse"][0]["matcher"] = json!("Edit");
    home.write(&doc.to_string());
    let before = home.bytes();
    for desired in [Some(&basic()), None] {
        let plan = home.plan(desired);
        assert!(plan.has_conflicts());
        let conflicts: Vec<_> = ops(&plan)
            .into_iter()
            .filter(|op| op.0 == OperationKind::Conflict)
            .collect();
        assert_eq!(
            conflicts,
            [
                (OperationKind::Conflict, "/model", Reason::UserEdited),
                (OperationKind::Conflict, "guard", Reason::HookModified),
            ]
        );
        assert!(matches!(
            apply(&plan),
            Err(ApplyError::Conflicts { count: 2 })
        ));
        assert_eq!(home.bytes(), before);
    }
}

#[test]
fn unmanaged_values_duplicates_and_type_clashes_conflict() {
    let home = Home::new();
    home.write(
        r#"{"model":"haiku","permissions":"none","hooks":{"Stop":[{"matcher":"*","hooks":[{"type":"command","command":"done.sh"}]}]}}"#,
    );
    let plan = home.plan(Some(&basic()));
    assert_eq!(
        ops(&plan),
        [
            (OperationKind::Conflict, "/model", Reason::UserValue),
            (
                OperationKind::Conflict,
                "/permissions/defaultMode",
                Reason::TypeClash
            ),
            (OperationKind::Add, "guard", Reason::Missing),
            (OperationKind::Conflict, "done", Reason::HookDuplicate),
        ]
    );
    assert_eq!(plan.operations[0].ownership, Ownership::Unmanaged);
}

#[test]
fn adopted_values_and_unmanaged_content_survive_uninstall() {
    let home = Home::new();
    home.write(
        r#"{"model":"opus","permissions":{"allow":["Read"]},"hooks":{"PreToolUse":[{"hooks":[{"type":"command","command":"mine"}]}]}}"#,
    );
    let plan = home.install(&basic());
    assert_eq!(
        ops(&plan)[0],
        (OperationKind::Noop, "/model", Reason::Adopted)
    );
    let removal = home.plan(None);
    assert_eq!(removal.summary.remove, 4);
    apply(&removal).unwrap();
    assert_eq!(
        home.json(),
        json!({
            "model": "opus",
            "permissions": {"allow": ["Read"]},
            "hooks": {"PreToolUse": [{"hooks": [{"type": "command", "command": "mine"}]}]}
        })
    );
    let state: Value = serde_json::from_slice(&home.state().unwrap()).unwrap();
    assert_eq!(state["harnesses"]["claude"]["user"]["values"], json!([]));
    // Nothing recorded is left to remove.
    let again = home.plan(None);
    assert!(again.operations.is_empty());
    assert_eq!(again.files.settings.action, FileAction::Unchanged);
}

#[test]
fn uninstall_drops_records_of_keys_already_gone_and_keeps_user_edits() {
    let home = Home::new();
    home.install(&basic());
    let mut doc = home.json();
    doc.as_object_mut().unwrap().shift_remove("model");
    doc["permissions"]["defaultMode"] = json!("acceptEdits");
    home.write(&doc.to_string());
    let plan = home.plan(None);
    assert_eq!(
        ops(&plan)[0],
        (OperationKind::Remove, "/model", Reason::Gone)
    );
    assert_eq!(
        ops(&plan)[1],
        (
            OperationKind::Conflict,
            "/permissions/defaultMode",
            Reason::UserEdited
        )
    );
}

#[test]
fn invalid_documents_and_symlinks_are_refused_untouched() {
    let home = Home::new();
    for text in ["{\"a\":", "[1,2]", "null"] {
        home.write(text);
        assert!(matches!(
            plan(&home.locations, Some(&basic())),
            Err(Error::Settings { .. })
        ));
        assert_eq!(home.bytes(), text.as_bytes());
    }
    let real = home.dir.path().join("real.json");
    fs::write(&real, "{}").unwrap();
    fs::remove_file(home.settings()).unwrap();
    std::os::unix::fs::symlink(&real, home.settings()).unwrap();
    assert!(matches!(
        plan(&home.locations, Some(&basic())),
        Err(Error::Settings { .. })
    ));
    assert_eq!(fs::read(&real).unwrap(), b"{}");
}

#[test]
fn a_symlinked_root_is_resolved_and_ownership_stays_bound_to_it() {
    let home = Home::new();
    let real = home.dir.path().join("real-claude");
    fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, &home.locations.claude_root).unwrap();
    home.install(&basic());
    assert_eq!(fs::read(real.join(SETTINGS)).unwrap(), home.bytes());
    let other = Locations {
        claude_root: home.dir.path().join("other"),
        ..home.locations.clone()
    };
    assert!(matches!(plan(&other, None), Err(Error::State { .. })));
}

#[test]
fn a_change_between_plan_and_apply_aborts_without_writing() {
    let home = Home::new();
    home.write(r#"{"user": 1}"#);
    let plan = home.plan(Some(&basic()));
    home.write(r#"{"user": 2}"#);
    assert!(matches!(apply(&plan), Err(ApplyError::Changed { .. })));
    assert_eq!(home.bytes(), br#"{"user": 2}"#);
    assert!(home.state().is_none());
    assert!(home.backups().is_empty());
}

#[test]
fn a_failed_commit_rolls_settings_back_and_the_journal_recovers() {
    let home = Home::new();
    home.write(r#"{"user": 1}"#);
    let plan = home.plan(Some(&basic()));
    let fail = |at| {
        move |step| {
            if step == at {
                Err(std::io::Error::other("injected"))
            } else {
                Ok(())
            }
        }
    };
    let error = apply_with(&plan, &mut fail(Step::Commit)).unwrap_err();
    assert!(error.to_string().contains("rolled back"), "{error}");
    assert_eq!(home.bytes(), br#"{"user": 1}"#);
    // The journal names the old digest, so the next plan uses the settled records.
    let retry = home.plan(Some(&basic()));
    assert_eq!(retry.summary.add, 4);
    apply(&retry).unwrap();
    assert_eq!(home.json()["model"], "opus");

    // A file Agenvil created is removed again.
    let fresh = Home::new();
    let plan = fresh.plan(Some(&basic()));
    apply_with(&plan, &mut fail(Step::Commit)).unwrap_err();
    assert!(!fresh.settings().exists());
    fresh.install(&basic());
}

#[test]
fn settings_matching_neither_side_of_a_journal_are_refused() {
    let home = Home::new();
    home.write("{}");
    let plan = home.plan(Some(&basic()));
    let settings = home.settings();
    let error = apply_with(&plan, &mut |step| {
        if step == Step::Commit {
            fs::write(&settings, r#"{"edited": true}"#)?;
            return Err(std::io::Error::other("injected"));
        }
        Ok(())
    })
    .unwrap_err();
    assert!(
        error.to_string().contains("keeps the new content"),
        "{error}"
    );
    assert_eq!(home.bytes(), br#"{"edited": true}"#);
    assert!(matches!(
        super::plan(&home.locations, None),
        Err(Error::State { .. })
    ));
}

#[test]
fn a_reinstall_that_reproduces_the_bytes_still_settles_its_journal() {
    let home = Home::new();
    let one = fragment(json!({"schema": 1, "values": {"/k": 1}}));
    home.install(&one);
    home.write("{}");
    home.install(&one);
    // Later edits must not look like an interrupted apply.
    home.write(r#"{"k": 1, "x": 2}"#);
    assert!(plan(&home.locations, None).is_ok());
}

#[test]
fn a_failed_settings_publication_puts_the_previous_records_back() {
    let fail_settings = |step| {
        if step == Step::Settings {
            Err(std::io::Error::other("injected"))
        } else {
            Ok(())
        }
    };
    let home = Home::new();
    home.write("{}");
    home.install(&fragment(json!({"schema": 1, "values": {"/a": 1}})));
    let records = home.state().unwrap();
    let plan = home.plan(Some(&fragment(json!({"schema": 1, "values": {"/a": 2}}))));
    apply_with(&plan, &mut { fail_settings }).unwrap_err();
    assert_eq!(home.state().unwrap(), records);
    assert!(super::plan(&home.locations, None).is_ok());

    let fresh = Home::new();
    let plan = fresh.plan(Some(&basic()));
    apply_with(&plan, &mut { fail_settings }).unwrap_err();
    assert!(fresh.state().is_none());
}

#[test]
fn a_value_edited_by_the_user_is_kept_on_removal_once_adopted() {
    let home = Home::new();
    home.install(&fragment(
        json!({"schema": 1, "values": {"/model": "opus"}}),
    ));
    home.write(r#"{"model": "sonnet"}"#);
    let plan = home.install(&fragment(
        json!({"schema": 1, "values": {"/model": "sonnet"}}),
    ));
    assert_eq!(
        ops(&plan)[0],
        (OperationKind::Noop, "/model", Reason::Adopted)
    );
    apply(&home.plan(None)).unwrap();
    assert_eq!(home.json(), json!({"model": "sonnet"}));
}

#[test]
fn the_new_backup_survives_pruning_when_older_names_sort_later() {
    let home = Home::new();
    home.write("{}");
    let dir = home
        .locations
        .local_root
        .join("state/backups/claude-settings");
    fs::create_dir_all(&dir).unwrap();
    for n in 0..10 {
        fs::write(
            dir.join(format!("9999999999999999999{n}-abcdef01.json")),
            "{}",
        )
        .unwrap();
    }
    let plan = home.plan(Some(&basic()));
    let backup = apply(&plan).unwrap().unwrap();
    assert!(backup.exists());
    assert_eq!(home.backups().len(), 10);
}

#[test]
fn backups_are_private_bounded_and_restorable() {
    let home = Home::new();
    home.write("{}");
    for n in 0..12 {
        home.install(&fragment(json!({"schema": 1, "values": {"/n": n}})));
    }
    let backups = home.backups();
    assert_eq!(backups.len(), 10);
    let newest = backups.last().unwrap();
    let path = home
        .locations
        .local_root
        .join("state/backups/claude-settings")
        .join(newest);
    assert_eq!(
        fs::metadata(&path).unwrap().permissions().mode() & 0o777,
        0o600
    );
    assert_eq!(
        serde_json::from_slice::<Value>(&fs::read(&path).unwrap()).unwrap(),
        json!({"n": 10})
    );

    assert!(matches!(
        restore(&home.locations, "../settings.json"),
        Err(ApplyError::InvalidBackup(_))
    ));
    restore(&home.locations, newest).unwrap();
    assert_eq!(home.json(), json!({"n": 10}));
    // The restored file is no longer Agenvil's last write, so a second restore is refused.
    assert!(matches!(
        restore(&home.locations, newest),
        Err(ApplyError::Changed { .. })
    ));
}

#[test]
fn planning_never_creates_directories_or_files() {
    let home = Home::new();
    let before = tree(home.dir.path());
    let plan = home.plan(Some(&basic()));
    assert_eq!(plan.files.settings.action, FileAction::Create);
    assert_eq!(tree(home.dir.path()), before);
    home.write("{}");
    let before = tree(home.dir.path());
    home.plan(Some(&basic()));
    home.plan(None);
    assert_eq!(tree(home.dir.path()), before);
}

#[test]
fn existing_file_modes_are_preserved() {
    let home = Home::new();
    home.write("{}");
    fs::set_permissions(home.settings(), fs::Permissions::from_mode(0o640)).unwrap();
    home.install(&basic());
    let mode = fs::metadata(home.settings()).unwrap().permissions().mode();
    assert_eq!(mode & 0o777, 0o640);
}

#[test]
fn json_plans_name_operations_files_and_summary() {
    let home = Home::new();
    let plan = home.plan(Some(&basic()));
    let value = serde_json::to_value(&plan).unwrap();
    assert_eq!(
        value["operations"][0],
        json!({"operation": "add", "kind": "value", "target": "/model", "ownership": "absent", "reason": "missing"})
    );
    assert_eq!(value["operations"][2]["event"], "PreToolUse");
    assert_eq!(value["files"]["settings"]["action"], "create");
    assert_eq!(value["summary"]["add"], 4);
}
