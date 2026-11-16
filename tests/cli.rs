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

/// Commands that still report "not implemented" (`inventory` is real now).
const UNIMPLEMENTED: [&[&str]; 4] = [&["check"], &["rules"], &["explain", "SOME-ID"], &["build"]];

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
    for args in UNIMPLEMENTED {
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
    for args in UNIMPLEMENTED {
        agenvil(&dir).args(args).assert().code(4);
    }
    agenvil(&dir).arg("inventory").assert().code(0);
    assert_eq!(entries(&dir), before);
}

mod inventory {
    use super::*;
    use serde_json::{Value, json};
    use std::fs;
    use std::path::Path;

    fn write(dir: &TempDir, path: &str, bytes: impl AsRef<[u8]>) {
        let full = dir.path().join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, bytes).unwrap();
    }

    fn run(dir: &TempDir, code: i32) -> Value {
        let out = agenvil(dir)
            .arg("inventory")
            .assert()
            .code(code)
            .get_output()
            .stdout
            .clone();
        serde_json::from_slice(&out).expect("stdout is JSON")
    }

    fn problems(inv: &Value) -> Vec<(&str, &str)> {
        inv["problems"]
            .as_array()
            .unwrap()
            .iter()
            .map(|p| (p["path"].as_str().unwrap(), p["kind"].as_str().unwrap()))
            .collect()
    }

    #[test]
    fn empty_project_is_an_empty_successful_inventory() {
        let dir = project(VALID);
        let inv = run(&dir, 0);
        assert_eq!(
            inv,
            json!({"schema": 1, "agents": [], "skills": [], "rules": [], "problems": []})
        );
    }

    #[test]
    fn lists_resources_with_frontmatter_and_is_deterministic() {
        let dir = project(VALID);
        write(
            &dir,
            "agents/reviewer.md",
            "\u{feff}---\r\nname: reviewer\r\nmodel: opus\r\neffort: high\r\nskills: [tdd, y]\r\ndescription: |\r\n  one\r\n  two\r\nx-extra: {a: 1}\r\n---\r\n# Body\r\n",
        );
        write(&dir, "skills/tdd/SKILL.md", "---\nname: tdd\n---\nbody\n");
        write(&dir, "rules/style.md", "plain markdown\n");
        let first = agenvil(&dir)
            .arg("inventory")
            .assert()
            .code(0)
            .get_output()
            .stdout
            .clone();
        let second = agenvil(&dir)
            .args(["inventory", "--format", "json"])
            .assert()
            .code(0)
            .get_output()
            .stdout
            .clone();
        assert_eq!(first, second);

        let inv: Value = serde_json::from_slice(&first).unwrap();
        let agent = &inv["agents"][0];
        assert_eq!(agent["name"], "reviewer");
        assert_eq!(agent["path"], "agents/reviewer.md");
        assert_eq!(agent["frontmatter"]["start_line"], 1);
        assert_eq!(agent["frontmatter"]["end_line"], 10);
        assert_eq!(agent["body_start_line"], 11);
        let meta = &agent["frontmatter"]["metadata"];
        assert_eq!(meta["model"], "opus");
        assert_eq!(meta["effort"], "high");
        assert_eq!(meta["skills"], json!(["tdd", "y"]));
        assert_eq!(meta["description"], "one\ntwo\n");
        assert_eq!(meta["x-extra"], json!({"a": 1}));
        assert_eq!(inv["skills"][0]["path"], "skills/tdd/SKILL.md");
        assert_eq!(inv["rules"][0]["frontmatter"], Value::Null);
        assert!(inv.get("root").is_none());
    }

    #[test]
    fn runs_read_only_from_outside_the_project_with_root() {
        let proj = project(VALID);
        write(&proj, "agents/a.md", "x");
        let before = entries(&proj);
        let elsewhere = tempfile::tempdir().unwrap();
        let out = agenvil(&elsewhere)
            .arg("--root")
            .arg(proj.path())
            .arg("inventory")
            .assert()
            .code(0)
            .get_output()
            .stdout
            .clone();
        let inv: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(inv["agents"][0]["path"], "agents/a.md");
        assert_eq!(entries(&proj), before);
        assert!(entries(&elsewhere).is_empty());
    }

    #[test]
    fn honors_configured_paths_and_gitignore() {
        let dir = project(&format!("{VALID}[paths]\nagents = \"src/agents\"\n"));
        write(&dir, ".gitignore", "src/agents/draft.md\n");
        write(&dir, "src/agents/draft.md", "x");
        write(&dir, "src/agents/final.md", "x");
        write(&dir, "agents/ignored-default-location.md", "x");
        let inv = run(&dir, 0);
        let paths: Vec<_> = inv["agents"]
            .as_array()
            .unwrap()
            .iter()
            .map(|a| &a["path"])
            .collect();
        assert_eq!(paths, ["src/agents/final.md"]);
    }

    #[test]
    fn malformed_resources_are_reported_and_exit_4_but_good_ones_are_listed() {
        let dir = project(VALID);
        write(&dir, "agents/good.md", "---\nname: g\n---\n");
        write(&dir, "agents/bad.md", "---\nname: [oops\n---\n");
        write(&dir, "agents/binary.md", b"ok\n\xff\xfe");
        write(&dir, "rules/unclosed.md", "---\nname: x\n");
        write(&dir, "skills/nofile/notes.md", "x");
        write(&dir, "agents/huge.md", vec![b'a'; 1024 * 1024 + 1]);
        let out = agenvil(&dir)
            .arg("inventory")
            .assert()
            .code(4)
            .stderr(predicate::str::contains("5 problem(s)"))
            .get_output()
            .stdout
            .clone();
        let inv: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(inv["agents"].as_array().unwrap().len(), 1);
        assert_eq!(
            problems(&inv),
            [
                ("agents/bad.md", "frontmatter"),
                ("agents/binary.md", "invalid-utf8"),
                ("agents/huge.md", "too-large"),
                ("rules/unclosed.md", "frontmatter"),
                ("skills/nofile", "missing-skill-file"),
            ]
        );
        assert_eq!(inv["problems"][0]["line"], 2);
        assert_eq!(inv["problems"][1]["line"], 2);
    }

    #[test]
    fn overlapping_configured_directories_exit_2_without_output() {
        let dir = project(&format!(
            "{VALID}[paths]\nagents = \"x\"\nrules = \"x/y\"\n"
        ));
        agenvil(&dir)
            .arg("inventory")
            .assert()
            .code(2)
            .stdout("")
            .stderr(predicate::str::contains("overlap"));
    }

    #[test]
    fn unknown_format_is_a_usage_error() {
        let dir = project(VALID);
        agenvil(&dir)
            .args(["inventory", "--format", "xml"])
            .assert()
            .code(2)
            .stdout("");
    }

    #[test]
    fn untrusted_content_is_never_executed() {
        let dir = project(VALID);
        let marker = dir.path().join("pwned");
        write(
            &dir,
            "agents/evil.md",
            format!(
                "---\nname: evil\nhooks: \"touch {0}\"\nx: !!python/object/apply:os.system [\"touch {0}\"]\n---\n`touch {0}`\n",
                marker.display()
            ),
        );
        write(&dir, "skills/s/SKILL.md", "---\nname: s\n---\n");
        write(
            &dir,
            "skills/s/run.sh",
            format!("touch {}", marker.display()),
        );
        let _ = agenvil(&dir).arg("inventory").assert();
        assert!(!Path::new(&marker).exists());
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_out_of_the_root_are_reported_and_never_read() {
        use std::os::unix::fs::symlink;
        let outside = tempfile::tempdir().unwrap();
        write(&outside, "secret.md", "---\nname: TOP-SECRET\n---\n");
        write(&outside, "skills/leak/SKILL.md", "TOP-SECRET");
        let dir = project(VALID);
        write(&dir, "agents/real.md", "x");
        symlink(
            outside.path().join("secret.md"),
            dir.path().join("agents/link.md"),
        )
        .unwrap();
        symlink(outside.path().join("skills"), dir.path().join("skills")).unwrap();
        symlink(outside.path(), dir.path().join("rules")).unwrap();
        let out = agenvil(&dir)
            .arg("inventory")
            .assert()
            .code(4)
            .get_output()
            .stdout
            .clone();
        let text = String::from_utf8(out.clone()).unwrap();
        assert!(!text.contains("TOP-SECRET"));
        let inv: Value = serde_json::from_slice(&out).unwrap();
        assert_eq!(
            problems(&inv),
            [
                ("agents/link.md", "symlink"),
                ("rules", "symlink"),
                ("skills", "symlink"),
            ]
        );
        assert_eq!(inv["agents"].as_array().unwrap().len(), 1);
    }

    #[cfg(unix)]
    #[test]
    fn a_symlinked_agenvil_toml_is_still_refused() {
        use std::os::unix::fs::symlink;
        let outside = project(VALID);
        let dir = tempfile::tempdir().unwrap();
        symlink(
            outside.path().join("agenvil.toml"),
            dir.path().join("agenvil.toml"),
        )
        .unwrap();
        agenvil(&dir).arg("inventory").assert().code(2).stdout("");
    }

    #[cfg(unix)]
    #[test]
    fn non_utf8_file_names_are_reported_not_silently_dropped() {
        use std::ffi::OsString;
        use std::os::unix::ffi::OsStringExt;
        let dir = project(VALID);
        write(&dir, "agents/ok.md", "x");
        fs::create_dir(dir.path().join("rules")).unwrap();
        let bad = OsString::from_vec(b"bad\xff.md".to_vec());
        // APFS and some other filesystems refuse non-UTF-8 names, so there is nothing to find.
        if fs::write(dir.path().join("agents").join(&bad), "x").is_err() {
            return;
        }
        fs::write(dir.path().join("rules").join(&bad), "x").unwrap();
        let inv = run(&dir, 4);
        assert_eq!(
            problems(&inv),
            [
                ("agents/bad\u{fffd}.md", "invalid-path"),
                ("rules/bad\u{fffd}.md", "invalid-path"),
            ]
        );
        assert_eq!(inv["agents"].as_array().unwrap().len(), 1);
    }
}
