//! Finds and reads resource files under the project root. Read-only, bounded, never leaves the root.
//!
//! The tree is untrusted. Symlinks are never followed or read (they are reported as problems),
//! file sizes, entry counts and total bytes are capped, and nothing is executed. Parsing the
//! files is [`crate::frontmatter`]'s job; this module only returns their text.
//!
//! Layout, relative to the configured directories: `agents/*.md`, `skills/*/SKILL.md`, `rules/*.md`.
//! Anything deeper or differently named is not a resource and is ignored. `.gitignore` files
//! inside the project root are honored; ignore files outside it (parent directories, global git
//! config) are not, so results do not depend on where the project is checked out.

use crate::config::Paths;
use crate::diagnostics::{Problem, ProblemKind};
use ignore::WalkBuilder;
use std::collections::BTreeMap;
use std::fs::{File, Metadata};
use std::io::{ErrorKind, Read};
use std::path::{Component, Path, PathBuf};

#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    Agent,
    Skill,
    Rule,
}

/// Scan limits. A limit that is hit is reported as a [`ProblemKind`], never silently dropped.
#[derive(Clone, Debug)]
pub struct Limits {
    /// Largest single file read.
    pub max_file_bytes: u64,
    /// Sum of all file sizes read.
    pub max_total_bytes: u64,
    /// Directory entries examined.
    pub max_entries: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_file_bytes: crate::config::MAX_FILE_BYTES,
            max_total_bytes: 64 * 1024 * 1024,
            max_entries: 10_000,
        }
    }
}

/// A resource file that was found and read.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Found {
    pub kind: Kind,
    /// File stem for agents and rules, directory name for skills.
    pub name: String,
    /// Relative to the project root, `/`-separated.
    pub path: String,
    /// The file exactly as it is on disk (a BOM and CRLF line endings are kept).
    pub source: String,
}

#[derive(Debug, Default)]
pub struct Discovery {
    /// Sorted by kind, then path.
    pub files: Vec<Found>,
    /// Sorted.
    pub problems: Vec<Problem>,
}

/// Configuration that makes discovery ambiguous; not a problem with any one file.
#[derive(Debug, PartialEq, Eq, thiserror::Error)]
#[error(
    "`paths.{first_key}` ({first}) and `paths.{second_key}` ({second}) overlap; use separate directories"
)]
pub struct OverlapError {
    first_key: &'static str,
    first: String,
    second_key: &'static str,
    second: String,
}

pub fn discover(root: &Path, paths: &Paths, limits: &Limits) -> Result<Discovery, OverlapError> {
    let configured = [
        (Kind::Agent, "agents", paths.agents.as_path()),
        (Kind::Skill, "skills", paths.skills.as_path()),
        (Kind::Rule, "rules", paths.rules.as_path()),
    ];
    for (i, (_, first_key, first)) in configured.iter().enumerate() {
        for (_, second_key, second) in &configured[i + 1..] {
            if first.starts_with(second) || second.starts_with(first) {
                return Err(OverlapError {
                    first_key,
                    first: display(first),
                    second_key,
                    second: display(second),
                });
            }
        }
    }

    let mut scan = Scan {
        root,
        limits,
        total_bytes: 0,
        out: Discovery::default(),
    };
    let dirs: Vec<_> = configured
        .iter()
        .filter(|(_, _, rel)| scan.is_scannable_dir(rel))
        .map(|&(kind, _, rel)| (kind, rel))
        .collect();
    scan.walk(&dirs);

    scan.out
        .files
        .sort_by(|a, b| (a.kind, &a.path).cmp(&(b.kind, &b.path)));
    scan.out.problems.sort();
    scan.out.problems.dedup();
    Ok(scan.out)
}

struct Scan<'a> {
    root: &'a Path,
    limits: &'a Limits,
    total_bytes: u64,
    out: Discovery,
}

impl Scan<'_> {
    fn problem(&mut self, path: &Path, line: Option<usize>, kind: ProblemKind, message: String) {
        self.out.problems.push(Problem {
            path: display(path),
            line,
            kind,
            message,
        });
    }

    /// Checks every component of `rel` without following symlinks. A missing directory is fine
    /// (nothing to discover) and returns `false` silently; anything else unusual is a problem.
    fn is_scannable_dir(&mut self, rel: &Path) -> bool {
        let mut at = PathBuf::new();
        for component in rel.components() {
            at.push(component);
            // shortcut: lstat-then-walk races a concurrent local attacker swapping in a symlink;
            // open directories with O_NOFOLLOW if that is in scope.
            match std::fs::symlink_metadata(self.root.join(&at)) {
                Ok(meta) if meta.file_type().is_symlink() => {
                    self.problem(
                        &at,
                        None,
                        ProblemKind::Symlink,
                        "symlinks are not followed".to_owned(),
                    );
                    return false;
                }
                Ok(meta) if !meta.is_dir() => {
                    self.problem(
                        &at,
                        None,
                        ProblemKind::NotADirectory,
                        "expected a directory".to_owned(),
                    );
                    return false;
                }
                Ok(_) => {}
                Err(e) if e.kind() == ErrorKind::NotFound => return false,
                Err(e) => {
                    self.problem(&at, None, ProblemKind::Unreadable, e.kind().to_string());
                    return false;
                }
            }
        }
        true
    }

    fn walk(&mut self, dirs: &[(Kind, &Path)]) {
        // Agents and rules sit one level below their directory, `SKILL.md` two.
        let max_depth = dirs
            .iter()
            .map(|(kind, rel)| rel.components().count() + if *kind == Kind::Skill { 2 } else { 1 })
            .max();
        let Some(max_depth) = max_depth else { return };

        let keep: Vec<PathBuf> = dirs.iter().map(|(_, rel)| rel.to_path_buf()).collect();
        let root = self.root.to_path_buf();
        let walker = WalkBuilder::new(self.root)
            // Only the project's own `.gitignore` files: no parents, global config or `.ignore`.
            .standard_filters(false)
            .git_ignore(true)
            .require_git(false)
            .follow_links(false)
            .max_depth(Some(max_depth))
            .sort_by_file_name(Ord::cmp)
            // Never descend into anything that is neither above nor inside a configured directory.
            .filter_entry(move |entry| {
                entry.path().strip_prefix(&root).is_ok_and(|rel| {
                    keep.iter()
                        .any(|dir| dir.starts_with(rel) || rel.starts_with(dir))
                })
            })
            .build();

        // skill directory -> whether its `SKILL.md` was seen (in whatever state).
        let mut skills: BTreeMap<PathBuf, bool> = BTreeMap::new();
        let mut entries = 0usize;
        for result in walker {
            entries += 1;
            if entries > self.limits.max_entries {
                self.problem(
                    Path::new("."),
                    None,
                    ProblemKind::LimitExceeded,
                    format!(
                        "more than {} directory entries; the inventory is incomplete",
                        self.limits.max_entries
                    ),
                );
                return;
            }
            let entry = match result {
                Ok(entry) => entry,
                Err(error) => {
                    // The error's own text embeds absolute paths, which would make output
                    // machine-dependent; keep only the I/O kind.
                    let why = error.io_error().map_or_else(
                        || "invalid ignore file".to_owned(),
                        |e| e.kind().to_string(),
                    );
                    self.problem(
                        Path::new("."),
                        None,
                        ProblemKind::Unreadable,
                        format!("could not read a directory entry: {why}"),
                    );
                    continue;
                }
            };
            let Ok(rel) = entry.path().strip_prefix(self.root) else {
                continue;
            };
            let rel = rel.to_path_buf();
            let Some(file_type) = entry.file_type() else {
                continue;
            };
            for &(kind, dir) in dirs {
                let Ok(inside) = rel.strip_prefix(dir) else {
                    continue;
                };
                let depth = inside.components().count();
                let name = rel.file_name().and_then(|n| n.to_str()).unwrap_or_default();
                let (wanted, skill_name) = match (kind, depth) {
                    (Kind::Skill, 1) => {
                        if file_type.is_dir() {
                            skills.entry(rel.clone()).or_insert(false);
                        } else if file_type.is_symlink() {
                            self.problem(
                                &rel,
                                None,
                                ProblemKind::Symlink,
                                "symlinks are not followed".to_owned(),
                            );
                        }
                        continue;
                    }
                    (Kind::Skill, 2) if name == "SKILL.md" => {
                        let skill_dir = rel.parent().unwrap_or(&rel).to_path_buf();
                        let skill_name = skill_dir
                            .file_name()
                            .and_then(|n| n.to_str())
                            .unwrap_or_default()
                            .to_owned();
                        skills.insert(skill_dir, true);
                        (true, skill_name)
                    }
                    (Kind::Agent | Kind::Rule, 1) if name.ends_with(".md") => {
                        (true, name.strip_suffix(".md").unwrap_or(name).to_owned())
                    }
                    _ => (false, String::new()),
                };
                if wanted && !file_type.is_dir() {
                    self.read(
                        kind,
                        skill_name,
                        &rel,
                        file_type.is_symlink(),
                        entry.metadata(),
                    );
                }
            }
        }

        for (dir, seen) in skills {
            if !seen {
                self.problem(
                    &dir,
                    None,
                    ProblemKind::MissingSkillFile,
                    "skill directory has no SKILL.md".to_owned(),
                );
            }
        }
    }

    fn read(
        &mut self,
        kind: Kind,
        name: String,
        rel: &Path,
        is_symlink: bool,
        metadata: Result<Metadata, ignore::Error>,
    ) {
        if is_symlink {
            return self.problem(
                rel,
                None,
                ProblemKind::Symlink,
                "symlinks are not followed".to_owned(),
            );
        }
        let Some(path) = portable(rel) else {
            return self.problem(
                rel,
                None,
                ProblemKind::InvalidPath,
                "path is not valid UTF-8".to_owned(),
            );
        };
        let meta = match metadata {
            Ok(meta) => meta,
            Err(_) => {
                return self.problem(
                    rel,
                    None,
                    ProblemKind::Unreadable,
                    "could not read file metadata".to_owned(),
                );
            }
        };
        if !meta.is_file() {
            return self.problem(
                rel,
                None,
                ProblemKind::NotRegularFile,
                "expected a regular file".to_owned(),
            );
        }
        let max = self.limits.max_file_bytes;
        if meta.len() > max {
            return self.problem(
                rel,
                None,
                ProblemKind::TooLarge,
                format!("larger than the {max}-byte limit"),
            );
        }
        if self.total_bytes + meta.len() > self.limits.max_total_bytes {
            return self.problem(
                rel,
                None,
                ProblemKind::LimitExceeded,
                format!(
                    "reading it would exceed the {}-byte total limit; the inventory is incomplete",
                    self.limits.max_total_bytes
                ),
            );
        }

        // `take` keeps the bound even if the file grows after the metadata check.
        let mut bytes = Vec::new();
        let read = File::open(self.root.join(rel))
            .and_then(|file| file.take(max + 1).read_to_end(&mut bytes));
        if let Err(e) = read {
            return self.problem(rel, None, ProblemKind::Unreadable, e.kind().to_string());
        }
        if bytes.len() as u64 > max {
            return self.problem(
                rel,
                None,
                ProblemKind::TooLarge,
                format!("larger than the {max}-byte limit"),
            );
        }
        self.total_bytes += bytes.len() as u64;
        match String::from_utf8(bytes) {
            Ok(source) => self.out.files.push(Found {
                kind,
                name,
                path,
                source,
            }),
            Err(e) => {
                let line = 1 + e.as_bytes()[..e.utf8_error().valid_up_to()]
                    .iter()
                    .filter(|&&b| b == b'\n')
                    .count();
                self.problem(
                    rel,
                    Some(line),
                    ProblemKind::InvalidUtf8,
                    "file is not valid UTF-8".to_owned(),
                );
            }
        }
    }
}

/// `/`-joined UTF-8 form of a relative path, identical on every OS.
fn portable(rel: &Path) -> Option<String> {
    let parts: Option<Vec<_>> = rel
        .components()
        .map(|c| match c {
            Component::Normal(part) => part.to_str(),
            _ => None,
        })
        .collect();
    parts.map(|parts| parts.join("/"))
}

fn display(rel: &Path) -> String {
    portable(rel).unwrap_or_else(|| rel.to_string_lossy().replace('\\', "/"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::Config;
    use std::fs;
    use tempfile::TempDir;

    fn tree(files: &[(&str, &str)]) -> TempDir {
        let dir = tempfile::tempdir().unwrap();
        for (path, text) in files {
            let full = dir.path().join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, text).unwrap();
        }
        dir
    }

    fn paths(extra: &str) -> Paths {
        let text = format!("schema = 1\n[project]\nname = \"x\"\n[paths]\n{extra}\n");
        Config::parse(&text, Path::new("agenvil.toml"))
            .unwrap()
            .paths
    }

    fn run(dir: &TempDir) -> Discovery {
        discover(dir.path(), &Paths::default(), &Limits::default()).unwrap()
    }

    fn found(d: &Discovery) -> Vec<(Kind, &str, &str)> {
        d.files
            .iter()
            .map(|f| (f.kind, f.name.as_str(), f.path.as_str()))
            .collect()
    }

    fn kinds(d: &Discovery) -> Vec<(&str, ProblemKind)> {
        d.problems
            .iter()
            .map(|p| (p.path.as_str(), p.kind))
            .collect()
    }

    #[test]
    fn finds_the_three_layouts_sorted() {
        let dir = tree(&[
            ("rules/b.md", "b"),
            ("agents/z.md", "z"),
            ("agents/a.md", "a"),
            ("skills/s2/SKILL.md", "s2"),
            ("skills/s1/SKILL.md", "s1"),
            ("rules/a.md", "a"),
        ]);
        let d = run(&dir);
        assert_eq!(
            found(&d),
            [
                (Kind::Agent, "a", "agents/a.md"),
                (Kind::Agent, "z", "agents/z.md"),
                (Kind::Skill, "s1", "skills/s1/SKILL.md"),
                (Kind::Skill, "s2", "skills/s2/SKILL.md"),
                (Kind::Rule, "a", "rules/a.md"),
                (Kind::Rule, "b", "rules/b.md"),
            ]
        );
        assert!(d.problems.is_empty());
    }

    #[test]
    fn missing_and_empty_directories_are_not_problems() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("agents")).unwrap();
        let d = run(&dir);
        assert!(d.files.is_empty() && d.problems.is_empty());
    }

    #[test]
    fn other_files_and_deeper_levels_are_not_resources() {
        let dir = tree(&[
            ("agents/readme.txt", ""),
            ("agents/sub/deep.md", ""),
            ("agents/x.MD", ""),
            ("rules/sub/deep.md", ""),
            ("skills/s/SKILL.md", ""),
            ("skills/s/notes.md", ""),
            ("skills/s/sub/SKILL.md", ""),
            ("skills/loose.md", ""),
        ]);
        let d = run(&dir);
        assert_eq!(found(&d), [(Kind::Skill, "s", "skills/s/SKILL.md")]);
        assert!(d.problems.is_empty(), "{:?}", d.problems);
    }

    #[test]
    fn skill_directory_without_skill_md_is_reported() {
        let dir = tree(&[
            ("skills/empty/other.md", ""),
            ("skills/skill.md/skill.md", ""),
        ]);
        let d = run(&dir);
        assert!(d.files.is_empty());
        assert_eq!(
            kinds(&d),
            [
                ("skills/empty", ProblemKind::MissingSkillFile),
                ("skills/skill.md", ProblemKind::MissingSkillFile),
            ]
        );
    }

    #[test]
    fn custom_nested_paths_and_normalisation() {
        let dir = tree(&[
            ("src/ai/agents/a.md", ""),
            ("docs/r/x.md", ""),
            ("s/k/SKILL.md", ""),
        ]);
        let p = paths("agents = \"./src//ai/agents/\"\nrules = \"docs/r\"\nskills = \"s\"");
        let d = discover(dir.path(), &p, &Limits::default()).unwrap();
        assert_eq!(
            found(&d),
            [
                (Kind::Agent, "a", "src/ai/agents/a.md"),
                (Kind::Skill, "k", "s/k/SKILL.md"),
                (Kind::Rule, "x", "docs/r/x.md"),
            ]
        );
    }

    #[test]
    fn overlapping_directories_are_a_configuration_error() {
        let dir = tempfile::tempdir().unwrap();
        for extra in [
            "agents = \"x\"\nrules = \"x\"",
            "agents = \"x\"\nrules = \"x/y\"",
            "skills = \"a/b\"\nrules = \"a\"",
        ] {
            let error = discover(dir.path(), &paths(extra), &Limits::default()).unwrap_err();
            assert!(error.to_string().contains("overlap"), "{extra}: {error}");
        }
        // Same prefix text but different components is fine.
        let p = paths("agents = \"ab\"\nrules = \"a\"");
        assert!(discover(dir.path(), &p, &Limits::default()).is_ok());
    }

    #[test]
    fn gitignore_in_the_root_is_honored_without_a_git_repo() {
        let dir = tree(&[
            (
                ".gitignore",
                "agents/draft-*.md\nskills/private/\n/rules/local.md\n",
            ),
            ("agents/draft-1.md", ""),
            ("agents/keep.md", ""),
            ("skills/private/SKILL.md", ""),
            ("skills/public/SKILL.md", ""),
            ("rules/local.md", ""),
            ("rules/shared.md", ""),
            ("agents/.gitignore", "inner.md\n"),
            ("agents/inner.md", ""),
        ]);
        let d = run(&dir);
        assert_eq!(
            found(&d),
            [
                (Kind::Agent, "keep", "agents/keep.md"),
                (Kind::Skill, "public", "skills/public/SKILL.md"),
                (Kind::Rule, "shared", "rules/shared.md"),
            ]
        );
        assert!(d.problems.is_empty(), "{:?}", d.problems);
    }

    #[test]
    fn ignore_files_outside_the_root_are_not_read() {
        let outer = tree(&[(".gitignore", "*.md\n"), ("proj/agents/a.md", "")]);
        let d = discover(
            &outer.path().join("proj"),
            &Paths::default(),
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(found(&d), [(Kind::Agent, "a", "agents/a.md")]);
    }

    #[test]
    fn source_is_returned_byte_for_byte() {
        let text = "\u{feff}---\r\nname: a\r\n---\r\nbody\r\n";
        let dir = tree(&[("agents/a.md", text)]);
        assert_eq!(run(&dir).files[0].source, text);
    }

    #[test]
    fn size_limits() {
        let dir = tree(&[("agents/big.md", "0123456789"), ("agents/ok.md", "01234")]);
        let limits = Limits {
            max_file_bytes: 5,
            ..Limits::default()
        };
        let d = discover(dir.path(), &Paths::default(), &limits).unwrap();
        assert_eq!(found(&d), [(Kind::Agent, "ok", "agents/ok.md")]);
        assert_eq!(kinds(&d), [("agents/big.md", ProblemKind::TooLarge)]);

        let limits = Limits {
            max_total_bytes: 12,
            ..Limits::default()
        };
        let d = discover(dir.path(), &Paths::default(), &limits).unwrap();
        assert_eq!(d.files.len(), 1);
        assert_eq!(kinds(&d).len(), 1);
        assert_eq!(d.problems[0].kind, ProblemKind::LimitExceeded);
    }

    #[test]
    fn entry_limit_stops_the_scan_and_says_so() {
        let files: Vec<_> = (0..20).map(|i| (format!("agents/{i:02}.md"), "")).collect();
        let dir = tree(
            &files
                .iter()
                .map(|(p, t)| (p.as_str(), *t))
                .collect::<Vec<_>>(),
        );
        let limits = Limits {
            max_entries: 5,
            ..Limits::default()
        };
        let d = discover(dir.path(), &Paths::default(), &limits).unwrap();
        assert!(d.files.len() < 5);
        assert_eq!(kinds(&d), [(".", ProblemKind::LimitExceeded)]);
    }

    #[test]
    fn invalid_utf8_reports_the_line() {
        let dir = tempfile::tempdir().unwrap();
        fs::create_dir(dir.path().join("agents")).unwrap();
        fs::write(
            dir.path().join("agents/bad.md"),
            b"ok\nstill ok\nbad \xff\n",
        )
        .unwrap();
        let d = run(&dir);
        assert!(d.files.is_empty());
        assert_eq!(kinds(&d), [("agents/bad.md", ProblemKind::InvalidUtf8)]);
        assert_eq!(d.problems[0].line, Some(3));
    }

    #[test]
    fn a_directory_named_like_a_resource_is_ignored() {
        let dir = tree(&[("agents/dir.md/inner.md", "")]);
        let d = run(&dir);
        assert!(d.files.is_empty() && d.problems.is_empty());
    }

    #[test]
    fn configured_path_that_is_a_file_is_reported() {
        let dir = tree(&[("agents", "not a dir"), ("rules/a.md", "")]);
        let d = run(&dir);
        assert_eq!(kinds(&d), [("agents", ProblemKind::NotADirectory)]);
        assert_eq!(found(&d), [(Kind::Rule, "a", "rules/a.md")]);
    }

    #[cfg(unix)]
    mod symlinks {
        use super::*;
        use std::os::unix::fs::symlink;

        #[test]
        fn symlinked_files_and_directories_are_reported_and_never_read() {
            let outside = tree(&[("secret.md", "TOP SECRET"), ("dir/SKILL.md", "TOP SECRET")]);
            let dir = tree(&[("agents/real.md", "real"), ("skills/ok/SKILL.md", "")]);
            let p = dir.path();
            symlink(outside.path().join("secret.md"), p.join("agents/link.md")).unwrap();
            symlink(outside.path().join("dir"), p.join("skills/linked")).unwrap();
            symlink(outside.path(), p.join("rules")).unwrap();
            let d = run(&dir);
            assert_eq!(
                found(&d),
                [
                    (Kind::Agent, "real", "agents/real.md"),
                    (Kind::Skill, "ok", "skills/ok/SKILL.md"),
                ]
            );
            assert_eq!(
                kinds(&d),
                [
                    ("agents/link.md", ProblemKind::Symlink),
                    ("rules", ProblemKind::Symlink),
                    ("skills/linked", ProblemKind::Symlink),
                ]
            );
            assert!(d.files.iter().all(|f| !f.source.contains("SECRET")));
        }

        #[test]
        fn symlink_in_a_parent_of_a_configured_directory_is_not_followed() {
            let outside = tree(&[("agents/a.md", "outside")]);
            let dir = tempfile::tempdir().unwrap();
            symlink(outside.path(), dir.path().join("src")).unwrap();
            let p = paths("agents = \"src/agents\"");
            let d = discover(dir.path(), &p, &Limits::default()).unwrap();
            assert!(d.files.is_empty());
            assert_eq!(kinds(&d), [("src", ProblemKind::Symlink)]);
        }

        #[test]
        fn symlinked_skill_file_and_dangling_links_are_reported() {
            let dir = tree(&[("skills/s/other", "")]);
            symlink("/nonexistent", dir.path().join("skills/s/SKILL.md")).unwrap();
            symlink("/nonexistent", dir.path().join("agents-dangling")).unwrap();
            fs::create_dir(dir.path().join("agents")).unwrap();
            symlink("nowhere.md", dir.path().join("agents/dangling.md")).unwrap();
            let d = run(&dir);
            assert!(d.files.is_empty());
            assert_eq!(
                kinds(&d),
                [
                    ("agents/dangling.md", ProblemKind::Symlink),
                    ("skills/s/SKILL.md", ProblemKind::Symlink),
                ]
            );
        }

        #[test]
        fn fifos_are_not_opened() {
            let dir = tree(&[("agents/a.md", "")]);
            let fifo = dir.path().join("agents/pipe.md");
            let status = std::process::Command::new("mkfifo")
                .arg(&fifo)
                .status()
                .unwrap();
            assert!(status.success());
            // Would hang forever if it were opened for reading.
            let d = run(&dir);
            assert_eq!(kinds(&d), [("agents/pipe.md", ProblemKind::NotRegularFile)]);
        }
    }
}
