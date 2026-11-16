//! Reads the few files rules need beyond the inventory (policy file, team defaults, `bin/`).
//!
//! Same posture as discovery: lstat every path component, never follow a symlink, cap sizes, and
//! turn every failure into a [`Problem`] so a rule never passes on input it could not read.

use std::fs::{self, File};
use std::io::{ErrorKind, Read};
use std::path::{Path, PathBuf};

use crate::config::RelPath;
use crate::diagnostics::{Problem, ProblemKind};

pub enum Input {
    Missing,
    Text(String),
    Problem(Problem),
}

pub struct BinFile {
    pub path: String,
    /// `None` where the platform has no executable bit.
    pub executable: Option<bool>,
    pub shebang: bool,
}

enum Resolved {
    Missing,
    Problem(Problem),
    Found(PathBuf, fs::Metadata),
}

fn problem(path: &str, kind: ProblemKind, message: &str) -> Problem {
    Problem {
        path: path.to_owned(),
        line: None,
        kind,
        message: message.to_owned(),
    }
}

/// Walks `rel` under `root` one component at a time without following symlinks.
fn resolve(root: &Path, rel: &RelPath, want_dir: bool) -> Resolved {
    let shown = rel.to_slash();
    let parts: Vec<_> = rel.as_path().iter().collect();
    let mut current = root.to_path_buf();
    for (index, part) in parts.iter().enumerate() {
        current.push(part);
        let meta = match fs::symlink_metadata(&current) {
            Ok(meta) => meta,
            Err(e) if e.kind() == ErrorKind::NotFound => return Resolved::Missing,
            Err(e) => {
                return Resolved::Problem(problem(
                    &shown,
                    ProblemKind::Unreadable,
                    &e.kind().to_string(),
                ));
            }
        };
        if meta.file_type().is_symlink() {
            return Resolved::Problem(problem(
                &shown,
                ProblemKind::Symlink,
                "symlinks are not followed",
            ));
        }
        let last = index + 1 == parts.len();
        if !last && !meta.is_dir() {
            return Resolved::Problem(problem(
                &shown,
                ProblemKind::NotADirectory,
                "a parent of this path is not a directory",
            ));
        }
        if last && want_dir && !meta.is_dir() {
            return Resolved::Problem(problem(
                &shown,
                ProblemKind::NotADirectory,
                "expected a directory",
            ));
        }
        if last && !want_dir && !meta.is_file() {
            return Resolved::Problem(problem(
                &shown,
                ProblemKind::NotRegularFile,
                "expected a regular file",
            ));
        }
        if last {
            return Resolved::Found(current, meta);
        }
    }
    Resolved::Missing
}

/// Reads a UTF-8 text file of at most `max` bytes.
pub fn read_text(root: &Path, rel: &RelPath, max: u64) -> Input {
    let shown = rel.to_slash();
    let (path, meta) = match resolve(root, rel, false) {
        Resolved::Missing => return Input::Missing,
        Resolved::Problem(p) => return Input::Problem(p),
        Resolved::Found(path, meta) => (path, meta),
    };
    let too_large = || {
        Input::Problem(problem(
            &shown,
            ProblemKind::TooLarge,
            &format!("larger than the {max}-byte limit"),
        ))
    };
    if meta.len() > max {
        return too_large();
    }
    // The size may change after the lstat, so the bound is enforced again on what is read.
    let mut bytes = Vec::new();
    if let Err(e) = File::open(&path).and_then(|f| f.take(max + 1).read_to_end(&mut bytes)) {
        return Input::Problem(problem(
            &shown,
            ProblemKind::Unreadable,
            &e.kind().to_string(),
        ));
    }
    if bytes.len() as u64 > max {
        return too_large();
    }
    match String::from_utf8(bytes) {
        Ok(text) => Input::Text(text),
        Err(_) => Input::Problem(problem(
            &shown,
            ProblemKind::InvalidUtf8,
            "file is not valid UTF-8",
        )),
    }
}

/// The regular files directly inside `rel`, sorted by name. A missing directory is empty.
pub fn bin_files(root: &Path, rel: &RelPath, max_entries: usize) -> (Vec<BinFile>, Vec<Problem>) {
    let shown = rel.to_slash();
    let dir = match resolve(root, rel, true) {
        Resolved::Missing => return (Vec::new(), Vec::new()),
        Resolved::Problem(p) => return (Vec::new(), vec![p]),
        Resolved::Found(dir, _) => dir,
    };
    let mut problems = Vec::new();
    let mut names = Vec::new();
    match fs::read_dir(&dir) {
        Ok(entries) => {
            for entry in entries {
                match entry {
                    Ok(entry) if names.len() < max_entries => names.push(entry.file_name()),
                    Ok(_) => {
                        problems.push(problem(
                            &shown,
                            ProblemKind::LimitExceeded,
                            &format!("more than {max_entries} entries; the directory is not fully checked"),
                        ));
                        break;
                    }
                    Err(e) => problems.push(problem(
                        &shown,
                        ProblemKind::Unreadable,
                        &e.kind().to_string(),
                    )),
                }
            }
        }
        Err(e) => {
            problems.push(problem(
                &shown,
                ProblemKind::Unreadable,
                &e.kind().to_string(),
            ));
        }
    }
    names.sort();

    let mut files = Vec::new();
    for name in names {
        let Some(name) = name.to_str() else {
            problems.push(problem(
                &shown,
                ProblemKind::InvalidPath,
                "a file name is not valid UTF-8",
            ));
            continue;
        };
        let path = format!("{shown}/{name}");
        let full = dir.join(name);
        let meta = match fs::symlink_metadata(&full) {
            Ok(meta) => meta,
            Err(e) => {
                problems.push(problem(
                    &path,
                    ProblemKind::Unreadable,
                    &e.kind().to_string(),
                ));
                continue;
            }
        };
        if meta.file_type().is_symlink() {
            problems.push(problem(
                &path,
                ProblemKind::Symlink,
                "symlinks are not followed",
            ));
        } else if meta.is_file() {
            let mut head = Vec::new();
            match File::open(&full).and_then(|f| f.take(2).read_to_end(&mut head)) {
                Ok(_) => files.push(BinFile {
                    path,
                    executable: executable(&meta),
                    shebang: head == b"#!",
                }),
                Err(e) => problems.push(problem(
                    &path,
                    ProblemKind::Unreadable,
                    &e.kind().to_string(),
                )),
            }
        } else if !meta.is_dir() {
            problems.push(problem(
                &path,
                ProblemKind::NotRegularFile,
                "expected a regular file",
            ));
        }
    }
    (files, problems)
}

#[cfg(unix)]
fn executable(meta: &fs::Metadata) -> Option<bool> {
    use std::os::unix::fs::PermissionsExt;
    Some(meta.permissions().mode() & 0o111 != 0)
}

#[cfg(not(unix))]
fn executable(_: &fs::Metadata) -> Option<bool> {
    None
}
