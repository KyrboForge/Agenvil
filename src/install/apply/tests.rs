use std::fs;
use std::os::unix::fs::{MetadataExt, symlink};
use std::path::PathBuf;

use serde_json::Value;

use super::native::apply_with;
use super::*;
use crate::install::{ConflictReason, Locations, PlanError, Reason, fsops::Point, plan};
use crate::{build, config::Config, inventory::Inventory};

struct Fixture {
    _temp: tempfile::TempDir,
    root: PathBuf,
    out: PathBuf,
    locations: Locations,
}

impl Fixture {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let base = fs::canonicalize(temp.path()).unwrap();
        let root = base.join("project");
        fs::create_dir(&root).unwrap();
        fs::create_dir(root.join("rules")).unwrap();
        fs::write(
            root.join("agenvil.toml"),
            "schema = 1\n[project]\nname = \"fixture\"\n",
        )
        .unwrap();
        let fixture = Self {
            root,
            out: base.join("staging"),
            locations: Locations {
                claude_root: base.join("home/.claude"),
                local_root: base.join("home/.agents.local"),
            },
            _temp: temp,
        };
        fixture.rule("a", "old a");
        fixture.rule("z", "old z");
        fixture.build();
        fs::create_dir_all(fixture.destination("user").parent().unwrap()).unwrap();
        fs::write(fixture.destination("user"), "user file").unwrap();
        fixture
    }

    fn rule(&self, name: &str, text: &str) {
        fs::write(self.root.join("rules").join(format!("{name}.md")), text).unwrap();
    }

    fn build(&self) {
        let config = Config::load(&self.root).unwrap();
        let inventory = Inventory::build(&self.root, &config, &build::limits()).unwrap();
        build::claude(&self.root, &config, &inventory, &self.out).unwrap();
    }

    fn plan(&self) -> Plan {
        self.plan_result().unwrap()
    }

    fn plan_result(&self) -> Result<Plan, PlanError> {
        plan(
            &self.root,
            &Config::load(&self.root).unwrap(),
            &self.out,
            &self.locations,
        )
    }

    fn ledger(&self) -> PathBuf {
        self.locations.local_root.join("state/ownership.json")
    }

    fn user(&self) -> Value {
        let value: Value = serde_json::from_slice(&fs::read(self.ledger()).unwrap()).unwrap();
        value["harnesses"]["claude"]["user"].clone()
    }

    fn destination(&self, name: &str) -> PathBuf {
        self.locations
            .claude_root
            .join("rules")
            .join(format!("{name}.md"))
    }

    fn user_file_survives(&self) {
        assert_eq!(
            fs::read_to_string(self.destination("user")).unwrap(),
            "user file"
        );
    }

    fn update_a(&self) -> Vec<u8> {
        apply(&self.plan()).unwrap();
        let before = fs::read(self.ledger()).unwrap();
        self.rule("a", "new a");
        self.build();
        before
    }
}

fn injected() -> io::Error {
    io::Error::other("injected failure")
}

#[test]
fn symlink_creation_failure_rolls_back_prior_links() {
    let p = Fixture::new();
    let mut creates = 0;
    let failure = apply_with(&p.plan(), &mut |point| {
        if point == Point::Create {
            creates += 1;
            if creates == 2 {
                return Err(injected());
            }
        }
        Ok(())
    })
    .unwrap_err();
    let report = failure.report().unwrap();
    assert_eq!(report.ledger, LedgerState::Unchanged);
    assert_eq!(report.operations[0].status, ApplyStatus::RolledBack);
    assert_eq!(report.operations[1].status, ApplyStatus::Failed);
    assert!(!p.ledger().exists());
    assert!(!p.destination("a").is_symlink());
    assert!(!p.destination("z").is_symlink());
    p.user_file_survives();
}

#[test]
fn managed_link_replacement_failure_restores_the_old_target() {
    let p = Fixture::new();
    let before = p.update_a();
    let old = fs::read_link(p.destination("a")).unwrap();
    let mut retired = false;
    let failure = apply_with(&p.plan(), &mut |point| {
        if point == Point::Replace {
            retired = true;
        }
        if point == Point::Create && retired {
            assert!(!p.destination("a").is_symlink());
            return Err(injected());
        }
        Ok(())
    })
    .unwrap_err();
    assert_eq!(failure.report().unwrap().ledger, LedgerState::Unchanged);
    assert_eq!(fs::read_link(p.destination("a")).unwrap(), old);
    assert_eq!(fs::read_to_string(p.destination("a")).unwrap(), "old a");
    assert_eq!(fs::read(p.ledger()).unwrap(), before);
    p.user_file_survives();
}

#[test]
fn obsolete_link_removal_failure_rolls_back_an_earlier_update() {
    let p = Fixture::new();
    let before = p.update_a();
    fs::remove_file(p.root.join("rules/z.md")).unwrap();
    p.build();
    let failure = apply_with(&p.plan(), &mut |point| {
        if point == Point::Remove {
            Err(injected())
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    let report = failure.report().unwrap();
    assert_eq!(report.operations[0].status, ApplyStatus::RolledBack);
    assert_eq!(report.operations[1].status, ApplyStatus::Failed);
    assert_eq!(fs::read_to_string(p.destination("a")).unwrap(), "old a");
    assert_eq!(fs::read_to_string(p.destination("z")).unwrap(), "old z");
    assert_eq!(fs::read(p.ledger()).unwrap(), before);
    p.user_file_survives();
}

#[test]
fn journal_persistence_failure_never_mutates_links() {
    let p = Fixture::new();
    let failure = apply_with(&p.plan(), &mut |point| {
        if point == Point::Journal {
            Err(injected())
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert!(matches!(
        failure,
        ApplyError::Failed {
            step: ApplyStep::Journal,
            ..
        }
    ));
    assert!(!p.ledger().exists());
    assert!(!p.destination("a").is_symlink());
    assert!(!p.destination("z").is_symlink());
    p.user_file_survives();
}

#[test]
fn commit_persistence_failure_rolls_back_and_preserves_last_applied() {
    let p = Fixture::new();
    let before = p.update_a();
    let failure = apply_with(&p.plan(), &mut |point| {
        if point == Point::Commit {
            assert_eq!(fs::read_to_string(p.destination("a")).unwrap(), "new a");
            assert_eq!(p.user()["entries"][0]["pending"]["operation"], "update");
            return Err(injected());
        }
        Ok(())
    })
    .unwrap_err();
    assert!(matches!(
        failure,
        ApplyError::Failed {
            step: ApplyStep::Commit,
            ..
        }
    ));
    assert_eq!(failure.report().unwrap().ledger, LedgerState::Unchanged);
    assert_eq!(fs::read_to_string(p.destination("a")).unwrap(), "old a");
    assert_eq!(fs::read(p.ledger()).unwrap(), before);
    p.user_file_survives();
}

#[test]
fn directory_sync_failure_after_ledger_publication_revokes_the_commit() {
    let p = Fixture::new();
    let before = p.update_a();
    let mut syncs = 0;
    let failure = apply_with(&p.plan(), &mut |point| {
        if point == Point::LedgerSync {
            syncs += 1;
            if syncs == 2 {
                assert!(p.user()["entries"][0]["pending"].is_null());
                return Err(injected());
            }
        }
        Ok(())
    })
    .unwrap_err();
    assert_eq!(failure.report().unwrap().ledger, LedgerState::Unchanged);
    assert_eq!(fs::read(p.ledger()).unwrap(), before);
    assert_eq!(fs::read_to_string(p.destination("a")).unwrap(), "old a");
    p.user_file_survives();
}

#[test]
fn rollback_restoration_failure_leaves_a_pending_journal_and_can_retry() {
    let p = Fixture::new();
    p.update_a();
    let before = p.user()["applied"].clone();
    let failure = apply_with(&p.plan(), &mut |point| {
        if matches!(point, Point::Create | Point::Rollback) {
            Err(injected())
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    let report = failure.report().unwrap();
    assert_eq!(report.ledger, LedgerState::Pending);
    assert_eq!(report.operations[0].status, ApplyStatus::RollbackFailed);
    assert_eq!(report.operations[0].observed, Some(Observed::Absent));
    assert!(!p.destination("a").is_symlink());
    assert_eq!(p.user()["applied"], before);
    assert_eq!(p.user()["entries"][0]["pending"]["operation"], "update");
    p.user_file_survives();

    apply(&p.plan()).unwrap();
    assert_eq!(fs::read_to_string(p.destination("a")).unwrap(), "new a");
    assert_ne!(p.user()["applied"], before);
    assert!(p.user()["entries"][0]["pending"].is_null());
}

#[test]
fn rollback_quarantine_allows_retry_after_old_link_restore_fails() {
    let p = Fixture::new();
    p.update_a();
    let applied = p.user()["applied"].clone();
    let mut created = None;
    let failure = apply_with(&p.plan(), &mut |point| {
        if point == Point::Commit {
            created = Some(fs::symlink_metadata(p.destination("a"))?);
            return Err(injected());
        }
        if point == Point::Rollback && !p.destination("a").is_symlink() {
            return Err(injected());
        }
        Ok(())
    })
    .unwrap_err();
    let report = failure.report().unwrap();
    assert_eq!(report.ledger, LedgerState::Pending);
    assert_eq!(report.operations[0].status, ApplyStatus::RollbackFailed);
    assert_eq!(report.operations[0].observed, Some(Observed::Absent));
    assert!(!p.destination("a").exists());
    assert_eq!(p.user()["applied"], applied);
    retry_with_retained_rollback_link(&p, &created.unwrap());
}

#[test]
fn rollback_quarantine_allows_retry_after_ledger_restore_fails() {
    let p = Fixture::new();
    p.update_a();
    let applied = p.user()["applied"].clone();
    let old_target = fs::read_link(p.destination("a")).unwrap();
    let mut created = None;
    let failure = apply_with(&p.plan(), &mut |point| {
        if point == Point::Commit {
            created = Some(fs::symlink_metadata(p.destination("a"))?);
        }
        if matches!(point, Point::Commit | Point::RestoreLedger) {
            return Err(injected());
        }
        Ok(())
    })
    .unwrap_err();
    let report = failure.report().unwrap();
    assert_eq!(report.ledger, LedgerState::Pending);
    assert_eq!(report.operations[0].status, ApplyStatus::RolledBack);
    assert!(report.recovery_error.is_some());
    assert_eq!(fs::read_link(p.destination("a")).unwrap(), old_target);
    assert_eq!(fs::read_to_string(p.destination("a")).unwrap(), "old a");
    assert_eq!(p.user()["applied"], applied);
    retry_with_retained_rollback_link(&p, &created.unwrap());
}

fn retry_with_retained_rollback_link(p: &Fixture, created: &fs::Metadata) {
    let user = p.user();
    let pending = &user["entries"][0]["pending"];
    assert_eq!(pending["operation"], "update");
    let rollback = pending["quarantines"].as_array().unwrap().last().unwrap();
    let quarantine = p
        .destination("a")
        .with_file_name(rollback["name"].as_str().unwrap());
    let retained = fs::symlink_metadata(&quarantine).unwrap();
    assert!(retained.is_symlink());
    assert_eq!(
        (retained.dev(), retained.ino()),
        (created.dev(), created.ino())
    );
    assert_eq!(fs::read_to_string(&quarantine).unwrap(), "new a");
    assert_eq!(fs::read_to_string(p.destination("z")).unwrap(), "old z");
    p.user_file_survives();

    apply(&p.plan()).unwrap();
    assert_eq!(
        rollback["identity"],
        serde_json::json!([created.dev(), created.ino()])
    );
    assert!(p.user()["entries"][0]["pending"].is_null());
    assert_eq!(fs::read_to_string(p.destination("a")).unwrap(), "new a");
    assert_eq!(fs::read_to_string(&quarantine).unwrap(), "new a");
    p.user_file_survives();
}

#[test]
fn pending_create_does_not_claim_an_identical_user_link() {
    let p = Fixture::new();
    let plan = p.plan();
    let mut creates = 0;
    let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = apply_with(&plan, &mut |point| {
            if point == Point::Create {
                creates += 1;
                assert!(creates != 2, "simulated abrupt interruption");
            }
            Ok(())
        });
    }));
    assert!(interrupted.is_err());
    assert_eq!(fs::read_to_string(p.destination("a")).unwrap(), "old a");
    assert!(!p.destination("z").is_symlink());
    assert!(p.user()["applied"].is_null());
    assert_eq!(p.user()["entries"][0]["pending"]["operation"], "create");
    assert_eq!(p.user()["entries"][1]["pending"]["operation"], "create");
    p.user_file_survives();

    let user_target = p.user()["entries"][1]["link_target"]
        .as_str()
        .unwrap()
        .to_owned();
    symlink(&user_target, p.destination("z")).unwrap();
    let retry = p.plan();
    assert!(
        retry
            .operations()
            .iter()
            .all(|op| { op.reason == Reason::Conflict(ConflictReason::PendingCreate) })
    );
    assert!(matches!(
        apply(&retry),
        Err(ApplyError::Conflicts { count: 2 })
    ));
    assert_eq!(
        fs::read_link(p.destination("z")).unwrap(),
        PathBuf::from(user_target)
    );
    fs::rename(p.destination("z"), p.destination("saved-z")).unwrap();
    fs::rename(p.destination("a"), p.destination("saved-a")).unwrap();
    apply(&p.plan()).unwrap();
    assert_eq!(fs::read_to_string(p.destination("z")).unwrap(), "old z");
    assert!(p.user()["entries"][0]["pending"].is_null());
    assert_eq!(p.user()["applied"]["kit"], "fixture");
}

#[test]
fn node_raced_into_an_obsolete_destination_is_quarantined_and_restored() {
    let p = Fixture::new();
    apply(&p.plan()).unwrap();
    fs::remove_file(p.root.join("rules/z.md")).unwrap();
    p.build();
    let original = fs::read_link(p.destination("z")).unwrap();
    let failure = apply_with(&p.plan(), &mut |point| {
        if point == Point::Remove {
            fs::remove_file(p.destination("z"))?;
            fs::write(p.destination("z"), "replacement belongs to user")?;
        }
        Ok(())
    })
    .unwrap_err();
    assert_eq!(
        failure.report().unwrap().operations[1].observed,
        Some(Observed::File)
    );
    assert_eq!(
        fs::read_to_string(p.destination("z")).unwrap(),
        "replacement belongs to user"
    );
    assert_eq!(fs::read_to_string(original).unwrap(), "old z");
    p.user_file_survives();
}

#[test]
fn link_creation_never_overwrites_a_destination_that_appears_mid_apply() {
    let p = Fixture::new();
    let foreign = p.root.join("foreign");
    let failure = apply_with(&p.plan(), &mut |point| {
        if point == Point::Create {
            symlink(&foreign, p.destination("a"))?;
        }
        Ok(())
    })
    .unwrap_err();
    assert_eq!(
        failure.report().unwrap().operations[0].status,
        ApplyStatus::Failed
    );
    assert_eq!(fs::read_link(p.destination("a")).unwrap(), foreign);
    assert!(!p.destination("z").is_symlink());
    p.user_file_survives();
}

#[test]
fn raced_node_survives_when_quarantine_restore_fails() {
    let p = Fixture::new();
    apply(&p.plan()).unwrap();
    let applied = p.user()["applied"].clone();
    fs::remove_file(p.root.join("rules/z.md")).unwrap();
    p.build();
    let failure = apply_with(&p.plan(), &mut |point| {
        if point == Point::Remove {
            fs::remove_file(p.destination("z"))?;
            fs::write(p.destination("z"), "raced user file")?;
        } else if point == Point::Rollback {
            fs::write(p.destination("z"), "second user file")?;
        }
        Ok(())
    })
    .unwrap_err();
    let report = failure.report().unwrap();
    assert_eq!(report.ledger, LedgerState::Pending);
    assert_eq!(report.operations[1].status, ApplyStatus::RollbackFailed);
    let quarantine = fs::read_dir(p.destination("z").parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .find(|path| {
            path.file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with(".tmp-agenvil-retired-link-")
        })
        .unwrap();
    assert_eq!(fs::read_to_string(quarantine).unwrap(), "raced user file");
    assert_eq!(
        fs::read_to_string(p.destination("z")).unwrap(),
        "second user file"
    );
    assert_eq!(p.user()["applied"], applied);
    assert_eq!(p.user()["entries"][1]["pending"]["operation"], "remove");
    p.user_file_survives();
}

#[test]
fn disappearing_private_resource_aborts_commit_and_rolls_back_links() {
    let p = Fixture::new();
    let private = p.locations.local_root.join("rules/a.md");
    fs::create_dir_all(private.parent().unwrap()).unwrap();
    fs::write(&private, "private a").unwrap();
    let failure = apply_with(&p.plan(), &mut |point| {
        if point == Point::Create && private.exists() {
            fs::remove_file(&private)?;
        }
        Ok(())
    })
    .unwrap_err();
    assert!(matches!(
        failure,
        ApplyError::Failed {
            step: ApplyStep::Commit,
            ..
        }
    ));
    assert!(!p.destination("a").is_symlink());
    assert!(!p.destination("z").is_symlink());
    assert!(!p.ledger().exists());
    p.user_file_survives();
}

#[test]
fn replacement_can_fail_before_retirement_without_changing_the_old_link() {
    let p = Fixture::new();
    let before = p.update_a();
    let old = fs::read_link(p.destination("a")).unwrap();
    let failure = apply_with(&p.plan(), &mut |point| {
        if point == Point::Replace {
            Err(injected())
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(
        failure.report().unwrap().operations[0].status,
        ApplyStatus::Failed
    );
    assert_eq!(fs::read_link(p.destination("a")).unwrap(), old);
    assert_eq!(fs::read(p.ledger()).unwrap(), before);
    p.user_file_survives();
}

#[test]
fn commit_compare_and_swap_preserves_a_changed_journal() {
    let p = Fixture::new();
    p.update_a();
    let previous = p.user()["applied"].clone();
    let mut changed = Vec::new();
    let failure = apply_with(&p.plan(), &mut |point| {
        if point == Point::Commit {
            changed = fs::read(p.ledger())?;
            changed.extend_from_slice(b" \n");
            fs::write(p.ledger(), &changed)?;
        }
        Ok(())
    })
    .unwrap_err();
    let report = failure.report().unwrap();
    assert_eq!(report.ledger, LedgerState::Unknown);
    assert_eq!(report.operations[0].status, ApplyStatus::RollbackFailed);
    assert!(report.recovery_error.is_some());
    assert_eq!(fs::read(p.ledger()).unwrap(), changed);
    assert_eq!(p.user()["applied"], previous);
    assert_eq!(fs::read_to_string(p.destination("a")).unwrap(), "new a");
    p.user_file_survives();
}

#[test]
fn ledger_exchange_restores_replacement_after_digest_comparison() {
    let p = Fixture::new();
    p.update_a();
    let mut foreign = fs::read(p.ledger()).unwrap();
    foreign.extend(
        b"
  
",
    );
    let candidate = p.ledger().with_file_name("user-ledger");
    fs::write(&candidate, &foreign).unwrap();
    let failure = apply_with(&p.plan(), &mut |point| {
        if point == Point::LedgerCompared && candidate.exists() {
            fs::rename(&candidate, p.ledger())?;
        }
        Ok(())
    })
    .unwrap_err();

    assert!(matches!(
        failure,
        ApplyError::Failed {
            step: ApplyStep::Journal,
            ..
        }
    ));
    assert_eq!(fs::read(p.ledger()).unwrap(), foreign);
    assert_eq!(fs::read_to_string(p.destination("a")).unwrap(), "old a");
}

#[test]
fn ledger_removal_restores_replacement_after_digest_comparison() {
    let p = Fixture::new();
    let failure = apply_with(&p.plan(), &mut |point| {
        if point == Point::Create {
            return Err(injected());
        }
        if point == Point::LedgerCompared && p.ledger().exists() {
            let candidate = p.ledger().with_file_name("user-ledger");
            fs::write(&candidate, "user ledger")?;
            fs::rename(candidate, p.ledger())?;
        }
        Ok(())
    })
    .unwrap_err();

    assert_eq!(failure.report().unwrap().ledger, LedgerState::Unknown);
    assert_eq!(fs::read_to_string(p.ledger()).unwrap(), "user ledger");
    assert!(!p.destination("a").is_symlink());
}

#[test]
fn replacement_after_quarantine_verification_is_retained_and_reported() {
    let p = Fixture::new();
    apply(&p.plan()).unwrap();
    fs::remove_file(p.root.join("rules/z.md")).unwrap();
    p.build();
    let mut quarantined = None;
    let failure = apply_with(&p.plan(), &mut |point| {
        if point == Point::RetireVerified {
            let entry = &p.user()["entries"][1];
            let name = entry["pending"]["quarantines"][0]["name"].as_str().unwrap();
            let path = p.destination("z").with_file_name(name);
            fs::rename(&path, p.destination("saved-z"))?;
            fs::write(&path, "user replacement")?;
            quarantined = Some(path);
        }
        Ok(())
    })
    .unwrap_err();

    let quarantined = quarantined.unwrap();
    assert_eq!(failure.report().unwrap().ledger, LedgerState::Pending);
    assert_eq!(
        fs::read_to_string(&quarantined).unwrap(),
        "user replacement"
    );
    assert!(
        matches!(p.plan_result(), Err(PlanError::Quarantine { quarantine, destination, .. })
        if quarantine == quarantined && destination == p.destination("z"))
    );
}

#[test]
fn crash_after_retirement_reports_journaled_raced_object_on_retry() {
    let p = Fixture::new();
    apply(&p.plan()).unwrap();
    fs::remove_file(p.root.join("rules/z.md")).unwrap();
    p.build();
    let interruption = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = apply_with(&p.plan(), &mut |point| {
            if point == Point::Remove {
                fs::rename(p.destination("z"), p.destination("saved-z"))?;
                fs::write(p.destination("z"), "raced user file")?;
            }
            assert_ne!(point, Point::Quarantined, "simulated abrupt interruption");
            Ok(())
        });
    }));
    assert!(interruption.is_err());
    let entry = &p.user()["entries"][1];
    let name = entry["pending"]["quarantines"][0]["name"].as_str().unwrap();
    let path = p.destination("z").with_file_name(name);
    assert_eq!(fs::read_to_string(&path).unwrap(), "raced user file");
    assert!(
        matches!(p.plan_result(), Err(PlanError::Quarantine { quarantine, destination, .. })
        if quarantine == path && destination == p.destination("z"))
    );
    fs::rename(path, p.destination("recovered-z")).unwrap();
    apply(&p.plan()).unwrap();
    assert_eq!(
        fs::read_to_string(p.destination("recovered-z")).unwrap(),
        "raced user file"
    );
}

#[test]
fn failed_sync_and_journal_restore_report_visible_commit_and_retry_is_noop() {
    let p = Fixture::new();
    p.update_a();
    let mut syncs = 0;
    let failure = apply_with(&p.plan(), &mut |point| {
        if point == Point::LedgerSync {
            syncs += 1;
        }
        if (point == Point::LedgerSync && syncs == 2) || point == Point::RestoreLedger {
            return Err(injected());
        }
        Ok(())
    })
    .unwrap_err();

    assert!(matches!(
        failure,
        ApplyError::CommitDurabilityUnconfirmed {
            ledger: LedgerState::Committed,
            ..
        }
    ));
    assert!(failure.report().unwrap().recovery_error.is_some());
    assert_eq!(fs::read_to_string(p.destination("a")).unwrap(), "new a");
    assert!(p.user()["entries"][0]["pending"].is_null());
    let retry = p.plan();
    assert_eq!(retry.summary().noop, retry.operations().len());
    assert_eq!(apply(&retry).unwrap().ledger, LedgerState::Unchanged);
}

#[test]
fn interrupted_retirement_of_verified_link_converges_on_retry() {
    let p = Fixture::new();
    apply(&p.plan()).unwrap();
    fs::remove_file(p.root.join("rules/z.md")).unwrap();
    p.build();
    let target = fs::read_link(p.destination("z")).unwrap();
    let interrupted = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _ = apply_with(&p.plan(), &mut |point| {
            assert_ne!(point, Point::Quarantined, "simulated abrupt interruption");
            Ok(())
        });
    }));
    assert!(interrupted.is_err());
    let user = p.user();
    let name = user["entries"][1]["pending"]["quarantines"][0]["name"]
        .as_str()
        .unwrap();
    let quarantine = p.destination("z").with_file_name(name);
    assert_eq!(fs::read_link(&quarantine).unwrap(), target);
    apply(&p.plan()).unwrap();
    assert_eq!(p.user()["entries"].as_array().unwrap().len(), 1);
    assert_eq!(fs::read_link(quarantine).unwrap(), target);
    assert_eq!(p.plan().summary().noop, 1);
}

#[test]
fn interrupted_uninstall_recovers_on_rerun() {
    use crate::install::lifecycle::{pending_operations, uninstall_apply, uninstall_plan};

    let p = Fixture::new();
    apply(&p.plan()).unwrap();
    let uninstall = uninstall_plan(&p.locations).unwrap();
    let failure = apply_with(uninstall.links(), &mut |point| {
        if matches!(point, Point::Commit | Point::Rollback) {
            Err(injected())
        } else {
            Ok(())
        }
    })
    .unwrap_err();
    assert_eq!(failure.report().unwrap().ledger, LedgerState::Pending);
    let pending = pending_operations(&p.locations).unwrap();
    assert_eq!(pending.links.len(), 2);
    assert!(!pending.settings);
    let retry = uninstall_plan(&p.locations).unwrap();
    uninstall_apply(&retry).unwrap();
    assert!(pending_operations(&p.locations).unwrap().is_empty());
    assert!(p.user().is_null());
    assert!(fs::symlink_metadata(p.destination("a")).is_err());
    assert!(fs::symlink_metadata(p.destination("z")).is_err());
    assert_eq!(
        fs::read_dir(p.destination("a").parent().unwrap())
            .unwrap()
            .count(),
        1
    );
    p.user_file_survives();
}

#[test]
fn uninstall_preserves_and_reports_a_replaced_retirement() {
    use crate::install::lifecycle::uninstall_plan;

    let p = Fixture::new();
    apply(&p.plan()).unwrap();
    let plan = uninstall_plan(&p.locations).unwrap();
    let mut quarantine = None;
    let failure = apply_with(plan.links(), &mut |point| {
        if point == Point::Commit {
            let user = p.user();
            let name = user["entries"][0]["pending"]["quarantines"][0]["name"]
                .as_str()
                .unwrap();
            let path = p.destination("a").with_file_name(name);
            fs::remove_file(&path)?;
            fs::write(&path, "concurrent user file")?;
            quarantine = Some(path);
        }
        Ok(())
    })
    .unwrap_err();

    let path = quarantine.unwrap();
    assert!(failure.to_string().contains(path.to_str().unwrap()));
    assert_eq!(failure.report().unwrap().ledger, LedgerState::Committed);
    assert_eq!(fs::read_to_string(path).unwrap(), "concurrent user file");
}
