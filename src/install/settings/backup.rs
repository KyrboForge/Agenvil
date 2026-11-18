use std::ffi::OsStr;
use std::io;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use crate::install::fsops::{Fs, write_file};
use crate::manifest;
use crate::stage::pinned::Root;

const KEEP: usize = 10;
const DIRS: [&str; 2] = ["backups", "claude-settings"];

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

/// Saves the bytes about to be replaced, then keeps only the newest backups; returns the name.
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
