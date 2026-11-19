use std::collections::{BTreeSet, HashSet};
use std::ffi::OsStr;
use std::fmt;
use std::io::{self, Read};
use std::path::Path;

use serde::Deserialize;
use serde::de::{Deserializer, MapAccess, Visitor};
use serde_json::Value;

use super::{Error, MAX_BYTES};
use crate::config::RelPath;
use crate::manifest;
use crate::stage::{PathKind, pinned::Root};

const SCHEMA: u32 = 1;
/// `env` never merges; hooks enter only through `hooks[]`, where each group is tracked by ID.
const RESERVED: [&str; 2] = ["env", "hooks"];
const CREDENTIALS: [&str; 3] = ["apiKeyHelper", "awsAuthRefresh", "awsCredentialExport"];

/// A reviewed settings fragment: the only keys and hook groups Agenvil manages.
#[derive(Debug, Default)]
pub struct Fragment {
    pub(super) values: Vec<(String, Value)>,
    pub(super) hooks: Vec<Hook>,
}

#[derive(Clone, Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Hook {
    pub id: String,
    pub event: String,
    pub group: Value,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Raw {
    schema: u32,
    #[serde(default)]
    values: Values,
    #[serde(default)]
    hooks: Vec<Hook>,
}

/// Keeps duplicate keys, which a JSON map would silently collapse.
#[derive(Default)]
struct Values(Vec<(String, Value)>);

impl<'de> Deserialize<'de> for Values {
    fn deserialize<D: Deserializer<'de>>(deserializer: D) -> Result<Self, D::Error> {
        struct Entries;
        impl<'de> Visitor<'de> for Entries {
            type Value = Values;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("an object of JSON pointers")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Values, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry()? {
                    entries.push(entry);
                }
                Ok(Values(entries))
            }
        }
        deserializer.deserialize_map(Entries)
    }
}

impl Fragment {
    /// Reads a kit source through the pinned checkout, rejecting descendant symlinks.
    pub(in crate::install) fn read_source(root: &Path, relative: &RelPath) -> Result<Self, Error> {
        let path = root.join(relative.as_path());
        let invalid = |detail| Error::Fragment {
            path: path.clone(),
            detail,
        };
        let read_error = |error: io::Error| match error.kind() {
            io::ErrorKind::NotFound => Error::MissingFragment { path: path.clone() },
            _ => invalid(error.to_string()),
        };
        let components = relative.to_slash();
        let (dirs, name) = components.rsplit_once('/').unwrap_or(("", &components));
        let canonical = root.canonicalize().map_err(read_error)?;
        let mut parent = Root::open_checked(&canonical)
            .map_err(read_error)?
            .ok_or_else(|| read_error(io::ErrorKind::NotFound.into()))?;
        for component in dirs.split('/').filter(|part| !part.is_empty()) {
            let component = OsStr::new(component);
            match parent.kind(component).map_err(read_error)? {
                None => return Err(Error::MissingFragment { path }),
                Some(PathKind::Directory) => {}
                Some(_) => {
                    return Err(invalid(
                        "parent is not a directory; symlinks are refused".into(),
                    ));
                }
            }
            parent = parent
                .child(component)
                .map_err(read_error)?
                .ok_or_else(|| read_error(io::ErrorKind::NotFound.into()))?;
        }
        let (bytes, _) = super::read(Some(&parent), name)
            .map_err(read_error)?
            .ok_or_else(|| read_error(io::ErrorKind::NotFound.into()))?;
        Self::parse(&bytes).map_err(invalid)
    }

    pub fn read(path: &Path) -> Result<Self, Error> {
        let invalid = |detail: String| Error::Fragment {
            path: path.to_owned(),
            detail,
        };
        let file = std::fs::File::open(path).map_err(|err| invalid(err.to_string()))?;
        if !file.metadata().is_ok_and(|meta| meta.is_file()) {
            return Err(invalid("not a regular file".to_owned()));
        }
        let mut bytes = Vec::new();
        file.take(MAX_BYTES + 1)
            .read_to_end(&mut bytes)
            .map_err(|err| invalid(err.to_string()))?;
        if bytes.len() as u64 > MAX_BYTES {
            return Err(invalid("exceeds the size limit".to_owned()));
        }
        Self::parse(&bytes).map_err(invalid)
    }

    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let raw: Raw = serde_json::from_slice(bytes).map_err(|err| err.to_string())?;
        if raw.schema != SCHEMA {
            return Err(format!("unsupported schema {}", raw.schema));
        }
        let mut seen = Vec::new();
        for (pointer, _) in &raw.values.0 {
            let parts =
                tokens(pointer).ok_or_else(|| format!("invalid JSON pointer `{pointer}`"))?;
            if RESERVED.contains(&parts[0].as_str()) {
                return Err(format!(
                    "`{pointer}` is reserved; Agenvil never merges env or raw hooks"
                ));
            }
            if parts
                .iter()
                .any(|part| CREDENTIALS.contains(&part.as_str()))
            {
                return Err(format!("`{pointer}` names a credential helper"));
            }
            seen.push(parts);
        }
        seen.sort();
        // Sorted token lists put a prefix directly before the first pointer below it.
        if let Some(pair) = seen.windows(2).find(|pair| pair[1].starts_with(&pair[0])) {
            return Err(format!(
                "pointers overlap: `{}` and `{}`",
                pointer(&pair[0]),
                pointer(&pair[1])
            ));
        }
        let mut ids = HashSet::new();
        for hook in &raw.hooks {
            if hook.id.is_empty()
                || !hook
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_' | b'.'))
            {
                return Err(format!("invalid hook id `{}`", hook.id));
            }
            if !ids.insert(&hook.id) {
                return Err(format!("duplicate hook id `{}`", hook.id));
            }
            if hook.event.is_empty() || !hook.event.bytes().all(|b| b.is_ascii_alphanumeric()) {
                return Err(format!("invalid hook event `{}`", hook.event));
            }
            let valid_group = hook
                .group
                .get("hooks")
                .and_then(Value::as_array)
                .is_some_and(|entries| !entries.is_empty() && entries.iter().all(Value::is_object));
            if !hook.group.is_object() || !valid_group {
                return Err(format!(
                    "hook `{}` needs a group object with a nonempty `hooks` array of objects",
                    hook.id
                ));
            }
        }
        Ok(Self {
            values: raw.values.0,
            hooks: raw.hooks,
        })
    }
}

/// RFC 6901 reference tokens of a non-root pointer; empty tokens are rejected.
pub(super) fn tokens(pointer: &str) -> Option<Vec<String>> {
    let rest = pointer.strip_prefix('/')?;
    rest.split('/')
        .map(|token| {
            let mut out = String::new();
            let mut chars = token.chars();
            while let Some(c) = chars.next() {
                if c != '~' {
                    out.push(c);
                    continue;
                }
                match chars.next()? {
                    '0' => out.push('~'),
                    '1' => out.push('/'),
                    _ => return None,
                }
            }
            (!out.is_empty()).then_some(out)
        })
        .collect()
}

pub(super) fn pointer(tokens: &[String]) -> String {
    tokens
        .iter()
        .map(|token| format!("/{}", token.replace('~', "~0").replace('/', "~1")))
        .collect()
}

/// Commands a hook group runs; a recorded group whose set reappears changed was edited by a user.
pub(super) fn commands(group: &Value) -> BTreeSet<&str> {
    group
        .get("hooks")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|hook| hook.get("command")?.as_str())
        .collect()
}

pub(super) fn canonical_digest(value: &Value) -> String {
    let mut value = value.clone();
    value.sort_all_objects();
    manifest::sha256(value.to_string().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pointers_round_trip_and_reject_bad_escapes() {
        assert_eq!(tokens("/a~1b/c~0d").unwrap(), ["a/b", "c~d"]);
        assert_eq!(pointer(&tokens("/a~1b/c~0d").unwrap()), "/a~1b/c~0d");
        for bad in ["", "/", "a", "/a//b", "/a~2", "/a~"] {
            assert!(tokens(bad).is_none(), "{bad}");
        }
    }

    #[test]
    fn rejects_reserved_overlapping_duplicate_and_unknown_input() {
        let cases = [
            r#"{"schema":1,"values":{"/env/X":1}}"#,
            r#"{"schema":1,"values":{"/env":{}}}"#,
            r#"{"schema":1,"values":{"/hooks/PreToolUse":[]}}"#,
            r#"{"schema":1,"values":{"/apiKeyHelper":"x"}}"#,
            r#"{"schema":1,"values":{"/a/awsAuthRefresh":"x"}}"#,
            r#"{"schema":1,"values":{"/awsCredentialExport":"x"}}"#,
            r#"{"schema":1,"values":{"/a":1,"/a/b":2}}"#,
            r#"{"schema":1,"values":{"/a":1,"/a":2}}"#,
            r#"{"schema":1,"extra":true}"#,
            r#"{"schema":2}"#,
            r#"{"schema":1,"hooks":[{"id":"x","event":"Stop","group":{"hooks":[]}}]}"#,
            r#"{"schema":1,"hooks":[{"id":"x","event":"Stop","group":{"hooks":[{}]},"x":1}]}"#,
        ];
        for case in cases {
            assert!(Fragment::parse(case.as_bytes()).is_err(), "{case}");
        }
        let hook =
            r#"{"id":"x","event":"Stop","group":{"hooks":[{"type":"command","command":"a"}]}}"#;
        let duplicate = format!(r#"{{"schema":1,"hooks":[{hook},{hook}]}}"#);
        assert!(Fragment::parse(duplicate.as_bytes()).is_err());
        let ok = format!(r#"{{"schema":1,"values":{{"/a/b":1,"/ab":2}},"hooks":[{hook}]}}"#);
        assert!(Fragment::parse(ok.as_bytes()).is_ok());
    }
}
