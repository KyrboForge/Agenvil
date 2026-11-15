//! Typed, strict loading of `agenvil.toml`.

use std::path::{Component, Path, PathBuf};

use serde::Deserialize;

/// Config file name, looked up directly under the project root.
pub const FILE_NAME: &str = "agenvil.toml";
/// The only supported `schema` value.
pub const SCHEMA_VERSION: i64 = 1;

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("cannot read {}: {source}", .path.display())]
    Read {
        path: PathBuf,
        source: std::io::Error,
    },
    #[error("invalid {}: {source}", .path.display())]
    Parse {
        path: PathBuf,
        source: toml::de::Error,
    },
    #[error("invalid {}: missing required key `schema` (expected `schema = {SCHEMA_VERSION}`)", .path.display())]
    MissingSchema { path: PathBuf },
    #[error("invalid {}: unsupported schema version {found} (this agenvil supports {SCHEMA_VERSION})", .path.display())]
    UnsupportedSchema { path: PathBuf, found: i64 },
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
}

impl Default for Paths {
    fn default() -> Self {
        let rel = |p: &str| RelPath(PathBuf::from(p));
        Self {
            agents: rel("agents"),
            skills: rel("skills"),
            rules: rel("rules"),
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
}

impl Default for ClaudeTarget {
    fn default() -> Self {
        Self { enabled: true }
    }
}

/// Accepted but empty: allowed (model, effort) pairs are not defined yet.
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

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ClaudeModels {}

/// A non-empty path relative to the project root that cannot escape it.
///
/// shortcut: checked lexically, symlinks are not resolved; resolve them when discovery reads the tree.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(try_from = "String")]
pub struct RelPath(PathBuf);

impl RelPath {
    pub fn as_path(&self) -> &Path {
        &self.0
    }
}

impl TryFrom<String> for RelPath {
    type Error = String;

    fn try_from(value: String) -> Result<Self, String> {
        let path = PathBuf::from(&value);
        let relative = path
            .components()
            .all(|c| matches!(c, Component::Normal(_) | Component::CurDir));
        if !relative || !path.components().any(|c| matches!(c, Component::Normal(_))) {
            return Err(format!(
                "path `{value}` must be non-empty, relative to the project root and stay inside it"
            ));
        }
        Ok(Self(path))
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
        let path = root.join(FILE_NAME);
        let text = std::fs::read_to_string(&path).map_err(|source| ConfigError::Read {
            path: path.clone(),
            source,
        })?;
        Self::parse(&text, &path)
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
            Some(SCHEMA_VERSION) => toml::from_str(text).map_err(parse_err),
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
    fn minimal_config_gets_documented_defaults() {
        let config = parse(MINIMAL).unwrap();
        assert_eq!(config.project.name, "demo");
        assert_eq!(config.paths.agents.as_path(), Path::new("agents"));
        assert_eq!(config.paths.skills.as_path(), Path::new("skills"));
        assert_eq!(config.paths.rules.as_path(), Path::new("rules"));
        assert!(config.targets.claude.enabled);
    }

    #[test]
    fn documented_example_parses() {
        let config = parse(include_str!("../examples/agenvil.toml")).unwrap();
        assert_eq!(config.project.name, "team-agents-kit");
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
    fn paths_cannot_be_empty_absolute_or_escape_the_root() {
        for bad in ["", ".", "/etc", "../x", "a/../../b", "a/.."] {
            let message = err(&format!(
                "schema = 1\n[project]\nname = \"x\"\n[paths]\nagents = \"{bad}\"\n"
            ));
            assert!(message.contains("stay inside"), "{bad:?} -> {message}");
        }
        assert!(
            parse("schema = 1\n[project]\nname = \"x\"\n[paths]\nagents = \"./a/b\"\n").is_ok()
        );
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
}
