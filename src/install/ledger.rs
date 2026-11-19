use std::collections::BTreeMap;
use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::value::RawValue;

use super::{Key, Kind, Layer, is_digest, same_target};
use crate::{
    manifest,
    stage::{PathKind, pinned::Root},
};

pub(super) const FILE: &str = "ownership.json";
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) const LOCK: &str = "ownership.lock";
const SCHEMA: u32 = 1;

#[derive(Debug, thiserror::Error)]
pub enum LedgerError {
    #[error("cannot read {}: {source}; check the state directory and file permissions", path.display())]
    Read {
        path: PathBuf,
        #[source]
        source: io::Error,
    },
    #[error("invalid ownership ledger {} ({detail}); inspect or restore a trusted ledger before syncing", path.display())]
    Invalid { path: PathBuf, detail: String },
    #[error("ownership ledger {} uses schema {schema}; use an Agenvil version supporting that schema", path.display())]
    Schema { path: PathBuf, schema: u32 },
    #[error("ownership ledger {} belongs to {}; sync against that Claude config root", path.display(), recorded.display())]
    Root { path: PathBuf, recorded: PathBuf },
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Envelope {
    pub schema: u32,
    pub generator: manifest::Generator,
    pub harnesses: BTreeMap<String, Box<RawValue>>,
}

pub(super) type Scopes = BTreeMap<String, Box<RawValue>>;

pub(super) const RETIRED_PREFIX: &str = ".tmp-agenvil-retired-link-";
pub(super) type NodeId = (u64, u64);

pub(super) struct Ledger {
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    envelope: Envelope,
    pub user: Option<User>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub bytes: Option<Vec<u8>>,
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub digest: Option<String>,
}

#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct User {
    pub root: PathBuf,
    pub applied: Option<Applied>,
    pub entries: Vec<Entry>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Applied {
    pub kit: String,
    pub project_root: PathBuf,
    pub manifest_sha256: String,
    pub generator: manifest::Generator,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Entry {
    pub kind: Kind,
    pub name: String,
    pub destination: String,
    pub link_target: PathBuf,
    pub layer: Layer,
    pub kit: Option<String>,
    pub project_root: Option<PathBuf>,
    pub resource_sha256: Option<String>,
    pub identity: Option<NodeId>,
    pub pending: Option<Pending>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub enum Intent {
    #[serde(rename = "create")]
    Create,
    #[serde(rename = "update")]
    Update,
    #[serde(rename = "remove")]
    Remove,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Pending {
    pub operation: Intent,
    pub previous_target: Option<PathBuf>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub quarantines: Vec<Quarantine>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Quarantine {
    pub name: String,
    pub target: PathBuf,
    pub identity: Option<NodeId>,
}

impl Entry {
    pub fn key(&self) -> Key {
        Key {
            kind: self.kind,
            name: self.name.clone(),
        }
    }

    /// Raw target text alone is not ownership: the link must also be the recorded node.
    pub fn owns(&self, target: &Path, identity: Option<NodeId>) -> bool {
        let pending = self.pending.as_ref();
        let recorded = same_target(&self.link_target, target)
            || pending
                .and_then(|pending| pending.previous_target.as_deref())
                .is_some_and(|previous| same_target(previous, target));
        recorded
            && identity.is_some()
            && (self.identity == identity
                || pending.is_some_and(|pending| {
                    pending
                        .quarantines
                        .iter()
                        .any(|item| item.identity == identity && same_target(&item.target, target))
                }))
    }

    pub fn pending_create(&self) -> bool {
        self.pending
            .as_ref()
            .is_some_and(|pending| pending.operation == Intent::Create)
    }

    fn valid(&self, local: &Path) -> bool {
        let key = self.key();
        if !key.is_portable() || self.destination != key.destination() {
            return false;
        }
        let valid_target = match self.layer {
            Layer::Team => {
                self.kit.is_some()
                    && self
                        .project_root
                        .as_ref()
                        .is_some_and(|root| root.is_absolute())
                    && self.resource_sha256.as_ref().is_some_and(|digest| {
                        is_digest(digest)
                            && same_target(&self.link_target, &key.snapshot_path(local, digest))
                    })
            }
            Layer::Private => {
                self.kit.is_none()
                    && self.project_root.is_none()
                    && self.resource_sha256.is_none()
                    && same_target(&self.link_target, &key.private_path(local))
            }
        };
        // A settled link must carry the node identity that proves it is still the installed link.
        valid_target
            && (self.pending.is_some() || self.identity.is_some())
            && self.pending.as_ref().is_none_or(|pending| {
                pending.quarantines.iter().all(|item| {
                    item.name
                        .strip_prefix(RETIRED_PREFIX)
                        .is_some_and(|suffix| {
                            !suffix.is_empty()
                                && suffix
                                    .bytes()
                                    .all(|byte| byte.is_ascii_digit() || byte == b'-')
                        })
                        && (same_target(&item.target, &key.private_path(local))
                            || target_digest(&key, local, &item.target).is_some())
                })
            })
            && self
                .pending
                .as_ref()
                .is_none_or(|pending| match pending.operation {
                    Intent::Create | Intent::Remove => pending.previous_target.is_none(),
                    Intent::Update => pending.previous_target.as_deref().is_some_and(|previous| {
                        same_target(previous, &key.private_path(local))
                            || target_digest(&key, local, previous).is_some()
                    }),
                })
    }
}

pub(super) fn target_digest<'a>(key: &Key, local: &Path, target: &'a Path) -> Option<&'a str> {
    let digest = target.parent()?.file_name()?.to_str()?;
    (is_digest(digest) && same_target(target, &key.snapshot_path(local, digest))).then_some(digest)
}

impl Ledger {
    pub fn read(state: Option<&Root>, local: &Path, root: &Path) -> Result<Self, LedgerError> {
        let path = local.join("state").join(FILE);
        let invalid = |detail| LedgerError::Invalid {
            path: path.clone(),
            detail,
        };
        let bytes = read_bytes(state).map_err(|source| LedgerError::Read {
            path: path.clone(),
            source,
        })?;
        let Some(bytes) = bytes else {
            return Ok(Self {
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                envelope: Envelope {
                    schema: SCHEMA,
                    generator: generator(),
                    harnesses: BTreeMap::new(),
                },
                user: None,
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                bytes: None,
                #[cfg(any(target_os = "linux", target_os = "macos"))]
                digest: None,
            });
        };
        #[derive(Deserialize)]
        struct Header {
            schema: u32,
        }
        let bad_json = |err: serde_json::Error| {
            invalid(format!(
                "invalid schema or JSON at line {}, column {}",
                err.line(),
                err.column()
            ))
        };
        let header: Header = serde_json::from_slice(&bytes).map_err(bad_json)?;
        if header.schema != SCHEMA {
            return Err(LedgerError::Schema {
                path,
                schema: header.schema,
            });
        }
        let envelope: Envelope = serde_json::from_slice(&bytes).map_err(bad_json)?;
        let user = if let Some(claude) = envelope.harnesses.get("claude") {
            let scopes: Scopes = serde_json::from_str(claude.get())
                .map_err(|_| invalid("invalid Claude namespace".to_owned()))?;
            scopes
                .get("user")
                .map(|value| {
                let mut user: User = serde_json::from_str(value.get())
                        .map_err(|_| invalid("invalid fields in the Claude user namespace".to_owned()))?;
                    if !user.root.is_absolute()
                        || user.applied.as_ref().is_some_and(|applied| {
                            !is_digest(&applied.manifest_sha256)
                                || !applied.project_root.is_absolute()
                        })
                    {
                        return Err(invalid(
                            "invalid Claude user root or applied provenance".to_owned(),
                        ));
                    }
                    if !user.entries.is_empty() && !same_target(&user.root, root) {
                        return Err(LedgerError::Root {
                            path: path.clone(),
                            recorded: user.root,
                        });
                    }
                    user.entries
                        .sort_by(|a, b| (a.kind, &a.name).cmp(&(b.kind, &b.name)));
                    if user
                        .entries
                        .windows(2)
                        .any(|pair| pair[0].key() == pair[1].key())
                    {
                        return Err(invalid("duplicate Claude user entry".to_owned()));
                    }
                    for (index, entry) in user.entries.iter().enumerate() {
                        if !entry.valid(local) {
                            return Err(invalid(format!(
                                "invalid destination, target or journal data in Claude user entry {}",
                                index + 1
                            )));
                        }
                    }
                    Ok(user)
                })
                .transpose()?
                .filter(|user| !user.entries.is_empty())
        } else {
            None
        };
        #[cfg(any(target_os = "linux", target_os = "macos"))]
        let digest = Some(manifest::sha256(&bytes));
        Ok(Self {
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            envelope,
            user,
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            bytes: Some(bytes),
            #[cfg(any(target_os = "linux", target_os = "macos"))]
            digest,
        })
    }

    #[cfg(any(target_os = "linux", target_os = "macos"))]
    pub fn encoded(&self, user: &User) -> io::Result<Vec<u8>> {
        let mut envelope = self.envelope.clone();
        envelope.generator = generator();
        let mut scopes: Scopes = envelope
            .harnesses
            .get("claude")
            .map(|claude| serde_json::from_str(claude.get()))
            .transpose()?
            .unwrap_or_default();
        if user.entries.is_empty() {
            scopes.remove("user");
        } else {
            scopes.insert("user".to_owned(), serde_json::value::to_raw_value(user)?);
        }
        if scopes.is_empty() {
            envelope.harnesses.remove("claude");
        } else {
            envelope.harnesses.insert(
                "claude".to_owned(),
                RawValue::from_string(serde_json::to_string_pretty(&scopes)?)?,
            );
        }
        let mut bytes = serde_json::to_vec_pretty(&envelope)?;
        bytes.push(b'\n');
        if bytes.len() as u64 > manifest::MAX_BYTES {
            return Err(io::Error::other("ownership ledger exceeds the size limit"));
        }
        Ok(bytes)
    }

    pub fn entries(&self) -> impl Iterator<Item = &Entry> {
        self.user.iter().flat_map(|user| &user.entries)
    }
}

pub(super) fn read_bytes(state: Option<&Root>) -> io::Result<Option<Vec<u8>>> {
    let Some(state) = state else { return Ok(None) };
    match state.kind(OsStr::new(FILE))? {
        None => Ok(None),
        Some(PathKind::File) => {
            let bytes = state
                .read(FILE, manifest::MAX_BYTES + 1)?
                .ok_or_else(|| io::Error::other("ownership ledger changed while opening it"))?;
            if bytes.len() as u64 > manifest::MAX_BYTES {
                return Err(io::Error::other("ownership ledger exceeds the size limit"));
            }
            Ok(Some(bytes))
        }
        Some(_) => Err(io::Error::other("ownership ledger must be a regular file")),
    }
}

pub(super) fn generator() -> manifest::Generator {
    manifest::Generator {
        name: manifest::OWNER.to_owned(),
        version: env!("CARGO_PKG_VERSION").to_owned(),
    }
}
