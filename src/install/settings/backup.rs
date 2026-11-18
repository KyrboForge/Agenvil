use std::ffi::OsStr;
use std::io;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::path::{Path, PathBuf};
#[cfg(any(target_os = "linux", target_os = "macos"))]
use std::time::{SystemTime, UNIX_EPOCH};

use serde::Serialize;

use super::read;
#[cfg(any(target_os = "linux", target_os = "macos"))]
use crate::install::fsops::{Fs, write_file};
use crate::manifest;
use crate::stage::{PathKind, pinned::Root};

#[cfg(any(target_os = "linux", target_os = "macos"))]
const KEEP: usize = 10;
const DIRS: [&str; 2] = ["backups", "claude-settings"];

#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn path(local: &Path, name: &str) -> PathBuf {
    local.join("state").join(DIRS[0]).join(DIRS[1]).join(name)
}

/// `<UTC Unix nanoseconds, zero-padded>-<sha8>.json`: sorts by age; nothing else is ever pruned.
pub(super) fn is_name(name: &str) -> bool {
    name.strip_suffix(".json")
        .and_then(|stem| stem.split_once('-'))
        .is_some_and(|(time, sha)| {
            time.len() == 20
                && time.bytes().all(|b| b.is_ascii_digit())
                && sha.len() == 8
                && sha.bytes().all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
        })
}

pub(super) fn dir(state: &Root) -> io::Result<Option<Root>> {
    match state.child(OsStr::new(DIRS[0]))? {
        Some(backups) => backups.child(OsStr::new(DIRS[1])),
        None => Ok(None),
    }
}

/// Metadata for a regular settings backup.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Backup {
    pub name: String,
    /// Zero-padded Unix epoch nanoseconds (UTC), encoded as a string for exact JSON output.
    pub timestamp_utc_ns: String,
    pub size: u64,
    pub sha256: String,
}

impl Backup {
    pub(super) fn verified(name: &str, bytes: &[u8]) -> Option<Self> {
        let sha256 = manifest::sha256(bytes);
        if name.get(21..29)? != &sha256[..8] {
            return None;
        }
        Some(Self {
            name: name.to_owned(),
            timestamp_utc_ns: name.get(..20)?.to_owned(),
            size: bytes.len() as u64,
            sha256,
        })
    }
}

pub(super) fn list(state: Option<&Root>) -> io::Result<Vec<Backup>> {
    let Some(dir) = state.map(dir).transpose()?.flatten() else {
        return Ok(Vec::new());
    };
    let mut backups = Vec::new();
    for name in dir.names(4096)?.iter().filter_map(|name| name.to_str()) {
        if is_name(name)
            && dir.kind(OsStr::new(name))? == Some(PathKind::File)
            && let Some((bytes, _)) = read(Some(&dir), name)?
            && let Some(backup) = Backup::verified(name, &bytes)
        {
            backups.push(backup);
        }
    }
    Ok(backups)
}

/// Saves the bytes about to be replaced, then keeps only the newest backups; returns the name.
#[cfg(any(target_os = "linux", target_os = "macos"))]
pub(super) fn save(fs: &mut Fs, state: &Root, bytes: &[u8]) -> io::Result<String> {
    let backups = fs.dir(state, OsStr::new(DIRS[0]))?;
    let dir = fs.dir(&backups, OsStr::new(DIRS[1]))?;
    let time = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_nanos();
    let name = format!("{time:020}-{}.json", &manifest::sha256(bytes)[..8]);
    // Exclusive creation, mode 0600 and fsync.
    write_file(&dir, &name, bytes, 0o600)?;
    fs.sync(&dir)?;
    let names: Vec<_> = dir
        .names(4096)?
        .into_iter()
        .filter_map(|name| name.into_string().ok())
        .filter(|old| is_name(old) && *old != name)
        .collect();
    // The new backup is never pruned, even if the clock went backwards.
    for old in &names[..names.len().saturating_sub(KEEP - 1)] {
        dir.remove(old, |file| Ok(file.metadata()?.is_file()))?;
    }
    Ok(name)
}
