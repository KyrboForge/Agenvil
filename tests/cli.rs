//! Runs the real `agenvil` binary from temporary directories outside this repository.

use assert_cmd::Command;
use predicates::prelude::*;
use tempfile::TempDir;

const VALID: &str = "schema = 1\n[project]\nname = \"demo\"\n";
const SUBCOMMANDS: [&[&str]; 5] = [
    &["check"],
    &["inventory"],
    &["rules"],
    &["explain", "SOME-ID"],
    &["build"],
];

fn agenvil(cwd: &TempDir) -> Command {
    let mut cmd = Command::cargo_bin("agenvil").unwrap();
    cmd.current_dir(cwd.path());
    cmd
}

fn project(config: &str) -> TempDir {
    let dir = tempfile::tempdir().unwrap();
    std::fs::write(dir.path().join("agenvil.toml"), config).unwrap();
    dir
}

fn entries(dir: &TempDir) -> Vec<String> {
    let mut names: Vec<_> = std::fs::read_dir(dir.path())
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    names
}

#[test]
fn version_succeeds() {
    agenvil(&tempfile::tempdir().unwrap())
        .arg("--version")
        .assert()
        .code(0)
        .stdout(predicate::str::starts_with(format!(
            "agenvil {}",
            env!("CARGO_PKG_VERSION")
        )));
}

#[test]
fn help_lists_commands_and_root_option() {
    let out = agenvil(&tempfile::tempdir().unwrap())
        .arg("--help")
        .assert()
        .code(0)
        .get_output()
        .stdout
        .clone();
    let help = String::from_utf8(out).unwrap();
    for needle in [
        "check",
        "inventory",
        "rules",
        "explain",
        "build",
        "--root <DIR>",
    ] {
        assert!(help.contains(needle), "help is missing {needle:?}:\n{help}");
    }
}

#[test]
fn usage_errors_exit_2() {
    let dir = tempfile::tempdir().unwrap();
    for args in [&[][..], &["bogus"], &["explain"], &["check", "--bogus"]] {
        agenvil(&dir).args(args).assert().code(2);
    }
}

#[test]
fn missing_config_exits_2_and_names_the_file() {
    let dir = tempfile::tempdir().unwrap();
    agenvil(&dir)
        .arg("check")
        .assert()
        .code(2)
        .stderr(predicate::str::contains("agenvil.toml"));
}

#[test]
fn invalid_config_exits_2_for_every_command() {
    for (config, message) in [
        ("[project]\nname = \"x\"\n", "missing required key `schema`"),
        (
            "schema = 2\n[project]\nname = \"x\"\n",
            "unsupported schema version 2",
        ),
        (
            "schema = 1\n[project]\nname = \"x\"\nbogus = 1\n",
            "unknown field `bogus`",
        ),
        (
            "schema = 1\n[project]\nname = \"x\"\n[paths]\nagents = \"../out\"\n",
            "stay inside",
        ),
    ] {
        let dir = project(config);
        for args in SUBCOMMANDS {
            agenvil(&dir)
                .args(args)
                .assert()
                .code(2)
                .stderr(predicate::str::contains(message));
        }
    }
}

#[test]
fn valid_config_is_never_reported_as_clean() {
    let dir = project(VALID);
    for args in SUBCOMMANDS {
        agenvil(&dir)
            .args(args)
            .assert()
            .code(4)
            .stdout("")
            .stderr(predicate::str::contains("not implemented yet"));
    }
}

#[test]
fn root_is_cwd_by_default_or_explicit_from_anywhere() {
    let proj = project(VALID);
    let elsewhere = tempfile::tempdir().unwrap();
    // cwd without a config: error, even though another directory has one.
    agenvil(&elsewhere).arg("check").assert().code(2);
    // explicit root, run from outside the project.
    agenvil(&elsewhere)
        .args(["check", "--root"])
        .arg(proj.path())
        .assert()
        .code(4);
    agenvil(&elsewhere)
        .arg("--root")
        .arg(proj.path())
        .arg("check")
        .assert()
        .code(4);
    // default root is the working directory.
    agenvil(&proj).arg("check").assert().code(4);
}

#[test]
fn commands_leave_the_project_untouched() {
    let dir = project(VALID);
    let before = entries(&dir);
    for args in SUBCOMMANDS {
        agenvil(&dir).args(args).assert().code(4);
    }
    assert_eq!(entries(&dir), before);
}
