use std::io;

use serde::Serialize;

use super::{Observed, Plan};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyStatus {
    Unchanged,
    Done,
    Failed,
    NotRun,
    RolledBack,
    RollbackFailed,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum LedgerState {
    Unchanged,
    Committed,
    Pending,
    Unknown,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ApplyStep {
    Lock,
    Snapshot,
    Journal,
    Link,
    Commit,
}

#[derive(Debug, Serialize)]
pub struct OperationReport {
    pub destination: String,
    pub status: ApplyStatus,
    pub observed: Option<Observed>,
    pub rollback_error: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct ApplyReport {
    pub ledger: LedgerState,
    pub operations: Vec<OperationReport>,
    pub recovery_error: Option<String>,
}

#[derive(Debug, thiserror::Error)]
pub enum ApplyError {
    #[error("the plan has {count} conflict(s); resolve them and re-plan")]
    Conflicts { count: usize },
    #[error("managed symlink installation is unsupported on this platform")]
    Unsupported,
    #[error("managed symlink installation capability is unavailable: {source}")]
    CapabilityMissing {
        #[source]
        source: io::Error,
        report: Option<Box<ApplyReport>>,
    },
    #[error("the installation changed since planning ({detail}); re-plan before applying")]
    Changed { detail: String },
    #[error("ownership.lock is busy; retry after the other sync finishes")]
    Locked,
    #[error("commit durability unconfirmed (visible ledger: {ledger:?}): {source}")]
    CommitDurabilityUnconfirmed {
        ledger: LedgerState,
        #[source]
        source: io::Error,
        report: Box<ApplyReport>,
    },
    #[error("sync failed during {step:?}: {source}")]
    Failed {
        step: ApplyStep,
        #[source]
        source: io::Error,
        report: Box<ApplyReport>,
    },
}

impl ApplyError {
    pub fn report(&self) -> Option<&ApplyReport> {
        match self {
            Self::Failed { report, .. } | Self::CommitDurabilityUnconfirmed { report, .. } => {
                Some(report)
            }
            Self::CapabilityMissing { report, .. } => report.as_deref(),
            _ => None,
        }
    }

    pub fn exit(&self) -> crate::diagnostics::Exit {
        use crate::diagnostics::Exit;
        match self {
            Self::Conflicts { .. } | Self::Changed { .. } | Self::Locked => Exit::Findings,
            _ => Exit::Usage,
        }
    }
}

/// Apply a previously generated plan, rechecking its observations before any link mutation.
pub fn apply(plan: &Plan) -> Result<ApplyReport, ApplyError> {
    if plan.has_conflicts() {
        return Err(ApplyError::Conflicts {
            count: plan.summary().conflict,
        });
    }
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        native::apply_with(plan, &mut |_| Ok(()))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        super::fsops::probe().map_err(|source| ApplyError::CapabilityMissing {
            source,
            report: None,
        })?;
        Err(ApplyError::Unsupported)
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
mod native {
    use std::collections::BTreeMap;
    use std::ffi::OsStr;
    use std::path::{Path, PathBuf};

    use super::*;
    use crate::install::actual::{
        Actual, Resolved, Roots, SnapshotCheck, check_quarantines, child, inspect_snapshot,
        node_identity, observe, private_layer,
    };
    use crate::install::fsops::{Fs, Point, QuarantineError, temp_name};
    use crate::install::ledger::{self, Entry, Intent, Pending, Quarantine, User};
    use crate::install::plan::{OperationKind, SnapshotStatus, digest_for, old_target};
    use crate::install::{Key, Kind, Layer};
    use crate::{manifest, stage::pinned::Root};

    pub(in crate::install) fn apply_with(
        plan: &Plan,
        inject: &mut dyn FnMut(Point) -> io::Result<()>,
    ) -> Result<ApplyReport, ApplyError> {
        let roots = preflight(plan)?;
        let mut fs = Fs(inject);
        let mut report = ApplyReport {
            ledger: LedgerState::Unchanged,
            operations: plan
                .operations()
                .iter()
                .map(|op| OperationReport {
                    destination: op.destination.clone(),
                    status: if op.operation == OperationKind::Noop {
                        ApplyStatus::Unchanged
                    } else {
                        ApplyStatus::NotRun
                    },
                    observed: Some(op.observed.clone()),
                    rollback_error: None,
                })
                .collect(),
            recovery_error: None,
        };
        let retirements: Vec<_> = plan
            .operations()
            .iter()
            .map(|op| {
                old_target(op)
                    .filter(|_| op.operation != OperationKind::Noop)
                    .map(|target| Quarantine {
                        name: temp_name("retired-link"),
                        target: target.to_owned(),
                        identity: Some(plan.actual.node_ids[&op.key()]),
                    })
            })
            .collect();
        let (journal, final_user) = users(plan, &retirements);
        let restore = plan
            .snapshots()
            .iter()
            .any(|snapshot| snapshot.status == SnapshotStatus::Materialize);
        let ledger_update = plan
            .operations()
            .iter()
            .any(|op| op.operation != OperationKind::Noop)
            || plan.ledger.user.as_ref().is_none_or(|old| {
                old.applied != final_user.applied || old.entries != final_user.entries
            });
        if !restore && !ledger_update {
            return Ok(report);
        }

        let state = match roots.state {
            Some(state) => state,
            None => match fs.path(&plan.paths.local.join("state")) {
                Ok(state) => state,
                Err(err) => return Err(failure(ApplyStep::Lock, err, report)),
            },
        };
        let _lock = match fs.lock(&state) {
            Ok(lock) => lock,
            Err(err) if err.kind() == io::ErrorKind::WouldBlock => return Err(ApplyError::Locked),
            Err(err) => return Err(failure(ApplyStep::Lock, err, report)),
        };
        let roots = preflight(plan)?;
        if !same_dir(Some(&state), roots.state.as_ref()).map_err(changed)? {
            return Err(changed("state directory replaced"));
        }

        for snapshot in plan
            .snapshots()
            .iter()
            .filter(|snapshot| snapshot.status == SnapshotStatus::Materialize)
        {
            let key = Key {
                kind: snapshot.kind,
                name: snapshot.name.clone(),
            };
            let resource = &plan.desired.resources[&key];
            if let Err(err) = fs.snapshot(&state, &key, resource) {
                return Err(failure(ApplyStep::Snapshot, err, report));
            }
        }
        if let Err(err) = verify_snapshots(plan, &state) {
            return Err(failure(ApplyStep::Snapshot, err, report));
        }
        if !ledger_update {
            return Ok(report);
        }

        let old = plan.ledger.digest.as_deref();
        let (journal_bytes, final_bytes) = match plan
            .ledger
            .encoded(&journal)
            .and_then(|journal| Ok((journal, plan.ledger.encoded(&final_user)?)))
        {
            Ok(bytes) => bytes,
            Err(err) => return Err(failure(ApplyStep::Journal, err, report)),
        };
        let journal_digest = manifest::sha256(&journal_bytes);
        if let Err(err) = fs.persist(&state, old, Some(&journal_bytes), Point::Journal) {
            if err.disputed {
                report.ledger = visible_ledger(&state, &journal_digest, None, old);
            } else {
                restore_ledger(plan, &state, &mut fs, &journal_digest, &mut report);
            }
            return Err(failure(ApplyStep::Journal, err.source, report));
        }
        report.ledger = LedgerState::Pending;

        let mut links = Links {
            root: roots.claude,
            parents: BTreeMap::new(),
            undo: Vec::new(),
            retirements: &retirements,
        };
        if let Err(err) = links.run(plan, &mut fs, &mut report) {
            recover(
                plan,
                &state,
                &mut fs,
                &mut links,
                &journal,
                &journal_digest,
                &mut report,
            );
            return Err(failure(ApplyStep::Link, err, report));
        }
        if let Err(err) = links.verify(plan, &state, &journal) {
            if matches!(
                err.get_ref()
                    .and_then(|cause| cause.downcast_ref::<super::super::PlanError>()),
                Some(super::super::PlanError::Quarantine { .. })
            ) {
                return Err(failure(ApplyStep::Commit, err, report));
            }
            recover(
                plan,
                &state,
                &mut fs,
                &mut links,
                &journal,
                &journal_digest,
                &mut report,
            );
            return Err(failure(ApplyStep::Commit, err, report));
        }
        let final_digest = manifest::sha256(&final_bytes);
        if let Err(err) = fs.persist(
            &state,
            Some(&journal_digest),
            Some(&final_bytes),
            Point::Commit,
        ) {
            // Re-establish the journal before undoing links if the commit was published.
            let restore = if err.published {
                fs.persist(
                    &state,
                    Some(&final_digest),
                    Some(&journal_bytes),
                    Point::RestoreLedger,
                )
                .err()
                .map(|restore| restore.source.to_string())
            } else if err.disputed {
                Some(err.source.to_string())
            } else {
                None
            };
            if let Some(restore) = restore {
                report.ledger = visible_ledger(&state, &journal_digest, Some(&final_digest), old);
                report.recovery_error = Some(restore);
                return Err(ApplyError::CommitDurabilityUnconfirmed {
                    ledger: report.ledger,
                    source: err.source,
                    report: Box::new(report),
                });
            }
            recover(
                plan,
                &state,
                &mut fs,
                &mut links,
                &journal,
                &journal_digest,
                &mut report,
            );
            return Err(failure(ApplyStep::Commit, err.source, report));
        }
        report.ledger = LedgerState::Committed;
        Ok(report)
    }

    fn preflight(plan: &Plan) -> Result<Roots, ApplyError> {
        if Resolved::load(&plan.locations).map_err(changed)? != plan.paths {
            return Err(changed("Claude config or local root changed"));
        }
        let roots = plan.paths.open().map_err(changed)?;
        let digest = ledger::read_bytes(roots.state.as_ref())
            .map_err(changed)?
            .as_deref()
            .map(manifest::sha256);
        if digest != plan.ledger.digest {
            return Err(changed("ownership ledger bytes differ"));
        }
        let actual =
            Actual::inspect(&plan.paths, &roots, &plan.desired, &plan.ledger).map_err(changed)?;
        if actual != plan.actual {
            return Err(changed(
                "destinations, private resources or snapshots differ",
            ));
        }
        Ok(roots)
    }

    fn changed(detail: impl ToString) -> ApplyError {
        ApplyError::Changed {
            detail: detail.to_string(),
        }
    }

    fn failure(step: ApplyStep, source: io::Error, report: ApplyReport) -> ApplyError {
        if source.kind() == io::ErrorKind::Unsupported {
            ApplyError::CapabilityMissing {
                source,
                report: Some(Box::new(report)),
            }
        } else {
            ApplyError::Failed {
                step,
                source,
                report: Box::new(report),
            }
        }
    }

    fn users(plan: &Plan, retirements: &[Option<Quarantine>]) -> (User, User) {
        use OperationKind::{Conflict, Create, Noop, Remove, Update};
        let old: BTreeMap<_, _> = plan
            .ledger
            .entries()
            .map(|entry| (entry.key(), entry))
            .collect();
        let mut journal = User {
            root: plan.root().to_owned(),
            applied: plan
                .ledger
                .user
                .as_ref()
                .and_then(|user| user.applied.clone()),
            entries: Vec::new(),
        };
        let mut committed = User {
            root: journal.root.clone(),
            applied: Some(plan.desired.applied.clone()),
            entries: Vec::new(),
        };
        for (op, retirement) in plan.operations().iter().zip(retirements) {
            let existing = old.get(&op.key()).copied();
            let mut quarantines = existing
                .and_then(|entry| entry.pending.as_ref())
                .map_or_else(Vec::new, |pending| pending.quarantines.clone());
            quarantines.extend(retirement.clone());
            let wanted = op.source.as_ref().map(|_| {
                let layer = op.layer.expect("desired entry has a layer");
                Entry {
                    kind: op.kind,
                    name: op.name.clone(),
                    destination: op.destination.clone(),
                    link_target: op.link_target.clone().expect("desired entry has a target"),
                    layer,
                    kit: (layer == Layer::Team).then(|| plan.desired.applied.kit.clone()),
                    resource_sha256: digest_for(op, &plan.paths.local),
                    pending: None,
                }
            });
            if let Some(wanted) = &wanted {
                committed.entries.push(wanted.clone());
            }
            let pending = match op.operation {
                Create | Update => {
                    let mut wanted = wanted.expect("create and update have a desired entry");
                    wanted.pending = Some(Pending {
                        operation: if op.operation == Create {
                            Intent::Create
                        } else {
                            Intent::Update
                        },
                        previous_target: old_target(op).map(Path::to_owned),
                        quarantines,
                    });
                    Some(wanted)
                }
                Remove => {
                    let mut entry = existing.expect("removal requires ownership").clone();
                    // A retry may still find the old side of an interrupted update. Retain that proof.
                    if old_target(op)
                        .is_some_and(|target| super::super::same_target(target, &entry.link_target))
                    {
                        entry.pending = Some(Pending {
                            operation: Intent::Remove,
                            previous_target: None,
                            quarantines,
                        });
                    } else {
                        entry
                            .pending
                            .as_mut()
                            .expect("removing a previous target")
                            .quarantines = quarantines;
                    }
                    Some(entry)
                }
                Noop => existing.cloned(),
                Conflict => unreachable!("conflict plans have already been refused"),
            };
            journal.entries.extend(pending);
        }
        (journal, committed)
    }

    fn same_dir(left: Option<&Root>, right: Option<&Root>) -> io::Result<bool> {
        match (left, right) {
            (None, None) => Ok(true),
            (Some(left), Some(right)) => {
                let a = rustix::fs::fstat(left)?;
                let b = rustix::fs::fstat(right)?;
                Ok((a.st_dev, a.st_ino) == (b.st_dev, b.st_ino))
            }
            _ => Ok(false),
        }
    }

    fn verify_snapshots(plan: &Plan, state: &Root) -> io::Result<()> {
        for snapshot in plan.snapshots() {
            let key = Key {
                kind: snapshot.kind,
                name: snapshot.name.clone(),
            };
            if inspect_snapshot(Some(state), &key, &snapshot.resource_sha256)?
                != SnapshotCheck::Verified
            {
                return Err(io::Error::other(format!(
                    "snapshot changed at {}",
                    snapshot.path.display()
                )));
            }
        }
        Ok(())
    }

    struct Links<'a> {
        retirements: &'a [Option<Quarantine>],
        root: Option<Root>,
        parents: BTreeMap<Kind, Root>,
        undo: Vec<(usize, Undo)>,
    }

    enum Undo {
        Delete(Quarantine),
        Restore(PathBuf),
    }

    impl Links<'_> {
        fn run(
            &mut self,
            plan: &Plan,
            fs: &mut Fs<'_>,
            report: &mut ApplyReport,
        ) -> io::Result<()> {
            for (index, op) in plan.operations().iter().enumerate() {
                if op.operation == OperationKind::Noop {
                    continue;
                }
                if let Err(err) = self.execute(plan, index, fs) {
                    let item = &mut report.operations[index];
                    if err
                        .get_ref()
                        .is_some_and(|cause| cause.is::<QuarantineError>())
                    {
                        item.status = ApplyStatus::RollbackFailed;
                        item.rollback_error = Some(err.to_string());
                    } else {
                        item.status = ApplyStatus::Failed;
                    }
                    return Err(io::Error::new(
                        err.kind(),
                        format!("{}: {err}", op.destination),
                    ));
                }
                report.operations[index].status = ApplyStatus::Done;
                report.operations[index].observed = Some(expected(op));
            }
            Ok(())
        }

        fn execute(&mut self, plan: &Plan, index: usize, fs: &mut Fs<'_>) -> io::Result<()> {
            let op = &plan.operations()[index];
            if self.root.is_none() {
                self.root = Some(fs.path(plan.root())?);
            }
            let root = self.root.as_ref().expect("Claude root opened");
            if let std::collections::btree_map::Entry::Vacant(entry) = self.parents.entry(op.kind) {
                entry.insert(fs.dir(root, OsStr::new(op.kind.directory()))?);
            }
            let parent = &self.parents[&op.kind];
            let leaf = op.key().leaf();
            if observe(Some(parent), &leaf)? != op.observed {
                return Err(io::Error::other("destination changed after planning"));
            }
            if let Some(item) = &self.retirements[index] {
                let point = if op.operation == OperationKind::Remove {
                    Point::Remove
                } else {
                    Point::Replace
                };
                fs.retire(parent, &leaf, item, point)?;
                self.undo.push((index, Undo::Restore(item.target.clone())));
            }
            if let Some(wanted) = op.link_target.as_deref().filter(|_| op.source.is_some()) {
                fs.link(parent, &leaf, wanted, Point::Create)?;
                let identity = node_identity(parent, &leaf)
                    .and_then(|id| id.ok_or_else(|| io::Error::other("created link disappeared")));
                self.undo.push((
                    index,
                    Undo::Delete(Quarantine {
                        name: temp_name("retired-link"),
                        target: wanted.to_owned(),
                        identity: identity.as_ref().ok().copied(),
                    }),
                ));
                identity?;
            }
            fs.sync(parent)
        }

        fn verify(&self, plan: &Plan, state: &Root, journal: &User) -> io::Result<()> {
            if Resolved::load(&plan.locations).map_err(io::Error::other)? != plan.paths {
                return Err(io::Error::other("Claude config or local root changed"));
            }
            let roots = plan.paths.open().map_err(io::Error::other)?;
            if !same_dir(Some(state), roots.state.as_ref())?
                || !same_dir(self.root.as_ref(), roots.claude.as_ref())?
            {
                return Err(io::Error::other(
                    "managed directory was replaced during apply",
                ));
            }
            if private_layer(roots.local.as_ref())?.0 != plan.actual.private {
                return Err(io::Error::other("private resources changed during apply"));
            }
            for kind in Kind::ALL {
                let mut operations = plan
                    .operations()
                    .iter()
                    .filter(|op| op.kind == kind)
                    .peekable();
                if operations.peek().is_none() {
                    continue;
                }
                let parent = child(roots.claude.as_ref(), kind.directory())?;
                if let Some(parent) = &parent {
                    for entry in journal.entries.iter().filter(|entry| entry.kind == kind) {
                        check_quarantines(parent, plan.root(), entry).map_err(io::Error::other)?;
                    }
                }
                for op in operations {
                    if observe(parent.as_ref(), &op.key().leaf())? != expected(op) {
                        return Err(io::Error::other(format!(
                            "{} changed during apply",
                            op.destination
                        )));
                    }
                }
            }
            verify_snapshots(plan, state)
        }

        fn record_rollback(&self, plan: &Plan, journal: &mut User) -> bool {
            let quarantines: BTreeMap<_, _> = self
                .undo
                .iter()
                .filter_map(|(index, undo)| match undo {
                    Undo::Delete(item) if item.identity.is_some() => {
                        Some((plan.operations()[*index].destination.as_str(), item))
                    }
                    _ => None,
                })
                .collect();
            for entry in &mut journal.entries {
                if let Some(item) = quarantines.get(entry.destination.as_str()) {
                    entry
                        .pending
                        .as_mut()
                        .expect("created link has a pending entry")
                        .quarantines
                        .push((*item).clone());
                }
            }
            !quarantines.is_empty()
        }

        fn rollback(
            &mut self,
            plan: &Plan,
            fs: &mut Fs<'_>,
            report: &mut ApplyReport,
            journaled: bool,
        ) -> bool {
            let mut clean = report
                .operations
                .iter()
                .all(|item| item.status != ApplyStatus::RollbackFailed);
            for (index, undo) in self.undo.drain(..).rev() {
                let op = &plan.operations()[index];
                let parent = &self.parents[&op.kind];
                let leaf = op.key().leaf();
                let result = match undo {
                    Undo::Delete(item) if journaled && item.identity.is_some() => {
                        fs.retire(parent, &leaf, &item, Point::Rollback)
                    }
                    Undo::Delete(_) => {
                        Err(io::Error::other("rollback link identity was not journaled"))
                    }
                    Undo::Restore(target) => fs.link(parent, &leaf, &target, Point::Rollback),
                }
                .and_then(|()| fs.sync(parent));
                let item = &mut report.operations[index];
                match result {
                    Ok(()) if item.status == ApplyStatus::Done => {
                        item.status = ApplyStatus::RolledBack
                    }
                    Ok(()) => {}
                    Err(err) => {
                        clean = false;
                        item.status = ApplyStatus::RollbackFailed;
                        item.rollback_error = Some(match item.rollback_error.take() {
                            Some(previous) => format!("{previous}; {err}"),
                            None => err.to_string(),
                        });
                    }
                }
            }
            for (op, item) in plan.operations().iter().zip(&mut report.operations) {
                item.observed = child(self.root.as_ref(), op.kind.directory())
                    .and_then(|parent| observe(parent.as_ref(), &op.key().leaf()))
                    .ok();
            }
            clean
        }
    }

    fn expected(op: &super::super::plan::Operation) -> Observed {
        if op.source.is_some() {
            Observed::Symlink {
                target: op.link_target.clone().expect("desired link has a target"),
            }
        } else {
            Observed::Absent
        }
    }

    fn recover(
        plan: &Plan,
        state: &Root,
        fs: &mut Fs<'_>,
        links: &mut Links<'_>,
        journal: &User,
        journal_digest: &str,
        report: &mut ApplyReport,
    ) {
        let mut rollback_journal = journal.clone();
        let mut rollback_digest = None;
        let prepared = (|| -> io::Result<()> {
            if links.record_rollback(plan, &mut rollback_journal) {
                let bytes = plan.ledger.encoded(&rollback_journal)?;
                rollback_digest = Some(manifest::sha256(&bytes));
                fs.persist(state, Some(journal_digest), Some(&bytes), Point::Journal)
                    .map_err(|err| err.source)?;
            }
            Ok(())
        })();
        if let Err(err) = &prepared {
            report.recovery_error = Some(err.to_string());
        }
        let current_digest = rollback_digest.as_deref().unwrap_or(journal_digest);
        if links.rollback(plan, fs, report, prepared.is_ok()) {
            restore_ledger(plan, state, fs, current_digest, report);
        } else if !ledger_matches(state, Some(current_digest)).unwrap_or(false)
            && !ledger_matches(state, Some(journal_digest)).unwrap_or(false)
        {
            report.ledger = LedgerState::Unknown;
        }
    }

    fn visible_ledger(
        state: &Root,
        journal: &str,
        committed: Option<&str>,
        original: Option<&str>,
    ) -> LedgerState {
        match ledger::read_bytes(Some(state)) {
            Ok(bytes) => {
                let digest = bytes.as_deref().map(manifest::sha256);
                let digest = digest.as_deref();
                if committed.is_some() && digest == committed {
                    LedgerState::Committed
                } else if digest == Some(journal) {
                    LedgerState::Pending
                } else if digest == original {
                    LedgerState::Unchanged
                } else {
                    LedgerState::Unknown
                }
            }
            Err(_) => LedgerState::Unknown,
        }
    }

    fn ledger_matches(state: &Root, expected: Option<&str>) -> io::Result<bool> {
        Ok(ledger::read_bytes(Some(state))?
            .as_deref()
            .map(manifest::sha256)
            .as_deref()
            == expected)
    }

    fn restore_ledger(
        plan: &Plan,
        state: &Root,
        fs: &mut Fs<'_>,
        journal: &str,
        report: &mut ApplyReport,
    ) {
        let old = plan.ledger.digest.as_deref();
        let restored = (|| -> io::Result<()> {
            if ledger_matches(state, old)? {
                return Ok(());
            }
            fs.persist(
                state,
                Some(journal),
                plan.ledger.bytes.as_deref(),
                Point::RestoreLedger,
            )
            .map_err(|err| err.source)
        })();
        match restored {
            Ok(()) => report.ledger = LedgerState::Unchanged,
            Err(err) => {
                report.ledger = if ledger_matches(state, Some(journal)).unwrap_or(false) {
                    LedgerState::Pending
                } else {
                    LedgerState::Unknown
                };
                report.recovery_error = Some(err.to_string());
            }
        }
    }
}

#[cfg(all(test, any(target_os = "linux", target_os = "macos")))]
mod tests;
