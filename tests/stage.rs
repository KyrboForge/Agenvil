//! `build --check` and `build --clean`: drift classes, exit codes, read-only guarantees and the
//! adversarial cases (forged manifests, symlinks, foreign files, limits). Every project is a temp
//! directory with its own HOME; nothing reads the real one.

mod common;

use std::fs;
use std::path::PathBuf;

use common::{agenvil_in, snapshot};
use predicates::prelude::*;
use serde_json::{Value, json};
use tempfile::TempDir;

const CONFIG: &str = "schema = 1\n\n[project]\nname = \"demo\"\n";
const OUT: &str = ".generated/claude";
const AGENT: &str = "---\nname: a\ndescription: Agent A.\n---\nBody.\n";
const AGENT_B: &str = "---\nname: b\ndescription: Agent B.\n---\nBody.\n";
const A: &str = ".claude/agents/a.md";
const B: &str = ".claude/agents/b.md";

struct Project {
    dir: TempDir,
    home: TempDir,
}

impl Project {
    /// A built project with agents `a` and `b`.
    fn built() -> Self {
        let p = Self {
            dir: tempfile::tempdir().unwrap(),
            home: tempfile::tempdir().unwrap(),
        };
        p.write("agenvil.toml", CONFIG.as_bytes());
        p.write("agents/a.md", AGENT.as_bytes());
        p.write("agents/b.md", AGENT_B.as_bytes());
        p.run(&[], None).success();
        p
    }

    fn write(&self, path: &str, bytes: &[u8]) {
        let full = self.dir.path().join(path);
        fs::create_dir_all(full.parent().unwrap()).unwrap();
        fs::write(full, bytes).unwrap();
    }

    fn path(&self, rel: &str) -> PathBuf {
        self.dir.path().join(rel)
    }

    fn out(&self) -> PathBuf {
        self.path(OUT)
    }

    fn run(&self, flags: &[&str], out: Option<&str>) -> assert_cmd::assert::Assert {
        let mut cmd = agenvil_in(self.dir.path(), self.home.path());
        cmd.args(["build", "--target", "claude"]);
        if let Some(out) = out {
            cmd.args(["--out", out]);
        }
        cmd.args(flags).assert()
    }

    fn check(&self) -> assert_cmd::assert::Assert {
        self.run(&["--check"], None)
    }

    fn manifest(&self) -> Value {
        serde_json::from_slice(&fs::read(self.out().join("manifest.json")).unwrap()).unwrap()
    }

    fn set_manifest(&self, manifest: &Value) {
        fs::write(
            self.out().join("manifest.json"),
            serde_json::to_vec_pretty(manifest).unwrap(),
        )
        .unwrap();
    }

    /// Drops `b` from the sources, so its generated file becomes stale.
    fn retire_b(&self) {
        fs::remove_file(self.path("agents/b.md")).unwrap();
    }

    /// Runs `--check` and asserts it changed nothing.
    fn check_is_read_only(&self) -> assert_cmd::assert::Assert {
        let before = snapshot(self.dir.path());
        let result = self.check();
        assert_eq!(
            snapshot(self.dir.path()),
            before,
            "--check modified the project"
        );
        result
    }
}

#[test]
fn a_matching_tree_passes_and_stays_untouched() {
    let p = Project::built();
    p.check_is_read_only()
        .success()
        .stdout(predicate::str::contains("matches the sources"));
}

#[test]
fn a_missing_staging_directory_is_drift_and_is_never_created() {
    let p = Project::built();
    let other = p.path("elsewhere");
    p.run(&["--check"], Some("elsewhere"))
        .code(1)
        .stdout(predicate::str::contains("missing"));
    assert!(!other.exists());
}

#[test]
fn a_missing_file_is_missing() {
    let p = Project::built();
    fs::remove_file(p.out().join(A)).unwrap();
    p.check_is_read_only()
        .code(1)
        .stdout(predicate::str::contains("missing".to_string()).and(predicate::str::contains(A)));
}

#[test]
fn a_modified_file_is_changed_with_size_or_digest() {
    let p = Project::built();
    fs::write(p.out().join(A), "tampered").unwrap();
    p.check_is_read_only().code(1).stdout(
        predicate::str::contains("changed").and(predicate::str::contains("bytes, expected")),
    );
    // Same size, different bytes: caught by the digest.
    let same_len = AGENT.replace("Body.", "Bodz.");
    assert_eq!(same_len.len(), AGENT.len());
    fs::write(p.out().join(A), same_len).unwrap();
    p.check_is_read_only()
        .code(1)
        .stdout(predicate::str::contains("sha256"));
}

#[test]
fn a_file_the_sources_no_longer_generate_is_stale() {
    let p = Project::built();
    p.retire_b();
    p.check_is_read_only()
        .code(1)
        .stdout(predicate::str::contains("stale").and(predicate::str::contains(B)));
}

#[test]
fn a_foreign_file_is_extra_and_never_touched() {
    let p = Project::built();
    p.write(&format!("{OUT}/notes.txt"), b"mine");
    p.check_is_read_only()
        .code(1)
        .stdout(predicate::str::contains("extra").and(predicate::str::contains("notes.txt")));
    assert_eq!(fs::read(p.out().join("notes.txt")).unwrap(), b"mine");
}

#[test]
fn a_missing_or_malformed_manifest_is_manifest_invalid() {
    let p = Project::built();
    fs::remove_file(p.out().join("manifest.json")).unwrap();
    p.check_is_read_only()
        .code(1)
        .stdout(predicate::str::contains("manifest-invalid"));
    p.write(&format!("{OUT}/manifest.json"), b"{ not json");
    p.check_is_read_only()
        .code(1)
        .stdout(predicate::str::contains("manifest-invalid"));
}

#[test]
fn a_forged_or_unsupported_manifest_is_never_trusted() {
    for (field, value) in [
        ("schema", json!(2)),
        ("owner", json!("someone")),
        ("target", json!("codex")),
    ] {
        let p = Project::built();
        let mut m = p.manifest();
        m[field] = value;
        p.set_manifest(&m);
        p.check_is_read_only()
            .code(1)
            .stdout(predicate::str::contains("manifest-invalid"));
    }
    // A digest forged to match a tampered file is still "changed": the plan, not the manifest,
    // is the source of truth for what the file should be.
    let p = Project::built();
    fs::write(p.out().join(A), "tampered").unwrap();
    let mut m = p.manifest();
    m["files"][0]["sha256"] = json!(agenvil::manifest::sha256(b"tampered"));
    m["files"][0]["bytes"] = json!(8);
    p.set_manifest(&m);
    p.check_is_read_only()
        .code(1)
        .stdout(predicate::str::contains("changed"));
    // Unknown fields are refused, not ignored.
    let p = Project::built();
    let mut m = p.manifest();
    m["extra"] = json!(true);
    p.set_manifest(&m);
    p.check_is_read_only().code(1);
}

#[test]
fn unsafe_manifest_paths_are_exit_2_and_other_bad_paths_are_drift() {
    for escape in [
        "../outside.md",
        "/etc/passwd",
        "a/../../b.md",
        "c:/x.md",
        "a\\b.md",
    ] {
        let p = Project::built();
        let mut m = p.manifest();
        m["files"][0]["path"] = json!(escape);
        p.set_manifest(&m);
        p.check_is_read_only()
            .code(2)
            .stdout(predicate::str::contains("unsafe"));
    }
    for bad in [
        ".claude/agents/nul.md",
        ".claude/agents/x.md.",
        ".claude/agents/a?.md",
    ] {
        let p = Project::built();
        let mut m = p.manifest();
        m["files"][0]["path"] = json!(bad);
        p.set_manifest(&m);
        p.check_is_read_only()
            .code(1)
            .stdout(predicate::str::contains("manifest-invalid"));
    }
}

#[test]
fn duplicate_and_case_colliding_manifest_paths_are_manifest_invalid() {
    let p = Project::built();
    let mut m = p.manifest();
    let first = m["files"][0].clone();
    m["files"].as_array_mut().unwrap().push(first);
    p.set_manifest(&m);
    p.check_is_read_only()
        .code(1)
        .stdout(predicate::str::contains("listed twice"));

    let p = Project::built();
    let mut m = p.manifest();
    let mut upper = m["files"][0].clone();
    upper["path"] = json!(
        A.to_uppercase()
            .replace(".CLAUDE", ".claude")
            .replace("AGENTS", "agents")
    );
    m["files"].as_array_mut().unwrap().push(upper);
    p.set_manifest(&m);
    p.check_is_read_only()
        .code(1)
        .stdout(predicate::str::contains("case-insensitive"));

    // A file and a directory of the same path.
    let p = Project::built();
    let mut m = p.manifest();
    let mut nested = m["files"][0].clone();
    nested["path"] = json!(format!("{A}/inner.md"));
    m["files"].as_array_mut().unwrap().push(nested);
    p.set_manifest(&m);
    p.check_is_read_only()
        .code(1)
        .stdout(predicate::str::contains("is a file and a directory"));
}

#[cfg(unix)]
#[test]
fn symlinks_in_the_tree_are_unsafe_and_never_followed() {
    let p = Project::built();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret"), "secret").unwrap();
    std::os::unix::fs::symlink(outside.path(), p.out().join("link")).unwrap();
    p.check_is_read_only()
        .code(2)
        .stdout(predicate::str::contains("unsafe").and(predicate::str::contains("symlink")));

    // A generated file swapped for a link to the same bytes is still not trusted.
    let p = Project::built();
    fs::remove_file(p.out().join(A)).unwrap();
    fs::write(outside.path().join("a.md"), AGENT).unwrap();
    std::os::unix::fs::symlink(outside.path().join("a.md"), p.out().join(A)).unwrap();
    p.check_is_read_only().code(2);
    assert_eq!(fs::read(outside.path().join("secret")).unwrap(), b"secret");
}

#[cfg(unix)]
#[test]
fn an_output_directory_that_is_a_symlink_or_a_file_is_exit_2() {
    let p = Project::built();
    let target = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(target.path(), p.path("linked")).unwrap();
    p.run(&["--check"], Some("linked")).code(2);
    assert_eq!(fs::read_dir(target.path()).unwrap().count(), 0);
    p.write("plain", b"file");
    p.run(&["--check"], Some("plain")).code(2);
}

#[test]
fn unsafe_output_locations_are_exit_2_for_check_and_clean() {
    let p = Project::built();
    for flags in [
        &["--check"][..],
        &["--clean"][..],
        &["--clean", "--apply"][..],
    ] {
        p.run(flags, Some("../escape")).code(2);
        p.run(flags, Some(".")).code(2);
        p.run(flags, Some("agents")).code(2);
        let claude = p.home.path().join(".claude");
        p.run(flags, Some(claude.to_str().unwrap())).code(2);
    }
    assert!(!p.home.path().join(".claude").exists());
}

#[test]
fn invalid_or_incomplete_sources_cannot_be_checked() {
    let p = Project::built();
    p.write("agents/broken.md", b"---\nname: [unterminated\n---\nbody\n");
    let before = snapshot(p.out().as_path());
    p.check().code(4);
    assert_eq!(snapshot(p.out().as_path()), before);

    let p = Project::built();
    p.write(
        "agents/nameless.md",
        b"---\ndescription: no name\n---\nbody\n",
    );
    p.check()
        .code(1)
        .stderr(predicate::str::contains("sources are invalid"));
}

#[test]
fn a_large_file_is_changed_without_being_read_and_too_many_entries_cannot_be_verified() {
    let p = Project::built();
    let big = fs::File::create(p.out().join(A)).unwrap();
    big.set_len(17 * 1024 * 1024).unwrap();
    drop(big);
    p.check_is_read_only()
        .code(1)
        .stdout(predicate::str::contains("changed"));

    let p = Project::built();
    let junk = p.out().join("junk");
    fs::create_dir(&junk).unwrap();
    for i in 0..30_001 {
        fs::write(junk.join(format!("{i}")), b"").unwrap();
    }
    p.check_is_read_only()
        .code(4)
        .stderr(predicate::str::contains("cannot verify"));
}

#[test]
fn the_flags_combine_only_in_documented_ways() {
    let p = Project::built();
    p.run(&["--apply"], None).code(2);
    p.run(&["--check", "--clean"], None).code(2);
    p.run(&["--check", "--apply"], None).code(2);
}

// ---- cleanup ----

#[test]
fn clean_is_a_dry_run_by_default_and_lists_the_exact_files() {
    let p = Project::built();
    p.retire_b();
    let before = snapshot(p.dir.path());
    p.run(&["--clean"], None)
        .success()
        .stdout(predicate::str::contains("would remove").and(predicate::str::contains(B)))
        .stdout(predicate::str::contains("dry run"));
    assert_eq!(snapshot(p.dir.path()), before);
}

#[test]
fn clean_apply_removes_only_stale_files_and_leaves_a_matching_tree() {
    let p = Project::built();
    p.retire_b();
    p.run(&["--clean", "--apply"], None)
        .success()
        .stdout(predicate::str::contains("removed").and(predicate::str::contains(B)));
    assert!(!p.out().join(B).exists());
    assert!(p.out().join(A).exists());
    p.check().success();
    // Nothing left to do.
    p.run(&["--clean", "--apply"], None)
        .success()
        .stdout(predicate::str::contains("nothing to clean"));
}

#[test]
fn clean_prunes_directories_it_emptied_but_not_ones_with_foreign_content() {
    let p = Project::built();
    p.write("skills/s/SKILL.md", b"---\nname: s\ndescription: S.\n---\n");
    p.write("skills/s/refs/r.md", b"r");
    p.run(&[], None).success();
    fs::remove_dir_all(p.path("skills/s/refs")).unwrap();
    p.run(&["--clean", "--apply"], None).success();
    assert!(!p.out().join(".claude/skills/s/refs").exists());
    assert!(p.out().join(".claude/skills/s/SKILL.md").exists());

    let p = Project::built();
    p.write(&format!("{OUT}/.claude/agents/mine.md"), b"mine");
    p.retire_b();
    p.run(&["--clean", "--apply"], None).success();
    assert!(!p.out().join(B).exists());
    assert_eq!(
        fs::read(p.out().join(".claude/agents/mine.md")).unwrap(),
        b"mine"
    );
    assert!(p.out().join(".claude/agents").is_dir());
}

#[test]
fn clean_never_deletes_a_modified_file_or_an_unknown_one() {
    let p = Project::built();
    p.retire_b();
    fs::write(p.out().join(B), "my edit").unwrap();
    p.write(&format!("{OUT}/notes.txt"), b"mine");
    p.run(&["--clean", "--apply"], None)
        .success()
        .stdout(predicate::str::contains("kept").and(predicate::str::contains(B)));
    assert_eq!(fs::read(p.out().join(B)).unwrap(), b"my edit");
    assert_eq!(fs::read(p.out().join("notes.txt")).unwrap(), b"mine");
    // Still reported afterwards, so it is not forgotten.
    p.check().code(1).stdout(predicate::str::contains("stale"));
}

#[cfg(unix)]
#[test]
fn an_unsafe_file_blocks_cleanup_instead_of_being_skipped() {
    let p = Project::built();
    p.retire_b();
    let outside = tempfile::tempdir().unwrap();
    std::os::unix::fs::symlink(outside.path(), p.out().join("link")).unwrap();
    let before = snapshot(p.out().as_path());
    p.run(&["--clean", "--apply"], None)
        .code(2)
        .stderr(predicate::str::contains("cleanup blocked"));
    assert_eq!(snapshot(p.out().as_path()), before);
    assert!(p.out().join(B).exists());
}

#[test]
fn a_distrusted_manifest_blocks_cleanup() {
    let p = Project::built();
    p.retire_b();
    let mut m = p.manifest();
    m["owner"] = json!("someone");
    p.set_manifest(&m);
    let before = snapshot(p.out().as_path());
    p.run(&["--clean", "--apply"], None).code(1);
    assert_eq!(snapshot(p.out().as_path()), before);

    // A forged path pointing outside the tree is never deleted, nor even looked at.
    let p = Project::built();
    p.retire_b();
    let victim = p.path("victim.md");
    fs::write(&victim, "precious").unwrap();
    let mut m = p.manifest();
    m["files"][1]["path"] = json!("../../victim.md");
    p.set_manifest(&m);
    p.run(&["--clean", "--apply"], None).code(2);
    assert_eq!(fs::read(&victim).unwrap(), b"precious");
}

#[test]
fn clean_of_a_missing_directory_does_nothing() {
    let p = Project::built();
    p.run(&["--clean", "--apply"], Some("nowhere")).success();
    assert!(!p.path("nowhere").exists());
}

#[test]
fn personal_claude_config_is_never_touched() {
    let p = Project::built();
    let claude = p.home.path().join(".claude/agents");
    fs::create_dir_all(&claude).unwrap();
    fs::write(claude.join("mine.md"), "mine").unwrap();
    p.retire_b();
    p.run(&["--clean", "--apply"], None).success();
    p.check().code(0);
    assert_eq!(fs::read(claude.join("mine.md")).unwrap(), b"mine");
}
