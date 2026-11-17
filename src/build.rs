//! Builds the Claude-native staging tree from an [`Inventory`].
//!
//! The output is a *staging* tree, never an installation: `<out>/.claude/{agents,skills,rules}`,
//! `<out>/manifest.json` and, only when `paths.instructions` is set, `<out>/CLAUDE.md`. Sources are untrusted data: nothing is executed or fetched, and
//! symlinks and special files are rejected. The tree is written to a sibling temp directory and
//! renamed into place only after every file was written, so a failed build leaves no partial output.
//!
//! Files are copied byte for byte. Only an explicit `[[targets.claude.map]]` override rewrites the
//! `model` and `effort` lines of the frontmatter, and the rewrite is verified by parsing it again.

use std::collections::{BTreeMap, BTreeSet};
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::config::Config;
use crate::diagnostics::{Exit, Problem, ProblemKind};
use crate::discovery::Limits;
use crate::frontmatter;
use crate::inventory::{Inventory, Resource};
use crate::manifest::{self, Manifest, Transform};
use crate::report;
use crate::stage;
use crate::target::{self, Resolution, Via};

pub use crate::manifest::{FILE as MANIFEST_FILE, VERSION as MANIFEST_VERSION};
/// Default output directory, relative to the project root. Git-ignored by convention.
pub const DEFAULT_OUT: &str = ".generated/claude";
/// Claude Code preloads at most this many skills into one agent.
const MAX_PRELOADED_SKILLS: usize = 32;
const MAX_DEPTH: usize = 16;
/// Instructions are read into every Claude Code session, so far less than a skill asset is allowed.
const MAX_INSTRUCTIONS_BYTES: u64 = 1024 * 1024;

/// Scan limits for a build: the inventory defaults, with room for binary skill assets.
pub fn limits() -> Limits {
    Limits {
        max_file_bytes: 16 * 1024 * 1024,
        ..Limits::default()
    }
}

/// A problem with the sources that makes the build fail.
#[derive(Debug, PartialEq, Eq)]
pub struct Issue {
    pub path: String,
    pub message: String,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error(
        "the claude target is disabled: set `enabled = true` under [targets.claude] in agenvil.toml"
    )]
    Disabled,
    #[error("unsafe output directory `{path}`: {reason}", path = .0.display(), reason = .1)]
    Out(PathBuf, String),
    #[error("cannot build, the input is incomplete:\n{}", report::problems(.0).trim_end())]
    Incomplete(Vec<Problem>),
    #[error("cannot build, the sources are invalid:\n{}", list(.0))]
    Invalid(Vec<Issue>),
    #[error("the generated tree failed validation, nothing was written:\n{}", list(.0))]
    Output(Vec<Issue>),
    #[error("cannot verify the staging tree: {0}")]
    Unverifiable(String),
    #[error("could not write the output: {0}")]
    Io(#[from] io::Error),
}

fn list(issues: &[Issue]) -> String {
    let lines: Vec<_> = issues
        .iter()
        .map(|i| format!("  {}: {}", i.path, i.message))
        .collect();
    lines.join("\n")
}

impl Error {
    pub fn exit(&self) -> Exit {
        match self {
            Self::Disabled | Self::Out(..) => Exit::Usage,
            Self::Incomplete(_) | Self::Unverifiable(_) => Exit::Unverifiable,
            Self::Invalid(_) | Self::Output(_) => Exit::Findings,
            Self::Io(_) => Exit::Internal,
        }
    }
}

/// One file of the expected staging tree: what a build writes and `build --check` compares to.
#[derive(Debug)]
pub struct Entry {
    /// `/`-separated, relative to the output directory.
    pub path: String,
    /// Relative to the project root.
    pub source: String,
    pub bytes: Vec<u8>,
    pub transform: Transform,
    /// Executable bit of a skill script (Unix only).
    #[cfg_attr(not(unix), allow(dead_code))]
    pub exec: bool,
}

#[derive(Debug, PartialEq, Eq)]
pub struct Summary {
    pub out: PathBuf,
    pub agents: usize,
    pub skills: usize,
    pub rules: usize,
    /// Whether the project-root `CLAUDE.md` was generated.
    pub instructions: bool,
    pub files: usize,
}

/// Builds the Claude staging tree into `out`.
pub fn claude(
    root: &Path,
    config: &Config,
    inventory: &Inventory,
    out: &Path,
) -> Result<Summary, Error> {
    if !config.targets.claude.enabled {
        return Err(Error::Disabled);
    }
    check_out(root, config, out)?;
    let entries = expected(root, config, inventory)?;
    publish(out, &entries)?;
    Ok(Summary {
        out: out.to_path_buf(),
        agents: inventory.agents.len(),
        skills: inventory.skills.len(),
        rules: inventory.rules.len(),
        instructions: config.paths.instructions.is_some(),
        files: entries.len(),
    })
}

/// The files a build would write, sorted by path. The one place the expected tree is computed:
/// a build writes it, `build --check` and cleanup compare against it.
pub fn expected(root: &Path, config: &Config, inventory: &Inventory) -> Result<Vec<Entry>, Error> {
    if !config.targets.claude.enabled {
        return Err(Error::Disabled);
    }
    if !inventory.problems.is_empty() {
        return Err(Error::Incomplete(inventory.problems.clone()));
    }
    plan(root, config, inventory)
}

/// The manifest of `entries`, files in the order given.
pub fn manifest_of(entries: &[Entry]) -> Manifest {
    Manifest::new(
        entries
            .iter()
            .map(|e| manifest::File {
                path: e.path.clone(),
                source: e.source.clone(),
                sha256: manifest::sha256(&e.bytes),
                bytes: e.bytes.len() as u64,
                transform: e.transform,
            })
            .collect(),
    )
}

fn plan(root: &Path, config: &Config, inventory: &Inventory) -> Result<Vec<Entry>, Error> {
    let mut issues = Vec::new();
    let mut problems = Vec::new();
    let mut entries = Vec::new();
    let limits = limits();
    let skill_names: BTreeSet<&str> = inventory.skills.iter().map(|s| s.name.as_str()).collect();

    let names = inventory
        .agents
        .iter()
        .chain(&inventory.skills)
        .chain(&inventory.rules);
    for res in names {
        if !portable_name(&res.name) {
            issues.push(Issue {
                path: res.path.clone(),
                message: "the name must not contain `\\`, `:` or control characters".to_owned(),
            });
        }
    }

    let mut agent_ids: BTreeMap<String, &str> = BTreeMap::new();
    for agent in &inventory.agents {
        let mut fail = |message: String| {
            issues.push(Issue {
                path: agent.path.clone(),
                message,
            })
        };
        let field = |key: &str| agent.frontmatter.as_ref().and_then(|f| f.get(key));
        let text = |key: &str| {
            field(key)
                .and_then(Value::as_str)
                .filter(|s| !s.trim().is_empty())
        };
        if agent.frontmatter.is_none() {
            fail("an agent needs frontmatter with `name` and `description`".to_owned());
        }
        match text("name") {
            Some(name) => {
                if let Some(first) = agent_ids.insert(name.to_owned(), &agent.path) {
                    fail(format!("agent name `{name}` is also used by {first}"));
                }
            }
            None if agent.frontmatter.is_some() => {
                fail(
                    "`name` must be a non-empty string; Claude Code identifies agents by it"
                        .to_owned(),
                );
            }
            None => {}
        }
        if text("description").is_none() && agent.frontmatter.is_some() {
            fail("`description` must be a non-empty string; Claude Code requires it".to_owned());
        }
        for message in skill_refs(field("skills"), &skill_names) {
            fail(message);
        }
        match resource_entry(config, agent, format!(".claude/agents/{}.md", agent.name)) {
            Ok(entry) => entries.push(entry),
            Err(message) => fail(message),
        }
    }

    // One walk for every skill: the entry and byte budgets cover all skill directories together.
    let mut walk = Walk {
        root,
        limits: &limits,
        seen: 0,
        used: 0,
        files: Vec::new(),
        problems: Vec::new(),
    };
    for skill in &inventory.skills {
        let dir = skill.path.rsplit_once('/').map_or("", |(dir, _)| dir);
        let name_field = skill.frontmatter.as_ref().and_then(|f| f.get("name"));
        match name_field {
            Some(Value::String(n)) if *n == skill.name => {}
            None | Some(Value::Null) => issues.push(Issue {
                path: skill.path.clone(),
                message: format!(
                    "frontmatter `name` is required and must equal the directory name `{}`",
                    skill.name
                ),
            }),
            Some(other) => issues.push(Issue {
                path: skill.path.clone(),
                message: format!(
                    "frontmatter `name` ({other}) must equal the directory name `{}`; \
                     agents reference skills by that name",
                    skill.name
                ),
            }),
        }
        let prefix = format!(".claude/skills/{}", skill.name);
        walk.dir(dir, "", 0);
        let mut skill_md = match resource_entry(config, skill, format!("{prefix}/SKILL.md")) {
            Ok(entry) => Some(entry),
            Err(message) => {
                issues.push(Issue {
                    path: skill.path.clone(),
                    message,
                });
                None
            }
        };
        for asset in std::mem::take(&mut walk.files) {
            if asset.under == "SKILL.md" {
                // The checked (and possibly rewritten) copy, not a second read of the file.
                entries.extend(skill_md.take());
            } else {
                entries.push(Entry {
                    path: format!("{prefix}/{}", asset.under),
                    source: asset.source,
                    bytes: asset.bytes,
                    transform: Transform::Copy,
                    exec: asset.exec,
                });
            }
        }
    }

    problems.append(&mut walk.problems);

    for rule in &inventory.rules {
        entries.push(Entry {
            path: format!(".claude/rules/{}.md", rule.name),
            source: rule.path.clone(),
            bytes: rule.source.clone().into_bytes(),
            transform: Transform::Copy,
            exec: false,
        });
    }

    if let Some(source) = &config.paths.instructions {
        match instructions(root, config, source.as_path(), &limits) {
            Ok(entry) => entries.push(entry),
            Err(Ok(issue)) => issues.push(issue),
            Err(Err(problem)) => problems.push(problem),
        }
    }

    if !problems.is_empty() {
        problems.sort();
        return Err(Error::Incomplete(problems));
    }
    for entry in &entries {
        if let Some((_, why)) = manifest::path_flaw(&entry.path) {
            issues.push(Issue {
                path: entry.source.clone(),
                message: format!("output `{}` is not a portable path: {why}", entry.path),
            });
        }
    }
    let total: u64 = entries.iter().map(|e| e.bytes.len() as u64).sum();
    if total > limits.max_total_bytes {
        issues.push(Issue {
            path: ".".to_owned(),
            message: format!(
                "the output is {total} bytes; the limit is {}",
                limits.max_total_bytes
            ),
        });
    }
    issues.extend(collisions(
        entries.iter().map(|e| (e.path.as_str(), e.source.as_str())),
    ));
    if !issues.is_empty() {
        return Err(Error::Invalid(issues));
    }
    entries.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(entries)
}

/// The project-root `CLAUDE.md`, copied byte for byte from the configured source. Nothing in it is
/// interpreted: no templating, no `@import` resolution. A missing, unsafe or unreadable source is
/// a failure, never an empty file. `Err(Ok(_))` is a source-model error, `Err(Err(_))` an input one.
fn instructions(
    root: &Path,
    config: &Config,
    source: &Path,
    limits: &Limits,
) -> Result<Entry, Result<Issue, Problem>> {
    let parts: Option<Vec<&str>> = source
        .components()
        .map(|c| c.as_os_str().to_str())
        .collect();
    let shown = parts.map_or_else(
        || source.to_string_lossy().into_owned(),
        |parts| parts.join("/"),
    );
    let problem = |kind, message: &str| {
        Err(Err(Problem {
            path: shown.clone(),
            line: None,
            kind,
            message: message.to_owned(),
        }))
    };
    if source.to_str().is_none() {
        return problem(ProblemKind::InvalidPath, "the path must be valid UTF-8");
    }
    if let Some((_, why)) = manifest::path_flaw(&shown) {
        return problem(ProblemKind::InvalidPath, &why);
    }
    let paths = &config.paths;
    if [&paths.agents, &paths.skills, &paths.rules]
        .iter()
        .any(|dir| source.starts_with(dir.as_path()))
    {
        return Err(Ok(Issue {
            path: shown,
            message: "`paths.instructions` must not be inside the agents, skills or rules \
                      directories; those are separate concepts"
                .to_owned(),
        }));
    }
    // Every component is checked, so a symlinked parent cannot lead outside the project root.
    let mut path = root.to_path_buf();
    let mut meta = None;
    for part in source.components() {
        path.push(part);
        match fs::symlink_metadata(&path) {
            Ok(m) if m.is_symlink() => {
                return problem(
                    ProblemKind::Symlink,
                    "symlinks are never followed or copied",
                );
            }
            Ok(m) => meta = Some(m),
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return problem(
                    ProblemKind::Missing,
                    "`paths.instructions` is set but the file does not exist",
                );
            }
            Err(_) => return problem(ProblemKind::Unreadable, "could not read file metadata"),
        }
    }
    if !meta.is_some_and(|m| m.is_file()) {
        return problem(ProblemKind::NotRegularFile, "expected a regular file");
    }
    let max = limits.max_file_bytes.min(MAX_INSTRUCTIONS_BYTES);
    let too_large = || {
        problem(
            ProblemKind::TooLarge,
            &format!("larger than the {max}-byte limit"),
        )
    };
    let mut bytes = Vec::new();
    let read = File::open(&path).and_then(|f| f.take(max + 1).read_to_end(&mut bytes));
    if let Err(e) = read {
        return problem(ProblemKind::Unreadable, &e.kind().to_string());
    }
    if bytes.len() as u64 > max {
        return too_large();
    }
    if std::str::from_utf8(&bytes).is_err() {
        return problem(ProblemKind::InvalidUtf8, "instructions must be UTF-8 text");
    }
    Ok(Entry {
        path: manifest::INSTRUCTIONS.to_owned(),
        source: shown,
        bytes,
        transform: Transform::Copy,
        exec: false,
    })
}

/// Problems with an agent's `skills:` list. Claude Code skips an unknown skill silently, so
/// this is where it becomes an error.
fn skill_refs(value: Option<&Value>, known: &BTreeSet<&str>) -> Vec<String> {
    let items = match value {
        None | Some(Value::Null) => return Vec::new(),
        Some(Value::Array(items)) => items,
        Some(_) => return vec!["`skills` must be a YAML list of skill names".to_owned()],
    };
    let mut problems = Vec::new();
    let mut seen = BTreeSet::new();
    if items.len() > MAX_PRELOADED_SKILLS {
        problems.push(format!(
            "`skills` lists {} skills; Claude Code preloads at most {MAX_PRELOADED_SKILLS}",
            items.len()
        ));
    }
    for item in items {
        let Some(name) = item.as_str() else {
            problems.push(format!("`skills` entry {item} is not a string"));
            continue;
        };
        if !seen.insert(name) {
            problems.push(format!("`skills` lists `{name}` twice"));
        } else if !known.contains(name) {
            problems.push(format!(
                "`skills` references `{name}`, which is not a skill in this project"
            ));
        }
    }
    problems
}

/// The entry for an agent or a skill's `SKILL.md`: copied as is unless a mapping applies.
fn resource_entry(config: &Config, res: &Resource, path: String) -> Result<Entry, String> {
    let mut entry = Entry {
        path,
        source: res.path.clone(),
        bytes: res.source.clone().into_bytes(),
        transform: Transform::Copy,
        exec: false,
    };
    let Some(fm) = &res.frontmatter else {
        return Ok(entry);
    };
    match target::claude(config, fm.get("model"), fm.get("effort")).map_err(|d| d.to_string())? {
        Resolution::Resolved {
            target,
            via: Via::Override,
            ..
        } => {
            let text = rewrite(res, &target)?;
            if text != res.source {
                entry.bytes = text.into_bytes();
                entry.transform = Transform::ModelEffortOverride;
            }
        }
        // Nothing to change: the source is already what Claude should read.
        Resolution::Inherit | Resolution::Partial { .. } | Resolution::Resolved { .. } => {}
    }
    Ok(entry)
}

/// Replaces the top-level `model:` and `effort:` lines of the frontmatter. Anything that is not
/// a plain one-line `key: value` (flow style, multi-line values, duplicates) is refused: the
/// result is parsed again and must differ from the source in exactly these two values.
fn rewrite(res: &Resource, target: &[String; 2]) -> Result<String, String> {
    let fm = res
        .frontmatter
        .as_ref()
        .ok_or("no frontmatter to rewrite")?;
    let refused = |why: &str| {
        format!(
            "cannot apply the model mapping to this frontmatter ({why}); write `model` and `effort` as plain one-line values"
        )
    };
    let [model, effort] = target;
    let mut seen = (0, 0);
    let mut out = String::with_capacity(res.source.len());
    for (index, line) in res.source.split_inclusive('\n').enumerate() {
        let line_no = index + 1;
        let inside = line_no > fm.start_line && line_no < fm.end_line;
        let key = line.split(':').next().unwrap_or("").trim_end();
        let new = match (inside && !line.starts_with(char::is_whitespace), key) {
            (true, "model") if line.contains(':') => Some(("model", model, &mut seen.0)),
            (true, "effort") if line.contains(':') => Some(("effort", effort, &mut seen.1)),
            _ => None,
        };
        match new {
            Some((key, value, count)) => {
                *count += 1;
                let eol = &line[line.trim_end_matches(['\r', '\n']).len()..];
                out.push_str(&format!("{key}: {value}{eol}"));
            }
            None => out.push_str(line),
        }
    }
    if seen != (1, 1) {
        return Err(refused(
            "`model` and `effort` must each appear once at the top level",
        ));
    }
    let mut expected = fm.metadata.clone();
    expected.insert("model".to_owned(), Value::String(model.clone()));
    expected.insert("effort".to_owned(), Value::String(effort.clone()));
    match frontmatter::parse(&out) {
        Ok(parsed)
            if parsed
                .frontmatter
                .as_ref()
                .is_some_and(|f| f.metadata == expected) =>
        {
            Ok(out)
        }
        _ => Err(refused(
            "the rewritten frontmatter does not parse back as expected",
        )),
    }
}

/// Output paths that differ only by case (or a file that is also a directory) would overwrite
/// each other on macOS and Windows.
fn collisions<'a>(paths: impl Iterator<Item = (&'a str, &'a str)>) -> Vec<Issue> {
    // shortcut: plain lowercase, no Unicode normalisation (NFC/NFD); extend if sources with such
    // names appear. Windows reserved names are refused separately by `manifest::path_flaw`.
    let mut seen: BTreeMap<String, (String, &str)> = BTreeMap::new();
    let mut issues = Vec::new();
    for (path, source) in paths {
        let mut prefix = String::new();
        for part in path.split('/') {
            if !prefix.is_empty() {
                prefix.push('/');
            }
            prefix.push_str(part);
            let (spelling, first) = seen
                .entry(prefix.to_lowercase())
                .or_insert_with(|| (prefix.clone(), source));
            if *spelling != prefix {
                issues.push(Issue {
                    path: source.to_owned(),
                    message: format!(
                        "output `{prefix}` collides with `{spelling}` (from {first}) on a case-insensitive file system"
                    ),
                });
                break;
            }
        }
    }
    issues
}

struct Asset {
    /// Path under the skill directory.
    under: String,
    /// Path relative to the project root.
    source: String,
    bytes: Vec<u8>,
    exec: bool,
}

/// A bounded, symlink-free walk of one skill directory.
struct Walk<'a> {
    root: &'a Path,
    limits: &'a Limits,
    seen: usize,
    /// Bytes read so far, over all skills.
    used: u64,
    files: Vec<Asset>,
    problems: Vec<Problem>,
}

impl Walk<'_> {
    fn problem(&mut self, path: &str, kind: ProblemKind, message: &str) {
        self.problems.push(Problem {
            path: path.to_owned(),
            line: None,
            kind,
            message: message.to_owned(),
        });
    }

    /// `dir` is relative to the root, `under` to the skill directory (empty for the top).
    fn dir(&mut self, dir: &str, under: &str, depth: usize) {
        let Ok(read) = fs::read_dir(self.root.join(dir)) else {
            return self.problem(dir, ProblemKind::Unreadable, "could not read directory");
        };
        // Never collect more than the budget allows: a huge directory is cut off while reading.
        let room = self.limits.max_entries.saturating_sub(self.seen);
        let mut names = Vec::new();
        for entry in read.take(room + 1) {
            match entry {
                Ok(entry) => names.push(entry.file_name()),
                // A skipped entry would be a silently incomplete build.
                Err(_) => {
                    return self.problem(
                        dir,
                        ProblemKind::Unreadable,
                        "could not read a directory entry",
                    );
                }
            }
        }
        if names.len() > room {
            return self.problem(
                dir,
                ProblemKind::LimitExceeded,
                "too many entries in skill directories; the build is incomplete",
            );
        }
        names.sort();
        for name in names {
            self.seen += 1;
            let shown = format!("{dir}/{}", name.to_string_lossy());
            let Some(name) = name.to_str().filter(|n| portable_name(n)) else {
                self.problem(
                    &shown,
                    ProblemKind::InvalidPath,
                    "file names must be UTF-8 without `\\`, `:` or control characters",
                );
                continue;
            };
            let (rel, below) = (format!("{dir}/{name}"), join(under, name));
            let Ok(meta) = fs::symlink_metadata(self.root.join(&rel)) else {
                self.problem(
                    &rel,
                    ProblemKind::Unreadable,
                    "could not read file metadata",
                );
                continue;
            };
            if meta.is_symlink() {
                self.problem(
                    &rel,
                    ProblemKind::Symlink,
                    "symlinks are never followed or copied",
                );
            } else if meta.is_dir() {
                if depth >= MAX_DEPTH {
                    self.problem(
                        &rel,
                        ProblemKind::LimitExceeded,
                        "directories nested too deep",
                    );
                } else {
                    self.dir(&rel, &below, depth + 1);
                }
            } else if meta.is_file() {
                self.file(rel, below, &meta);
            } else {
                self.problem(&rel, ProblemKind::NotRegularFile, "expected a regular file");
            }
        }
    }

    fn file(&mut self, rel: String, below: String, meta: &fs::Metadata) {
        let size = meta.len();
        let (max, total) = (self.limits.max_file_bytes, self.limits.max_total_bytes);
        let room = total.saturating_sub(self.used);
        if size > max {
            return self.problem(
                &rel,
                ProblemKind::TooLarge,
                &format!("larger than the {max}-byte limit"),
            );
        }
        // The size may be stale, so the bound is enforced again on the bytes actually read.
        let mut bytes = Vec::new();
        let read = File::open(self.root.join(&rel)).and_then(|f| {
            f.take(max.min(room).saturating_add(1))
                .read_to_end(&mut bytes)
        });
        if let Err(e) = read {
            return self.problem(&rel, ProblemKind::Unreadable, &e.kind().to_string());
        }
        let len = bytes.len() as u64;
        if len > max {
            return self.problem(
                &rel,
                ProblemKind::TooLarge,
                &format!("larger than the {max}-byte limit"),
            );
        }
        if len > room {
            return self.problem(
                &rel,
                ProblemKind::LimitExceeded,
                &format!("skills exceed the {total}-byte total limit; the build is incomplete"),
            );
        }
        #[cfg(unix)]
        let exec = std::os::unix::fs::PermissionsExt::mode(&meta.permissions()) & 0o111 != 0;
        #[cfg(not(unix))]
        let exec = false;
        self.used += len;
        self.files.push(Asset {
            under: below,
            source: rel,
            bytes,
            exec,
        });
    }
}

fn join(under: &str, name: &str) -> String {
    if under.is_empty() {
        name.to_owned()
    } else {
        format!("{under}/{name}")
    }
}

fn portable_name(name: &str) -> bool {
    !name.is_empty() && !name.contains(['\\', ':']) && !name.chars().any(char::is_control)
}

/// Rejects an output directory that could damage something that is not Agenvil staging output.
pub fn check_out(root: &Path, config: &Config, out: &Path) -> Result<(), Error> {
    if location(root, config, out)?
        && let Some(why) = foreign(out)?
    {
        return Err(Error::Out(out.to_path_buf(), why));
    }
    Ok(())
}

/// Checks where `out` is, not what is in it: read-only commands stop here. `Ok(true)` if it
/// exists as a directory, `Ok(false)` if it does not exist yet.
pub fn location(root: &Path, config: &Config, out: &Path) -> Result<bool, Error> {
    let fail = |reason: &str| Error::Out(out.to_path_buf(), reason.to_owned());
    if out.components().any(|c| c == Component::ParentDir) {
        return Err(fail("`..` is not allowed"));
    }
    let wanted = resolve(out)?;
    let forbidden = ["HOME", "USERPROFILE"]
        .iter()
        .filter_map(std::env::var_os)
        .map(|home| Path::new(&home).join(".claude"))
        .chain(std::env::var_os("CLAUDE_CONFIG_DIR").map(PathBuf::from))
        .filter_map(|p| resolve(&p).ok());
    for dir in forbidden {
        // Either direction: inside the config directory, or a parent of it (HOME itself).
        if wanted.starts_with(&dir) || dir.starts_with(&wanted) {
            return Err(fail(
                "builds are staging only and never write in or around the Claude configuration directory",
            ));
        }
    }
    if resolve(root)?.starts_with(&wanted) {
        return Err(fail(
            "the project root must not be inside the output directory",
        ));
    }
    let paths = &config.paths;
    for source in [&paths.agents, &paths.skills, &paths.rules] {
        if wanted.starts_with(resolve(&root.join(source.as_path()))?) {
            return Err(fail(
                "the output directory must not be inside a source directory",
            ));
        }
    }
    if let Some(source) = &paths.instructions
        && resolve(&root.join(source.as_path()))?.starts_with(&wanted)
    {
        return Err(fail(
            "the output directory must not contain the instructions source",
        ));
    }
    let meta = match fs::symlink_metadata(out) {
        Ok(meta) => meta,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(e.into()),
    };
    if !meta.is_dir() {
        return Err(fail(
            "it exists and is not a directory (symlinks are not followed)",
        ));
    }
    Ok(true)
}

/// Why `dir` is not safe to replace, or `None` if it is empty or holds only an earlier Agenvil
/// build: its `manifest.json` has the right owner and target and lists every file in it, unchanged.
fn foreign(dir: &Path) -> io::Result<Option<String>> {
    if fs::read_dir(dir)?.next().is_none() {
        return Ok(None);
    }
    stage::foreign(dir)
}

/// Absolute path with the longest existing prefix canonicalised, so a symlinked parent cannot
/// hide a forbidden location.
fn resolve(path: &Path) -> io::Result<PathBuf> {
    let abs = std::path::absolute(path)?;
    let mut rest = Vec::new();
    let mut base = abs.as_path();
    loop {
        if let Ok(real) = fs::canonicalize(base) {
            return Ok(rest.iter().rev().fold(real, |acc, part| acc.join(part)));
        }
        match (base.file_name(), base.parent()) {
            (Some(name), Some(parent)) => {
                rest.push(name.to_owned());
                base = parent;
            }
            _ => return Ok(abs),
        }
    }
}

/// Writes the tree next to `out` and renames it into place; removes the temp tree on any failure.
fn publish(out: &Path, entries: &[Entry]) -> Result<(), Error> {
    let manifest = manifest_of(entries).to_json().map_err(io::Error::from)?;
    let parent = match out.parent() {
        Some(p) if !p.as_os_str().is_empty() => p,
        _ => Path::new("."),
    };
    fs::create_dir_all(parent)?;
    let name = out
        .file_name()
        .map_or_else(String::new, |n| n.to_string_lossy().into_owned());
    let pid = std::process::id();
    let tmp = parent.join(format!(".{name}.agenvil-tmp-{pid}"));
    fs::create_dir(&tmp)?;
    let written = write_tree(&tmp, entries, &manifest)
        .map_err(Error::from)
        .and_then(|()| verify_written(&tmp))
        .and_then(|()| replace(out, &tmp, parent, &name, pid));
    if written.is_err() {
        let _ = fs::remove_dir_all(&tmp);
    }
    written
}

/// The last gate before the swap: what is on disk is a valid generated tree, read back.
fn verify_written(tmp: &Path) -> Result<(), Error> {
    let found = stage::validate_dir(tmp)?;
    if found.is_empty() {
        return Ok(());
    }
    Err(Error::Output(
        found
            .into_iter()
            .map(|f| Issue {
                path: f.path,
                message: f.detail,
            })
            .collect(),
    ))
}

fn write_tree(dir: &Path, entries: &[Entry], manifest: &str) -> io::Result<()> {
    for entry in entries {
        let path = entry
            .path
            .split('/')
            .fold(dir.to_path_buf(), |p, part| p.join(part));
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }
        let mut file = OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)?;
        file.write_all(&entry.bytes)?;
        // Scripts keep their executable bit; no other mode bit of an untrusted source is copied.
        #[cfg(unix)]
        if entry.exec {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(fs::Permissions::from_mode(0o755))?;
        }
    }
    fs::write(dir.join(MANIFEST_FILE), manifest)
}

fn replace(out: &Path, tmp: &Path, parent: &Path, name: &str, pid: u32) -> Result<(), Error> {
    if fs::symlink_metadata(out).is_err() {
        return Ok(fs::rename(tmp, out)?);
    }
    let old = parent.join(format!(".{name}.agenvil-old-{pid}"));
    fs::rename(out, &old)?;
    // Ownership was checked before the build, but the directory may have changed since. Check
    // again on the moved copy, where nothing can add to it, before anything is deleted.
    let checked = foreign(&old)
        .map_err(Error::from)
        .and_then(|why| match why {
            None => Ok(()),
            Some(why) => Err(Error::Out(out.to_path_buf(), why)),
        });
    if let Err(e) = checked {
        let _ = fs::rename(&old, out);
        return Err(e);
    }
    if let Err(e) = fs::rename(tmp, out) {
        let _ = fs::rename(&old, out);
        return Err(e.into());
    }
    // The new tree is already in place; a leftover backup is only clutter.
    if let Err(e) = fs::remove_dir_all(&old) {
        eprintln!(
            "warning: could not remove the previous output {}: {e}",
            old.display()
        );
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn missing_instructions_are_classified_as_missing_input_with_portable_provenance() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::parse(
            "schema = 1\n[project]\nname = \"demo\"\n[paths]\ninstructions = \"project/instructions.md\"\n",
            Path::new("agenvil.toml"),
        )
        .unwrap();
        let source = config.paths.instructions.as_ref().unwrap().as_path();
        let Err(Err(problem)) = instructions(dir.path(), &config, source, &limits()) else {
            panic!("missing instructions must be an input problem");
        };
        assert_eq!(problem.kind, ProblemKind::Missing);
        assert_eq!(problem.path, "project/instructions.md");
        assert!(problem.message.contains("paths.instructions"));
        assert!(problem.message.contains("file does not exist"));
    }

    #[test]
    fn collisions_ignore_case_and_catch_file_directory_clashes() {
        let found = collisions(
            [
                (".claude/agents/Foo.md", "agents/Foo.md"),
                (".claude/agents/foo.md", "agents/foo.md"),
                (".claude/skills/a/SKILL.md", "skills/a/SKILL.md"),
                (".claude/skills/A/SKILL.md", "skills/A/SKILL.md"),
                (".claude/agents/bar.md", "agents/bar.md"),
            ]
            .into_iter(),
        );
        let paths: Vec<_> = found.iter().map(|i| i.path.as_str()).collect();
        assert_eq!(paths, ["agents/foo.md", "skills/A/SKILL.md"]);
        assert!(found[0].message.contains("case-insensitive"));
    }

    #[test]
    fn replace_rechecks_ownership_after_moving_the_old_output_aside() {
        let dir = tempfile::tempdir().unwrap();
        let (out, tmp) = (dir.path().join("out"), dir.path().join("tmp"));
        fs::create_dir_all(&out).unwrap();
        fs::create_dir_all(&tmp).unwrap();
        // The directory became foreign after check_out passed: it has no manifest.
        fs::write(out.join("notes.txt"), "mine").unwrap();
        let err = replace(&out, &tmp, dir.path(), "out", 1).unwrap_err();
        assert!(matches!(err, Error::Out(..)));
        assert_eq!(fs::read(out.join("notes.txt")).unwrap(), b"mine");
        assert!(tmp.exists());
    }

    #[test]
    fn skill_refs_flag_missing_duplicate_and_malformed_entries() {
        let known = BTreeSet::from(["tdd", "commit"]);
        assert!(skill_refs(None, &known).is_empty());
        assert!(skill_refs(Some(&json!(["tdd", "commit"])), &known).is_empty());
        let found = skill_refs(Some(&json!(["tdd", "tdd", "nope", 3])), &known);
        assert_eq!(found.len(), 3);
        assert!(found[0].contains("twice"));
        assert!(found[1].contains("not a skill in this project"));
        assert!(found[2].contains("not a string"));
        assert_eq!(skill_refs(Some(&json!("tdd")), &known).len(), 1);
    }
}
