use std::collections::BTreeMap;
use std::fmt;
use std::path::{Path, PathBuf};

use serde::{Serialize, Serializer};

use super::actual::{Actual, Resolved, SnapshotCheck, overlaps};
use super::desired::Desired;
use super::ledger::Ledger;
use super::{Harness, Kind, Layer, Locations, PlanError, Scope, same_target};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use super::{Key, ledger::target_digest};
use crate::config::Config;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum OperationKind {
    Create,
    Update,
    Remove,
    Noop,
    Conflict,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Ownership {
    Managed,
    Unmanaged,
    Absent,
    Mismatch,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ConflictReason {
    UserOwned,
    OwnershipMismatch,
    UnsafeAncestor,
    CorruptSnapshot,
    BrokenManagedTarget,
    CaseCollision,
    CaseAlias,
    PendingCreate,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(untagged)]
pub enum Reason {
    Conflict(ConflictReason),
    Reconcile(ReconcileReason),
}

impl fmt::Display for Reason {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        use ConflictReason as Conflict;
        use ReconcileReason as Reconcile;
        f.write_str(match self {
            Self::Conflict(Conflict::UserOwned) => "destination is user-owned",
            Self::Conflict(Conflict::OwnershipMismatch) => {
                "destination differs from the ownership ledger"
            }
            Self::Conflict(Conflict::UnsafeAncestor) => {
                "destination parent is a symlink or non-directory"
            }
            Self::Conflict(Conflict::CorruptSnapshot) => "managed snapshot failed verification",
            Self::Conflict(Conflict::BrokenManagedTarget) => {
                "managed snapshot is missing and cannot be restored from staging"
            }
            Self::Conflict(Conflict::CaseCollision) => "desired names differ only in case",
            Self::Conflict(Conflict::CaseAlias) => {
                "destinations resolve to the same filesystem entry"
            }
            Self::Conflict(Conflict::PendingCreate) => {
                "pending create is occupied; ownership cannot be determined"
            }
            Self::Reconcile(Reconcile::Missing) => "destination is absent",
            Self::Reconcile(Reconcile::Changed) => "desired link target changed",
            Self::Reconcile(Reconcile::Obsolete) => "resource is no longer desired",
            Self::Reconcile(Reconcile::Current) => "link is current",
            Self::Reconcile(Reconcile::ManagedLinkRemoved) => "managed link was removed",
            Self::Reconcile(Reconcile::RestoreSnapshot) => {
                "restore the missing snapshot from staging"
            }
            Self::Reconcile(Reconcile::Pending) => "reconcile a pending journal entry",
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum ReconcileReason {
    Missing,
    Changed,
    Obsolete,
    Current,
    ManagedLinkRemoved,
    RestoreSnapshot,
    Pending,
}

#[derive(Clone, Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Observed {
    Absent,
    File,
    Directory,
    Symlink {
        #[serde(serialize_with = "serialize_link")]
        target: PathBuf,
    },
    Other,
    UnsafeAncestor,
}

fn serialize_link<S: Serializer>(path: &Path, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&path.to_string_lossy())
}

impl PartialEq for Observed {
    fn eq(&self, other: &Self) -> bool {
        match (self, other) {
            (Self::Symlink { target: left }, Self::Symlink { target: right }) => {
                same_target(left, right)
            }
            _ => std::mem::discriminant(self) == std::mem::discriminant(other),
        }
    }
}

impl Eq for Observed {}

#[derive(Clone, Debug, Serialize)]
pub struct Operation {
    pub operation: OperationKind,
    pub kind: Kind,
    pub name: String,
    pub layer: Option<Layer>,
    pub destination: String,
    pub source: Option<PathBuf>,
    pub link_target: Option<PathBuf>,
    pub ownership: Ownership,
    pub observed: Observed,
    pub reason: Reason,
}

impl Operation {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub(super) fn key(&self) -> Key {
        Key {
            kind: self.kind,
            name: self.name.clone(),
        }
    }
}

#[derive(Clone, Debug, Default, Serialize)]
pub struct Summary {
    pub create: usize,
    pub update: usize,
    pub remove: usize,
    pub noop: usize,
    pub conflict: usize,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum SnapshotStatus {
    Materialize,
    Verified,
    Corrupt,
    Missing,
}

#[derive(Clone, Debug, Serialize)]
pub struct Snapshot {
    pub kind: Kind,
    pub name: String,
    pub resource_sha256: String,
    pub path: PathBuf,
    pub status: SnapshotStatus,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum IgnoredReason {
    InvalidName,
    WrongType,
    MissingSkillFile,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Ignored {
    pub kind: Kind,
    pub path: String,
    pub reason: IgnoredReason,
}

#[derive(Serialize)]
pub struct Plan {
    schema: u32,
    harness: Harness,
    scope: Scope,
    root: PathBuf,
    kit: Option<String>,
    operations: Vec<Operation>,
    summary: Summary,
    snapshots: Vec<Snapshot>,
    ignored: Vec<Ignored>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[serde(skip)]
    pub(super) desired: Desired,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[serde(skip)]
    pub(super) ledger: Ledger,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[serde(skip)]
    pub(super) actual: Actual,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[serde(skip)]
    pub(super) paths: Resolved,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    #[serde(skip)]
    pub(super) locations: Locations,
}

impl Plan {
    pub fn root(&self) -> &Path {
        &self.root
    }
    pub fn operations(&self) -> &[Operation] {
        &self.operations
    }
    pub fn summary(&self) -> &Summary {
        &self.summary
    }
    pub fn snapshots(&self) -> &[Snapshot] {
        &self.snapshots
    }
    pub fn ignored(&self) -> &[Ignored] {
        &self.ignored
    }
    pub fn has_conflicts(&self) -> bool {
        self.summary.conflict != 0
    }
}

impl fmt::Debug for Plan {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Plan")
            .field("root", &self.root)
            .field("operations", &self.operations)
            .field("summary", &self.summary)
            .field("snapshots", &self.snapshots)
            .field("ignored", &self.ignored)
            .finish_non_exhaustive()
    }
}

/// Verify staging and inspect the local install, then reconcile the three inputs in memory.
pub fn plan(
    root: &Path,
    config: &Config,
    out: &Path,
    locations: &Locations,
) -> Result<Plan, PlanError> {
    let desired = Desired::load(root, config, out)?;
    let paths = Resolved::load(locations)?;
    let out = std::fs::canonicalize(out).map_err(|err| PlanError::inspect(out, err))?;
    if overlaps(&out, &paths.local) || overlaps(&out, &paths.claude) {
        return Err(PlanError::UnsafePath {
            path: out,
            reason: "staging and installation locations must be disjoint",
        });
    }
    let roots = paths.open()?;
    let ledger = Ledger::read(roots.state.as_ref(), &paths.local, &paths.claude)?;
    let requested = desired.applied.as_ref().expect("staging has provenance");
    if let Some((kit, project_root)) = ledger
        .user
        .as_ref()
        .and_then(|user| user.applied.as_ref())
        .map(|applied| (&applied.kit, &applied.project_root))
        .into_iter()
        .chain(
            ledger
                .entries()
                .filter_map(|entry| entry.kit.as_ref().zip(entry.project_root.as_ref())),
        )
        .find(|(kit, project_root)| {
            **kit != requested.kit || !same_target(project_root, &requested.project_root)
        })
    {
        return Err(PlanError::KitMismatch {
            installed: kit.clone(),
            installed_root: project_root.clone(),
            requested: requested.kit.clone(),
            requested_root: requested.project_root.clone(),
        });
    }
    reconcile_plan(locations, paths, &roots, desired, ledger, Some(&out))
}

pub(super) fn uninstall_plan(locations: &Locations) -> Result<Plan, PlanError> {
    let paths = Resolved::load(locations)?;
    let roots = paths.open()?;
    let ledger = Ledger::read(roots.state.as_ref(), &paths.local, &paths.claude)?;
    reconcile_plan(locations, paths, &roots, Desired::default(), ledger, None)
}

fn reconcile_plan(
    locations: &Locations,
    paths: Resolved,
    roots: &super::actual::Roots,
    desired: Desired,
    ledger: Ledger,
    out: Option<&Path>,
) -> Result<Plan, PlanError> {
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    let _ = locations;
    let actual = Actual::inspect(&paths, roots, &desired, &ledger)?;
    let (operations, summary, snapshots) = reconcile(&desired, &ledger, &actual, &paths, out);
    Ok(Plan {
        schema: 1,
        harness: Harness::Claude,
        scope: Scope::User,
        root: paths.claude.clone(),
        kit: desired
            .applied
            .as_ref()
            .or_else(|| ledger.user.as_ref().and_then(|user| user.applied.as_ref()))
            .map(|applied| applied.kit.clone()),
        operations,
        summary,
        snapshots,
        ignored: actual.ignored.clone(),
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        desired,
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        ledger,
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        actual,
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        paths,
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        locations: locations.clone(),
    })
}

fn reconcile(
    desired: &Desired,
    ledger: &Ledger,
    actual: &Actual,
    paths: &Resolved,
    out: Option<&Path>,
) -> (Vec<Operation>, Summary, Vec<Snapshot>) {
    use OperationKind::{Conflict, Create, Noop, Remove, Update};
    use ReconcileReason as Why;

    let old: BTreeMap<_, _> = ledger.entries().map(|entry| (entry.key(), entry)).collect();
    let snapshots: Vec<_> = actual
        .snapshots
        .iter()
        .map(|((key, digest), status)| {
            let status = match status {
                SnapshotCheck::Verified => SnapshotStatus::Verified,
                SnapshotCheck::Corrupt => SnapshotStatus::Corrupt,
                SnapshotCheck::Missing
                    if desired
                        .resources
                        .get(key)
                        .is_some_and(|resource| resource.digest == *digest) =>
                {
                    SnapshotStatus::Materialize
                }
                SnapshotCheck::Missing => SnapshotStatus::Missing,
            };
            Snapshot {
                kind: key.kind,
                name: key.name.clone(),
                resource_sha256: digest.clone(),
                path: key.snapshot_path(&paths.local, digest),
                status,
            }
        })
        .collect();
    let mut snapshot_conflicts = BTreeMap::new();
    let mut repair = BTreeMap::new();
    for snapshot in &snapshots {
        let key = (snapshot.kind, snapshot.name.as_str());
        match snapshot.status {
            SnapshotStatus::Corrupt => {
                snapshot_conflicts.insert(key, ConflictReason::CorruptSnapshot);
            }
            SnapshotStatus::Missing => {
                snapshot_conflicts
                    .entry(key)
                    .or_insert(ConflictReason::BrokenManagedTarget);
            }
            SnapshotStatus::Materialize => {
                repair.insert(key, snapshot.path.as_path());
            }
            SnapshotStatus::Verified => {}
        }
    }

    let mut case_names = BTreeMap::<_, usize>::new();
    for key in actual
        .nodes
        .keys()
        .filter(|key| desired.resources.contains_key(key) || actual.private.contains(key))
    {
        *case_names
            .entry((key.kind, key.name.to_lowercase()))
            .or_default() += 1;
    }

    let mut summary = Summary::default();
    let mut operations = Vec::new();
    for (key, observed) in &actual.nodes {
        let entry = old.get(key).copied();
        let connection = if actual.private.contains(key) {
            let path = key.private_path(&paths.local);
            Some((Layer::Private, path.clone(), path))
        } else {
            desired.resources.get(key).map(|resource| {
                (
                    Layer::Team,
                    out.expect("team resource has staging")
                        .join(".claude")
                        .join(key.destination()),
                    key.snapshot_path(&paths.local, &resource.digest),
                )
            })
        };
        let ownership = match (observed, entry) {
            (Observed::Absent, _) => Ownership::Absent,
            (_, Some(entry)) if entry.pending_create() => Ownership::Mismatch,
            (Observed::Symlink { target }, Some(entry))
                if entry.owns(target, actual.node_id(key)) =>
            {
                Ownership::Managed
            }
            (_, None) => Ownership::Unmanaged,
            (_, Some(_)) => Ownership::Mismatch,
        };
        let label = (key.kind, key.name.as_str());
        let conflict = match (observed, ownership) {
            (Observed::UnsafeAncestor, _) => Some(ConflictReason::UnsafeAncestor),
            _ if actual.aliases.contains(key) => Some(ConflictReason::CaseAlias),
            (_, Ownership::Mismatch) if entry.is_some_and(|entry| entry.pending_create()) => {
                Some(ConflictReason::PendingCreate)
            }
            (_, Ownership::Unmanaged) => Some(ConflictReason::UserOwned),
            (_, Ownership::Mismatch) => Some(ConflictReason::OwnershipMismatch),
            _ if connection.is_some() && case_names[&(key.kind, key.name.to_lowercase())] > 1 => {
                Some(ConflictReason::CaseCollision)
            }
            _ => snapshot_conflicts.get(&label).copied(),
        };
        let (operation, reason) = if let Some(reason) = conflict {
            (Conflict, Reason::Conflict(reason))
        } else {
            let (operation, reason) = match (&connection, observed) {
                (Some(_), Observed::Absent) if entry.is_some() => (Create, Why::ManagedLinkRemoved),
                (Some(_), Observed::Absent) => (Create, Why::Missing),
                (Some((_, _, wanted)), Observed::Symlink { target })
                    if same_target(wanted, target) =>
                {
                    let reason = if repair
                        .get(&label)
                        .is_some_and(|path| same_target(path, target))
                    {
                        Why::RestoreSnapshot
                    } else if entry.is_some_and(|entry| entry.pending.is_some()) {
                        Why::Pending
                    } else {
                        Why::Current
                    };
                    (Noop, reason)
                }
                (Some(_), _) => (Update, Why::Changed),
                (None, Observed::Absent) if desired.applied.is_none() && entry.is_some() => {
                    (Remove, Why::ManagedLinkRemoved)
                }
                (None, Observed::Absent) => (Noop, Why::ManagedLinkRemoved),
                (None, _) => (Remove, Why::Obsolete),
            };
            (operation, Reason::Reconcile(reason))
        };
        match operation {
            Create => summary.create += 1,
            Update => summary.update += 1,
            Remove => summary.remove += 1,
            Noop => summary.noop += 1,
            Conflict => summary.conflict += 1,
        }
        let (layer, source, link_target) = match connection {
            Some((layer, source, target)) => (Some(layer), Some(source), Some(target)),
            None => (
                entry.map(|entry| entry.layer),
                None,
                entry.map(|entry| {
                    if let Observed::Symlink { target } = observed
                        && entry.owns(target, actual.node_id(key))
                    {
                        target.clone()
                    } else {
                        entry.link_target.clone()
                    }
                }),
            ),
        };
        operations.push(Operation {
            operation,
            kind: key.kind,
            name: key.name.clone(),
            layer,
            destination: key.destination(),
            source,
            link_target,
            ownership,
            observed: observed.clone(),
            reason,
        });
    }
    (operations, summary, snapshots)
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn old_target(operation: &Operation) -> Option<&Path> {
    match &operation.observed {
        Observed::Symlink { target } if operation.ownership == Ownership::Managed => Some(target),
        _ => None,
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn digest_for(operation: &Operation, local: &Path) -> Option<String> {
    operation
        .link_target
        .as_deref()
        .and_then(|target| target_digest(&operation.key(), local, target))
        .map(str::to_owned)
}
