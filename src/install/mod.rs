//! Reconcile an approved Claude staging tree with a user's managed symlinks.

mod actual;
mod apply;
mod desired;
mod fsops;
mod ledger;
mod plan;

pub use apply::{
    ApplyError, ApplyReport, ApplyStatus, ApplyStep, LedgerState, OperationReport, apply,
};
pub use ledger::LedgerError;
pub use plan::{
    ConflictReason, Ignored, IgnoredReason, Observed, Operation, OperationKind, Ownership, Plan,
    Reason, ReconcileReason, Snapshot, SnapshotStatus, Summary, plan,
};

use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{build, discovery::OverlapError, manifest, stage};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Locations {
    pub claude_root: PathBuf,
    pub local_root: PathBuf,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Harness {
    Claude,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Scope {
    User,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Agent,
    Skill,
    Rule,
}

impl Kind {
    const ALL: [Self; 3] = [Self::Agent, Self::Skill, Self::Rule];

    fn directory(self) -> &'static str {
        match self {
            Self::Agent => "agents",
            Self::Skill => "skills",
            Self::Rule => "rules",
        }
    }

    fn from_directory(directory: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|kind| kind.directory() == directory)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Layer {
    Team,
    Private,
}

#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Key {
    kind: Kind,
    name: String,
}

impl Key {
    fn from_filename(kind: Kind, leaf: &str) -> Option<Self> {
        let name = match kind {
            Kind::Skill => leaf,
            Kind::Agent | Kind::Rule => leaf.strip_suffix(".md")?,
        };
        let key = Self {
            kind,
            name: name.to_owned(),
        };
        key.is_portable().then_some(key)
    }

    fn is_portable(&self) -> bool {
        !self.name.is_empty()
            && !self.name.starts_with('.')
            && !self.name.contains('/')
            && manifest::path_flaw(&self.name).is_none()
            && manifest::path_flaw(&self.leaf()).is_none()
    }

    fn leaf(&self) -> String {
        match self.kind {
            Kind::Skill => self.name.clone(),
            Kind::Agent | Kind::Rule => format!("{}.md", self.name),
        }
    }

    fn destination(&self) -> String {
        format!("{}/{}", self.kind.directory(), self.leaf())
    }

    fn private_path(&self, local: &Path) -> PathBuf {
        local.join(self.destination())
    }

    fn snapshot_path(&self, local: &Path, digest: &str) -> PathBuf {
        local.join("state/snapshots").join(digest).join(self.leaf())
    }
}

#[derive(Debug, thiserror::Error)]
pub enum PlanError {
    #[error(transparent)]
    Build(#[from] build::Error),
    #[error(transparent)]
    Inventory(#[from] OverlapError),
    #[error("staging tree at {} needs a clean `agenvil build --target claude --check` before sync", out.display())]
    Staging {
        out: PathBuf,
        findings: Vec<stage::Finding>,
    },
    #[error("unsafe installation location {} ({reason})", path.display())]
    UnsafePath { path: PathBuf, reason: &'static str },
    #[error("invalid staged resource path `{0}`")]
    Resource(String),
    #[error(transparent)]
    Ledger(#[from] LedgerError),
    #[error(
        "installed kit {installed:?} differs from requested kit {requested:?}; kit switching is unsupported"
    )]
    KitMismatch {
        installed: String,
        requested: String,
    },
    #[error("retirement journal could not verify {}; preserve it and resolve its original destination {}", quarantine.display(), destination.display())]
    Quarantine {
        quarantine: PathBuf,
        destination: PathBuf,
        #[source]
        source: Option<io::Error>,
    },
    #[error("could not inspect {}: {source}", path.display())]
    Inspect {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
}

impl PlanError {
    fn inspect(path: &Path, source: io::Error) -> Self {
        if source.kind() == io::ErrorKind::NotADirectory {
            return Self::UnsafePath {
                path: path.to_owned(),
                reason: "symlink or non-directory ancestor",
            };
        }
        Self::Inspect {
            path: path.to_owned(),
            source,
        }
    }

    pub fn exit(&self) -> crate::diagnostics::Exit {
        use crate::diagnostics::Exit;
        match self {
            Self::Staging { findings, .. } if !stage::is_unsafe(findings) => Exit::Findings,
            Self::Build(error) => error.exit(),
            _ => Exit::Usage,
        }
    }
}

fn same_target(left: &Path, right: &Path) -> bool {
    left.as_os_str() == right.as_os_str()
}

fn is_digest(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}
