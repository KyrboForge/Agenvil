//! Portable project instructions: opt-in copying, input safety, drift and ownership.
//! Every CLI invocation uses a temporary project and a separate, empty HOME.

mod common;

use std::fs;
use std::path::PathBuf;

use agenvil::manifest::{self, File, Manifest, Transform};
use common::{agenvil_in, snapshot};
use predicates::prelude::*;
use tempfile::TempDir;

const CONFIG: &str = "schema = 1\n\n[project]\nname = \"my-agents\"\n";
const OUT: &str = ".generated/claude";
const AGENT: &str = "---\nname: reviewer\ndescription: Reviews changes.\n---\nReview carefully.\n";
const SKILL: &str = "---\nname: tdd\ndescription: Test first.\n---\n# TDD\n";
const RULE: &str = "# Style\n\nKeep changes focused.\n";
const INSTRUCTIONS: &str =
    "# Project instructions\r\n\r\nZażółć gęślą jaźń 🦀\r\nNo trailing newline";

struct Project {
    dir: TempDir,
    home: TempDir,
}

impl Project {
    fn new(source: Option<&str>) -> Self {
        let p = Self {
            dir: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
        };
        p.configure(source);
        p.write("agents/reviewer.md", AGENT.as_bytes());
        p.write("skills/tdd/SKILL.md", SKILL.as_bytes());
        p.write("rules/style.md", RULE.as_bytes());
        p
    }

    fn configured() -> Self {
        let p = Self::new(Some("instructions.md"));
        p.write("instructions.md", INSTRUCTIONS.as_bytes());
        p
    }

    fn built() -> Self {
        let p = Self::configured();
        p.run(&[]).success();
        p
    }

    fn configure(&self, source: Option<&str>) {
        let paths = source.map_or_else(String::new, |source| {
            format!("\n[paths]\ninstructions = {source:?}\n")
        });
        self.write("agenvil.toml", format!("{CONFIG}{paths}").as_bytes());
    }

    fn path(&self, path: &str) -> PathBuf {
        self.dir.path().join(path)
    }

    fn out(&self) -> PathBuf {
        self.path(OUT)
    }

    fn write(&self, path: &str, bytes: &[u8]) {
        let full = self.path(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, bytes).unwrap();
    }

    fn run_to(&self, flags: &[&str], out: &str) -> assert_cmd::assert::Assert {
        let mut cmd = agenvil_in(self.dir.path(), self.home.path());
        cmd.args(["build", "--target", "claude", "--out", out])
            .args(flags);
        let result = cmd.assert();
        assert_eq!(snapshot(self.home.path()), [], "HOME was modified");
        result
    }

    fn run(&self, flags: &[&str]) -> assert_cmd::assert::Assert {
        self.run_to(flags, OUT)
    }

    fn read_only(&self, flags: &[&str]) -> assert_cmd::assert::Assert {
        let before = snapshot(self.dir.path());
        let result = self.run(flags);
        assert_eq!(snapshot(self.dir.path()), before, "tree was modified");
        result
    }

    fn manifest(&self) -> Manifest {
        Manifest::parse(&fs::read(self.out().join(manifest::FILE)).unwrap()).unwrap()
    }

    fn check(&self) -> assert_cmd::assert::Assert {
        let result = agenvil_in(self.dir.path(), self.home.path())
            .arg("check")
            .assert();
        assert_eq!(snapshot(self.home.path()), [], "HOME was modified");
        result
    }

    fn rejects_source(&self, code: i32, message: &str) {
        self.run(&[]).code(code).stderr(
            predicate::str::contains("instructions").and(predicate::str::contains(message)),
        );
        self.check().code(code).stderr(
            predicate::str::contains("instructions").and(predicate::str::contains(message)),
        );
        assert!(!self.out().exists(), "failed build created output");
    }
}

fn copied(path: &str, source: &str, bytes: &[u8]) -> File {
    File {
        path: path.to_owned(),
        source: source.to_owned(),
        sha256: manifest::sha256(bytes),
        bytes: bytes.len() as u64,
        transform: Transform::Copy,
    }
}

#[test]
fn without_instructions_the_existing_tree_and_manifest_are_unchanged() {
    let p = Project::new(None);
    // A conventional filename alone must never opt a project in.
    p.write("instructions.md", INSTRUCTIONS.as_bytes());
    p.run(&[]).success();
    assert!(!p.out().join("CLAUDE.md").exists());
    let expected = Manifest::new(vec![
        copied(
            ".claude/agents/reviewer.md",
            "agents/reviewer.md",
            AGENT.as_bytes(),
        ),
        copied(".claude/rules/style.md", "rules/style.md", RULE.as_bytes()),
        copied(
            ".claude/skills/tdd/SKILL.md",
            "skills/tdd/SKILL.md",
            SKILL.as_bytes(),
        ),
    ]);
    assert_eq!(
        fs::read_to_string(p.out().join(manifest::FILE)).unwrap(),
        expected.to_json().unwrap()
    );
    let files: Vec<_> = snapshot(&p.out())
        .into_iter()
        .filter(|(path, _)| !path.ends_with('/'))
        .collect();
    assert_eq!(
        files,
        vec![
            (
                ".claude/agents/reviewer.md".to_owned(),
                AGENT.as_bytes().to_vec()
            ),
            (
                ".claude/rules/style.md".to_owned(),
                RULE.as_bytes().to_vec()
            ),
            (
                ".claude/skills/tdd/SKILL.md".to_owned(),
                SKILL.as_bytes().to_vec()
            ),
            (
                manifest::FILE.to_owned(),
                expected.to_json().unwrap().into_bytes()
            ),
        ]
    );
}

#[test]
fn explicit_instructions_record_copy_provenance_digest_bytes_and_sorted_portable_paths() {
    let p = Project::new(Some("project/instructions.md"));
    p.write("project/instructions.md", INSTRUCTIONS.as_bytes());
    p.run(&[]).success();
    let m = p.manifest();
    assert!(m.flaws().is_empty());
    assert_eq!(
        m.files.iter().map(|f| f.path.as_str()).collect::<Vec<_>>(),
        [
            ".claude/agents/reviewer.md",
            ".claude/rules/style.md",
            ".claude/skills/tdd/SKILL.md",
            "CLAUDE.md"
        ]
    );
    assert_eq!(
        m.files.last().unwrap(),
        &copied(
            "CLAUDE.md",
            "project/instructions.md",
            INSTRUCTIONS.as_bytes()
        )
    );
    assert!(
        m.files
            .iter()
            .all(|f| !f.path.contains('\\') && !f.source.contains('\\'))
    );
    assert_eq!(
        fs::read(p.out().join("CLAUDE.md")).unwrap(),
        INSTRUCTIONS.as_bytes()
    );
}

#[test]
fn instructions_preserve_unicode_crlf_and_absence_of_a_trailing_newline() {
    let p = Project::built();
    let bytes = fs::read(p.out().join("CLAUDE.md")).unwrap();
    assert_eq!(bytes, INSTRUCTIONS.as_bytes());
    assert!(bytes.windows(2).any(|pair| pair == b"\r\n"));
    assert!(!bytes.ends_with(b"\n"));
}

#[test]
fn two_builds_produce_identical_instruction_trees_and_manifests() {
    let p = Project::built();
    let first = snapshot(&p.out());
    p.run_to(&[], "second").success();
    assert_eq!(snapshot(&p.path("second")), first);
    p.run(&[]).success();
    assert_eq!(snapshot(&p.out()), first);
}

#[test]
fn valid_instructions_pass_check() {
    let p = Project::configured();
    p.check().success();
    p.run(&[]).success();
}

#[test]
fn missing_configured_instructions_are_actionable_incomplete_input_without_output() {
    let p = Project::new(Some("instructions.md"));
    p.rejects_source(4, "file does not exist");
    p.run(&[]).code(4).stderr(
        predicate::str::contains("paths.instructions").and(predicate::str::contains("missing")),
    );
    assert!(!p.path(".generated").exists());
}

#[test]
fn missing_instructions_preserve_previous_good_output_for_build_check_and_clean() {
    let p = Project::built();
    fs::remove_file(p.path("instructions.md")).unwrap();
    for flags in [&[][..], &["--check"], &["--clean"], &["--clean", "--apply"]] {
        p.read_only(flags)
            .code(4)
            .stderr(predicate::str::contains("file does not exist"));
    }
}

#[test]
fn invalid_utf8_instructions_fail_without_output() {
    let p = Project::configured();
    p.write("instructions.md", b"# Instructions\n\xff\xfe");
    p.rejects_source(4, "UTF-8");
}

#[test]
fn instructions_larger_than_one_mib_fail_without_output() {
    let p = Project::configured();
    p.write("instructions.md", &vec![b'a'; 1024 * 1024 + 1]);
    p.rejects_source(4, "1048576-byte limit");
}

#[test]
fn instructions_at_the_one_mib_limit_are_copied() {
    let p = Project::configured();
    let bytes = vec![b'a'; 1024 * 1024];
    p.write("instructions.md", &bytes);
    p.run(&[]).success();
    assert_eq!(fs::read(p.out().join("CLAUDE.md")).unwrap(), bytes);
}

#[cfg(unix)]
#[test]
fn symlinked_instructions_source_is_rejected() {
    let p = Project::new(Some("instructions.md"));
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("source.md"), INSTRUCTIONS).unwrap();
    std::os::unix::fs::symlink(outside.path().join("source.md"), p.path("instructions.md"))
        .unwrap();
    p.rejects_source(4, "symlink");
    assert_eq!(
        fs::read(outside.path().join("source.md")).unwrap(),
        INSTRUCTIONS.as_bytes()
    );
}

#[cfg(unix)]
#[test]
fn symlinked_parent_of_instructions_source_is_rejected() {
    let p = Project::new(Some("project/instructions.md"));
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("instructions.md"), INSTRUCTIONS).unwrap();
    std::os::unix::fs::symlink(outside.path(), p.path("project")).unwrap();
    p.rejects_source(4, "symlink");
    assert_eq!(
        fs::read(outside.path().join("instructions.md")).unwrap(),
        INSTRUCTIONS.as_bytes()
    );
}

#[test]
fn instructions_source_that_is_a_directory_is_rejected() {
    let p = Project::new(Some("instructions.md"));
    fs::create_dir(p.path("instructions.md")).unwrap();
    p.rejects_source(4, "regular file");
}

#[test]
fn instructions_inside_agents_are_invalid_sources() {
    let p = Project::new(Some("agents/reviewer.md"));
    p.rejects_source(1, "must not be inside the agents, skills or rules");
}

#[test]
fn instructions_inside_rules_are_invalid_sources() {
    let p = Project::new(Some("rules/style.md"));
    p.rejects_source(1, "must not be inside the agents, skills or rules");
}

#[test]
fn instructions_inside_skills_are_invalid_sources() {
    let p = Project::new(Some("skills/tdd/SKILL.md"));
    p.rejects_source(1, "must not be inside the agents, skills or rules");
}

#[test]
fn an_output_directory_containing_the_instructions_source_is_rejected() {
    let p = Project::new(Some("stage/instructions.md"));
    p.write("stage/instructions.md", INSTRUCTIONS.as_bytes());
    for flags in [&[][..], &["--check"], &["--clean"], &["--clean", "--apply"]] {
        let before = snapshot(p.dir.path());
        p.run_to(flags, "stage")
            .code(2)
            .stderr(predicate::str::contains(
                "must not contain the instructions source",
            ));
        assert_eq!(snapshot(p.dir.path()), before);
    }
}

#[test]
fn modified_claude_md_is_changed_and_check_is_read_only() {
    let p = Project::built();
    fs::write(
        p.out().join("CLAUDE.md"),
        INSTRUCTIONS.replace("Project", "Changed"),
    )
    .unwrap();
    p.read_only(&["--check"]).code(1).stdout(
        predicate::str::contains("changed")
            .and(predicate::str::contains("CLAUDE.md"))
            .and(predicate::str::contains("sha256")),
    );
}

#[test]
fn deleted_claude_md_is_missing_and_check_is_read_only() {
    let p = Project::built();
    fs::remove_file(p.out().join("CLAUDE.md")).unwrap();
    p.read_only(&["--check"])
        .code(1)
        .stdout(predicate::str::contains("missing").and(predicate::str::contains("CLAUDE.md")));
}

#[test]
fn removing_the_declaration_reports_stale_instructions() {
    let p = Project::built();
    p.configure(None);
    p.read_only(&["--check"])
        .code(1)
        .stdout(predicate::str::contains("stale").and(predicate::str::contains("CLAUDE.md")));
}

#[test]
fn clean_dry_run_lists_retired_instructions_and_removes_nothing() {
    let p = Project::built();
    p.configure(None);
    p.read_only(&["--clean"]).success().stdout(
        predicate::str::contains("would remove").and(predicate::str::contains("CLAUDE.md")),
    );
}

#[test]
fn clean_apply_deletes_unchanged_managed_instructions_and_rewrites_the_manifest() {
    let p = Project::built();
    p.configure(None);
    let before = snapshot(&p.out());
    p.run(&["--clean", "--apply"])
        .success()
        .stdout(predicate::str::contains("removed").and(predicate::str::contains("CLAUDE.md")));
    assert!(!p.out().join("CLAUDE.md").exists());
    let expected = Manifest::new(p.manifest().files);
    assert!(expected.files.iter().all(|f| f.path != "CLAUDE.md"));
    assert_eq!(expected.files.len(), 3);
    let mut after_cleanup = before;
    after_cleanup.retain(|(path, _)| path != "CLAUDE.md");
    after_cleanup
        .iter_mut()
        .find(|(path, _)| path == manifest::FILE)
        .unwrap()
        .1 = expected.to_json().unwrap().into_bytes();
    assert_eq!(snapshot(&p.out()), after_cleanup);
    p.read_only(&["--check"]).success();
}

#[test]
fn modified_claude_md_survives_cleanup_and_remains_owned() {
    let p = Project::built();
    p.configure(None);
    fs::write(p.out().join("CLAUDE.md"), b"My local changes").unwrap();
    p.read_only(&["--clean", "--apply"])
        .success()
        .stdout(predicate::str::contains("kept").and(predicate::str::contains("CLAUDE.md")));
    assert_eq!(
        fs::read(p.out().join("CLAUDE.md")).unwrap(),
        b"My local changes"
    );
    assert!(p.manifest().files.iter().any(|f| f.path == "CLAUDE.md"));
    p.read_only(&["--check"])
        .code(1)
        .stdout(predicate::str::contains("stale"));
}

#[test]
fn unowned_claude_md_makes_build_refuse_a_foreign_output_and_clean_keeps_it() {
    let p = Project::configured();
    p.write(&format!("{OUT}/CLAUDE.md"), b"My instructions");
    p.read_only(&[])
        .code(2)
        .stderr(predicate::str::contains("not safe to replace"));
    for flags in [&["--clean"][..], &["--clean", "--apply"]] {
        p.read_only(flags)
            .code(1)
            .stdout(predicate::str::contains("manifest-invalid"));
    }
    assert_eq!(
        fs::read(p.out().join("CLAUDE.md")).unwrap(),
        b"My instructions"
    );
}

#[test]
fn unowned_claude_md_in_an_otherwise_managed_output_is_never_overwritten_or_removed() {
    let p = Project::new(None);
    p.run(&[]).success();
    p.write(&format!("{OUT}/CLAUDE.md"), b"My instructions");
    p.configure(Some("instructions.md"));
    p.write("instructions.md", INSTRUCTIONS.as_bytes());
    p.read_only(&[])
        .code(2)
        .stderr(predicate::str::contains("not written by Agenvil"));
    p.configure(None);
    p.read_only(&["--clean", "--apply"]).success();
    assert_eq!(
        fs::read(p.out().join("CLAUDE.md")).unwrap(),
        b"My instructions"
    );
    assert!(p.manifest().files.iter().all(|f| f.path != "CLAUDE.md"));
}

#[test]
fn repeated_build_with_instructions_then_check_is_ok() {
    let p = Project::built();
    p.run(&[]).success();
    p.read_only(&["--check"])
        .success()
        .stdout(predicate::str::contains("matches the sources"));
}
