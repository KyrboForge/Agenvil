//! Typed, strict loading of `agenvil.toml`.

use std::collections::BTreeMap;
use std::fs::File;
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Deserialize;

use crate::target::{PairMap, claude_native, claude_supports};

/// Config file name, looked up directly under the project root.
pub const FILE_NAME: &str = "agenvil.toml";
/// The only supported `schema` value.
pub const SCHEMA_VERSION: i64 = 1;
/// Size limit for `agenvil.toml`; real files are a few hundred bytes.
pub const MAX_FILE_BYTES: u64 = 1024 * 1024;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {}: {source}", .path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid {}: must be a regular file, not a symlink, directory or special file", .path.display())]
    NotRegularFile { path: PathBuf },
    #[error("invalid {}: larger than the {MAX_FILE_BYTES}-byte limit", .path.display())]
    TooLarge { path: PathBuf },
    #[error("invalid {}: {source}", .path.display())]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("invalid {}: missing required key `schema` (expected `schema = {SCHEMA_VERSION}`)", .path.display())]
    MissingSchema { path: PathBuf },
    #[error("invalid {}: unsupported schema version {found} (this agenvil supports {SCHEMA_VERSION})", .path.display())]
    UnsupportedSchema { path: PathBuf, found: i64 },
    #[error("invalid {}:\n{}", .path.display(), .problems.join("\n"))]
    Policy {
        path: PathBuf,
        problems: Vec<String>,
    },
}

/// Unknown keys are rejected everywhere so typos never pass silently.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    pub schema: i64,
    pub project: Project,
    #[serde(default)]
    pub paths: Paths,
    #[serde(default)]
    pub targets: Targets,
    #[serde(default)]
    pub policy: Policy,
    /// Per-rule severity overrides, keyed by rule ID. The IDs are checked by the validator.
    #[serde(default)]
    pub rules: BTreeMap<String, Level>,
    #[serde(default)]
    pub ignore: Vec<Ignore>,
    #[serde(default)]
    pub hygiene: Hygiene,
    /// Opt-in legacy behavior; absent in a new project.
    #[serde(default)]
    pub compat: Compat,
}

/// `[compat]`: named bundles of legacy defaults. Nothing here is on unless the project asks.
#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Compat {
    pub profile: Option<Profile>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Profile {
    /// The Team Agents Kit conventions: every agent declares model, effort and tools; the model
    /// table in `POLICY.md`; `config/defaults.toml`; `bin/` scripts; personal-path hygiene; the
    /// five-pair model policy; SK002 on.
    Tak,
}

/// What a rule does when it is configured in `[rules]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Level {
    Error,
    Warning,
    Off,
}

/// Suppresses one rule for one file. A reason is mandatory so ignores stay reviewable.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Ignore {
    pub rule: String,
    pub path: RelPath,
    pub reason: String,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Hygiene {
    /// Regular expressions (Rust syntax, linear-time) that must not occur in the checked files.
    /// HY001 runs only when this is set.
    pub personal_patterns: Option<Vec<String>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Project {
    pub name: String,
}

/// Source locations relative to the project root.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Paths {
    pub agents: RelPath,
    pub skills: RelPath,
    pub rules: RelPath,
    /// One harness-neutral Markdown file of project-wide instructions. Nothing is built from it
    /// unless it is set; the Claude target emits it as the project-root `CLAUDE.md`.
    pub instructions: Option<RelPath>,
    /// Markdown file with the generated model table; PO001 runs only when this is set.
    pub policy: Option<RelPath>,
    /// Team defaults, validated by CF001 only when this is set.
    pub defaults: Option<RelPath>,
    /// Scripts checked by HY002 only when this is set.
    pub bin: Option<RelPath>,
}

impl Default for Paths {
    fn default() -> Self {
        let rel = |p: &str| RelPath(PathBuf::from(p));
        Self {
            agents: rel("agents"),
            skills: rel("skills"),
            rules: rel("rules"),
            instructions: None,
            policy: None,
            defaults: None,
            bin: None,
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Targets {
    pub claude: ClaudeTarget,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ClaudeTarget {
    pub enabled: bool,
    /// Explicit source-pair to target-pair overrides. A pair without an entry is used as is, or
    /// rejected when Claude does not support it; there is no implicit fallback.
    pub map: Vec<PairMap>,
}

impl Default for ClaudeTarget {
    fn default() -> Self {
        Self {
            enabled: true,
            map: Vec::new(),
        }
    }
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Policy {
    pub models: Models,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct Models {
    pub claude: ClaudeModels,
}

/// Allowlists for the native Claude `model` and `effort` values agents may declare. An unset
/// list allows every native value (an alias or a full `claude-*` ID; the levels Claude Code
/// documents); a set list is authoritative. `pairs` restricts which model and effort may be
/// combined.
#[derive(Debug, Default, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields, default)]
pub struct ClaudeModels {
    pub models: Option<Vec<String>>,
    pub efforts: Option<Vec<String>>,
    pub pairs: Option<Vec<[String; 2]>>,
}

impl ClaudeModels {
    fn tak() -> Self {
        let own = |items: &[&str]| Some(items.iter().map(|s| (*s).to_owned()).collect());
        let pair = |m: &str, e: &str| [m.to_owned(), e.to_owned()];
        Self {
            models: own(&["haiku", "sonnet", "opus"]),
            efforts: own(&["low", "medium", "high", "xhigh"]),
            pairs: Some(vec![
                pair("haiku", "low"),
                pair("sonnet", "medium"),
                pair("sonnet", "high"),
                pair("opus", "high"),
                pair("opus", "xhigh"),
            ]),
        }
    }
}

/// A non-empty path relative to the project root that cannot escape it.
///
/// Validated on the string, not with `Path::components`, so a file is accepted or rejected
/// the same way on every OS: `/` separators only; `\`, `:`, a leading `/` and `..` are rejected.
///
/// shortcut: checked lexically, symlinks are not resolved; resolve them when discovery reads the tree.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct RelPath(PathBuf);

impl RelPath {
    pub fn as_path(&self) -> &Path {
        &self.0
    }

    /// `/`-joined form, identical on every OS (validated input is always UTF-8).
    pub fn to_slash(&self) -> String {
        let parts: Vec<_> = self.0.iter().map(|s| s.to_string_lossy()).collect();
        parts.join("/")
    }
}

impl TryFrom<String> for RelPath {
    type Error = String;

    fn try_from(value: String) -> Result<Self, String> {
        let segments = || value.split('/');
        let portable = !value.starts_with('/') && !value.contains(['\\', ':']);
        if !portable
            || segments().any(|s| s == "..")
            || segments().all(|s| s.is_empty() || s == ".")
        {
            return Err(format!(
                "path `{value}` must be non-empty, use `/` separators, be relative to the project root and stay inside it"
            ));
        }
        // `./agents//` and `agents` are the same directory; discovery compares components.
        Ok(Self(
            segments().filter(|s| !s.is_empty() && *s != ".").collect(),
        ))
    }
}

/// Read before the strict parse so an unsupported (future) schema is reported as such
/// instead of as unknown keys.
#[derive(Deserialize)]
struct SchemaProbe {
    schema: Option<i64>,
}

impl Config {
    /// Loads `<root>/agenvil.toml`.
    pub fn load(root: &Path) -> Result<Self, ConfigError> {
        Self::load_file(&root.join(FILE_NAME))
    }

    /// Loads the config from an explicit file path.
    pub fn load_file(path: &Path) -> Result<Self, ConfigError> {
        let path = path.to_path_buf();
        let read_err = |source| ConfigError::Read {
            path: path.clone(),
            source,
        };
        // lstat, so a symlink pointing outside the root is refused unread; also refuses FIFOs before open.
        // shortcut: lstat-then-open races a concurrent local attacker; open with O_NOFOLLOW if that is in scope.
        let meta = std::fs::symlink_metadata(&path).map_err(read_err)?;
        if !meta.is_file() {
            return Err(ConfigError::NotRegularFile { path: path.clone() });
        }
        let mut text = String::new();
        if meta.len() <= MAX_FILE_BYTES {
            // `take` keeps the bound even if the file grows after the metadata check.
            File::open(&path)
                .and_then(|file| file.take(MAX_FILE_BYTES + 1).read_to_string(&mut text))
                .map_err(read_err)?;
        }
        if meta.len() > MAX_FILE_BYTES || text.len() as u64 > MAX_FILE_BYTES {
            return Err(ConfigError::TooLarge { path: path.clone() });
        }
        Self::parse(&text, &path)
    }

    /// Whether the legacy Team Agents Kit profile is on.
    pub fn tak(&self) -> bool {
        self.compat.profile == Some(Profile::Tak)
    }

    /// Fills what the TAK profile defaults and the project did not set itself.
    fn apply_profile(&mut self) {
        if !self.tak() {
            return;
        }
        let rel = |p: &str| Some(RelPath(PathBuf::from(p)));
        let paths = &mut self.paths;
        paths.policy = paths.policy.take().or_else(|| rel("POLICY.md"));
        paths.defaults = paths
            .defaults
            .take()
            .or_else(|| rel("config/defaults.toml"));
        paths.bin = paths.bin.take().or_else(|| rel("bin"));
        self.hygiene
            .personal_patterns
            .get_or_insert_with(|| vec!["/Users/".to_owned(), r"~/\.agents/".to_owned()]);
        // The TAK table is one unit: filling part of it would contradict what the project set.
        let claude = &mut self.policy.models.claude;
        if *claude == ClaudeModels::default() {
            *claude = ClaudeModels::tak();
        }
    }

    /// Every inconsistency in the Claude model policy and its target mappings, so one run lists
    /// them all. The values themselves may be any strings: whether the target supports them is
    /// decided at resolution time, except for mapping targets, which must be supported here.
    fn model_policy_problems(&self) -> Vec<String> {
        let (policy, map) = (&self.policy.models.claude, &self.targets.claude.map);
        let mut problems = Vec::new();
        let mut list = |key: &str, items: Vec<&String>| {
            let mut seen = Vec::new();
            for item in items {
                if item.trim().is_empty() {
                    problems.push(format!("{key}: values must not be empty"));
                } else if seen.contains(&item) {
                    problems.push(format!("{key}: `{item}` is listed twice"));
                }
                seen.push(item);
            }
        };
        if let Some(models) = &policy.models {
            list("[policy.models.claude].models", models.iter().collect());
        }
        if let Some(efforts) = &policy.efforts {
            list("[policy.models.claude].efforts", efforts.iter().collect());
        }
        let mut pairs: Vec<&[String; 2]> = Vec::new();
        for pair in policy.pairs.iter().flatten() {
            let [model, effort] = pair;
            if policy.models.as_ref().is_some_and(|m| !m.contains(model)) {
                problems.push(format!(
                    "[policy.models.claude].pairs: {model}/{effort} uses a model missing from `models`"
                ));
            }
            if policy.efforts.as_ref().is_some_and(|e| !e.contains(effort)) {
                problems.push(format!(
                    "[policy.models.claude].pairs: {model}/{effort} uses an effort missing from `efforts`"
                ));
            }
            if pairs.contains(&pair) {
                problems.push(format!(
                    "[policy.models.claude].pairs: {model}/{effort} is listed twice"
                ));
            }
            pairs.push(pair);
        }
        let mut sources: Vec<&[String; 2]> = Vec::new();
        for entry in map {
            let [model, effort] = &entry.source;
            if policy.pairs.is_some() && !pairs.contains(&&entry.source) {
                problems.push(format!(
                    "[[targets.claude.map]]: source {model}/{effort} is not in [policy.models.claude].pairs"
                ));
            }
            if sources.contains(&&entry.source) {
                problems.push(format!(
                    "[[targets.claude.map]]: source {model}/{effort} is mapped twice"
                ));
            }
            sources.push(&entry.source);
            if let Err(reason) = claude_native(&entry.source) {
                problems.push(format!(
                    "[[targets.claude.map]]: source {model}/{effort} is not native Claude frontmatter: {reason}"
                ));
            }
            if let Err(reason) = claude_supports(&entry.target) {
                problems.push(format!(
                    "[[targets.claude.map]]: target {}/{} for source {model}/{effort} is not supported: {reason}",
                    entry.target[0], entry.target[1]
                ));
            }
        }
        problems
    }

    /// Parses `text`; `path` is only used in error messages.
    pub fn parse(text: &str, path: &Path) -> Result<Self, ConfigError> {
        let parse_err = |source| ConfigError::Parse {
            path: path.to_owned(),
            source,
        };
        let probe: SchemaProbe = toml::from_str(text).map_err(parse_err)?;
        match probe.schema {
            None => Err(ConfigError::MissingSchema {
                path: path.to_owned(),
            }),
            Some(SCHEMA_VERSION) => {
                let mut config: Self = toml::from_str(text).map_err(parse_err)?;
                config.apply_profile();
                let problems = config.model_policy_problems();
                if problems.is_empty() {
                    Ok(config)
                } else {
                    Err(ConfigError::Policy {
                        path: path.to_owned(),
                        problems,
                    })
                }
            }
            Some(found) => Err(ConfigError::UnsupportedSchema {
                path: path.to_owned(),
                found,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const MINIMAL: &str = "schema = 1\n[project]\nname = \"demo\"\n";

    fn parse(text: &str) -> Result<Config, ConfigError> {
        Config::parse(text, Path::new("agenvil.toml"))
    }

    fn err(text: &str) -> String {
        parse(text).unwrap_err().to_string()
    }

    #[test]
    fn project_instructions_are_unset_by_default_and_accept_a_relative_source() {
        assert!(parse(MINIMAL).unwrap().paths.instructions.is_none());
        let config = parse(&format!(
            "{MINIMAL}[paths]\ninstructions = \"project/instructions.md\"\n"
        ))
        .unwrap();
        assert_eq!(
            config.paths.instructions.unwrap().as_path(),
            Path::new("project/instructions.md")
        );
    }

    #[test]
    fn instructions_paths_cannot_contain_parent_traversal() {
        for bad in ["../instructions.md", "project/../instructions.md"] {
            let message = err(&format!("{MINIMAL}[paths]\ninstructions = {bad:?}\n"));
            assert!(message.contains("stay inside"), "{bad}: {message}");
        }
    }

    #[test]
    fn instructions_paths_cannot_be_absolute_on_any_platform() {
        for bad in [
            "/instructions.md",
            "C:/instructions.md",
            "//server/share/instructions.md",
        ] {
            let message = err(&format!("{MINIMAL}[paths]\ninstructions = {bad:?}\n"));
            assert!(
                message.contains("relative to the project root"),
                "{bad}: {message}"
            );
        }
    }

    #[test]
    fn minimal_config_gets_documented_defaults() {
        let config = parse(MINIMAL).unwrap();
        assert_eq!(config.project.name, "demo");
        assert_eq!(config.paths.agents.as_path(), Path::new("agents"));
        assert_eq!(config.paths.skills.as_path(), Path::new("skills"));
        assert_eq!(config.paths.rules.as_path(), Path::new("rules"));
        assert!(config.targets.claude.enabled);
    }

    #[test]
    fn minimal_config_has_no_tak_assumptions() {
        let config = parse(MINIMAL).unwrap();
        assert!(!config.tak());
        assert!(config.paths.policy.is_none() && config.paths.defaults.is_none());
        assert!(config.paths.bin.is_none() && config.hygiene.personal_patterns.is_none());
        let claude = &config.policy.models.claude;
        assert!(claude.models.is_none() && claude.efforts.is_none() && claude.pairs.is_none());
    }

    #[test]
    fn tak_profile_fills_only_what_the_project_left_unset() {
        let config = parse(&format!(
            "{MINIMAL}[compat]\nprofile = \"tak\"\n[paths]\nbin = \"scripts\"\n[policy.models.claude]\nmodels = [\"opus\"]\nefforts = [\"high\"]\npairs = [[\"opus\", \"high\"]]\n"
        ))
        .unwrap();
        assert!(config.tak());
        assert_eq!(config.paths.policy.unwrap().to_slash(), "POLICY.md");
        assert_eq!(
            config.paths.defaults.unwrap().to_slash(),
            "config/defaults.toml"
        );
        assert_eq!(config.paths.bin.unwrap().to_slash(), "scripts");
        assert_eq!(config.hygiene.personal_patterns.unwrap().len(), 2);
        let claude = config.policy.models.claude;
        assert_eq!(claude.models.unwrap(), ["opus"]);
        assert_eq!(claude.pairs.unwrap().len(), 1);

        let config = parse(&format!("{MINIMAL}[compat]\nprofile = \"tak\"\n")).unwrap();
        assert_eq!(config.policy.models.claude.pairs.unwrap().len(), 5);
        assert!(
            err(&format!("{MINIMAL}[compat]\nprofile = \"other\"\n")).contains("unknown variant")
        );
    }

    #[test]
    fn invalid_model_policy_lists_every_problem() {
        let message = err(&format!(
            "{MINIMAL}[policy.models.claude]\nmodels = [\"opus\", \"opus\", \"\"]\nefforts = [\"high\"]\npairs = [[\"opus\", \"low\"], [\"sonnet\", \"high\"], [\"opus\", \"high\"], [\"opus\", \"high\"]]\n"
        ));
        for needle in [
            "models: `opus` is listed twice",
            "models: values must not be empty",
            "opus/low uses an effort missing from `efforts`",
            "sonnet/high uses a model missing from `models`",
            "opus/high is listed twice",
        ] {
            assert!(message.contains(needle), "{needle}\n{message}");
        }
    }

    #[test]
    fn invalid_target_mappings_are_rejected() {
        let message = err(&format!(
            "{MINIMAL}[policy.models.claude]\npairs = [[\"opus\", \"high\"]]\n[[targets.claude.map]]\nsource = [\"opus\", \"max\"]\ntarget = [\"opus\", \"high\"]\n[[targets.claude.map]]\nsource = [\"opus\", \"high\"]\ntarget = [\"opus\", \"ultra\"]\n[[targets.claude.map]]\nsource = [\"opus\", \"high\"]\ntarget = [\"opus\", \"high\"]\n"
        ));
        for needle in [
            "source opus/max is not in [policy.models.claude].pairs",
            "source opus/high is mapped twice",
            "`ultra` is not a Claude effort level",
        ] {
            assert!(message.contains(needle), "{needle}\n{message}");
        }
        // Sources must be native; targets must be verifiably supported, not merely well formed.
        let message = err(&format!(
            "{MINIMAL}[policy.models.claude]\nmodels = [\"gpt-x\", \"opus\"]\nefforts = [\"high\", \"xhigh\"]\npairs = [[\"gpt-x\", \"high\"], [\"opus\", \"xhigh\"], [\"opus\", \"high\"]]\n[[targets.claude.map]]\nsource = [\"gpt-x\", \"high\"]\ntarget = [\"opus\", \"high\"]\n[[targets.claude.map]]\nsource = [\"opus\", \"xhigh\"]\ntarget = [\"claude-opus-4-6\", \"xhigh\"]\n[[targets.claude.map]]\nsource = [\"opus\", \"high\"]\ntarget = [\"claude-not-a-real-model\", \"high\"]\n"
        ));
        for needle in [
            "source gpt-x/high is not native Claude frontmatter",
            "`claude-opus-4-6` does not support effort `xhigh`",
            "`claude-not-a-real-model` is not a model ID with documented effort support",
        ] {
            assert!(message.contains(needle), "{needle}\n{message}");
        }
        // Mapping keys are strict like the rest of the file.
        assert!(parse(&format!("{MINIMAL}[[targets.claude.map]]\nsource = [\"a\", \"b\"]\ntarget = [\"a\", \"b\"]\nx = 1\n")).is_err());
    }

    #[test]
    fn documented_example_parses() {
        let config = parse(include_str!("../examples/agenvil.toml")).unwrap();
        assert!(!config.tak());
        assert!(config.paths.policy.is_none());

        let config = parse(include_str!("../examples/tak/agenvil.toml")).unwrap();
        assert_eq!(config.project.name, "team-agents-kit");
        assert!(config.tak());
    }

    #[test]
    fn overrides_are_applied() {
        let config = parse(
            "schema = 1\n[project]\nname = \"x\"\n[paths]\nagents = \"src/agents\"\n[targets.claude]\nenabled = false\n",
        )
        .unwrap();
        assert_eq!(config.paths.agents.as_path(), Path::new("src/agents"));
        assert_eq!(config.paths.skills.as_path(), Path::new("skills"));
        assert!(!config.targets.claude.enabled);
    }

    #[test]
    fn unknown_keys_are_rejected_at_every_level() {
        for extra in [
            "bogus = 1\n[project]\nname = \"x\"",
            "[project]\nname = \"x\"\nbogus = 1",
            "[project]\nname = \"x\"\n[paths]\nbogus = \"a\"",
            "[project]\nname = \"x\"\n[targets.claude]\nbogus = true",
            "[project]\nname = \"x\"\n[targets.codex]\nenabled = true",
            "[project]\nname = \"x\"\n[policy.models.claude]\nbogus = 1",
        ] {
            let message = err(&format!("schema = 1\n{extra}\n"));
            assert!(message.contains("unknown field"), "{extra:?} -> {message}");
            assert!(message.contains("agenvil.toml"), "{message}");
        }
    }

    #[test]
    fn schema_is_required_and_must_be_supported() {
        assert!(matches!(
            parse("[project]\nname = \"x\"\n"),
            Err(ConfigError::MissingSchema { .. })
        ));
        for found in [0, 2] {
            // Unknown keys of a newer schema must not mask the version error.
            let text = format!("schema = {found}\nnew_key = true\n[project]\nname = \"x\"\n");
            assert!(
                matches!(parse(&text), Err(ConfigError::UnsupportedSchema { found: f, .. }) if f == found)
            );
        }
        assert!(err("schema = \"1\"\n").contains("invalid type"));
    }

    #[test]
    fn project_name_is_required() {
        assert!(err("schema = 1\n").contains("missing field `project`"));
        assert!(err("schema = 1\n[project]\n").contains("missing field `name`"));
    }

    #[test]
    fn paths_drop_dot_and_empty_segments() {
        let cfg = parse(&format!("{MINIMAL}[paths]\nagents = \"./a//b/./\"\n")).unwrap();
        assert_eq!(cfg.paths.agents.as_path(), Path::new("a/b"));
    }

    #[test]
    fn paths_cannot_be_empty_absolute_or_escape_the_root() {
        // Same verdict on every OS: Windows drive and UNC forms are rejected on Unix too.
        for bad in [
            "",
            ".",
            "./",
            "/etc",
            "../x",
            "a/../../b",
            "a/..",
            r"a\b",
            r"C:\Users\x",
            "C:foo",
            "C:/x",
            r"\\server\share",
            "//server/share",
        ] {
            // TOML literal strings, so backslashes reach the validator unescaped.
            let message = err(&format!(
                "schema = 1\n[project]\nname = \"x\"\n[paths]\nagents = '{bad}'\n"
            ));
            assert!(message.contains("stay inside"), "{bad:?} -> {message}");
        }
        for good in ["./a/b", "a//b", ".agents", "a b/c"] {
            let text = format!("schema = 1\n[project]\nname = \"x\"\n[paths]\nagents = '{good}'\n");
            assert!(parse(&text).is_ok(), "{good:?}");
        }
    }

    #[test]
    fn load_reads_from_root_and_reports_missing_file() {
        let dir = tempfile::tempdir().unwrap();
        assert!(matches!(
            Config::load(dir.path()),
            Err(ConfigError::Read { .. })
        ));
        std::fs::write(dir.path().join(FILE_NAME), MINIMAL).unwrap();
        assert_eq!(Config::load(dir.path()).unwrap().project.name, "demo");
    }

    #[test]
    fn load_rejects_non_regular_files() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join(FILE_NAME)).unwrap();
        assert!(matches!(
            Config::load(dir.path()),
            Err(ConfigError::NotRegularFile { .. })
        ));
    }

    #[cfg(unix)]
    #[test]
    fn load_refuses_symlinked_config_without_reading_its_target() {
        let outside = tempfile::tempdir().unwrap();
        let target = outside.path().join("private.toml");
        std::fs::write(&target, "TOP-SECRET = = broken").unwrap();
        let root = tempfile::tempdir().unwrap();
        std::os::unix::fs::symlink(&target, root.path().join(FILE_NAME)).unwrap();

        let error = Config::load(root.path()).unwrap_err();
        assert!(
            matches!(error, ConfigError::NotRegularFile { .. }),
            "{error}"
        );
        assert!(!error.to_string().contains("TOP-SECRET"));
    }

    #[test]
    fn load_enforces_the_size_limit() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join(FILE_NAME);
        let padded =
            |len: u64| format!("{MINIMAL}#{}", "x".repeat(len as usize - MINIMAL.len() - 1));

        std::fs::write(&path, padded(MAX_FILE_BYTES)).unwrap();
        assert!(Config::load(dir.path()).is_ok());
        std::fs::write(&path, padded(MAX_FILE_BYTES + 1)).unwrap();
        assert!(matches!(
            Config::load(dir.path()),
            Err(ConfigError::TooLarge { .. })
        ));
    }
}
