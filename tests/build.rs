//! `agenvil build --target claude`: the staging tree, its manifest and the safety contract.
//! Every project is a temp directory with its own HOME; nothing reads the real one.

mod common;

use std::fs;
use std::path::Path;

use common::{agenvil_in, snapshot};
use predicates::prelude::*;
use serde_json::Value;
use tempfile::TempDir;

const CONFIG: &str = "schema = 1\n\n[project]\nname = \"demo\"\n";
const OUT: &str = ".generated/claude";

const AGENT: &str = "---\nname: reviewer\ndescription: Reviews diffs.\ntools: Read, Grep\ncolor: blue\n---\nBody.\n";
const SKILL: &str = "---\nname: tdd\ndescription: Test first.\n---\n# TDD\n";

struct Project {
    dir: TempDir,
    home: TempDir,
}

impl Project {
    /// A project with `agenvil.toml` (unless a file named so is given) and `files`.
    fn new(files: &[(&str, &[u8])]) -> Self {
        let project = Self {
            dir: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
        };
        if !files.iter().any(|(p, _)| *p == "agenvil.toml") {
            project.write("agenvil.toml", CONFIG.as_bytes());
        }
        for (path, bytes) in files {
            project.write(path, bytes);
        }
        project
    }

    fn write(&self, path: &str, bytes: &[u8]) {
        let full = self.dir.path().join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, bytes).unwrap();
    }

    fn path(&self, rel: &str) -> std::path::PathBuf {
        self.dir.path().join(rel)
    }

    fn build(&self) -> assert_cmd::assert::Assert {
        self.build_to(None)
    }

    fn build_to(&self, out: Option<&str>) -> assert_cmd::assert::Assert {
        let mut cmd = agenvil_in(self.dir.path(), self.home.path());
        cmd.args(["build", "--target", "claude"]);
        if let Some(out) = out {
            cmd.args(["--out", out]);
        }
        cmd.assert()
    }

    fn manifest(&self) -> Value {
        let text = fs::read_to_string(self.path(OUT).join("manifest.json")).unwrap();
        serde_json::from_str(&text).unwrap()
    }

    fn staged(&self, rel: &str) -> Vec<u8> {
        fs::read(self.path(OUT).join(rel)).unwrap()
    }
}

fn manifest_paths(manifest: &Value) -> Vec<&str> {
    manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .map(|f| f["path"].as_str().unwrap())
        .collect()
}

fn file_entry<'a>(manifest: &'a Value, path: &str) -> &'a Value {
    manifest["files"]
        .as_array()
        .unwrap()
        .iter()
        .find(|f| f["path"] == path)
        .unwrap()
}

#[test]
fn minimal_standalone_project_builds_without_any_tak_files() {
    let p = Project::new(&[
        ("agents/reviewer.md", AGENT.as_bytes()),
        ("skills/tdd/SKILL.md", SKILL.as_bytes()),
    ]);
    // No POLICY.md, config/defaults.toml, kit:model-table or bin/ anywhere.
    assert!(!p.path("POLICY.md").exists());
    p.build()
        .success()
        .stdout(predicate::str::contains(
            "built 1 agent(s), 1 skill(s) and 0 rule(s) as 2 file(s)",
        ))
        .stdout(predicate::str::contains("staging only"));

    assert_eq!(p.staged(".claude/agents/reviewer.md"), AGENT.as_bytes());
    assert_eq!(p.staged(".claude/skills/tdd/SKILL.md"), SKILL.as_bytes());
    let manifest = p.manifest();
    assert_eq!(manifest["schema"], 1);
    assert_eq!(manifest["target"], "claude");
    assert_eq!(manifest["owner"], "agenvil");
    assert_eq!(
        manifest_paths(&manifest),
        [".claude/agents/reviewer.md", ".claude/skills/tdd/SKILL.md"]
    );
}

#[test]
fn skill_without_model_or_effort_is_a_standard_skill() {
    let p = Project::new(&[("skills/tdd/SKILL.md", SKILL.as_bytes())]);
    p.build().success();
    assert_eq!(p.staged(".claude/skills/tdd/SKILL.md"), SKILL.as_bytes());
    assert_eq!(
        file_entry(&p.manifest(), ".claude/skills/tdd/SKILL.md")["transform"],
        "copy"
    );
}

#[test]
fn skill_directory_is_copied_byte_for_byte_with_nested_files() {
    let binary: Vec<u8> = (0..=255u8).chain([0, 13, 10, 0xff]).collect();
    let p = Project::new(&[
        ("skills/tdd/SKILL.md", SKILL.as_bytes()),
        ("skills/tdd/assets/img/logo.png", &binary),
        ("skills/tdd/references/deep/notes.md", b"crlf\r\nline\r\n"),
        ("skills/tdd/scripts/run.sh", b"#!/bin/sh\necho hi\n"),
    ]);
    p.build().success();
    let base = ".claude/skills/tdd";
    assert_eq!(p.staged(&format!("{base}/assets/img/logo.png")), binary);
    assert_eq!(
        p.staged(&format!("{base}/references/deep/notes.md")),
        b"crlf\r\nline\r\n"
    );
    assert_eq!(
        p.staged(&format!("{base}/scripts/run.sh")),
        b"#!/bin/sh\necho hi\n"
    );
}

#[cfg(unix)]
#[test]
fn script_keeps_its_executable_bit_and_nothing_else_does() {
    use std::os::unix::fs::PermissionsExt;
    let p = Project::new(&[
        ("skills/tdd/SKILL.md", SKILL.as_bytes()),
        ("skills/tdd/scripts/run.sh", b"#!/bin/sh\n"),
        ("skills/tdd/notes.md", b"x"),
    ]);
    let mode = |rel: &str| fs::metadata(p.path(rel)).unwrap().permissions().mode();
    fs::set_permissions(
        p.path("skills/tdd/scripts/run.sh"),
        fs::Permissions::from_mode(0o700),
    )
    .unwrap();
    fs::set_permissions(
        p.path("skills/tdd/notes.md"),
        fs::Permissions::from_mode(0o666),
    )
    .unwrap();
    p.build().success();
    assert_eq!(
        mode(".generated/claude/.claude/skills/tdd/scripts/run.sh") & 0o777,
        0o755
    );
    assert_eq!(
        mode(".generated/claude/.claude/skills/tdd/notes.md") & 0o111,
        0
    );
}

#[test]
fn manifest_records_source_sha256_and_size() {
    let p = Project::new(&[
        ("skills/tdd/SKILL.md", SKILL.as_bytes()),
        ("skills/tdd/references/a.txt", b"abc"),
    ]);
    p.build().success();
    let manifest = p.manifest();
    let entry = file_entry(&manifest, ".claude/skills/tdd/references/a.txt");
    assert_eq!(entry["source"], "skills/tdd/references/a.txt");
    assert_eq!(entry["bytes"], 3);
    assert_eq!(
        entry["sha256"],
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(entry["transform"], "copy");
    // `manifest.json` describes the other files, not itself.
    assert!(!manifest_paths(&manifest).contains(&"manifest.json"));
}

#[test]
fn builds_are_deterministic_and_rebuilds_replace_the_owned_output() {
    let p = Project::new(&[
        ("agents/b.md", b"---\nname: b\ndescription: B.\n---\n"),
        ("agents/a.md", b"---\nname: a\ndescription: A.\n---\n"),
        ("skills/tdd/SKILL.md", SKILL.as_bytes()),
        ("rules/rust.md", b"---\npaths:\n  - \"src/**\"\n---\nFmt.\n"),
    ]);
    p.build().success();
    let first = snapshot(&p.path(OUT));
    p.build().success();
    assert_eq!(snapshot(&p.path(OUT)), first);
    // Sorted, portable, `/`-separated.
    assert_eq!(
        manifest_paths(&p.manifest()),
        [
            ".claude/agents/a.md",
            ".claude/agents/b.md",
            ".claude/rules/rust.md",
            ".claude/skills/tdd/SKILL.md"
        ]
    );
    // A source that is gone also disappears from the replaced output.
    fs::remove_file(p.path("agents/b.md")).unwrap();
    p.build().success();
    assert!(!p.path(".generated/claude/.claude/agents/b.md").exists());
    // No temp or backup directory is left next to the output.
    let left: Vec<_> = fs::read_dir(p.path(".generated"))
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(left, ["claude"]);
}

#[test]
fn rules_are_emitted_as_native_rule_files() {
    let rule = "---\npaths:\n  - \"src/**/*.rs\"\n---\nUse rustfmt.\n";
    let p = Project::new(&[("rules/rust.md", rule.as_bytes())]);
    p.build().success();
    assert_eq!(p.staged(".claude/rules/rust.md"), rule.as_bytes());
    // No CLAUDE.md source exists in the Agenvil schema, so none is invented.
    assert!(!p.path(".generated/claude/CLAUDE.md").exists());
    assert!(!p.path(".generated/claude/.claude/CLAUDE.md").exists());
}

#[test]
fn agent_preloads_skills_natively_and_keeps_unrelated_metadata() {
    let agent = "---\nname: reviewer\ndescription: Reviews.\nskills:\n  - tdd\n  - commit\nx-team: platform\nmcpServers: {}\n---\nBody.\n";
    let p = Project::new(&[
        ("agents/reviewer.md", agent.as_bytes()),
        ("skills/tdd/SKILL.md", SKILL.as_bytes()),
        (
            "skills/commit/SKILL.md",
            b"---\nname: commit\ndescription: Commit.\n---\n",
        ),
    ]);
    p.build().success();
    // The reference stays the native `skills:` list; unknown fields are kept untouched.
    assert_eq!(p.staged(".claude/agents/reviewer.md"), agent.as_bytes());
}

#[test]
fn missing_duplicate_and_malformed_skill_references_fail_the_build() {
    for (skills, message) in [
        ("skills:\n  - ghost\n", "`ghost`, which is not a skill"),
        ("skills:\n  - tdd\n  - tdd\n", "`tdd` twice"),
        ("skills: tdd\n", "must be a YAML list"),
        ("skills:\n  - 3\n", "is not a string"),
    ] {
        let agent = format!("---\nname: a\ndescription: A.\n{skills}---\n");
        let p = Project::new(&[
            ("agents/a.md", agent.as_bytes()),
            ("skills/tdd/SKILL.md", SKILL.as_bytes()),
        ]);
        p.build()
            .code(1)
            .stderr(predicate::str::contains("agents/a.md"))
            .stderr(predicate::str::contains(message));
        assert!(!p.path(".generated").exists(), "{skills}");
    }
}

#[test]
fn agent_needs_native_name_and_description_and_a_unique_name() {
    let p = Project::new(&[("agents/a.md", b"---\nname: a\n---\nBody\n")]);
    p.build()
        .code(1)
        .stderr(predicate::str::contains("`description` must be"));

    let p = Project::new(&[
        ("agents/a.md", b"---\nname: same\ndescription: A.\n---\n"),
        ("agents/b.md", b"---\nname: same\ndescription: B.\n---\n"),
    ]);
    p.build().code(1).stderr(predicate::str::contains(
        "agent name `same` is also used by",
    ));
}

#[test]
fn skill_name_must_match_its_directory() {
    let p = Project::new(&[(
        "skills/tdd/SKILL.md",
        b"---\nname: other\ndescription: X.\n---\n",
    )]);
    p.build()
        .code(1)
        .stderr(predicate::str::contains("must equal the directory name"));
}

#[test]
fn concrete_and_partial_model_and_effort_are_kept_as_written() {
    let both = "---\nname: a\ndescription: A.\nmodel: opus\neffort: high\n---\n";
    let model_only = "---\nname: b\ndescription: B.\nmodel: sonnet\n---\n";
    let effort_only = "---\nname: c\ndescription: C.\neffort: low\n---\n";
    let inherit = "---\nname: d\ndescription: D.\nmodel: inherit\n---\n";
    let skill = "---\nname: tdd\ndescription: T.\nmodel: haiku\neffort: low\n---\n";
    let p = Project::new(&[
        ("agents/a.md", both.as_bytes()),
        ("agents/b.md", model_only.as_bytes()),
        ("agents/c.md", effort_only.as_bytes()),
        ("agents/d.md", inherit.as_bytes()),
        ("skills/tdd/SKILL.md", skill.as_bytes()),
    ]);
    p.build().success();
    assert_eq!(p.staged(".claude/agents/a.md"), both.as_bytes());
    assert_eq!(p.staged(".claude/agents/b.md"), model_only.as_bytes());
    assert_eq!(p.staged(".claude/agents/c.md"), effort_only.as_bytes());
    assert_eq!(p.staged(".claude/agents/d.md"), inherit.as_bytes());
    assert_eq!(p.staged(".claude/skills/tdd/SKILL.md"), skill.as_bytes());
}

#[test]
fn explicit_override_rewrites_only_model_and_effort() {
    let config = "schema = 1\n[project]\nname = \"x\"\n\n[policy.models.claude]\nmodels = [\"opus\"]\nefforts = [\"max\"]\npairs = [[\"opus\", \"max\"]]\n\n[[targets.claude.map]]\nsource = [\"opus\", \"max\"]\ntarget = [\"opus\", \"xhigh\"]\n";
    let agent = "---\nname: a\ndescription: Keeps  spacing.\nmodel: opus\neffort: max\ncolor: blue\n---\nBody\n";
    let p = Project::new(&[
        ("agenvil.toml", config.as_bytes()),
        ("agents/a.md", agent.as_bytes()),
        ("skills/tdd/SKILL.md", SKILL.as_bytes()),
    ]);
    p.build().success();
    assert_eq!(
        String::from_utf8(p.staged(".claude/agents/a.md")).unwrap(),
        agent.replace("effort: max", "effort: xhigh")
    );
    let manifest = p.manifest();
    let entry = file_entry(&manifest, ".claude/agents/a.md");
    assert_eq!(entry["transform"], "model-effort-override");
    assert_eq!(entry["source"], "agents/a.md");
    // The hash is of what was written, so drift detection compares like with like.
    assert_ne!(entry["bytes"], agent.len());
    assert_eq!(
        file_entry(&manifest, ".claude/skills/tdd/SKILL.md")["transform"],
        "copy"
    );
}

#[test]
fn unsupported_pair_fails_instead_of_falling_back() {
    // The default policy does not allow `haiku/max`; the build must not guess a substitute.
    let agent = "---\nname: a\ndescription: A.\nmodel: haiku\neffort: max\n---\n";
    let p = Project::new(&[("agents/a.md", agent.as_bytes())]);
    p.build()
        .code(1)
        .stderr(predicate::str::contains("agents/a.md"))
        .stderr(predicate::str::contains("haiku/max"));
    assert!(!p.path(".generated").exists());
}

#[test]
fn unsupported_and_disabled_targets_are_rejected() {
    let p = Project::new(&[("agents/a.md", b"---\nname: a\ndescription: A.\n---\n")]);
    agenvil_in(p.dir.path(), p.home.path())
        .args(["build", "--target", "codex"])
        .assert()
        .code(2)
        .stderr(predicate::str::contains("claude"));
    agenvil_in(p.dir.path(), p.home.path())
        .arg("build")
        .assert()
        .code(2);

    let config = "schema = 1\n[project]\nname = \"x\"\n[targets.claude]\nenabled = false\n";
    let p = Project::new(&[("agenvil.toml", config.as_bytes())]);
    p.build()
        .code(2)
        .stderr(predicate::str::contains("target is disabled"));
    assert!(!p.path(".generated").exists());
}

#[test]
fn out_directory_is_configurable() {
    let p = Project::new(&[("agents/a.md", b"---\nname: a\ndescription: A.\n---\n")]);
    p.build_to(Some("stage/here")).success();
    assert!(p.path("stage/here/.claude/agents/a.md").is_file());
    assert!(p.path("stage/here/manifest.json").is_file());
    assert!(!p.path(".generated").exists());
}

#[test]
fn case_variants_collide_before_anything_is_written() {
    let p = Project::new(&[("agents/Foo.md", b"---\nname: Foo\ndescription: A.\n---\n")]);
    p.write("agents/foo.md", b"---\nname: foo\ndescription: B.\n---\n");
    // macOS and Windows file systems fold both spellings into one source file.
    let both = fs::read_dir(p.path("agents")).unwrap().count() == 2;
    if !both {
        return;
    }
    p.build()
        .code(1)
        .stderr(predicate::str::contains("case-insensitive"));
    assert!(!p.path(".generated").exists());
}

#[cfg(unix)]
#[test]
fn symlinks_in_skills_are_rejected_not_followed() {
    use std::os::unix::fs::symlink;
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret"), "secret").unwrap();
    for target in [outside.path().join("secret"), "../../agenvil.toml".into()] {
        let p = Project::new(&[("skills/tdd/SKILL.md", SKILL.as_bytes())]);
        symlink(&target, p.path("skills/tdd/link")).unwrap();
        p.build()
            .code(4)
            .stderr(predicate::str::contains("skills/tdd/link"))
            .stderr(predicate::str::contains("symlink"));
        assert!(!p.path(".generated").exists());
    }
}

#[cfg(unix)]
#[test]
fn symlinked_skill_directory_and_sources_are_rejected() {
    use std::os::unix::fs::symlink;
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("SKILL.md"), SKILL).unwrap();
    let p = Project::new(&[]);
    fs::create_dir_all(p.path("skills")).unwrap();
    symlink(outside.path(), p.path("skills/tdd")).unwrap();
    p.build().code(4);
    assert!(!p.path(".generated").exists());
}

#[test]
fn unsafe_output_directories_are_refused() {
    let p = Project::new(&[("agents/a.md", b"---\nname: a\ndescription: A.\n---\n")]);
    // Traversal.
    p.build_to(Some("../escape"))
        .code(2)
        .stderr(predicate::str::contains("`..` is not allowed"));
    // The Claude configuration directory, spelled directly and through a parent.
    let claude = p.home.path().join(".claude/staging");
    p.build_to(Some(claude.to_str().unwrap()))
        .code(2)
        .stderr(predicate::str::contains(
            "never write in or around the Claude configuration",
        ));
    p.build_to(Some(p.home.path().join(".claude").to_str().unwrap()))
        .code(2);
    // HOME itself contains the configuration directory.
    p.build_to(Some(p.home.path().to_str().unwrap())).code(2);
    // Inside a source directory the output would be copied into itself.
    p.build_to(Some("agents/out"))
        .code(2)
        .stderr(predicate::str::contains("inside a source directory"));
    assert!(!p.path("agents/out").exists());
    // The project itself.
    p.build_to(Some(".")).code(2);
    assert!(!p.home.path().join(".claude").exists());
    assert!(!p.path(".generated").exists());
}

#[test]
fn build_never_touches_home() {
    let p = Project::new(&[
        ("agents/a.md", b"---\nname: a\ndescription: A.\n---\n"),
        ("skills/tdd/SKILL.md", SKILL.as_bytes()),
    ]);
    fs::create_dir_all(p.home.path().join(".claude/agents")).unwrap();
    fs::write(p.home.path().join(".claude/agents/mine.md"), "mine").unwrap();
    let before = snapshot(p.home.path());
    p.build().success();
    assert_eq!(snapshot(p.home.path()), before);
}

#[test]
fn existing_unowned_output_is_never_overwritten_or_deleted() {
    let p = Project::new(&[("agents/a.md", b"---\nname: a\ndescription: A.\n---\n")]);
    // A directory that holds someone else's files.
    p.write("stage/notes.txt", b"mine");
    p.build_to(Some("stage"))
        .code(2)
        .stderr(predicate::str::contains("no Agenvil manifest.json"));
    assert_eq!(fs::read(p.path("stage/notes.txt")).unwrap(), b"mine");

    // A file where the directory should be.
    p.write("file-out", b"mine");
    p.build_to(Some("file-out")).code(2);
    assert_eq!(fs::read(p.path("file-out")).unwrap(), b"mine");

    // An owned output that a user added a file to.
    p.build().success();
    p.write(".generated/claude/.claude/agents/extra.md", b"mine");
    p.build()
        .code(2)
        .stderr(predicate::str::contains("was not written by Agenvil"));
    assert_eq!(
        fs::read(p.path(".generated/claude/.claude/agents/extra.md")).unwrap(),
        b"mine"
    );
    // A manifest of another tool is not ours either.
    p.write(
        "other/manifest.json",
        br#"{"schema":1,"target":"claude","owner":"someone","files":[]}"#,
    );
    p.build_to(Some("other")).code(2);

    // An empty directory is fine.
    fs::create_dir_all(p.path("empty")).unwrap();
    p.build_to(Some("empty")).success();
}

#[test]
fn failed_build_leaves_previous_output_and_no_partial_tree() {
    let p = Project::new(&[("agents/a.md", b"---\nname: a\ndescription: A.\n---\n")]);
    p.build().success();
    let good = snapshot(&p.path(".generated"));

    // A source that breaks the build after others were valid.
    p.write(
        "agents/z.md",
        b"---\nname: z\ndescription: Z.\nskills:\n  - ghost\n---\n",
    );
    p.build().code(1);
    assert_eq!(snapshot(&p.path(".generated")), good);

    // An incomplete inventory (a skill file that is not text) is not a success either.
    fs::remove_file(p.path("agents/z.md")).unwrap();
    p.write("skills/tdd/SKILL.md", &[0xff, 0xfe, 0x00]);
    p.build().code(4);
    assert_eq!(snapshot(&p.path(".generated")), good);
}

#[cfg(unix)]
#[test]
fn foreign_names_in_an_owned_output_block_replacement() {
    use std::os::unix::ffi::OsStrExt;
    let p = Project::new(&[("agents/a.md", b"---\nname: a\ndescription: A.\n---\n")]);
    p.build().success();
    // A name that is not UTF-8 can never be listed in the manifest.
    let odd = std::ffi::OsStr::from_bytes(b"caf\xe9.md");
    // shortcut: APFS refuses such names, so the test only runs where the file system allows them.
    if fs::write(p.path(".generated/claude/.claude/agents").join(odd), "mine").is_err() {
        return;
    }
    p.build()
        .code(2)
        .stderr(predicate::str::contains("was not written by Agenvil"));
    assert!(
        p.path(".generated/claude/.claude/agents")
            .join(odd)
            .exists()
    );
}

#[cfg(unix)]
#[test]
fn a_manifest_that_is_not_a_regular_file_is_never_opened() {
    let p = Project::new(&[("agents/a.md", b"---\nname: a\ndescription: A.\n---\n")]);
    fs::create_dir_all(p.path("stage")).unwrap();
    let fifo = p.path("stage/manifest.json");
    let made = std::process::Command::new("mkfifo").arg(&fifo).status();
    if !made.is_ok_and(|s| s.success()) {
        return;
    }
    // Opening a FIFO for reading would block forever.
    p.build_to(Some("stage"))
        .code(2)
        .stderr(predicate::str::contains("no Agenvil manifest.json"));
}

#[test]
fn entry_names_that_are_not_portable_fail_the_build() {
    let p = Project::new(&[("agents/a.md", b"---\nname: a\ndescription: A.\n---\n")]);
    p.write("rules/a:b.md", b"rule\n");
    p.build().failure();
    assert!(!p.path(".generated").exists());
}

#[test]
fn the_byte_budget_covers_all_skills_together() {
    let files: Vec<(String, Vec<u8>)> = (0..5)
        .map(|i| {
            let body = SKILL.replace("tdd", &format!("s{i}"));
            (format!("skills/s{i}/SKILL.md"), body.into_bytes())
        })
        .collect();
    let refs: Vec<(&str, &[u8])> = files
        .iter()
        .map(|(p, b)| (p.as_str(), b.as_slice()))
        .collect();
    let p = Project::new(&refs);
    // Each file is within the per-file limit; together they exceed the 64 MiB total.
    for i in 0..5 {
        let big = fs::File::create(p.path(&format!("skills/s{i}/big.bin"))).unwrap();
        big.set_len(16 * 1024 * 1024).unwrap();
    }
    p.build()
        .code(4)
        .stderr(predicate::str::contains("total limit"));
    assert!(!p.path(".generated").exists());
}

#[test]
fn oversized_skill_asset_makes_the_build_incomplete() {
    let p = Project::new(&[("skills/tdd/SKILL.md", SKILL.as_bytes())]);
    let big = fs::File::create(p.path("skills/tdd/big.bin")).unwrap();
    big.set_len(17 * 1024 * 1024).unwrap();
    p.build()
        .code(4)
        .stderr(predicate::str::contains("skills/tdd/big.bin"));
    assert!(!p.path(".generated").exists());
}

#[test]
fn the_standalone_example_builds_as_documented() {
    let example = Path::new(env!("CARGO_MANIFEST_DIR")).join("examples/standalone");
    let p = Project::new(&[]);
    copy(&example, p.dir.path());
    p.build().success();
    let manifest = p.manifest();
    assert_eq!(
        manifest_paths(&manifest),
        [
            ".claude/agents/helper.md",
            ".claude/agents/reviewer.md",
            ".claude/rules/rust.md",
            ".claude/skills/commit/SKILL.md",
            ".claude/skills/tdd/SKILL.md",
            ".claude/skills/tdd/assets/logo.png",
            ".claude/skills/tdd/references/cycle.md",
            ".claude/skills/tdd/scripts/run.sh",
        ]
    );
    for entry in manifest["files"].as_array().unwrap() {
        let source = fs::read(p.path(entry["source"].as_str().unwrap())).unwrap();
        assert_eq!(p.staged(entry["path"].as_str().unwrap()), source);
    }
}

fn copy(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}
