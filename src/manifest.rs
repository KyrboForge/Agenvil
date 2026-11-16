//! The build manifest (`manifest.json`, schema 1): what a staging tree claims to own.
//!
//! The same type is written by the build and read back by `build --check` and cleanup. An entry is
//! an *ownership claim*, never an authorisation: it is trusted only after [`Manifest::flaws`]
//! found nothing wrong, and cleanup additionally re-checks the file's digest and the layout.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};

/// Version of `manifest.json`. Bumped on any breaking change to it.
pub const VERSION: u32 = 1;
pub const FILE: &str = "manifest.json";
pub const OWNER: &str = "agenvil";
pub const TARGET: &str = "claude";
/// Largest manifest read, and the most files it may list or a staging tree may hold.
pub const MAX_BYTES: u64 = 64 * 1024 * 1024;
pub const MAX_ENTRIES: usize = 30_000;
/// Directories below which Claude Code reads generated files, under `.claude/`.
const LAYOUT: [&str; 3] = ["agents", "rules", "skills"];

/// Lowercase hex SHA-256 of `bytes`, the digest format of the manifest.
pub fn sha256(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum Transform {
    /// Byte for byte.
    Copy,
    /// `model` and `effort` lines rewritten to the pair of an explicit target mapping.
    ModelEffortOverride,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Manifest {
    pub schema: u32,
    pub target: String,
    /// Every file listed below belongs to Agenvil; `manifest.json` itself is not listed.
    pub owner: String,
    pub generator: Generator,
    pub files: Vec<File>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Generator {
    pub name: String,
    pub version: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct File {
    /// Relative to the output directory, `/`-separated.
    pub path: String,
    /// Relative to the project root.
    pub source: String,
    pub sha256: String,
    pub bytes: u64,
    pub transform: Transform,
}

/// Something wrong with a manifest. `unsafe_path` separates a path that could escape or alias
/// the staging root (traversal, absolute, `\`, `:`) from a merely malformed manifest.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Flaw {
    pub path: String,
    pub unsafe_path: bool,
    pub message: String,
}

impl Manifest {
    pub fn new(files: Vec<File>) -> Self {
        Self {
            schema: VERSION,
            target: TARGET.to_owned(),
            owner: OWNER.to_owned(),
            generator: Generator {
                name: "agenvil".to_owned(),
                version: env!("CARGO_PKG_VERSION").to_owned(),
            },
            files,
        }
    }

    /// The exact bytes of `manifest.json`: pretty JSON and a final newline, nothing else.
    pub fn to_json(&self) -> serde_json::Result<String> {
        let mut json = serde_json::to_string_pretty(self)?;
        json.push('\n');
        Ok(json)
    }

    /// Parses `manifest.json` strictly: unknown fields, a wrong type or an unsupported schema
    /// version is an error message, never a partial manifest.
    pub fn parse(bytes: &[u8]) -> Result<Self, String> {
        let value: Value =
            serde_json::from_slice(bytes).map_err(|e| format!("not valid JSON: {e}"))?;
        match value.get("schema") {
            Some(Value::Number(n)) if n.as_u64() == Some(u64::from(VERSION)) => {}
            Some(other) => {
                return Err(format!(
                    "unsupported schema {other}; this Agenvil reads schema {VERSION}"
                ));
            }
            None => return Err("no `schema` field".to_owned()),
        }
        serde_json::from_value(value).map_err(|e| format!("does not match schema {VERSION}: {e}"))
    }

    /// Every reason this manifest cannot be trusted, in a stable order. Empty means the claims
    /// may be compared with the tree; it does not make any claim true.
    pub fn flaws(&self) -> Vec<Flaw> {
        let mut flaws = Vec::new();
        let mut add = |path: &str, unsafe_path: bool, message: String| {
            flaws.push(Flaw {
                path: path.to_owned(),
                unsafe_path,
                message,
            });
        };
        if self.target != TARGET {
            add(
                FILE,
                false,
                format!("target `{}` is not `{TARGET}`", self.target),
            );
        }
        if self.owner != OWNER {
            add(
                FILE,
                false,
                format!("owner `{}` is not `{OWNER}`", self.owner),
            );
        }
        if self.generator.name != "agenvil" {
            add(
                FILE,
                false,
                format!("generator `{}` is not `agenvil`", self.generator.name),
            );
        }
        if self.files.len() > MAX_ENTRIES {
            add(
                FILE,
                false,
                format!(
                    "lists {} files; the limit is {MAX_ENTRIES}",
                    self.files.len()
                ),
            );
            return flaws_sorted(flaws);
        }
        let total: u64 = self.files.iter().fold(0, |t, f| t.saturating_add(f.bytes));
        let max_total = crate::build::limits().max_total_bytes;
        if total > max_total {
            add(
                FILE,
                false,
                format!("lists {total} bytes in total; the limit is {max_total}"),
            );
        }
        // Lowercased, so `A.md` and `a.md` collide as they would on macOS and Windows.
        let mut seen: BTreeMap<String, &str> = BTreeMap::new();
        for file in &self.files {
            let path = file.path.as_str();
            if let Some((unsafe_path, why)) = path_flaw(path) {
                add(path, unsafe_path, why);
            } else if !in_layout(path) {
                add(
                    path,
                    false,
                    "outside .claude/agents, .claude/rules and .claude/skills, so it can never \
                     be owned"
                        .to_owned(),
                );
            }
            if let Some((unsafe_path, why)) = source_flaw(&file.source) {
                add(
                    path,
                    unsafe_path,
                    format!("source `{}`: {why}", file.source),
                );
            }
            if !is_sha256(&file.sha256) {
                add(
                    path,
                    false,
                    "sha256 is not 64 lowercase hex digits".to_owned(),
                );
            }
            if file.bytes > crate::build::limits().max_file_bytes {
                add(
                    path,
                    false,
                    format!("{} bytes exceeds the file limit", file.bytes),
                );
            }
            match seen.insert(path.to_lowercase(), path) {
                Some(first) if first == path => add(path, false, "listed twice".to_owned()),
                Some(first) => add(
                    path,
                    false,
                    format!("collides with `{first}` on a case-insensitive file system"),
                ),
                None => {}
            }
        }
        // A file that is also another entry's directory cannot exist.
        let names: BTreeSet<&String> = seen.keys().collect();
        for (lower, path) in &seen {
            let mut prefix = String::new();
            for part in lower
                .split('/')
                .take(lower.split('/').count().saturating_sub(1))
            {
                prefix.push_str(part);
                if names.contains(&prefix) {
                    add(path, false, format!("`{prefix}` is a file and a directory"));
                    break;
                }
                prefix.push('/');
            }
        }
        flaws_sorted(flaws)
    }
}

fn flaws_sorted(mut flaws: Vec<Flaw>) -> Vec<Flaw> {
    flaws.sort();
    flaws.dedup();
    flaws
}

fn is_sha256(s: &str) -> bool {
    s.len() == 64 && s.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

fn in_layout(path: &str) -> bool {
    let mut parts = path.split('/');
    parts.next() == Some(".claude")
        && parts.next().is_some_and(|d| LAYOUT.contains(&d))
        && parts.next().is_some()
}

/// Why `path` is not a valid output path, as `(unsafe, reason)`. Valid means: relative,
/// `/`-separated, no `.`/`..`/empty components, and a legal name on Linux, macOS and Windows.
pub fn path_flaw(path: &str) -> Option<(bool, String)> {
    if let Some(flaw) = source_flaw(path) {
        return Some(flaw);
    }
    for part in path.split('/') {
        let stem = part.split('.').next().unwrap_or("").trim_end();
        let reserved = ["CON", "PRN", "AUX", "NUL"]
            .iter()
            .any(|r| stem.eq_ignore_ascii_case(r))
            || (stem.len() == 4
                && (stem[..3].eq_ignore_ascii_case("COM")
                    || stem[..3].eq_ignore_ascii_case("LPT"))
                && stem.as_bytes()[3].is_ascii_digit());
        if reserved {
            return Some((false, format!("`{part}` is a reserved name on Windows")));
        }
        if part.ends_with(['.', ' ']) {
            return Some((
                false,
                format!("`{part}` ends with a dot or a space (invalid on Windows)"),
            ));
        }
        if part.contains(['<', '>', '"', '|', '?', '*']) {
            return Some((
                false,
                format!("`{part}` contains a character invalid on Windows"),
            ));
        }
        if part.len() > 255 {
            return Some((
                false,
                "a path component is longer than 255 bytes".to_owned(),
            ));
        }
    }
    (path.len() > 1024).then(|| (false, "longer than 1024 bytes".to_owned()))
}

/// The part of [`path_flaw`] that only protects the file system: used for a manifest `source`,
/// whose names are the user's own layout and may be legal on one platform only.
fn source_flaw(path: &str) -> Option<(bool, String)> {
    if path.is_empty() {
        return Some((false, "empty path".to_owned()));
    }
    if path.starts_with('/') || path.contains(['\\', ':']) {
        return Some((true, "absolute, `\\` or `:` in a path".to_owned()));
    }
    for part in path.split('/') {
        match part {
            ".." => return Some((true, "`..` traversal".to_owned())),
            "" | "." => return Some((false, "empty or `.` path component".to_owned())),
            _ if part.chars().any(char::is_control) => {
                return Some((false, "control character in a path".to_owned()));
            }
            _ => {}
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn file(path: &str) -> File {
        File {
            path: path.to_owned(),
            source: "agents/a.md".to_owned(),
            sha256: "0".repeat(64),
            bytes: 1,
            transform: Transform::Copy,
        }
    }

    fn flaws(paths: &[&str]) -> Vec<Flaw> {
        Manifest::new(paths.iter().map(|p| file(p)).collect()).flaws()
    }

    #[test]
    fn a_well_formed_manifest_has_no_flaws() {
        assert!(flaws(&[".claude/agents/a.md", ".claude/skills/s/SKILL.md"]).is_empty());
    }

    #[test]
    fn traversal_absolute_and_backslash_paths_are_unsafe_not_just_invalid() {
        for bad in [
            "../x",
            ".claude/../../x",
            "/etc/passwd",
            "C:/x",
            "a\\b",
            ".claude\\agents\\a.md",
        ] {
            let found = flaws(&[bad]);
            assert!(found.iter().any(|f| f.unsafe_path), "{bad}: {found:?}");
        }
    }

    #[test]
    fn invalid_paths_are_rejected_on_every_platform() {
        for bad in [
            ".claude/agents/con.md",
            ".claude/agents/NUL",
            ".claude/agents/com1.txt",
            ".claude/agents/lpt9.md",
            ".claude/agents/a.md.",
            ".claude/agents/a.md ",
            ".claude/agents/a?.md",
            ".claude/agents//a.md",
            ".claude/agents/./a.md",
            ".claude/agents/a\n.md",
            "manifest.json",
            "notes.txt",
            ".claude/other/a.md",
            ".claude/agents",
        ] {
            let found = flaws(&[bad]);
            assert!(
                !found.is_empty() && found.iter().all(|f| !f.unsafe_path),
                "{bad}: {found:?}"
            );
        }
        // Not reserved: the device name only counts as a whole stem.
        assert!(flaws(&[".claude/agents/console.md", ".claude/agents/com10.md"]).is_empty());
    }

    #[test]
    fn duplicates_case_collisions_and_file_directory_conflicts_are_flagged() {
        let dup = flaws(&[".claude/agents/a.md", ".claude/agents/a.md"]);
        assert!(dup.iter().any(|f| f.message == "listed twice"));
        let case = flaws(&[".claude/agents/A.md", ".claude/agents/a.md"]);
        assert!(case.iter().any(|f| f.message.contains("case-insensitive")));
        let conflict = flaws(&[".claude/skills/s", ".claude/skills/s/SKILL.md"]);
        assert!(
            conflict
                .iter()
                .any(|f| f.message.contains("file and a directory")),
            "{conflict:?}"
        );
    }

    #[test]
    fn parse_is_strict_about_schema_fields_and_types() {
        let good = Manifest::new(vec![file(".claude/agents/a.md")])
            .to_json()
            .unwrap();
        assert!(Manifest::parse(good.as_bytes()).is_ok());
        assert!(
            Manifest::parse(b"{")
                .unwrap_err()
                .contains("not valid JSON")
        );
        assert!(Manifest::parse(b"{}").unwrap_err().contains("no `schema`"));
        let v2 = good.replace("\"schema\": 1", "\"schema\": 2");
        assert!(
            Manifest::parse(v2.as_bytes())
                .unwrap_err()
                .contains("unsupported schema 2")
        );
        let extra = good.replace("\"owner\"", "\"admin\": true, \"owner\"");
        assert!(Manifest::parse(extra.as_bytes()).is_err());
        let bytes = good.replace("\"bytes\": 1", "\"bytes\": \"1\"");
        assert!(Manifest::parse(bytes.as_bytes()).is_err());
    }

    #[test]
    fn wrong_owner_target_and_digest_are_flagged() {
        let mut m = Manifest::new(vec![file(".claude/agents/a.md")]);
        m.owner = "someone".to_owned();
        m.target = "codex".to_owned();
        m.files[0].sha256 = "ABC".to_owned();
        let messages: Vec<_> = m.flaws().into_iter().map(|f| f.message).collect();
        assert_eq!(messages.len(), 3, "{messages:?}");
    }
}
