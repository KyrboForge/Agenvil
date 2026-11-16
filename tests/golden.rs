//! Golden snapshots of `check` output: `--format json|github|human` on stdout and the stderr
//! summary. They pin the schema, field order, sorting, rule IDs, severities, locations, messages,
//! fix hints, GitHub escaping and the exit status byte for byte. They are Agenvil's own regression
//! baseline, not output of the legacy shell validator (see `docs/compatibility.md`).
//!
//! A deliberate change: `AGENVIL_BLESS=1 cargo test --test golden`, then review the diff.

mod common;

use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};

use common::fixtures::{Case, TESTS, blessing, cases, run};

const FORMATS: [(&str, &str); 3] = [("json", "json"), ("github", "txt"), ("human", "txt")];

fn golden_dir() -> PathBuf {
    Path::new(TESTS).join("golden")
}

/// `(kind, file name, expected bytes)` for every snapshot a case asks for.
fn expected(case: &Case) -> Vec<(String, String, Vec<u8>)> {
    let mut out = Vec::new();
    for kind in &case.expect.golden {
        let (ext, format) = match kind.as_str() {
            "stderr" => ("txt", "json"),
            k => (
                FORMATS
                    .iter()
                    .find(|(f, _)| *f == k)
                    .unwrap_or_else(|| panic!("{}: unknown golden `{k}`", case.name))
                    .1,
                k,
            ),
        };
        let run = run(case, format);
        assert_eq!(run.exit, case.expect.exit, "{}: exit status", case.name);
        let bytes = if kind == "stderr" {
            run.stderr.into_bytes()
        } else {
            run.stdout
        };
        out.push((kind.clone(), format!("{}.{ext}", case.stem()), bytes));
    }
    out
}

#[test]
fn goldens_match() {
    let bless = blessing();
    let mut wanted = BTreeSet::new();
    let mut failures = Vec::new();
    for case in cases().iter().filter(|c| c.applies()) {
        for (kind, file, bytes) in expected(case) {
            let path = golden_dir().join(&kind).join(&file);
            wanted.insert(path.clone());
            if bless {
                fs::create_dir_all(path.parent().unwrap()).unwrap();
                fs::write(&path, &bytes).unwrap();
            } else {
                match fs::read(&path) {
                    Ok(old) if old == bytes => {}
                    Ok(old) => failures.push(format!(
                        "{kind}/{file} differs\n--- committed\n{}\n--- actual\n{}",
                        String::from_utf8_lossy(&old),
                        String::from_utf8_lossy(&bytes)
                    )),
                    Err(_) => failures.push(format!("{kind}/{file} is missing")),
                }
            }
        }
    }

    // Snapshots no fixture asks for any more.
    for path in files(&golden_dir()) {
        let kind = path
            .parent()
            .unwrap()
            .file_name()
            .unwrap()
            .to_string_lossy();
        let stem = path.file_stem().unwrap().to_string_lossy();
        if wanted.contains(&path) || path.file_name().unwrap() == "coverage.md" {
            continue;
        }
        if bless {
            fs::remove_file(&path).unwrap();
        } else {
            failures.push(format!("{kind}/{stem}: stale snapshot, no fixture uses it"));
        }
    }
    assert!(
        failures.is_empty(),
        "{}\n\nIf the change is intended: AGENVIL_BLESS=1 cargo test --test golden",
        failures.join("\n\n")
    );
}

fn files(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let Ok(read) = fs::read_dir(dir) else {
        return out;
    };
    for entry in read {
        let path = entry.unwrap().path();
        if path.is_dir() {
            out.extend(files(&path));
        } else {
            out.push(path);
        }
    }
    out
}
