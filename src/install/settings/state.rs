use std::collections::BTreeMap;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use serde_json::value::RawValue;

use super::Error;
use super::fragment::{canonical_digest, tokens};
use crate::install::ledger::{Envelope, Scopes, generator};
use crate::install::{is_digest, same_target};

pub(super) const FILE: &str = "managed.json";
const SCHEMA: u32 = 1;

/// The Claude user namespace of `managed.json`: what Agenvil wrote into `settings.json`.
#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct User {
    pub root: PathBuf,
    pub values: Vec<ValueRecord>,
    pub hooks: Vec<HookRecord>,
    /// Digest of the `settings.json` bytes Agenvil last published.
    pub last_write: Option<String>,
    /// Records journaled before a `settings.json` publication, settled after it.
    pub pending: Option<Pending>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Pending {
    pub before: Option<String>,
    pub after: String,
    pub values: Vec<ValueRecord>,
    pub hooks: Vec<HookRecord>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct ValueRecord {
    pub pointer: String,
    pub value: Value,
    /// `null` when the key was absent; a wrapped value otherwise, so JSON `null` stays distinct.
    pub previous: Option<Previous>,
    /// Parent objects Agenvil created for this key, pruned on removal once empty.
    pub created: Vec<String>,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Previous {
    pub value: Value,
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct HookRecord {
    pub id: String,
    pub event: String,
    pub group: Value,
    pub sha256: String,
    pub created: Vec<String>,
}

pub(super) struct State {
    envelope: Envelope,
    pub user: Option<User>,
}

impl State {
    pub fn parse(bytes: Option<&[u8]>, path: &Path, root: &Path) -> Result<Self, Error> {
        let invalid = |detail: &str| Error::State {
            path: path.to_owned(),
            detail: detail.to_owned(),
        };
        let Some(bytes) = bytes else {
            return Ok(Self {
                envelope: Envelope {
                    schema: SCHEMA,
                    generator: generator(),
                    harnesses: BTreeMap::new(),
                },
                user: None,
            });
        };
        #[derive(Deserialize)]
        struct Header {
            schema: u32,
        }
        let header: Header =
            serde_json::from_slice(bytes).map_err(|_| invalid("invalid schema or JSON"))?;
        if header.schema != SCHEMA {
            return Err(invalid(&format!("unsupported schema {}", header.schema)));
        }
        let envelope: Envelope =
            serde_json::from_slice(bytes).map_err(|_| invalid("invalid envelope"))?;
        let mut user = None;
        if let Some(claude) = envelope.harnesses.get("claude") {
            let scopes: Scopes = serde_json::from_str(claude.get())
                .map_err(|_| invalid("invalid Claude namespace"))?;
            if let Some(raw) = scopes.get("user") {
                let parsed: User = serde_json::from_str(raw.get())
                    .map_err(|_| invalid("invalid fields in the Claude user namespace"))?;
                if !parsed.root.is_absolute() || !valid(&parsed) {
                    return Err(invalid("invalid root, record or digest"));
                }
                if !same_target(&parsed.root, root) {
                    return Err(invalid(&format!(
                        "it belongs to the Claude config root {}",
                        parsed.root.display()
                    )));
                }
                user = Some(parsed);
            }
        }
        Ok(Self { envelope, user })
    }

    pub fn encoded(&self, user: Option<&User>) -> serde_json::Result<Vec<u8>> {
        let mut envelope = self.envelope.clone();
        envelope.generator = generator();
        let mut scopes: Scopes = envelope
            .harnesses
            .get("claude")
            .map(|claude| serde_json::from_str(claude.get()))
            .transpose()?
            .unwrap_or_default();
        match user {
            Some(user) => scopes.insert("user".to_owned(), serde_json::value::to_raw_value(user)?),
            None => scopes.remove("user"),
        };
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
        Ok(bytes)
    }
}

fn valid(user: &User) -> bool {
    let records = |values: &[ValueRecord], hooks: &[HookRecord]| {
        let mut pointers = HashSet::new();
        let mut ids = HashSet::new();
        values.iter().all(|record| {
            tokens(&record.pointer).is_some()
                && pointers.insert(&record.pointer)
                && record.created.iter().all(|p| tokens(p).is_some())
        }) && hooks.iter().all(|record| {
            ids.insert(&record.id)
                && record.sha256 == canonical_digest(&record.group)
                && record.created.iter().all(|p| tokens(p).is_some())
        })
    };
    records(&user.values, &user.hooks)
        && user.last_write.as_deref().is_none_or(is_digest)
        && user.pending.as_ref().is_none_or(|pending| {
            records(&pending.values, &pending.hooks)
                && is_digest(&pending.after)
                && pending.before.as_deref().is_none_or(is_digest)
        })
}

pub(super) fn hook_record(
    id: &str,
    event: &str,
    group: &Value,
    created: Vec<String>,
) -> HookRecord {
    HookRecord {
        id: id.to_owned(),
        event: event.to_owned(),
        group: group.clone(),
        sha256: canonical_digest(group),
        created,
    }
}
