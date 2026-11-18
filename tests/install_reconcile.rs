#![cfg(any(target_os = "linux", target_os = "macos"))]

use std::collections::BTreeMap;
use std::fs::{self, File};
use std::io::Read;
use std::os::unix::fs::{MetadataExt, PermissionsExt, symlink};
use std::path::{Path, PathBuf};

use agenvil::build;
use agenvil::config::Config;
use agenvil::install::{
    ApplyError, ConflictReason, Layer, LedgerError, LedgerState, Locations, OperationKind, Plan,
    PlanError, Reason, ReconcileReason, SnapshotStatus, apply, plan,
};
use agenvil::inventory::Inventory;
use serde_json::{Value, value::RawValue};
use tempfile::TempDir;

const CONFIG: &str = "schema = 1\n[project]\nname = \"demo\"\n";
const AGENT: &str = "---\nname: reviewer\ndescription: Review changes.\n---\nReview carefully.\n";
const SKILL: &str = "---\nname: tdd\ndescription: Test first.\n---\nWrite tests.\n";
const RULE: &str = "# Style\nKeep changes focused.\n";

struct Project {
    project: TempDir,
    staging: TempDir,
    home: TempDir,
    locations: Locations,
}

impl Project {
    fn built() -> Self {
        let home = tempfile::tempdir().unwrap();
        let locations = Locations {
            claude_root: home.path().join("custom-claude"),
            local_root: home.path().join(".agents.local"),
        };
        let p = Self {
            project: tempfile::tempdir().unwrap(),
            staging: tempfile::tempdir().unwrap(),
            home,
            locations,
        };
        p.write("agenvil.toml", CONFIG);
        p.write("agents/reviewer.md", AGENT);
        p.write("skills/tdd/SKILL.md", SKILL);
        p.write("rules/style.md", RULE);
        p.build();
        p
    }

    fn write(&self, path: &str, text: impl AsRef<[u8]>) {
        put(&self.project.path().join(path), text);
    }

    fn config(&self) -> Config {
        Config::load(self.project.path()).unwrap()
    }

    fn build(&self) {
        let root = self.project.path();
        let config = self.config();
        let inventory = Inventory::build(root, &config, &build::limits()).unwrap();
        build::claude(root, &config, &inventory, &self.out()).unwrap();
    }

    fn out(&self) -> PathBuf {
        self.staging.path().join("approved")
    }

    fn plan_result(&self) -> Result<Plan, PlanError> {
        plan(
            self.project.path(),
            &self.config(),
            &self.out(),
            &self.locations,
        )
    }

    fn plan(&self) -> Plan {
        self.plan_result().unwrap()
    }

    fn apply(&self) {
        apply(&self.plan()).unwrap();
    }

    fn dest(&self, path: &str) -> PathBuf {
        self.locations.claude_root.join(path)
    }

    fn ledger(&self) -> PathBuf {
        self.locations.local_root.join("state/ownership.json")
    }

    fn ledger_json(&self) -> Value {
        serde_json::from_slice(&fs::read(self.ledger()).unwrap()).unwrap()
    }

    fn snapshot_count(&self) -> usize {
        fs::read_dir(self.locations.local_root.join("state/snapshots"))
            .unwrap()
            .count()
    }

    fn source(&self, path: &str) -> PathBuf {
        self.project.path().join(path)
    }
}

fn put(path: &Path, text: impl AsRef<[u8]>) {
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, text).unwrap();
}

#[test]
fn empty_home_first_install() {
    let p = Project::built();
    assert_eq!(fs::read_dir(p.home.path()).unwrap().count(), 0);
    let plan = p.plan();
    assert_eq!(plan.summary().create, 3);
    let report = apply(&plan).unwrap();
    assert_eq!(report.ledger, LedgerState::Committed);
    assert!(
        p.locations
            .local_root
            .join("state/ownership.lock")
            .is_file()
    );
    let ledger = p.ledger_json();
    assert_eq!(ledger["schema"], 1);
    let user = &ledger["harnesses"]["claude"]["user"];
    assert_eq!(user["applied"]["kit"], "demo");
    assert_eq!(
        user["applied"]["manifest_sha256"],
        agenvil::manifest::sha256(&fs::read(p.out().join("manifest.json")).unwrap())
    );
    assert_eq!(user["entries"].as_array().unwrap().len(), 3);
    assert!(
        user["entries"]
            .as_array()
            .unwrap()
            .iter()
            .all(|entry| entry["pending"].is_null())
    );
}

#[test]
fn agent_skill_directory_and_rule_symlinks_point_to_snapshots() {
    let p = Project::built();
    let plan = p.plan();
    apply(&plan).unwrap();
    for (path, file, contents) in [
        ("agents/reviewer.md", "", AGENT),
        ("skills/tdd", "SKILL.md", SKILL),
        ("rules/style.md", "", RULE),
    ] {
        let op = plan
            .operations()
            .iter()
            .find(|op| op.destination == path)
            .unwrap();
        let dest = p.dest(path);
        assert!(fs::symlink_metadata(&dest).unwrap().is_symlink());
        let target = fs::read_link(&dest).unwrap();
        assert_eq!(target, *op.link_target.as_ref().unwrap());
        assert!(target.starts_with(plan.snapshots()[0].path.parent().unwrap().parent().unwrap()));
        assert_eq!(
            fs::read_to_string(if file.is_empty() {
                dest
            } else {
                dest.join(file)
            })
            .unwrap(),
            contents
        );
    }
}

#[test]
fn preserves_full_skill_directory_and_executable_bits() {
    let p = Project::built();
    p.write("skills/tdd/scripts/check.sh", "#!/bin/sh\nexit 71\n");
    p.write("skills/tdd/references/check.md", "# Reference\n");
    p.write("skills/tdd/assets/data.bin", [0, 255, 128, 55]);
    fs::set_permissions(
        p.source("skills/tdd/scripts/check.sh"),
        fs::Permissions::from_mode(0o755),
    )
    .unwrap();
    p.build();
    p.apply();

    let skill = p.dest("skills/tdd");
    assert_eq!(fs::read_to_string(skill.join("SKILL.md")).unwrap(), SKILL);
    assert_eq!(
        fs::read_to_string(skill.join("scripts/check.sh")).unwrap(),
        "#!/bin/sh\nexit 71\n"
    );
    assert_eq!(
        fs::read_to_string(skill.join("references/check.md")).unwrap(),
        "# Reference\n"
    );
    assert_eq!(
        fs::read(skill.join("assets/data.bin")).unwrap(),
        [0, 255, 128, 55]
    );
    assert_eq!(
        fs::metadata(skill.join("scripts/check.sh")).unwrap().mode() & 0o777,
        0o555
    );
    assert_eq!(
        fs::metadata(skill.join("assets/data.bin")).unwrap().mode() & 0o777,
        0o444
    );
    assert_eq!(p.plan().summary().noop, 3);
}

#[test]
fn repeat_is_idempotent_and_writes_nothing() {
    let p = Project::built();
    p.apply();
    let before = fs::metadata(p.ledger()).unwrap();
    let bytes = fs::read(p.ledger()).unwrap();
    let link = fs::symlink_metadata(p.dest("skills/tdd")).unwrap();
    let snap_count = p.snapshot_count();
    let plan = p.plan();
    assert_eq!(plan.summary().noop, 3);
    assert_eq!(apply(&plan).unwrap().ledger, LedgerState::Unchanged);
    let after = fs::metadata(p.ledger()).unwrap();
    assert_eq!(before.modified().unwrap(), after.modified().unwrap());
    assert_eq!(before.ino(), after.ino());
    assert_eq!(fs::read(p.ledger()).unwrap(), bytes);
    assert_eq!(
        fs::symlink_metadata(p.dest("skills/tdd")).unwrap().ino(),
        link.ino()
    );
    assert_eq!(p.snapshot_count(), snap_count);
}

#[test]
fn adds_a_new_resource_without_replacing_other_links() {
    let p = Project::built();
    p.apply();
    let before = fs::symlink_metadata(p.dest("agents/reviewer.md"))
        .unwrap()
        .ino();
    p.write("rules/second.md", "# Second\n");
    p.build();
    let plan = p.plan();
    assert_eq!(plan.summary().create, 1);
    apply(&plan).unwrap();
    assert_eq!(
        fs::read_to_string(p.dest("rules/second.md")).unwrap(),
        "# Second\n"
    );
    assert_eq!(
        fs::symlink_metadata(p.dest("agents/reviewer.md"))
            .unwrap()
            .ino(),
        before
    );
}

#[test]
fn content_change_updates_one_link_and_keeps_old_snapshot() {
    let p = Project::built();
    p.apply();
    let before = fs::read_link(p.dest("rules/style.md")).unwrap();
    p.write("rules/style.md", "# Changed\n");
    p.build();
    let plan = p.plan();
    assert_eq!(plan.summary().update, 1);
    apply(&plan).unwrap();
    assert_ne!(fs::read_link(p.dest("rules/style.md")).unwrap(), before);
    assert_eq!(
        fs::read_to_string(p.dest("rules/style.md")).unwrap(),
        "# Changed\n"
    );
    assert_eq!(fs::read_to_string(before).unwrap(), RULE);
    assert_eq!(p.snapshot_count(), 4);
}

#[test]
fn removes_only_the_obsolete_managed_resource() {
    let p = Project::built();
    p.apply();
    put(&p.dest("skills/unrelated/SKILL.md"), "User skill");
    fs::remove_dir_all(p.source("skills/tdd")).unwrap();
    p.build();
    let plan = p.plan();
    assert_eq!(plan.summary().remove, 1);
    apply(&plan).unwrap();
    assert!(!p.dest("skills/tdd").is_symlink());
    assert_eq!(
        fs::read_to_string(p.dest("skills/unrelated/SKILL.md")).unwrap(),
        "User skill"
    );
}

#[test]
fn renames_a_resource_using_remove_and_create() {
    let p = Project::built();
    p.apply();
    fs::rename(p.source("rules/style.md"), p.source("rules/new-style.md")).unwrap();
    p.build();
    let plan = p.plan();
    assert_eq!((plan.summary().remove, plan.summary().create), (1, 1));
    apply(&plan).unwrap();
    assert!(!p.dest("rules/style.md").is_symlink());
    assert_eq!(
        fs::read_to_string(p.dest("rules/new-style.md")).unwrap(),
        RULE
    );
}

fn refuses_collision(p: &Project, reason: ConflictReason) {
    let plan = p.plan();
    let operation = plan
        .operations()
        .iter()
        .find(|op| op.destination == "agents/reviewer.md")
        .unwrap();
    assert_eq!(operation.operation, OperationKind::Conflict);
    assert_eq!(operation.reason, Reason::Conflict(reason));
    assert!(matches!(
        apply(&plan),
        Err(ApplyError::Conflicts { count: 1 })
    ));
}

#[test]
fn conflicts_with_a_user_owned_file_and_aborts_the_whole_apply() {
    let p = Project::built();
    put(&p.dest("agents/reviewer.md"), "User edits");
    refuses_collision(&p, ConflictReason::UserOwned);
    assert_eq!(
        fs::read_to_string(p.dest("agents/reviewer.md")).unwrap(),
        "User edits"
    );
    assert!(!p.dest("rules").exists());
    assert!(!p.locations.local_root.exists());
}

#[test]
fn conflicts_with_a_user_owned_directory() {
    let p = Project::built();
    put(&p.dest("agents/reviewer.md/note"), "User notes");
    refuses_collision(&p, ConflictReason::UserOwned);
    assert_eq!(
        fs::read_to_string(p.dest("agents/reviewer.md/note")).unwrap(),
        "User notes"
    );
}

#[test]
fn conflicts_with_a_user_owned_foreign_symlink() {
    let p = Project::built();
    let foreign = p.home.path().join("foreign.md");
    put(&foreign, "User notes");
    let dest = p.dest("agents/reviewer.md");
    fs::create_dir_all(dest.parent().unwrap()).unwrap();
    symlink(&foreign, &dest).unwrap();
    refuses_collision(&p, ConflictReason::UserOwned);
    assert_eq!(fs::read_link(dest).unwrap(), foreign);
}

#[test]
fn conflicts_with_a_user_owned_file_even_with_identical_bytes() {
    let p = Project::built();
    put(&p.dest("agents/reviewer.md"), AGENT);
    refuses_collision(&p, ConflictReason::UserOwned);
    assert!(!p.dest("agents/reviewer.md").is_symlink());
    assert_eq!(
        fs::read_to_string(p.dest("agents/reviewer.md")).unwrap(),
        AGENT
    );
}

#[test]
fn private_override_wins_and_private_content_stays_untouched() {
    let p = Project::built();
    p.apply();
    let private = p.locations.local_root.join("agents/reviewer.md");
    put(&private, "Private reviewer");
    let timestamp = fs::metadata(&private).unwrap().modified().unwrap();
    let plan = p.plan();
    assert_eq!(plan.summary().update, 1);
    assert_eq!(plan.operations()[0].layer, Some(Layer::Private));
    apply(&plan).unwrap();
    assert_eq!(
        fs::read_to_string(p.dest("agents/reviewer.md")).unwrap(),
        "Private reviewer"
    );
    assert_eq!(
        fs::canonicalize(&private).unwrap(),
        fs::read_link(p.dest("agents/reviewer.md")).unwrap()
    );
    assert_eq!(
        fs::metadata(&private).unwrap().modified().unwrap(),
        timestamp
    );
    assert_eq!(fs::read_to_string(&private).unwrap(), "Private reviewer");
    fs::remove_file(private).unwrap();
    p.apply();
    assert_eq!(
        fs::read_to_string(p.dest("agents/reviewer.md")).unwrap(),
        AGENT
    );
}

#[test]
fn redirected_managed_symlink_is_a_conflict() {
    let p = Project::built();
    p.apply();
    let dest = p.dest("agents/reviewer.md");
    let target = fs::read_link(&dest).unwrap();
    let altered = format!(
        "{}/./{}",
        target.parent().unwrap().display(),
        target.file_name().unwrap().to_str().unwrap()
    );
    fs::remove_file(&dest).unwrap();
    symlink(&altered, &dest).unwrap();
    refuses_collision(&p, ConflictReason::OwnershipMismatch);
    assert_eq!(fs::read_link(dest).unwrap().as_os_str(), altered.as_str());
}

#[test]
fn managed_symlink_replaced_by_a_real_file_is_a_conflict() {
    let p = Project::built();
    p.apply();
    let dest = p.dest("agents/reviewer.md");
    fs::remove_file(&dest).unwrap();
    put(&dest, "My own agent");
    refuses_collision(&p, ConflictReason::OwnershipMismatch);
    assert_eq!(fs::read_to_string(dest).unwrap(), "My own agent");
}

#[test]
fn broken_target_is_restored_when_verified_bytes_are_available() {
    let p = Project::built();
    p.apply();
    let dest = p.dest("agents/reviewer.md");
    let target = fs::read_link(&dest).unwrap();
    fs::remove_file(&target).unwrap();
    fs::remove_dir(target.parent().unwrap()).unwrap();
    assert!(dest.is_symlink());
    assert!(!dest.exists());
    let plan = p.plan();
    assert_eq!(
        plan.operations()[0].reason,
        Reason::Reconcile(ReconcileReason::RestoreSnapshot)
    );
    assert!(
        plan.snapshots()
            .iter()
            .any(|snapshot| snapshot.status == SnapshotStatus::Materialize)
    );
    apply(&plan).unwrap();
    assert_eq!(fs::read_to_string(dest).unwrap(), AGENT);
}

#[test]
fn broken_target_without_verified_bytes_is_a_conflict() {
    let p = Project::built();
    p.apply();
    let target = fs::read_link(p.dest("agents/reviewer.md")).unwrap();
    fs::remove_file(&target).unwrap();
    fs::remove_dir(target.parent().unwrap()).unwrap();
    p.write("agents/reviewer.md", AGENT.replace("carefully", "quickly"));
    p.build();
    refuses_collision(&p, ConflictReason::BrokenManagedTarget);
}

#[test]
fn corrupt_snapshot_is_a_conflict() {
    let p = Project::built();
    p.apply();
    let target = fs::read_link(p.dest("agents/reviewer.md")).unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o644)).unwrap();
    fs::write(&target, "corrupt").unwrap();
    fs::set_permissions(&target, fs::Permissions::from_mode(0o444)).unwrap();
    refuses_collision(&p, ConflictReason::CorruptSnapshot);
    assert_eq!(fs::read_to_string(target).unwrap(), "corrupt");
}

#[test]
fn deleted_managed_link_is_recreated() {
    let p = Project::built();
    p.apply();
    let dest = p.dest("agents/reviewer.md");
    fs::remove_file(&dest).unwrap();
    let plan = p.plan();
    assert_eq!(plan.summary().create, 1);
    apply(&plan).unwrap();
    assert_eq!(fs::read_to_string(dest).unwrap(), AGENT);
}

#[test]
fn installed_links_survive_deleted_source_checkout_and_staging() {
    let p = Project::built();
    p.apply();
    fs::remove_dir_all(p.project.path()).unwrap();
    fs::remove_dir_all(p.staging.path()).unwrap();
    assert_eq!(
        fs::read_to_string(p.dest("agents/reviewer.md")).unwrap(),
        AGENT
    );
    assert_eq!(
        fs::read_to_string(p.dest("skills/tdd/SKILL.md")).unwrap(),
        SKILL
    );
    assert_eq!(fs::read_to_string(p.dest("rules/style.md")).unwrap(), RULE);
}

#[test]
fn corrupt_ledger_is_an_actionable_error() {
    let p = Project::built();
    put(&p.ledger(), "{malformed");
    assert!(matches!(
        p.plan_result(),
        Err(PlanError::Ledger(LedgerError::Invalid { .. }))
    ));
    assert_eq!(fs::read_to_string(p.ledger()).unwrap(), "{malformed");
    assert!(!p.dest("agents").exists());
}

#[test]
fn unsupported_ledger_schema_is_an_actionable_error() {
    let p = Project::built();
    put(&p.ledger(), r#"{"schema":7,"future_field":true}"#);
    assert!(matches!(
        p.plan_result(),
        Err(PlanError::Ledger(LedgerError::Schema { schema: 7, .. }))
    ));
    assert!(!p.dest("agents").exists());
}

#[test]
fn atomic_ledger_update_preserves_the_open_old_file() {
    let p = Project::built();
    p.apply();
    let mut old = File::open(p.ledger()).unwrap();
    let old_bytes = fs::read(p.ledger()).unwrap();
    let old_inode = old.metadata().unwrap().ino();
    p.write("rules/second.md", "# Second\n");
    p.build();
    p.apply();
    let mut bytes = Vec::new();
    old.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, old_bytes);
    assert_ne!(fs::read(p.ledger()).unwrap(), bytes);
    let metadata = fs::metadata(p.ledger()).unwrap();
    assert_ne!(metadata.ino(), old_inode);
    assert_eq!(metadata.mode() & 0o777, 0o600);
}

#[test]
fn destination_that_appears_between_plan_and_apply_is_preserved() {
    let p = Project::built();
    let plan = p.plan();
    put(&p.dest("agents/reviewer.md"), "User file");
    assert!(matches!(apply(&plan), Err(ApplyError::Changed { .. })));
    assert_eq!(
        fs::read_to_string(p.dest("agents/reviewer.md")).unwrap(),
        "User file"
    );
    assert!(!p.locations.local_root.exists());
}

#[test]
fn ledger_changed_between_plan_and_apply_is_preserved() {
    let p = Project::built();
    p.apply();
    let plan = p.plan();
    let mut bytes = fs::read(p.ledger()).unwrap();
    bytes.push(b' ');
    fs::write(p.ledger(), &bytes).unwrap();
    assert!(matches!(apply(&plan), Err(ApplyError::Changed { .. })));
    assert_eq!(fs::read(p.ledger()).unwrap(), bytes);
}

#[test]
fn symlink_resource_ancestors_are_rejected_without_touching_victims() {
    let p = Project::built();
    let victim = tempfile::tempdir().unwrap();
    put(&victim.path().join("reviewer.md"), "Protected");
    fs::create_dir_all(&p.locations.claude_root).unwrap();
    for kind in ["agents", "skills", "rules"] {
        symlink(victim.path(), p.dest(kind)).unwrap();
    }
    let plan = p.plan();
    assert_eq!(plan.summary().conflict, 3);
    assert!(
        plan.operations()
            .iter()
            .all(|op| op.reason == Reason::Conflict(ConflictReason::UnsafeAncestor))
    );
    assert!(matches!(apply(&plan), Err(ApplyError::Conflicts { .. })));
    assert_eq!(
        fs::read_to_string(victim.path().join("reviewer.md")).unwrap(),
        "Protected"
    );
    assert_eq!(fs::read_dir(victim.path()).unwrap().count(), 1);
    assert!(!p.locations.local_root.exists());
}

#[test]
fn symlink_config_root_parent_swapped_after_plan_leaves_victim_untouched() {
    let mut p = Project::built();
    let parent = p.home.path().join("parent");
    fs::create_dir(&parent).unwrap();
    p.locations.claude_root = parent.join("claude");
    let plan = p.plan();
    let victim = tempfile::tempdir().unwrap();
    put(
        &victim.path().join("claude/agents/reviewer.md"),
        "Protected",
    );
    fs::rename(&parent, parent.with_extension("moved")).unwrap();
    symlink(victim.path(), parent).unwrap();
    assert!(matches!(apply(&plan), Err(ApplyError::Changed { .. })));
    assert_eq!(
        fs::read_to_string(victim.path().join("claude/agents/reviewer.md")).unwrap(),
        "Protected"
    );
    assert!(!victim.path().join("claude/skills").exists());
    assert!(!p.locations.local_root.exists());
}

#[test]
fn claude_root_alias_resolves_interior_parent_component() {
    let p = Project::built();
    let actual = p.home.path().join("real");
    fs::create_dir(&actual).unwrap();
    fs::create_dir(p.home.path().join("intermediate")).unwrap();
    symlink("intermediate/../real", &p.locations.claude_root).unwrap();
    p.apply();
    assert_eq!(
        fs::read_to_string(actual.join("agents/reviewer.md")).unwrap(),
        AGENT
    );
    assert_eq!(
        p.ledger_json()["harnesses"]["claude"]["user"]["root"],
        fs::canonicalize(actual).unwrap().to_str().unwrap()
    );
}

#[test]
fn staged_project_instructions_are_never_installed_globally() {
    let p = Project::built();
    p.write(
        "agenvil.toml",
        format!("{CONFIG}\n[paths]\ninstructions = \"instructions.md\"\n"),
    );
    p.write("instructions.md", "# Instructions\n");
    p.build();
    assert!(p.out().join("CLAUDE.md").exists());
    let plan = p.plan();
    assert!(!serde_json::to_string(&plan).unwrap().contains("CLAUDE.md"));
    apply(&plan).unwrap();
    assert!(!p.dest("CLAUDE.md").exists());
}

#[test]
fn separate_harness_namespaces_survive_an_apply() {
    let p = Project::built();
    let codex = r#"{"user":{"root":"/elsewhere","foreign":[18446744073709551617,{"num":-0,"value":1.234567890123456789e-300}]},"later":true}"#;
    let project = r#"{"opaque":[9e+01,0.100000000000000000000001,{"big":1E+10000}]}"#;
    put(
        &p.ledger(),
        format!(
            r#"{{
        "schema": 1,
        "generator": {{ "name": "agenvil", "version": "legacy" }},
        "harnesses": {{ "codex": {codex}, "claude": {{ "project": {project} }} }}
    }}"#
        ),
    );
    p.apply();
    type Namespaces = BTreeMap<String, Box<RawValue>>;
    #[derive(serde::Deserialize)]
    struct Envelope {
        harnesses: Namespaces,
    }
    let envelope: Envelope = serde_json::from_slice(&fs::read(p.ledger()).unwrap()).unwrap();
    let claude: Namespaces = serde_json::from_str(envelope.harnesses["claude"].get()).unwrap();
    assert_eq!(envelope.harnesses["codex"].get(), codex);
    assert_eq!(claude["project"].get(), project);
    let user: Value = serde_json::from_str(claude["user"].get()).unwrap();
    assert_eq!(user["entries"].as_array().unwrap().len(), 3);
}

#[test]
fn plan_rejects_staging_that_differs_from_approved_sources() {
    let p = Project::built();
    fs::write(p.out().join(".claude/rules/style.md"), "# Unapproved\n").unwrap();
    assert!(matches!(p.plan_result(), Err(PlanError::Staging { .. })));
    assert_eq!(fs::read_dir(p.home.path()).unwrap().count(), 0);
}

#[test]
fn apply_builds_snapshots_from_verified_memory_even_if_staging_changes() {
    let p = Project::built();
    let plan = p.plan();
    fs::write(p.out().join(".claude/rules/style.md"), "# Unapproved\n").unwrap();
    apply(&plan).unwrap();
    assert_eq!(fs::read_to_string(p.dest("rules/style.md")).unwrap(), RULE);
}

#[test]
fn refuses_ledger_for_a_different_claude_config_root() {
    let mut p = Project::built();
    p.apply();
    let original = p.dest("agents/reviewer.md");
    p.locations.claude_root = p.home.path().join("other-claude");
    assert!(matches!(
        p.plan_result(),
        Err(PlanError::Ledger(LedgerError::Root { .. }))
    ));
    assert!(!p.locations.claude_root.exists());
    assert_eq!(fs::read_to_string(original).unwrap(), AGENT);
}

#[test]
fn reports_ignored_private_names_and_types() {
    let p = Project::built();
    put(&p.locations.local_root.join("agents/.hidden.md"), "private");
    put(
        &p.locations.local_root.join("rules/folder.md/note"),
        "private",
    );
    put(
        &p.locations.local_root.join("skills/incomplete/note"),
        "private",
    );
    let plan = p.plan();
    assert_eq!(plan.summary().create, 3);
    assert_eq!(plan.ignored().len(), 3);
    apply(&plan).unwrap();
    assert!(!p.dest("agents/.hidden.md").exists());
    assert!(!p.dest("rules/folder.md").exists());
    assert!(!p.dest("skills/incomplete").exists());
}

#[test]
fn activates_private_only_resources_and_retires_links_when_they_are_removed() {
    let p = Project::built();
    let private = p.locations.local_root.join("skills/private/SKILL.md");
    put(&private, "Private skill");
    p.apply();
    assert_eq!(
        fs::read_to_string(p.dest("skills/private/SKILL.md")).unwrap(),
        "Private skill"
    );
    fs::rename(
        private.parent().unwrap(),
        private.parent().unwrap().with_file_name("moved-out"),
    )
    .unwrap();
    let plan = p.plan();
    assert_eq!(plan.summary().remove, 1);
    apply(&plan).unwrap();
    assert!(!p.dest("skills/private").is_symlink());
    assert_eq!(
        fs::read_to_string(p.dest("skills/moved-out/SKILL.md")).unwrap(),
        "Private skill"
    );
}

#[test]
fn another_kit_cannot_plan_removal_of_installed_links() {
    let p = Project::built();
    p.apply();
    let ledger = fs::read(p.ledger()).unwrap();
    p.write("agenvil.toml", CONFIG.replace("demo", "another-kit"));
    p.build();
    assert!(
        matches!(p.plan_result(), Err(PlanError::KitMismatch { installed, requested, .. })
        if installed == "demo" && requested == "another-kit")
    );
    assert_eq!(fs::read(p.ledger()).unwrap(), ledger);
    assert_eq!(fs::read_to_string(p.dest("rules/style.md")).unwrap(), RULE);
}

#[test]
fn claude_and_local_roots_reject_case_aliases_in_both_directions() {
    let mut p = Project::built();
    let local = p.locations.local_root.clone();
    fs::create_dir_all(local.join("state")).unwrap();
    let alias = p.home.path().join(".AGENTS.LOCAL");
    if !alias.exists() {
        return;
    }
    for claude in [
        alias.clone(),
        alias.join("state/missing"),
        alias.join("missing"),
    ] {
        p.locations.claude_root = claude;
        assert!(matches!(p.plan_result(), Err(PlanError::UnsafePath { .. })));
    }
    p.locations.claude_root = alias;
    p.locations.local_root = local.join("state/missing");
    assert!(matches!(p.plan_result(), Err(PlanError::UnsafePath { .. })));
}

#[test]
fn case_only_rename_conflicts_on_both_spellings_on_case_insensitive_filesystem() {
    let p = Project::built();
    p.apply();
    let target = fs::read_link(p.dest("rules/style.md")).unwrap();
    if fs::read_link(p.dest("rules/Style.md")).is_err() {
        return;
    }
    fs::rename(p.source("rules/style.md"), p.source("rules/temp.md")).unwrap();
    fs::rename(p.source("rules/temp.md"), p.source("rules/Style.md")).unwrap();
    p.build();
    let plan = p.plan();
    for spelling in ["rules/style.md", "rules/Style.md"] {
        let op = plan
            .operations()
            .iter()
            .find(|op| op.destination == spelling)
            .unwrap();
        assert_eq!(op.reason, Reason::Conflict(ConflictReason::CaseAlias));
    }
    assert_eq!(plan.summary().remove, 0);
    assert!(matches!(
        apply(&plan),
        Err(ApplyError::Conflicts { count: 2 })
    ));
    assert_eq!(fs::read_link(p.dest("rules/style.md")).unwrap(), target);
}

#[test]
fn writable_snapshot_bucket_and_skill_directories_are_corrupt() {
    let p = Project::built();
    p.write("skills/tdd/support/helper.txt", "helper");
    p.build();
    p.apply();
    let skill = fs::read_link(p.dest("skills/tdd")).unwrap();
    let bucket = skill.parent().unwrap();
    for dir in [
        bucket.parent().unwrap(),
        bucket,
        &skill,
        &skill.join("support"),
    ] {
        fs::set_permissions(dir, fs::Permissions::from_mode(0o775)).unwrap();
        let plan = p.plan();
        let op = plan
            .operations()
            .iter()
            .find(|op| op.destination == "skills/tdd")
            .unwrap();
        assert_eq!(op.reason, Reason::Conflict(ConflictReason::CorruptSnapshot));
        assert!(matches!(apply(&plan), Err(ApplyError::Conflicts { .. })));
        fs::set_permissions(dir, fs::Permissions::from_mode(0o755)).unwrap();
    }
    assert!(!p.plan().has_conflicts());
}

#[test]
fn same_named_kit_from_another_checkout_cannot_claim_installed_links() {
    let a = Project::built();
    a.apply();
    let ledger = fs::read(a.ledger()).unwrap();
    let dest = a.dest("agents/reviewer.md");
    let (target, before) = (
        fs::read_link(&dest).unwrap(),
        fs::symlink_metadata(&dest).unwrap(),
    );
    let mut b = Project::built();
    b.locations = a.locations.clone();
    let refused = |b: &Project| {
        matches!(b.plan_result(),
            Err(PlanError::KitMismatch { installed, installed_root, requested, requested_root })
            if installed == "demo" && requested == "demo"
                && installed_root == fs::canonicalize(a.project.path()).unwrap()
                && requested_root == fs::canonicalize(b.project.path()).unwrap())
    };
    assert!(refused(&b));
    // Even a checkout that would only remove one of A's links is refused.
    fs::remove_file(b.source("agents/reviewer.md")).unwrap();
    b.build();
    assert!(refused(&b));
    assert_eq!(fs::read(a.ledger()).unwrap(), ledger);
    let after = fs::symlink_metadata(&dest).unwrap();
    assert_eq!(fs::read_link(&dest).unwrap(), target);
    assert_eq!((after.dev(), after.ino()), (before.dev(), before.ino()));
    assert_eq!(fs::read_to_string(a.dest("rules/style.md")).unwrap(), RULE);
}

#[test]
fn recreated_link_with_identical_target_is_user_owned() {
    let p = Project::built();
    p.apply();
    let dest = p.dest("agents/reviewer.md");
    let target = fs::read_link(&dest).unwrap();
    let managed = fs::symlink_metadata(&dest).unwrap().ino();
    // Create the replacement before the old link is freed so the filesystem cannot reuse its inode.
    let replacement = p.dest("agents/replacement");
    symlink(&target, &replacement).unwrap();
    fs::rename(&replacement, &dest).unwrap();
    let user = fs::symlink_metadata(&dest).unwrap().ino();
    assert_ne!(user, managed);
    fs::remove_file(p.source("agents/reviewer.md")).unwrap();
    p.build();
    refuses_collision(&p, ConflictReason::OwnershipMismatch);
    assert_eq!(fs::read_link(&dest).unwrap(), target);
    assert_eq!(fs::symlink_metadata(&dest).unwrap().ino(), user);
}

#[test]
fn ledger_rejects_a_missing_or_malformed_link_identity() {
    let p = Project::built();
    p.apply();
    let ledger = p.ledger_json();
    for identity in [
        Value::Null,
        serde_json::json!([1]),
        serde_json::json!("1:2"),
    ] {
        let mut changed = ledger.clone();
        changed["harnesses"]["claude"]["user"]["entries"][0]["identity"] = identity;
        fs::write(p.ledger(), serde_json::to_vec(&changed).unwrap()).unwrap();
        assert!(matches!(
            p.plan_result(),
            Err(PlanError::Ledger(LedgerError::Invalid { .. }))
        ));
    }
}
