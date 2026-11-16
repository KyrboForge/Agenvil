//! Helpers shared by the integration tests (`check.rs`, `compatibility.rs`, `golden.rs`).
//! Nothing here reads the real HOME, the network or this repository.
#![allow(dead_code)] // each test crate uses a different subset

use std::fs;
use std::path::Path;

use assert_cmd::Command;

pub mod fixtures;

/// `agenvil` run in `dir` with HOME pointing at `dir` too.
pub fn agenvil(dir: impl AsRef<Path>) -> Command {
    agenvil_in(dir.as_ref(), dir.as_ref())
}

/// `agenvil` run in `cwd` with an explicit, isolated HOME.
pub fn agenvil_in(cwd: &Path, home: &Path) -> Command {
    let mut cmd = Command::cargo_bin("agenvil").unwrap();
    // Nothing may depend on the developer's machine.
    cmd.current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("XDG_CONFIG_HOME", home)
        .env_remove("CLAUDE_CONFIG_DIR");
    cmd
}

/// The deferred rule IDs, comma-separated, as `agenvil rules` lists them.
pub fn deferred() -> String {
    let ids: Vec<_> = rules()
        .into_iter()
        .filter(|r| r.status == "deferred")
        .map(|r| r.id)
        .collect();
    assert!(!ids.is_empty());
    ids.join(",")
}

/// One row of `agenvil rules`.
pub struct RuleRow {
    pub id: String,
    pub severity: String,
    pub fast: String,
    pub status: String,
}

/// The rule registry as the CLI prints it, the single source of truth for the rule list.
pub fn rules() -> Vec<RuleRow> {
    let output = Command::cargo_bin("agenvil")
        .unwrap()
        .arg("rules")
        .output()
        .unwrap();
    String::from_utf8(output.stdout)
        .unwrap()
        .lines()
        .skip(1)
        .map(|l| {
            let mut cols = l.split_whitespace();
            let mut next = || cols.next().unwrap().to_owned();
            RuleRow {
                id: next(),
                severity: next(),
                fast: next(),
                status: next(),
            }
        })
        .collect()
}

/// Every file and directory under `dir` with its content, sorted, `/`-separated.
pub fn snapshot(dir: &Path) -> Vec<(String, Vec<u8>)> {
    fn walk(base: &Path, dir: &Path, out: &mut Vec<(String, Vec<u8>)>) {
        let mut entries: Vec<_> = fs::read_dir(dir)
            .unwrap()
            .map(|e| e.unwrap().path())
            .collect();
        entries.sort();
        for path in entries {
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/");
            if path.is_dir() {
                out.push((format!("{rel}/"), Vec::new()));
                walk(base, &path, out);
            } else {
                out.push((rel, fs::read(&path).unwrap()));
            }
        }
    }
    let mut out = Vec::new();
    walk(dir, dir, &mut out);
    out
}
