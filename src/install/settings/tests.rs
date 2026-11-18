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

    /// Generations Agenvil set aside beside `settings.json`, with their content.
    fn kept(&self) -> Vec<Vec<u8>> {
        fs::read_dir(&self.locations.claude_root)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with(".tmp-agenvil")
            })
            .map(|path| fs::read(path).unwrap())
            .collect()
    }
}

/// Fails the `nth` (1-based) fault point of `at`: 1 is before publication, 2 once visible.
fn fail_at(at: Step, nth: usize) -> impl FnMut(Step) -> std::io::Result<()> {
    let mut seen = 0;
    move |step| {
        if step == at {
            seen += 1;
            if seen == nth {
                return Err(std::io::Error::other("injected"));
            }
        }
        Ok(())
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
fn reconcile_swapped_hook_groups_in_place_in_either_order() {
    for reverse in [false, true] {
        let home = Home::new();
        home.install(&fragment(json!({"schema": 1, "hooks": [
            hook("first", "PreToolUse", "a"),
            hook("second", "PreToolUse", "b")
        ]})));

        let mut hooks = [
            hook("first", "PreToolUse", "b"),
            hook("second", "PreToolUse", "a"),
        ];
        if reverse {
            hooks.reverse();
        }
        let next = fragment(json!({"schema": 1, "hooks": hooks}));
        let plan = home.install(&next);
        assert_eq!(plan.summary.change, 2);

        let doc = home.json();
        let groups = &doc["hooks"]["PreToolUse"];
        assert_eq!(*groups, json!([group("b"), group("a")]));
        let state: Value = serde_json::from_slice(&home.state().unwrap()).unwrap();
        let records = state["harnesses"]["claude"]["user"]["hooks"]
            .as_array()
            .unwrap();
        for (index, id) in ["first", "second"].into_iter().enumerate() {
            let record = records.iter().find(|record| record["id"] == id).unwrap();
            assert_eq!(record["event"], "PreToolUse");
            assert_eq!(record["group"], groups[index]);
        }

        let repeated = home.plan(Some(&next));
        assert_eq!(repeated.operations.len(), 2);
        assert_eq!(repeated.summary.noop, 2);
        assert_eq!(repeated.files.settings.action, FileAction::Unchanged);
        assert_eq!(repeated.files.state.action, FileAction::Unchanged);
    }
}

#[test]
fn reconcile_hook_moves_without_shifting_later_sources() {
    let home = Home::new();
    home.install(&fragment(json!({"schema": 1, "hooks": [
        hook("obsolete", "PreToolUse", "obsolete"),
        hook("outside-left", "PreToolUse", "left"),
        hook("stays", "PreToolUse", "old"),
        hook("outside-right", "PreToolUse", "right")
    ]})));

    let next = fragment(json!({"schema": 1, "hooks": [
        hook("outside-right", "PostToolUse", "right"),
        hook("outside-left", "PostToolUse", "left"),
        hook("stays", "PreToolUse", "new")
    ]}));
    let plan = home.install(&next);
    assert_eq!(plan.summary.change, 3);
    assert_eq!(plan.summary.remove, 1);
    assert_eq!(
        home.json()["hooks"],
        json!({"PreToolUse": [group("new")], "PostToolUse": [group("right"), group("left")]})
    );
    assert_eq!(home.plan(Some(&next)).summary.noop, 3);
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
fn uninstall_keeps_a_value_record_when_its_parent_is_no_longer_an_object() {
    let home = Home::new();
    let desired = fragment(json!({"schema": 1, "values": {"/permissions/defaultMode": "plan"}}));
    home.install(&desired);
    let installed = home.json();
    home.write(r#"{"permissions":"off"}"#);
    let before = home.bytes();
    let recorded = home.state();

    let plan = home.plan(None);
    assert_eq!(
        ops(&plan),
        [(
            OperationKind::Conflict,
            "/permissions/defaultMode",
            Reason::TypeClash
        )]
    );
    assert_eq!(plan.operations[0].ownership, Ownership::Mismatch);
    assert_eq!(plan.files.settings.action, FileAction::Unchanged);
    assert_eq!(plan.files.state.action, FileAction::Unchanged);
    assert!(matches!(
        apply(&plan),
        Err(ApplyError::Conflicts { count: 1 })
    ));
    assert_eq!(home.bytes(), before);
    assert_eq!(home.state(), recorded);

    home.write(&installed.to_string());
    let removal = home.plan(None);
    assert_eq!(
        ops(&removal),
        [(
            OperationKind::Remove,
            "/permissions/defaultMode",
            Reason::Obsolete
        )]
    );
    apply(&removal).unwrap();
    assert_eq!(home.json(), json!({}));
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
    assert!(error.to_string().contains("not rolled back"), "{error}");
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
    let first = home.bytes();
    apply(&home.plan(None)).unwrap();
    home.install(&one);
    assert_eq!(home.bytes(), first);
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
fn a_managed_value_edited_to_match_the_new_desired_value_conflicts() {
    let home = Home::new();
    home.install(&fragment(
        json!({"schema": 1, "values": {"/model": "opus"}}),
    ));
    let recorded = home.state().unwrap();
    home.write(r#"{"model": "sonnet"}"#);
    let before = home.bytes();

    let next = fragment(json!({"schema": 1, "values": {"/model": "sonnet"}}));
    let plan = home.plan(Some(&next));
    assert_eq!(
        ops(&plan),
        [(OperationKind::Conflict, "/model", Reason::UserEdited)]
    );
    assert_eq!(plan.operations[0].ownership, Ownership::Mismatch);
    assert_eq!(plan.files.settings.action, FileAction::Unchanged);
    assert_eq!(plan.files.state.action, FileAction::Unchanged);
    assert!(matches!(
        apply(&plan),
        Err(ApplyError::Conflicts { count: 1 })
    ));
    assert_eq!(home.bytes(), before);
    assert_eq!(home.state().unwrap(), recorded);
    let state: Value = serde_json::from_slice(&recorded).unwrap();
    assert_eq!(
        state["harnesses"]["claude"]["user"]["values"][0]["value"],
        "opus"
    );
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

#[test]
fn a_recorded_value_the_user_deleted_is_a_conflict_until_removed() {
    let home = Home::new();
    home.install(&basic());
    let mut doc = home.json();
    doc.as_object_mut().unwrap().shift_remove("model");
    home.write(&doc.to_string());
    let plan = home.plan(Some(&basic()));
    assert_eq!(
        ops(&plan)[0],
        (OperationKind::Conflict, "/model", Reason::UserRemoved)
    );
    assert_eq!(plan.operations[0].ownership, Ownership::Mismatch);
    assert_eq!(plan.files.state.action, FileAction::Unchanged);
    // Re-adopting: `--remove` drops the stale record, then a reinstall adds the key again.
    apply(&home.plan(None)).unwrap();
    let plan = home.install(&basic());
    assert_eq!(
        ops(&plan)[0],
        (OperationKind::Add, "/model", Reason::Missing)
    );
    assert_eq!(home.json()["model"], "opus");
}

#[test]
fn managed_hook_groups_that_drifted_are_conflicts() {
    let home = Home::new();
    home.install(&basic());
    let mut doc = home.json();
    doc["hooks"]["PreToolUse"][0]["hooks"]
        .as_array_mut()
        .unwrap()
        .push(json!({"type": "command", "command": "extra.sh"}));
    doc["hooks"].as_object_mut().unwrap().shift_remove("Stop");
    home.write(&doc.to_string());
    let before = home.bytes();
    let install = home.plan(Some(&basic()));
    assert_eq!(
        ops(&install)[2..],
        [
            (OperationKind::Conflict, "guard", Reason::HookModified),
            (OperationKind::Conflict, "done", Reason::UserRemoved),
        ]
    );
    let removal = home.plan(None);
    assert_eq!(
        ops(&removal)[2..],
        [
            (OperationKind::Conflict, "guard", Reason::HookModified),
            (OperationKind::Remove, "done", Reason::Gone),
        ]
    );
    assert_eq!(home.bytes(), before);

    // Once the user's copy is gone too, `--remove` then a reinstall re-adds both groups.
    doc["hooks"]
        .as_object_mut()
        .unwrap()
        .shift_remove("PreToolUse");
    home.write(&doc.to_string());
    apply(&home.plan(None)).unwrap();
    home.install(&basic());
    assert_eq!(
        home.json()["hooks"]["PreToolUse"],
        json!([group("guard.sh")])
    );
    assert_eq!(home.json()["hooks"]["Stop"], json!([group("done.sh")]));
}

#[test]
fn a_managed_hook_event_replaced_by_a_non_array_keeps_its_record() {
    let home = Home::new();
    home.install(&basic());
    let mut doc = home.json();
    doc["hooks"]["Stop"] = json!("off");
    home.write(&doc.to_string());
    for desired in [Some(&basic()), None] {
        let plan = home.plan(desired);
        assert_eq!(
            ops(&plan)[3],
            (OperationKind::Conflict, "done", Reason::TypeClash)
        );
        assert!(matches!(
            apply(&plan),
            Err(ApplyError::Conflicts { count: 1 })
        ));
    }
}

#[test]
fn duplicate_managed_hook_groups_conflict_until_one_copy_is_removed() {
    let home = Home::new();
    let desired =
        |event, command| fragment(json!({"schema": 1, "hooks": [hook("done", event, command)]}));
    let original = desired("Stop", "done.sh");
    let updated = desired("Stop", "updated.sh");
    let moved = desired("PreToolUse", "done.sh");
    home.install(&original);
    let installed = home.json();
    let mut doc = installed.clone();
    let recorded_group = doc["hooks"]["Stop"][0].clone();
    doc["hooks"]["Stop"]
        .as_array_mut()
        .unwrap()
        .push(recorded_group);
    home.write(&doc.to_string());
    let before = home.bytes();
    let recorded = home.state();

    for desired in [Some(&updated), Some(&moved), Some(&original), None] {
        let plan = home.plan(desired);
        assert_eq!(
            ops(&plan),
            [(OperationKind::Conflict, "done", Reason::HookDuplicate)]
        );
        assert_eq!(plan.operations[0].ownership, Ownership::Mismatch);
        assert_eq!(plan.files.settings.action, FileAction::Unchanged);
        assert_eq!(plan.files.state.action, FileAction::Unchanged);
        assert!(matches!(
            apply(&plan),
            Err(ApplyError::Conflicts { count: 1 })
        ));
        assert_eq!(home.bytes(), before);
        assert_eq!(home.state(), recorded);
    }

    home.write(&installed.to_string());
    let update = home.install(&updated);
    assert_eq!(
        ops(&update),
        [(OperationKind::Change, "done", Reason::Changed)]
    );
    assert_eq!(home.json()["hooks"]["Stop"], json!([group("updated.sh")]));
    let removal = home.plan(None);
    assert_eq!(
        ops(&removal),
        [(OperationKind::Remove, "done", Reason::Obsolete)]
    );
    apply(&removal).unwrap();
    assert_eq!(home.json(), json!({}));
}

#[test]
fn hook_updates_and_moves_onto_an_unmanaged_command_conflict() {
    let home = Home::new();
    home.install(&basic());
    let mut doc = home.json();
    doc["hooks"]["PreToolUse"]
        .as_array_mut()
        .unwrap()
        .push(group("mine.sh"));
    doc["hooks"]["PostToolUse"] = json!([group("mine.sh")]);
    home.write(&doc.to_string());
    let before = home.bytes();
    for event in ["PreToolUse", "PostToolUse"] {
        let next = fragment(json!({"schema": 1, "hooks": [hook("guard", event, "mine.sh")]}));
        let plan = home.plan(Some(&next));
        assert!(
            ops(&plan).contains(&(OperationKind::Conflict, "guard", Reason::HookDuplicate)),
            "{event}"
        );
        assert!(matches!(apply(&plan), Err(ApplyError::Conflicts { .. })));
    }
    assert_eq!(home.bytes(), before);

    // Two managed groups swapping commands do not collide with each other.
    let home = Home::new();
    let pair = |a: &str, b: &str| {
        fragment(json!({"schema": 1, "hooks": [hook("x", "Stop", a), hook("y", "Stop", b)]}))
    };
    home.install(&pair("a.sh", "b.sh"));
    let plan = home.install(&pair("b.sh", "a.sh"));
    assert_eq!(plan.summary.change, 2);
    let stop = home.json()["hooks"]["Stop"].clone();
    assert_eq!(stop.as_array().unwrap().len(), 2);
    assert_eq!(home.plan(Some(&pair("b.sh", "a.sh"))).summary.noop, 2);
}

#[test]
fn a_value_replaced_by_its_parent_object_is_not_pruned() {
    let home = Home::new();
    home.write("{}");
    home.install(&fragment(json!({"schema": 1, "values": {"/a/b/c": 1}})));
    home.install(&fragment(json!({"schema": 1, "values": {"/a": {"b": {}}}})));
    assert_eq!(home.json(), json!({"a": {"b": {}}}));
    let plan = home.plan(Some(&fragment(
        json!({"schema": 1, "values": {"/a": {"b": {}}}}),
    )));
    assert_eq!(ops(&plan), [(OperationKind::Noop, "/a", Reason::Current)]);
}

#[test]
fn results_beyond_the_read_limit_are_refused_before_writing() {
    let big = "x".repeat(MAX_BYTES as usize / 2 + 1);
    // An adopted value is stored twice in managed.json.
    let home = Home::new();
    home.write(&json!({"big": big}).to_string());
    let adopt = fragment(json!({"schema": 1, "values": {"/big": big}}));
    let error = plan(&home.locations, Some(&adopt)).unwrap_err();
    assert!(error.to_string().contains("8 MiB"), "{error}");
    // Two halves fit the fragment and the file, but not their merge.
    let other = fragment(json!({"schema": 1, "values": {"/more": big}}));
    let error = plan(&home.locations, Some(&other)).unwrap_err();
    assert!(error.to_string().contains("settings.json"), "{error}");
    assert!(home.state().is_none());
    assert!(home.backups().is_empty());
}

#[test]
fn rolling_back_a_created_file_never_deletes_a_concurrent_save() {
    let home = Home::new();
    let plan = home.plan(Some(&basic()));
    let settings = home.settings();
    apply_with(&plan, &mut |step| {
        if step == Step::Commit {
            fs::write(&settings, r#"{"claude": true}"#)?;
            return Err(std::io::Error::other("injected"));
        }
        Ok(())
    })
    .unwrap_err();
    assert_eq!(home.bytes(), br#"{"claude": true}"#);
    assert!(home.kept().is_empty());
}

#[test]
fn a_concurrent_replacement_during_publication_is_put_back() {
    let home = Home::new();
    home.write("{}");
    let records = home.state();
    let plan = home.plan(Some(&basic()));
    let settings = home.settings();
    let real = home.dir.path().join("real.json");
    fs::write(&real, "{}").unwrap();
    // A symlink appears between the check and the exchange.
    let mut first = true;
    let error = apply_with(&plan, &mut |step| {
        if step == Step::Settings && std::mem::take(&mut first) {
            fs::remove_file(&settings)?;
            std::os::unix::fs::symlink(&real, &settings)?;
        }
        Ok(())
    })
    .unwrap_err();
    assert!(error.to_string().contains("put back"), "{error}");
    assert!(fs::symlink_metadata(&settings).unwrap().is_symlink());
    assert_eq!(fs::read(&real).unwrap(), b"{}");
    assert!(home.kept().is_empty());
    assert_eq!(home.state(), records);
}

#[test]
fn a_newer_generation_is_never_swapped_away_during_recovery() {
    let home = Home::new();
    home.write("{}");
    let plan = home.plan(Some(&basic()));
    let settings = home.settings();
    let newer = home.locations.claude_root.join("newer");
    let mut calls = 0;
    let error = apply_with(&plan, &mut |step| {
        if step == Step::Settings {
            calls += 1;
            if calls == 1 {
                fs::write(&settings, "1")?;
            } else {
                fs::write(&newer, "2")?;
                fs::rename(&newer, &settings)?;
            }
        }
        Ok(())
    })
    .unwrap_err();
    assert!(
        error.to_string().contains("was kept as .tmp-agenvil"),
        "{error}"
    );
    assert_eq!(home.bytes(), b"2");
    assert_eq!(home.kept(), [b"1".to_vec()]);
}

#[test]
fn failures_after_a_visible_write_keep_what_was_published() {
    // settings.json is visible: the journal stays, and the next plan settles from it.
    let home = Home::new();
    home.write("{}");
    let plan = home.plan(Some(&basic()));
    let error = apply_with(&plan, &mut fail_at(Step::Settings, 2)).unwrap_err();
    assert!(error.to_string().contains("not fully synced"), "{error}");
    assert_eq!(home.json()["model"], "opus");
    let next = home.plan(Some(&basic()));
    assert_eq!(next.summary.noop, 4);
    assert_eq!(next.files.state.action, FileAction::Modify);
    apply(&next).unwrap();

    // managed.json is committed: settings.json is not rolled back under it.
    let home = Home::new();
    home.write("{}");
    let plan = home.plan(Some(&basic()));
    let error = apply_with(&plan, &mut fail_at(Step::Commit, 2)).unwrap_err();
    assert!(error.to_string().contains("not fully synced"), "{error}");
    assert_eq!(home.json()["model"], "opus");
    let next = home.plan(Some(&basic()));
    assert_eq!(next.summary.noop, 4);
    assert_eq!(next.files.state.action, FileAction::Unchanged);
}
