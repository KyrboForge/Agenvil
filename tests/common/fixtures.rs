//! The validation fixture framework.
//!
//! Every case is `tests/fixtures/validation/<category>/<name>/` with a `project/` tree (the files
//! `agenvil check` sees) and an `expect.toml` (what must come out). A case is copied into a fresh
//! temporary directory with its own empty HOME, run once per requested format, and then checked for
//! the invariants every run must keep: the tree is unchanged, HOME is untouched and no repository
//! script ran. The expectations are *Agenvil's own* regression baseline; none of them is output of
//! the legacy shell validator (see `docs/compatibility.md`).

use std::fs;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use super::{agenvil_in, deferred, snapshot};

pub const TESTS: &str = concat!(env!("CARGO_MANIFEST_DIR"), "/tests");

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Expect {
    pub description: String,
    /// Extra `agenvil check` arguments (never `--format`).
    #[serde(default)]
    pub args: Vec<String>,
    /// `skip` (default) runs only implemented rules, `select` leaves deferred rules selected.
    #[serde(default)]
    pub deferred: Deferred,
    /// Only run on this platform (`unix`); the expectation does not hold elsewhere.
    #[serde(default)]
    pub platform: Option<String>,
    /// Names a tree that cannot be committed (see [`setup`]).
    #[serde(default)]
    pub setup: Option<String>,
    pub exit: i32,
    /// Every finding, in output order. Absent `line`/`column` mean `null`.
    #[serde(default)]
    pub findings: Vec<Finding>,
    #[serde(default)]
    pub stderr_contains: Vec<String>,
    #[serde(default)]
    pub stderr_excludes: Vec<String>,
    /// Snapshots under `tests/golden/`: `json`, `github`, `human`, `stderr`.
    #[serde(default)]
    pub golden: Vec<String>,
}

#[derive(Debug, Default, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum Deferred {
    #[default]
    Skip,
    Select,
}

#[derive(Debug, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct Finding {
    pub id: String,
    pub severity: String,
    pub path: String,
    pub line: Option<usize>,
    pub column: Option<usize>,
}

pub struct Case {
    /// `category/name`.
    pub name: String,
    pub dir: PathBuf,
    pub expect: Expect,
}

impl Case {
    /// Whether the expectation holds on this platform.
    pub fn applies(&self) -> bool {
        match self.expect.platform.as_deref() {
            None => true,
            Some("unix") => cfg!(unix),
            Some(other) => panic!("{}: unknown platform `{other}`", self.name),
        }
    }

    /// `agents/ag002-model` becomes `agents__ag002-model`, the golden file stem.
    pub fn stem(&self) -> String {
        self.name.replace('/', "__")
    }
}

/// All cases, sorted by name.
pub fn cases() -> Vec<Case> {
    let root = Path::new(TESTS).join("fixtures/validation");
    let mut out = Vec::new();
    for category in sorted_dirs(&root) {
        for dir in sorted_dirs(&category) {
            let name = format!(
                "{}/{}",
                category.file_name().unwrap().to_string_lossy(),
                dir.file_name().unwrap().to_string_lossy()
            );
            let text = fs::read_to_string(dir.join("expect.toml"))
                .unwrap_or_else(|e| panic!("{name}: expect.toml: {e}"));
            let expect = toml::from_str(&text).unwrap_or_else(|e| panic!("{name}: {e}"));
            assert!(
                dir.join("project").is_dir(),
                "{name}: a case needs a project/ tree"
            );
            out.push(Case { name, dir, expect });
        }
    }
    assert!(!out.is_empty(), "no fixtures under {}", root.display());
    out
}

fn sorted_dirs(dir: &Path) -> Vec<PathBuf> {
    let mut dirs: Vec<_> = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("{}: {e}", dir.display()))
        .map(|e| e.unwrap().path())
        .filter(|p| p.is_dir())
        .collect();
    dirs.sort();
    dirs
}

pub struct Run {
    pub exit: i32,
    pub stdout: Vec<u8>,
    pub stderr: String,
}

/// Runs `case` with `--format <format>` in a throwaway project and HOME, and enforces the
/// invariants that hold for every run.
pub fn run(case: &Case, format: &str) -> Run {
    assert!(
        !case.expect.args.iter().any(|a| a.starts_with("--format")),
        "{}: the runner owns --format",
        case.name
    );
    let tmp = tempfile::tempdir().unwrap();
    let project = tmp.path().join("project");
    let home = tmp.path().join("home");
    fs::create_dir(&home).unwrap();
    copy_tree(&case.dir.join("project"), &project);
    if let Some(name) = &case.expect.setup {
        setup(name, tmp.path(), &project);
    }
    let before = snapshot(&project);

    let mut cmd = agenvil_in(&project, &home);
    cmd.args(["check", "--format", format])
        .args(&case.expect.args);
    if case.expect.deferred == Deferred::Skip {
        cmd.args(["--skip", &deferred()]);
    }
    let output = cmd.output().unwrap();

    assert_eq!(
        snapshot(&project),
        before,
        "{}: check modified the project",
        case.name
    );
    assert!(
        snapshot(&home).is_empty(),
        "{}: check wrote to HOME",
        case.name
    );
    assert!(
        !project.join("executed").exists(),
        "{}: a repository script ran",
        case.name
    );
    Run {
        exit: output
            .status
            .code()
            .expect("agenvil was killed by a signal"),
        stdout: output.stdout,
        stderr: String::from_utf8(output.stderr).unwrap(),
    }
}

/// Fixtures that git or Windows cannot carry: huge files, tens of thousands of entries, links.
fn setup(name: &str, tmp: &Path, project: &Path) {
    match name {
        // One byte over the 1 MiB per-file limit.
        "oversized-agent" => {
            fs::write(project.join("agents/huge.md"), vec![b'a'; 1024 * 1024 + 1]).unwrap();
        }
        // One entry over the 10 000 directory entry limit.
        "many-entries" => {
            for i in 0..10_001 {
                fs::write(project.join(format!("agents/n{i}.txt")), b"").unwrap();
            }
        }
        // A link that points outside the project, at a file with valid content.
        "symlink-escape" => {
            #[cfg(unix)]
            {
                fs::write(
                    tmp.join("outside.md"),
                    "---\nname: outside\ndescription: x\nmodel: opus\neffort: high\ntools: [Read]\n---\n",
                )
                .unwrap();
                std::os::unix::fs::symlink("../../outside.md", project.join("agents/escape.md"))
                    .unwrap();
            }
            #[cfg(not(unix))]
            let _ = tmp;
        }
        other => panic!("unknown setup `{other}`"),
    }
}

fn copy_tree(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            // `fs::copy` keeps the permission bits, so an executable fixture stays executable.
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// Whether this run may rewrite committed snapshots (`AGENVIL_BLESS=1 cargo test`). Never in CI,
/// so a changed snapshot can only reach `main` as a reviewed diff.
pub fn blessing() -> bool {
    if std::env::var_os("AGENVIL_BLESS").is_none_or(|v| v != "1") {
        return false;
    }
    assert!(
        std::env::var_os("CI").is_none(),
        "AGENVIL_BLESS is refused in CI"
    );
    true
}
