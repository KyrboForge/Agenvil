//! Golden generated trees and the end-to-end drift flow, over synthetic standalone projects in
//! `tests/fixtures/build/` (no private fixtures, no Claude session, no credentials).
//!
//! Each golden is the whole staging tree of one fixture: paths, exact bytes, manifest and order.
//! The manifest's generator version is stored as `@VERSION@` so a release does not churn them.
//! A deliberate change: `AGENVIL_BLESS=1 cargo test --test build_golden`, then review the diff.
//! Blessing is refused when `CI` is set, so a pipeline can never rewrite its own baseline.

mod common;

use std::fs;
use std::path::{Path, PathBuf};

use agenvil::manifest::{self, Manifest, Transform};
use agenvil::stage;
use common::fixtures::{TESTS, blessing};
use common::{agenvil_in, snapshot};
use predicates::prelude::*;
use tempfile::TempDir;

const CASES: [&str; 2] = ["models", "skills-rules"];
const VERSION: &str = env!("CARGO_PKG_VERSION");

fn fixture(case: &str) -> PathBuf {
    Path::new(TESTS).join("fixtures/build").join(case)
}

fn golden(case: &str) -> PathBuf {
    Path::new(TESTS).join("golden-build").join(case)
}

fn copy_dir(from: &Path, to: &Path) {
    fs::create_dir_all(to).unwrap();
    for entry in fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_dir(&entry.path(), &target);
        } else {
            fs::copy(entry.path(), target).unwrap();
        }
    }
}

/// A copy of a fixture in a temp dir, with its own isolated HOME.
struct Project {
    dir: TempDir,
    home: TempDir,
}

impl Project {
    fn new(case: &str) -> Self {
        let dir = tempfile::tempdir().unwrap();
        copy_dir(&fixture(case), dir.path());
        Self {
            dir,
            home: tempfile::tempdir().unwrap(),
        }
    }

    fn stage(&self, name: &str) -> PathBuf {
        self.dir.path().join(name)
    }

    fn run(&self, flags: &[&str], out: &str) -> assert_cmd::assert::Assert {
        let mut cmd = agenvil_in(self.dir.path(), self.home.path());
        cmd.args(["build", "--target", "claude", "--out", out])
            .args(flags);
        cmd.assert()
    }
}

/// The staging tree as files, with the generator version replaced by the placeholder.
fn normalized(dir: &Path) -> Vec<(String, Vec<u8>)> {
    snapshot(dir)
        .into_iter()
        .map(|(path, bytes)| {
            if path == manifest::FILE {
                let text = String::from_utf8(bytes).unwrap();
                let swapped = text.replace(
                    &format!("\"version\": \"{VERSION}\""),
                    "\"version\": \"@VERSION@\"",
                );
                assert_ne!(text, swapped, "the manifest names the generator version");
                (path, swapped.into_bytes())
            } else {
                (path, bytes)
            }
        })
        .collect()
}

#[test]
fn goldens_match() {
    let bless = blessing();
    let mut failures = Vec::new();
    for case in CASES {
        let project = Project::new(case);
        project.run(&[], "stage").success();
        let actual = normalized(&project.stage("stage"));
        let dir = golden(case);
        if bless {
            if dir.exists() {
                fs::remove_dir_all(&dir).unwrap();
            }
            for (path, bytes) in &actual {
                if !path.ends_with('/') {
                    let file = dir.join(path);
                    fs::create_dir_all(file.parent().unwrap()).unwrap();
                    fs::write(file, bytes).unwrap();
                }
            }
        } else if !dir.exists() {
            failures.push(format!("{case}: golden is missing"));
        } else if snapshot(&dir) != actual {
            let listing = |s: &[(String, Vec<u8>)]| {
                s.iter()
                    .map(|(p, b)| format!("{p} ({} bytes)", b.len()))
                    .collect::<Vec<_>>()
                    .join("\n")
            };
            failures.push(format!(
                "{case}: generated tree differs from the golden\n--- committed\n{}\n--- actual\n{}",
                listing(&snapshot(&dir)),
                listing(&actual)
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{}\nIf the change is intended: AGENVIL_BLESS=1 cargo test --test build_golden",
        failures.join("\n\n")
    );
}

#[test]
fn golden_manifests_pin_paths_digests_order_and_transforms() {
    for case in CASES {
        let dir = golden(case);
        let text = fs::read_to_string(dir.join(manifest::FILE)).unwrap();
        let parsed = Manifest::parse(text.replace("@VERSION@", VERSION).as_bytes()).unwrap();
        assert!(parsed.flaws().is_empty(), "{case}: {:?}", parsed.flaws());
        let paths: Vec<_> = parsed.files.iter().map(|f| f.path.as_str()).collect();
        let mut sorted = paths.clone();
        sorted.sort();
        assert_eq!(paths, sorted, "{case}: manifest order is sorted by path");
        for file in &parsed.files {
            let bytes = fs::read(dir.join(&file.path)).unwrap();
            assert_eq!(
                file.sha256,
                manifest::sha256(&bytes),
                "{case}: {}",
                file.path
            );
            assert_eq!(file.bytes, bytes.len() as u64, "{case}: {}", file.path);
        }
        // Nothing on disk is unlisted.
        let on_disk: Vec<_> = snapshot(&dir)
            .into_iter()
            .filter(|(p, _)| !p.ends_with('/') && p != manifest::FILE)
            .map(|(p, _)| p)
            .collect();
        assert_eq!(
            on_disk, paths,
            "{case}: tree and manifest list the same files"
        );
    }
}

#[test]
fn model_and_effort_goldens_record_the_transform() {
    let text = fs::read_to_string(golden("models").join(manifest::FILE)).unwrap();
    let parsed = Manifest::parse(text.replace("@VERSION@", VERSION).as_bytes()).unwrap();
    let transform = |path: &str| {
        parsed
            .files
            .iter()
            .find(|f| f.path == path)
            .unwrap_or_else(|| panic!("{path} is listed"))
            .transform
    };
    // Resolved or mapped values rewrite the file; untouched sources are copied byte for byte.
    assert_eq!(
        transform(".claude/agents/mapped.md"),
        Transform::ModelEffortOverride
    );
    assert_eq!(transform(".claude/agents/unknown.md"), Transform::Copy);
    let mapped = fs::read_to_string(golden("models").join(".claude/agents/mapped.md")).unwrap();
    assert!(mapped.contains("effort: max"), "{mapped}");
    let unknown = fs::read_to_string(golden("models").join(".claude/agents/unknown.md")).unwrap();
    assert!(unknown.contains("x-team: platform"), "{unknown}");
}

#[test]
fn skill_goldens_keep_supporting_files_and_binary_assets() {
    let dir = golden("skills-rules");
    assert_eq!(
        fs::read(dir.join(".claude/skills/alpha/assets/blob.bin")).unwrap(),
        [0, 1, 2, 0xff, 0xfe, 0x0d, 0x0a, 0x80, 0, 0x1a]
    );
    for kept in [
        ".claude/skills/alpha/references/deep/more.md",
        ".claude/skills/alpha/scripts/run.sh",
        ".claude/rules/rust.md",
    ] {
        assert!(dir.join(kept).is_file(), "{kept}");
    }
}

#[test]
fn builds_are_reproducible_across_runs_and_directories() {
    for case in CASES {
        let project = Project::new(case);
        project.run(&[], "one").success();
        project.run(&[], "two").success();
        assert_eq!(
            snapshot(&project.stage("one")),
            snapshot(&project.stage("two"))
        );
        // Rebuilding over an earlier build changes nothing either.
        project.run(&[], "one").success();
        assert_eq!(
            snapshot(&project.stage("one")),
            snapshot(&project.stage("two"))
        );
    }
}

#[test]
fn generated_trees_validate_without_the_sources() {
    for case in CASES {
        let project = Project::new(case);
        project.run(&[], "stage").success();
        assert_eq!(
            stage::validate_dir(&project.stage("stage")).unwrap(),
            [],
            "{case}"
        );
        // The committed goldens are valid output too.
        let committed = golden(case);
        let found = stage::validate_dir(&committed).unwrap();
        assert!(
            found.iter().all(|f| f.kind != stage::Kind::Invalid),
            "{case}: {found:?}"
        );
    }
}

/// The CI flow: clean fixture, build to temp staging, validate, compare with the golden,
/// `--check` passes, mutate a generated file, `--check` fails, and the tree is left as it was.
#[test]
fn end_to_end_build_validate_golden_check_mutate() {
    for case in CASES {
        let project = Project::new(case);
        let stage_dir = project.stage("stage");

        // Missing staging directory: drift, and `--check` must not create it.
        project
            .run(&["--check"], "stage")
            .code(1)
            .stdout(predicate::str::contains("missing"));
        assert!(!stage_dir.exists(), "{case}: --check created the directory");

        project.run(&[], "stage").success();
        assert_eq!(stage::validate_dir(&stage_dir).unwrap(), [], "{case}");
        assert_eq!(snapshot(&golden(case)), normalized(&stage_dir), "{case}");
        project
            .run(&["--check"], "stage")
            .success()
            .stdout(predicate::str::contains("matches the sources"));

        let victim = stage_dir.join(
            normalized(&stage_dir)
                .into_iter()
                .map(|(p, _)| p)
                .find(|p| p.starts_with(".claude/") && !p.ends_with('/'))
                .unwrap(),
        );
        let mut bytes = fs::read(&victim).unwrap();
        bytes.extend_from_slice(b"\nedited\n");
        fs::write(&victim, bytes).unwrap();

        let before = snapshot(&stage_dir);
        project
            .run(&["--check"], "stage")
            .code(1)
            .stdout(predicate::str::contains("changed"));
        assert_eq!(
            snapshot(&stage_dir),
            before,
            "{case}: --check modified the tree"
        );
    }
}
