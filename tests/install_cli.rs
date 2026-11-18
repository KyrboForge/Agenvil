mod common;

use std::fs;
use std::path::{Path, PathBuf};

use common::agenvil_in;
use predicates::prelude::*;
use tempfile::TempDir;

struct CliFixture {
    project: TempDir,
    home: TempDir,
    config: PathBuf,
}

impl CliFixture {
    fn built() -> Self {
        let project = tempfile::tempdir().unwrap();
        let home = tempfile::tempdir().unwrap();
        let config = home.path().join("claude-for-test");
        fs::write(
            project.path().join("agenvil.toml"),
            "schema = 1\n[project]\nname = \"test\"\n",
        )
        .unwrap();
        fs::create_dir(project.path().join("rules")).unwrap();
        fs::write(project.path().join("rules/test.md"), "# Rule\n").unwrap();
        let fixture = Self {
            project,
            home,
            config,
        };
        fixture
            .command()
            .args(["build", "--target", "claude"])
            .assert()
            .success();
        fixture
    }

    fn command(&self) -> assert_cmd::Command {
        let mut command = agenvil_in(self.project.path(), self.home.path());
        command.env("CLAUDE_CONFIG_DIR", &self.config);
        command
    }

    fn sync(&self, options: &[&str]) -> assert_cmd::assert::Assert {
        self.command()
            .args(["sync", "--target", "claude"])
            .args(options)
            .assert()
    }
}

fn tree_digest(path: &Path) -> String {
    fn walk(path: &Path, base: &Path, entries: &mut Vec<(String, String)>) {
        for entry in fs::read_dir(path).unwrap() {
            let path = entry.unwrap().path();
            let rel = path
                .strip_prefix(base)
                .unwrap()
                .to_string_lossy()
                .into_owned();
            let kind = fs::symlink_metadata(&path).unwrap().file_type();
            let identity = if kind.is_symlink() {
                format!("link:{}", fs::read_link(&path).unwrap().display())
            } else if kind.is_dir() {
                walk(&path, base, entries);
                "directory".to_owned()
            } else {
                agenvil::manifest::sha256(&fs::read(&path).unwrap())
            };
            entries.push((rel, identity));
        }
    }
    let mut entries = Vec::new();
    walk(path, path, &mut entries);
    entries.sort();
    agenvil::manifest::sha256(&serde_json::to_vec(&entries).unwrap())
}

#[test]
fn deterministic_human_dry_run_output() {
    let p = CliFixture::built();
    let first = p.sync(&[]).success();
    let second = p.sync(&["--dry-run"]).success();
    assert_eq!(first.get_output().stdout, second.get_output().stdout);
    first.stdout(predicate::str::contains("Create rules/test.md"));
}

#[test]
fn explicit_dry_run_conflicts_with_apply() {
    let p = CliFixture::built();
    p.sync(&["--dry-run", "--apply"])
        .code(2)
        .stderr(predicate::str::contains(
            "the argument '--dry-run' cannot be used with '--apply'",
        ));
}

#[test]
fn deterministic_json_dry_run_output() {
    let p = CliFixture::built();
    let first = p.sync(&["--format", "json"]).success();
    let second = p.sync(&["--format", "json"]).success();
    assert_eq!(first.get_output().stdout, second.get_output().stdout);
    let plan: serde_json::Value = serde_json::from_slice(&first.get_output().stdout).unwrap();
    assert_eq!(plan["harness"], "claude");
    assert_eq!(plan["scope"], "user");
    assert_eq!(plan["operations"][0]["operation"], "create");
    assert_eq!(plan["operations"][0]["destination"], "rules/test.md");
    assert_eq!(plan["summary"]["create"], 1);
}

#[test]
fn dry_run_never_mutates_the_home_tree() {
    let p = CliFixture::built();
    fs::create_dir_all(p.home.path().join(".agents.local/agents")).unwrap();
    fs::write(
        p.home.path().join(".agents.local/agents/private.md"),
        "private content",
    )
    .unwrap();
    fs::write(p.home.path().join("unrelated"), "user content").unwrap();
    let before = tree_digest(p.home.path());
    p.sync(&[]).success();
    p.sync(&["--format", "json"]).success();
    assert_eq!(tree_digest(p.home.path()), before);
    assert!(!p.config.exists());
    assert!(!p.home.path().join(".agents.local/state").exists());
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
#[test]
fn cli_apply_uses_the_explicit_claude_config_directory() {
    let p = CliFixture::built();
    p.sync(&["--apply"]).success();
    assert!(p.config.join("rules/test.md").is_symlink());
    assert!(!p.home.path().join(".claude").exists());
    assert!(
        p.home
            .path()
            .join(".agents.local/state/ownership.json")
            .is_file()
    );
}

#[cfg(windows)]
#[test]
fn windows_apply_is_typed_and_fails_closed() {
    use agenvil::install::{ApplyError, Locations};
    let p = CliFixture::built();
    let root = p.project.path();
    let config = agenvil::config::Config::load(root).unwrap();
    let locations = Locations {
        claude_root: p.config.clone(),
        local_root: p.home.path().join(".agents.local"),
    };
    let before = tree_digest(p.home.path());
    let plan = agenvil::install::plan(
        root,
        &config,
        &root.join(agenvil::build::DEFAULT_OUT),
        &locations,
    )
    .unwrap();
    assert_eq!(plan.summary().create, 1);
    assert_eq!(tree_digest(p.home.path()), before);
    assert!(matches!(
        agenvil::install::apply(&plan),
        Err(ApplyError::Unsupported | ApplyError::CapabilityMissing { .. })
    ));
    p.sync(&["--apply"])
        .code(2)
        .stderr(predicate::str::contains("managed symlink installation"));
    assert_eq!(tree_digest(p.home.path()), before);
}
