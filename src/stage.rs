//! Reads a staging tree back and judges it: drift against the expected plan (`build --check`),
//! safe cleanup of stale files, and validation of the generated output on its own.
//!
//! Nothing here follows symlinks, and nothing here writes except [`clean`] with `apply`, which
//! deletes only files whose digest still matches the manifest that claims them.

use std::collections::{BTreeMap, BTreeSet};
use std::fmt;
use std::fs::{self, File, OpenOptions};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::build::{self, Entry, Error};
use crate::config::Config;
use crate::frontmatter;
use crate::manifest::{self, Manifest};
use crate::target;

/// Deepest directory nesting read; the builder itself stops at 16.
const MAX_DEPTH: usize = 24;
/// Claude Code preloads at most this many skills into one agent.
const MAX_PRELOADED_SKILLS: usize = 32;

/// What is wrong with a path. The declaration order is the order of the report.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Kind {
    /// Expected, not on disk.
    Missing,
    /// On disk, different bytes (size or SHA-256) than expected.
    Changed,
    /// Owned by the previous build according to the manifest, no longer generated.
    Stale,
    /// On disk, neither generated nor claimed by the manifest.
    Extra,
    /// The manifest is missing, malformed, forged, or disagrees with the plan.
    ManifestInvalid,
    /// A symlink, special file, non-UTF-8 name or escaping path: nothing is trusted or touched.
    Unsafe,
    /// The generated tree itself is not valid Claude output (see [`validate`]).
    Invalid,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Missing => "missing",
            Self::Changed => "changed",
            Self::Stale => "stale",
            Self::Extra => "extra",
            Self::ManifestInvalid => "manifest-invalid",
            Self::Unsafe => "unsafe",
            Self::Invalid => "invalid",
        })
    }
}

/// One difference. Sorted by `(kind, path, detail)`, which is the report order.
#[derive(Clone, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub struct Finding {
    pub kind: Kind,
    pub path: String,
    pub detail: String,
}

fn finding(kind: Kind, path: &str, detail: impl Into<String>) -> Finding {
    Finding {
        kind,
        path: path.to_owned(),
        detail: detail.into(),
    }
}

fn sorted(mut findings: Vec<Finding>) -> Vec<Finding> {
    findings.sort();
    findings.dedup();
    findings
}

/// Whether any finding means the output location is not safe to trust or touch (exit 2).
pub fn is_unsafe(findings: &[Finding]) -> bool {
    findings.iter().any(|f| f.kind == Kind::Unsafe)
}

/// The deterministic human-readable report: one line per finding, then a count.
pub fn report(out: &Path, findings: &[Finding]) -> String {
    if findings.is_empty() {
        return format!("ok: {} matches the sources\n", out.display());
    }
    let mut text = String::new();
    for f in findings {
        text.push_str(&format!(
            "{:<16} {}: {}\n",
            f.kind.to_string(),
            f.path,
            f.detail
        ));
    }
    text.push_str(&format!(
        "drift: {} finding(s) in {}\n",
        findings.len(),
        out.display()
    ));
    text
}

/// One thing found on disk.
enum Node {
    File(Vec<u8>),
    /// Larger than the per-file limit; not read.
    Oversize(u64),
    /// A symlink, a special file or a name that is not UTF-8; never read or followed.
    Unsafe(String),
}

/// Every file below a staging root, by `/`-separated relative path. Empty directories are not
/// recorded: they hold nothing that could drift.
struct Tree {
    nodes: BTreeMap<String, Node>,
    /// Files with an executable bit (Unix only; always empty elsewhere).
    runnable: BTreeSet<String>,
}

#[derive(Debug)]
enum ScanError {
    Io(io::Error),
    Limit(String),
}

impl From<io::Error> for ScanError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

impl From<ScanError> for Error {
    fn from(e: ScanError) -> Self {
        match e {
            ScanError::Io(e) => Self::Io(e),
            ScanError::Limit(why) => Self::Unverifiable(why),
        }
    }
}

struct Scan {
    nodes: BTreeMap<String, Node>,
    runnable: BTreeSet<String>,
    entries: usize,
    bytes: u64,
    max_file: u64,
    max_total: u64,
}

fn scan(root: &Path) -> Result<Tree, ScanError> {
    let limits = build::limits();
    let mut state = Scan {
        nodes: BTreeMap::new(),
        runnable: BTreeSet::new(),
        entries: 0,
        bytes: 0,
        max_file: limits.max_file_bytes,
        max_total: limits.max_total_bytes,
    };
    walk(root, "", 0, &mut state)?;
    Ok(Tree {
        nodes: state.nodes,
        runnable: state.runnable,
    })
}

fn walk(dir: &Path, under: &str, depth: usize, state: &mut Scan) -> Result<(), ScanError> {
    if depth > MAX_DEPTH {
        return Err(ScanError::Limit(format!(
            "directories are nested deeper than {MAX_DEPTH} levels at `{under}`"
        )));
    }
    let mut names: Vec<_> = fs::read_dir(dir)?
        .map(|e| e.map(|e| e.file_name()))
        .collect::<Result<_, _>>()?;
    names.sort();
    for name in names {
        state.entries += 1;
        if state.entries > manifest::MAX_ENTRIES {
            return Err(ScanError::Limit(format!(
                "more than {} entries",
                manifest::MAX_ENTRIES
            )));
        }
        let Some(text) = name.to_str() else {
            let shown = format!("{under}{}", name.to_string_lossy());
            state.nodes.insert(shown, Node::Unsafe(NOT_UTF8.to_owned()));
            continue;
        };
        let rel = format!("{under}{text}");
        let path = dir.join(&name);
        let meta = fs::symlink_metadata(&path)?;
        let kind = meta.file_type();
        if kind.is_dir() {
            walk(&path, &format!("{rel}/"), depth + 1, state)?;
        } else if kind.is_file() {
            if meta.len() > state.max_file {
                state.nodes.insert(rel, Node::Oversize(meta.len()));
                continue;
            }
            state.bytes += meta.len();
            if state.bytes > state.max_total {
                return Err(ScanError::Limit(format!(
                    "more than {} bytes in total",
                    state.max_total
                )));
            }
            let mut bytes = Vec::with_capacity(meta.len() as usize);
            let mut file = File::open(&path)?;
            // The path was a regular file a moment ago; if it was swapped, stop reading anyway.
            if !file.metadata()?.is_file() {
                state
                    .nodes
                    .insert(rel, Node::Unsafe("changed while it was read".to_owned()));
                continue;
            }
            #[cfg(unix)]
            if std::os::unix::fs::PermissionsExt::mode(&file.metadata()?.permissions()) & 0o111 != 0
            {
                state.runnable.insert(rel.clone());
            }
            (&mut file)
                .take(state.max_file + 1)
                .read_to_end(&mut bytes)?;
            if bytes.len() as u64 > state.max_file {
                state.nodes.insert(rel, Node::Oversize(bytes.len() as u64));
            } else {
                state.nodes.insert(rel, Node::File(bytes));
            }
        } else {
            let what = if kind.is_symlink() {
                "symlink"
            } else {
                "special file"
            };
            state.nodes.insert(rel, Node::Unsafe(what.to_owned()));
        }
    }
    Ok(())
}

/// The manifest of a tree, or why it cannot be trusted as `(unsafe, path, message)` entries.
fn load_manifest(tree: &Tree) -> Result<Manifest, Vec<Finding>> {
    let bad = |kind, message: String| Err(vec![finding(kind, manifest::FILE, message)]);
    let bytes = match tree.nodes.get(manifest::FILE) {
        None => {
            return bad(
                Kind::ManifestInvalid,
                "missing; the tree is not an Agenvil build".to_owned(),
            );
        }
        Some(Node::Unsafe(what)) => {
            return bad(Kind::Unsafe, format!("is a {what}, not a regular file"));
        }
        Some(Node::Oversize(n)) => {
            return bad(Kind::ManifestInvalid, format!("{n} bytes is too large"));
        }
        Some(Node::File(bytes)) if bytes.len() as u64 > manifest::MAX_BYTES => {
            return bad(Kind::ManifestInvalid, "is too large".to_owned());
        }
        Some(Node::File(bytes)) => bytes,
    };
    let manifest = match Manifest::parse(bytes) {
        Ok(m) => m,
        Err(why) => return bad(Kind::ManifestInvalid, why),
    };
    let flaws = manifest.flaws();
    if flaws.is_empty() {
        return Ok(manifest);
    }
    Err(flaws
        .into_iter()
        .map(|f| {
            let kind = if f.unsafe_path {
                Kind::Unsafe
            } else {
                Kind::ManifestInvalid
            };
            finding(kind, &f.path, f.message)
        })
        .collect())
}

fn digest_matches(node: &Node, sha256: &str, bytes: u64) -> bool {
    match node {
        Node::File(data) => data.len() as u64 == bytes && manifest::sha256(data) == sha256,
        Node::Oversize(_) | Node::Unsafe(_) => false,
    }
}

/// Compares the staging tree at `out` with `expected`, reading nothing else and writing nothing.
/// A missing `out` is one `missing` finding; an `out` that is a symlink or not a directory is an
/// [`Error::Out`].
pub fn check(
    root: &Path,
    config: &Config,
    out: &Path,
    expected: &[Entry],
) -> Result<Vec<Finding>, Error> {
    if !build::location(root, config, out)? {
        return Ok(vec![finding(
            Kind::Missing,
            ".",
            "the staging directory does not exist; run `agenvil build --target claude`",
        )]);
    }
    let tree = scan(out)?;
    let mut findings = compare(&tree, expected);
    findings.extend(validate_tree(&tree));
    Ok(sorted(findings))
}

fn compare(tree: &Tree, expected: &[Entry]) -> Vec<Finding> {
    let mut findings = Vec::new();
    for (path, node) in &tree.nodes {
        if let Node::Unsafe(what) = node {
            findings.push(finding(
                Kind::Unsafe,
                path,
                format!("{what}; not followed or trusted"),
            ));
        }
    }
    let trusted = match load_manifest(tree) {
        Ok(m) => Some(m),
        Err(mut flaws) => {
            findings.append(&mut flaws);
            None
        }
    };
    let owned: BTreeMap<&str, &manifest::File> = trusted
        .iter()
        .flat_map(|m| m.files.iter())
        .map(|f| (f.path.as_str(), f))
        .collect();
    let plan: BTreeMap<&str, &Entry> = expected.iter().map(|e| (e.path.as_str(), e)).collect();

    for entry in expected {
        let path = entry.path.as_str();
        let want = manifest::sha256(&entry.bytes);
        match tree.nodes.get(path) {
            None => findings.push(finding(Kind::Missing, path, "not in the staging tree")),
            Some(Node::Unsafe(_)) => {}
            Some(Node::Oversize(n)) => findings.push(finding(
                Kind::Changed,
                path,
                format!("{n} bytes, expected {}", entry.bytes.len()),
            )),
            Some(Node::File(data)) if data.len() != entry.bytes.len() => findings.push(finding(
                Kind::Changed,
                path,
                format!("{} bytes, expected {}", data.len(), entry.bytes.len()),
            )),
            Some(Node::File(data)) if manifest::sha256(data) != want => findings.push(finding(
                Kind::Changed,
                path,
                format!(
                    "sha256 {} differs from expected {want}",
                    manifest::sha256(data)
                ),
            )),
            // A script the build marks executable must still be (Unix only; a build keeps the bit).
            Some(Node::File(_)) if cfg!(unix) && entry.exec && !tree.runnable.contains(path) => {
                findings.push(finding(
                    Kind::Changed,
                    path,
                    "not executable, expected the executable bit",
                ));
            }
            Some(Node::File(_)) => {
                let Some(m) = trusted.as_ref().map(|_| owned.get(path)) else {
                    continue;
                };
                let agrees = m.is_some_and(|m| {
                    m.sha256 == want
                        && m.bytes == entry.bytes.len() as u64
                        && m.source == entry.source
                        && m.transform == entry.transform
                });
                if !agrees {
                    findings.push(finding(
                        Kind::ManifestInvalid,
                        path,
                        "the manifest does not describe this file as the build would",
                    ));
                }
            }
        }
    }

    for (path, node) in &tree.nodes {
        if path == manifest::FILE
            || plan.contains_key(path.as_str())
            || matches!(node, Node::Unsafe(_))
        {
            continue;
        }
        match owned.get(path.as_str()) {
            Some(m) if digest_matches(node, &m.sha256, m.bytes) => findings.push(finding(
                Kind::Stale,
                path,
                "written by the previous build, no longer generated",
            )),
            Some(_) => findings.push(finding(
                Kind::Stale,
                path,
                "no longer generated, and modified since the build",
            )),
            None => findings.push(finding(
                Kind::Extra,
                path,
                "not generated and not in the manifest",
            )),
        }
    }
    for path in owned.keys() {
        if !plan.contains_key(path) && !tree.nodes.contains_key(*path) {
            findings.push(finding(
                Kind::Stale,
                path,
                "listed in the manifest, no longer generated, already gone",
            ));
        }
    }
    findings
}

const NOT_UTF8: &str = "name is not valid UTF-8";

/// Why `dir` (not empty) may not be replaced by a build, or `None` if it holds only an earlier
/// Agenvil build: a valid manifest, and every file in the tree listed in it and unmodified.
/// Files the manifest lists but that are gone are fine.
pub fn foreign(dir: &Path) -> io::Result<Option<String>> {
    let tree = match scan(dir) {
        Ok(tree) => tree,
        Err(ScanError::Io(e)) => return Err(e),
        Err(ScanError::Limit(why)) => {
            return Ok(Some(format!(
                "it is too large to verify as Agenvil output ({why})"
            )));
        }
    };
    let manifest = match load_manifest(&tree) {
        Ok(m) => m,
        Err(flaws) => {
            let why = flaws.first().map_or("unreadable", |f| f.detail.as_str());
            return Ok(Some(format!(
                "it is not empty and has no Agenvil {} ({why}), so it is not safe to replace; \
                 choose another --out or remove it yourself",
                manifest::FILE
            )));
        }
    };
    let owned: BTreeMap<&str, &manifest::File> = manifest
        .files
        .iter()
        .map(|f| (f.path.as_str(), f))
        .collect();
    for (path, node) in &tree.nodes {
        if path == manifest::FILE {
            continue;
        }
        match (node, owned.get(path.as_str())) {
            // A name that is not UTF-8 can never be listed, so it is simply not Agenvil's.
            (Node::Unsafe(what), _) if *what != NOT_UTF8 => {
                return Ok(Some(format!(
                    "`{path}` is a {what}, so the directory is not safe to replace"
                )));
            }
            (_, None) => {
                return Ok(Some(format!(
                    "`{path}` was not written by Agenvil, so the directory is not safe to replace"
                )));
            }
            (node, Some(m)) if !digest_matches(node, &m.sha256, m.bytes) => {
                return Ok(Some(format!(
                    "`{path}` was changed since the build, so the directory is not safe to replace"
                )));
            }
            _ => {}
        }
    }
    Ok(None)
}

/// What [`clean`] decided, in path order.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct Cleanup {
    /// Owned, unmodified and no longer generated: deleted (or, in a dry run, would be).
    pub remove: Vec<String>,
    /// Listed in the manifest but already gone; only the manifest entry is dropped.
    pub forget: Vec<String>,
    /// Owned and no longer generated but modified since the build: left alone.
    pub keep: Vec<String>,
}

/// Removes the stale files of the staging tree at `out`: those the previous manifest claims,
/// the current plan no longer generates, and whose digest still matches. Dry run unless `apply`.
///
/// Returns the findings that block cleanup instead if the tree is not safe to touch: any
/// `unsafe`, `manifest-invalid` or `invalid` finding means nothing is deleted. A missing staging
/// directory has nothing to clean.
pub fn clean(
    root: &Path,
    config: &Config,
    out: &Path,
    expected: &[Entry],
    apply: bool,
) -> Result<Result<Cleanup, Vec<Finding>>, Error> {
    if !build::location(root, config, out)? {
        return Ok(Ok(Cleanup::default()));
    }
    let tree = scan(out)?;
    let manifest = match load_manifest(&tree) {
        Ok(m) => m,
        Err(blockers) => return Ok(Err(sorted(blockers))),
    };
    let blockers: Vec<_> = compare(&tree, expected)
        .into_iter()
        .filter(|f| matches!(f.kind, Kind::Unsafe | Kind::ManifestInvalid))
        .collect();
    if !blockers.is_empty() {
        return Ok(Err(sorted(blockers)));
    }
    let plan: BTreeMap<&str, &Entry> = expected.iter().map(|e| (e.path.as_str(), e)).collect();
    let mut plan_out = Cleanup::default();
    for m in &manifest.files {
        if plan.contains_key(m.path.as_str()) {
            continue;
        }
        match tree.nodes.get(&m.path) {
            None => plan_out.forget.push(m.path.clone()),
            Some(node) if digest_matches(node, &m.sha256, m.bytes) => {
                plan_out.remove.push(m.path.clone());
            }
            Some(_) => plan_out.keep.push(m.path.clone()),
        }
    }
    plan_out.remove.sort();
    plan_out.forget.sort();
    plan_out.keep.sort();
    if apply {
        apply_cleanup(out, &manifest, &plan_out)?;
    }
    Ok(Ok(plan_out))
}

fn apply_cleanup(out: &Path, manifest: &Manifest, cleanup: &Cleanup) -> Result<(), Error> {
    let owned: BTreeMap<&str, &manifest::File> = manifest
        .files
        .iter()
        .map(|f| (f.path.as_str(), f))
        .collect();
    for path in &cleanup.remove {
        let target = join(out, path);
        let m = owned[path.as_str()];
        // Checked again right before the delete: the tree may have changed since the scan.
        // Every ancestor must still be a real directory, or the path could leave `out`.
        let mut ancestor = out.to_path_buf();
        for part in path.split('/').take(path.split('/').count() - 1) {
            ancestor.push(part);
            if !fs::symlink_metadata(&ancestor)?.file_type().is_dir() {
                return Err(Error::Out(
                    out.to_path_buf(),
                    format!("`{path}` changed during cleanup; nothing more was deleted"),
                ));
            }
        }
        let meta = fs::symlink_metadata(&target)?;
        if !meta.file_type().is_file() || meta.len() != m.bytes {
            return Err(Error::Out(
                out.to_path_buf(),
                format!("`{path}` changed during cleanup; nothing more was deleted"),
            ));
        }
        if manifest::sha256(&fs::read(&target)?) != m.sha256 {
            return Err(Error::Out(
                out.to_path_buf(),
                format!("`{path}` changed during cleanup; nothing more was deleted"),
            ));
        }
        fs::remove_file(&target)?;
        // Prune directories this emptied; `remove_dir` refuses a directory with anything left.
        let mut dir = target.parent();
        while let Some(d) = dir.filter(|d| *d != out) {
            if fs::remove_dir(d).is_err() {
                break;
            }
            dir = d.parent();
        }
    }
    let dropped: BTreeSet<&str> = cleanup
        .remove
        .iter()
        .chain(&cleanup.forget)
        .map(String::as_str)
        .collect();
    let mut next = manifest.clone();
    next.files.retain(|f| !dropped.contains(f.path.as_str()));
    write_manifest(out, &next)?;
    Ok(())
}

/// Replaces `manifest.json` atomically: a crash leaves the old or the new one, never half.
fn write_manifest(out: &Path, manifest: &Manifest) -> Result<(), Error> {
    let json = manifest.to_json().map_err(io::Error::from)?;
    let tmp = out.join(format!(".manifest.json.agenvil-tmp-{}", std::process::id()));
    let mut file = OpenOptions::new().write(true).create_new(true).open(&tmp)?;
    let written = file
        .write_all(json.as_bytes())
        .and_then(|()| file.sync_all());
    drop(file);
    let done = written.and_then(|()| fs::rename(&tmp, out.join(manifest::FILE)));
    if done.is_err() {
        let _ = fs::remove_file(&tmp);
    }
    Ok(done?)
}

fn join(dir: &Path, rel: &str) -> PathBuf {
    rel.split('/')
        .fold(dir.to_path_buf(), |p, part| p.join(part))
}

/// Text of the cleanup decision, deterministic: the exact files, then what was kept.
pub fn cleanup_report(out: &Path, cleanup: &Cleanup, apply: bool) -> String {
    let mut text = String::new();
    let (remove, forget) = if apply {
        ("removed", "dropped from the manifest")
    } else {
        (
            "would remove",
            "would drop from the manifest (already gone)",
        )
    };
    for p in &cleanup.remove {
        text.push_str(&format!("{remove}  {p}\n"));
    }
    for p in &cleanup.forget {
        text.push_str(&format!("{forget}  {p}\n"));
    }
    for p in &cleanup.keep {
        text.push_str(&format!(
            "kept  {p}: modified since the build, delete it yourself if you do not need it\n"
        ));
    }
    if cleanup.remove.is_empty() && cleanup.forget.is_empty() && cleanup.keep.is_empty() {
        text.push_str(&format!("nothing to clean in {}\n", out.display()));
    } else if !apply {
        text.push_str("dry run: nothing was changed; pass --apply to do this\n");
    }
    text
}

/// Validates the generated tree in `dir` on its own, independent of the sources: the layout, each
/// file's frontmatter, `skills:` references, explicit model and effort, and that the manifest
/// describes exactly what is on disk. Source-only policy (the `check` rules such as PO001) is not
/// applied: it is about how sources are written, not about what Claude Code will read.
pub fn validate_dir(dir: &Path) -> Result<Vec<Finding>, Error> {
    let tree = scan(dir)?;
    let mut findings = validate_tree(&tree);
    match load_manifest(&tree) {
        Err(flaws) => findings.extend(flaws),
        Ok(manifest) => {
            for m in &manifest.files {
                match tree.nodes.get(&m.path) {
                    None => findings.push(finding(
                        Kind::Invalid,
                        &m.path,
                        "listed in the manifest but not written",
                    )),
                    Some(node) if !digest_matches(node, &m.sha256, m.bytes) => {
                        findings.push(finding(
                            Kind::Invalid,
                            &m.path,
                            "size or sha256 does not match the manifest",
                        ))
                    }
                    Some(_) => {}
                }
            }
            let listed: std::collections::BTreeSet<&str> =
                manifest.files.iter().map(|f| f.path.as_str()).collect();
            for path in tree.nodes.keys() {
                if path != manifest::FILE && !listed.contains(path.as_str()) {
                    findings.push(finding(
                        Kind::Invalid,
                        path,
                        "written but not listed in the manifest",
                    ));
                }
            }
        }
    }
    Ok(sorted(findings))
}

/// Content checks of the files under `.claude/`; all findings are [`Kind::Invalid`].
fn validate_tree(tree: &Tree) -> Vec<Finding> {
    let mut findings = Vec::new();
    let mut skills: BTreeMap<&str, bool> = BTreeMap::new();
    let mut agent_skills: Vec<(&str, Vec<String>)> = Vec::new();
    for (path, node) in &tree.nodes {
        let parts: Vec<&str> = path.split('/').collect();
        if parts.first() != Some(&".claude") || parts.len() < 3 {
            continue;
        }
        let mut bad = |why: String| findings.push(finding(Kind::Invalid, path, why));
        match (parts[1], &parts[2..]) {
            ("agents", [file]) if file.ends_with(".md") && file.len() > 3 => {
                if let Some(refs) = document(node, "agent", &mut bad, false) {
                    agent_skills.push((path, refs));
                }
            }
            ("agents", _) => {
                bad("an agent is a single `.md` file directly in .claude/agents".to_owned())
            }
            ("rules", rest) if rest.last().is_some_and(|f| f.ends_with(".md")) => {
                if let Node::File(bytes) = node
                    && std::str::from_utf8(bytes).is_err()
                {
                    bad("a rule must be UTF-8 text".to_owned());
                }
            }
            ("rules", _) => bad("a rule is a `.md` file in .claude/rules".to_owned()),
            ("skills", [name]) => bad(format!(
                "`{name}` is a file where a skill directory is expected"
            )),
            ("skills", [name, rest @ ..]) => {
                let marker = skills.entry(name).or_insert(false);
                if rest == ["SKILL.md"] {
                    *marker = true;
                    skill_name(name, &mut bad);
                    document(node, "skill", &mut bad, true);
                    skill_frontmatter_name(node, name, &mut bad);
                }
            }
            _ => {}
        }
    }
    for (name, has_skill_md) in &skills {
        if !has_skill_md {
            findings.push(finding(
                Kind::Invalid,
                &format!(".claude/skills/{name}"),
                "a skill directory needs a SKILL.md",
            ));
        }
    }
    for (path, refs) in agent_skills {
        let mut seen = std::collections::BTreeSet::new();
        for skill in refs {
            if !seen.insert(skill.clone()) {
                findings.push(finding(
                    Kind::Invalid,
                    path,
                    format!("preloads skill `{skill}` twice"),
                ));
            } else if skills.get(skill.as_str()) != Some(&true) {
                findings.push(finding(
                    Kind::Invalid,
                    path,
                    format!("preloads skill `{skill}`, which is not in the tree"),
                ));
            }
        }
    }
    findings
}

/// The Agent Skills name rule: lowercase letters, digits and single hyphens, at most 64.
fn skill_name(name: &str, bad: &mut impl FnMut(String)) {
    let valid = !name.is_empty()
        && name.len() <= 64
        && name.split('-').all(|p| {
            !p.is_empty()
                && p.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit())
        });
    if !valid {
        bad(format!(
            "`{name}` is not a valid skill name (lowercase letters, digits and single hyphens, at most 64)"
        ));
    }
}

fn skill_frontmatter_name(node: &Node, dir: &str, bad: &mut impl FnMut(String)) {
    let Node::File(bytes) = node else { return };
    let Ok(text) = std::str::from_utf8(bytes) else {
        return;
    };
    let Ok(parsed) = frontmatter::parse(text) else {
        return;
    };
    match parsed.frontmatter.as_ref().and_then(|f| f.get("name")) {
        None | Some(Value::Null) => {}
        Some(Value::String(n)) if n == dir => {}
        Some(other) => bad(format!(
            "frontmatter `name` ({other}) must equal the directory name `{dir}`"
        )),
    }
}

/// Checks an agent or skill document and returns the skills an agent preloads. `skill` documents
/// need a description of at most 1024 characters; agents need a name and a description.
fn document(
    node: &Node,
    what: &str,
    bad: &mut impl FnMut(String),
    skill: bool,
) -> Option<Vec<String>> {
    let bytes = match node {
        Node::File(bytes) => bytes,
        Node::Oversize(n) => {
            bad(format!("{n} bytes is larger than the file limit"));
            return None;
        }
        Node::Unsafe(_) => return None,
    };
    let Ok(text) = std::str::from_utf8(bytes) else {
        bad(format!("a {what} must be UTF-8 text"));
        return None;
    };
    let parsed = match frontmatter::parse(text) {
        Ok(p) => p,
        Err(e) => {
            bad(format!("frontmatter: {e}"));
            return None;
        }
    };
    let Some(front) = parsed.frontmatter else {
        bad(format!("a {what} needs frontmatter with a description"));
        return None;
    };
    let non_empty = |key: &str| {
        front
            .get(key)
            .and_then(Value::as_str)
            .is_some_and(|s| !s.trim().is_empty())
    };
    if !skill && !non_empty("name") {
        bad("`name` must be a non-empty string".to_owned());
    }
    if !non_empty("description") {
        bad("`description` must be a non-empty string".to_owned());
    } else if skill
        && front
            .get("description")
            .and_then(Value::as_str)
            .is_some_and(|d| d.chars().count() > 1024)
    {
        bad("`description` is longer than 1024 characters".to_owned());
    }
    resolved_pair(&front, bad);
    let mut refs = Vec::new();
    match front.get("skills") {
        None | Some(Value::Null) => {}
        Some(Value::Array(items)) if skill => {
            let _ = items;
            bad("`skills` is only meaningful on agents".to_owned());
        }
        Some(Value::Array(items)) => {
            if items.len() > MAX_PRELOADED_SKILLS {
                bad(format!(
                    "preloads {} skills; Claude Code reads at most {MAX_PRELOADED_SKILLS}",
                    items.len()
                ));
            }
            for item in items {
                match item.as_str() {
                    Some(s) => refs.push(s.to_owned()),
                    None => bad(format!("`skills` entry {item} is not a string")),
                }
            }
        }
        Some(_) if skill => {}
        Some(other) => bad(format!(
            "`skills` must be a list of skill names, not {other}"
        )),
    }
    Some(refs)
}

/// `model` and `effort` in the output are literal Claude values: nothing left to resolve.
fn resolved_pair(front: &frontmatter::Frontmatter, bad: &mut impl FnMut(String)) {
    let get = |key: &str, bad: &mut dyn FnMut(String)| match front.get(key) {
        None | Some(Value::Null) => None,
        Some(Value::String(s)) => Some(s.clone()),
        Some(other) => {
            bad(format!("`{key}` must be a string, not {other}"));
            None
        }
    };
    let model = get("model", bad).filter(|m| m != "inherit");
    let effort = get("effort", bad);
    if let Some(m) = &model
        && let Err(why) = target::native_model(m)
    {
        bad(why);
    }
    if let Some(e) = &effort
        && let Err(why) = target::native_effort(e)
    {
        bad(why);
    }
    if let (Some(m), Some(e)) = (model, effort)
        && let Err(why) = target::claude_supports(&[m, e])
    {
        bad(why);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::manifest::Transform;

    fn entry(path: &str, body: &str) -> Entry {
        Entry {
            path: path.to_owned(),
            source: "agents/a.md".to_owned(),
            bytes: body.as_bytes().to_vec(),
            transform: Transform::Copy,
            exec: false,
        }
    }

    fn write(dir: &Path, entries: &[Entry]) {
        for e in entries {
            let p = join(dir, &e.path);
            fs::create_dir_all(p.parent().unwrap()).unwrap();
            fs::write(p, &e.bytes).unwrap();
        }
        fs::write(
            dir.join(manifest::FILE),
            build::manifest_of(entries).to_json().unwrap(),
        )
        .unwrap();
    }

    fn kinds(findings: &[Finding]) -> Vec<(Kind, &str)> {
        findings.iter().map(|f| (f.kind, f.path.as_str())).collect()
    }

    const AGENT: &str = "---\nname: a\ndescription: d\n---\nbody\n";

    #[test]
    fn matching_tree_has_no_findings_and_kinds_sort_in_report_order() {
        let dir = tempfile::tempdir().unwrap();
        let plan = [entry(".claude/agents/a.md", AGENT)];
        write(dir.path(), &plan);
        let tree = scan(dir.path()).unwrap();
        assert!(compare(&tree, &plan).is_empty());
        assert!(Kind::Missing < Kind::Changed && Kind::Unsafe < Kind::Invalid);
    }

    #[test]
    fn compare_classifies_each_drift() {
        let dir = tempfile::tempdir().unwrap();
        let old = [
            entry(".claude/agents/a.md", AGENT),
            entry(".claude/agents/gone.md", AGENT),
            entry(".claude/agents/moved.md", AGENT),
        ];
        write(dir.path(), &old);
        fs::write(dir.path().join(".claude/agents/moved.md"), "edited").unwrap();
        fs::write(dir.path().join(".claude/agents/mine.md"), "mine").unwrap();
        let plan = [
            entry(".claude/agents/a.md", AGENT),
            entry(".claude/agents/new.md", AGENT),
        ];
        let found = compare(&scan(dir.path()).unwrap(), &plan);
        let found = sorted(found);
        assert_eq!(
            kinds(&found),
            [
                (Kind::Missing, ".claude/agents/new.md"),
                (Kind::Stale, ".claude/agents/gone.md"),
                (Kind::Stale, ".claude/agents/moved.md"),
                (Kind::Extra, ".claude/agents/mine.md"),
            ]
        );
        assert!(found[2].detail.contains("modified"));
    }

    #[test]
    fn an_untrusted_manifest_makes_everything_extra() {
        let dir = tempfile::tempdir().unwrap();
        let plan = [entry(".claude/agents/a.md", AGENT)];
        write(dir.path(), &plan);
        fs::write(dir.path().join(".claude/agents/x.md"), "x").unwrap();
        fs::write(dir.path().join(manifest::FILE), "{ nope").unwrap();
        let found = sorted(compare(&scan(dir.path()).unwrap(), &plan));
        assert_eq!(
            kinds(&found),
            [
                (Kind::Extra, ".claude/agents/x.md"),
                (Kind::ManifestInvalid, "manifest.json")
            ]
        );
    }

    #[cfg(unix)]
    #[test]
    fn symlinks_are_unsafe_and_never_followed() {
        let dir = tempfile::tempdir().unwrap();
        let secret = tempfile::tempdir().unwrap();
        fs::write(secret.path().join("s"), "secret").unwrap();
        let plan = [entry(".claude/agents/a.md", AGENT)];
        write(dir.path(), &plan);
        std::os::unix::fs::symlink(secret.path(), dir.path().join(".claude/skills")).unwrap();
        let found = compare(&scan(dir.path()).unwrap(), &plan);
        assert!(is_unsafe(&found), "{found:?}");
        assert_eq!(fs::read(secret.path().join("s")).unwrap(), b"secret");
    }

    #[test]
    fn foreign_tolerates_missing_files_but_not_edits_or_strangers() {
        let dir = tempfile::tempdir().unwrap();
        let plan = [
            entry(".claude/agents/a.md", AGENT),
            entry(".claude/agents/b.md", AGENT),
        ];
        write(dir.path(), &plan);
        assert_eq!(foreign(dir.path()).unwrap(), None);
        fs::remove_file(dir.path().join(".claude/agents/b.md")).unwrap();
        assert_eq!(foreign(dir.path()).unwrap(), None);
        fs::write(dir.path().join(".claude/agents/a.md"), "edited").unwrap();
        assert!(
            foreign(dir.path())
                .unwrap()
                .unwrap()
                .contains("changed since")
        );
        fs::write(dir.path().join(".claude/agents/a.md"), AGENT).unwrap();
        fs::write(dir.path().join("notes.txt"), "x").unwrap();
        assert!(
            foreign(dir.path())
                .unwrap()
                .unwrap()
                .contains("not written by Agenvil")
        );
    }

    #[test]
    fn validate_dir_accepts_valid_output_and_flags_broken_output() {
        let dir = tempfile::tempdir().unwrap();
        let skill = "---\nname: s\ndescription: d\n---\n";
        let good = [
            entry(
                ".claude/agents/a.md",
                "---\nname: a\ndescription: d\nskills: [s]\nmodel: opus\neffort: high\n---\n",
            ),
            entry(".claude/skills/s/SKILL.md", skill),
            entry(".claude/skills/s/references/r.md", "ref"),
            entry(".claude/rules/r.md", "rule"),
        ];
        write(dir.path(), &good);
        assert_eq!(validate_dir(dir.path()).unwrap(), []);

        let bad = tempfile::tempdir().unwrap();
        let broken = [
            entry(
                ".claude/agents/a.md",
                "---\nname: a\ndescription: d\nskills: [nope]\nmodel: fast\n---\n",
            ),
            entry(".claude/agents/b.md", "no frontmatter"),
            entry(
                ".claude/skills/Bad_Name/SKILL.md",
                "---\ndescription: d\n---\n",
            ),
            entry(".claude/skills/orphan/notes.md", "x"),
        ];
        write(bad.path(), &broken);
        let found = validate_dir(bad.path()).unwrap();
        let paths: Vec<_> = found.iter().map(|f| f.path.as_str()).collect();
        assert!(paths.contains(&".claude/agents/b.md"), "{found:?}");
        assert!(
            paths.contains(&".claude/skills/Bad_Name/SKILL.md"),
            "{found:?}"
        );
        assert!(paths.contains(&".claude/skills/orphan"), "{found:?}");
        assert!(
            found.iter().any(|f| f.detail.contains("not in the tree")),
            "{found:?}"
        );
        assert!(
            found
                .iter()
                .any(|f| f.detail.contains("not a Claude model")),
            "{found:?}"
        );
    }
}
