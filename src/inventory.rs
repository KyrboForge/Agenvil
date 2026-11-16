//! The inventory: every discovered resource with its parsed frontmatter, plus the problems met on
//! the way. This is the input the validator and the Claude builder work from.
//!
//! A file whose frontmatter does not parse is reported only as a [`Problem`]; it does not appear
//! among the resources, so consumers never see half-parsed data.

use serde::Serialize;
use std::path::Path;

use crate::config::Config;
use crate::diagnostics::{Problem, ProblemKind};
use crate::discovery::{self, Kind, Limits, OverlapError};
use crate::frontmatter::{self, Frontmatter};

/// Version of the JSON shape below. Bumped on any breaking change to it.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, Serialize)]
pub struct Inventory {
    pub schema: u32,
    pub agents: Vec<Resource>,
    pub skills: Vec<Resource>,
    pub rules: Vec<Resource>,
    pub problems: Vec<Problem>,
}

#[derive(Debug, Serialize)]
pub struct Resource {
    /// File stem for agents and rules, directory name for skills. The frontmatter `name`, if any,
    /// is just another metadata field.
    pub name: String,
    /// Relative to the project root, `/`-separated.
    pub path: String,
    /// Size of the file in bytes.
    pub bytes: usize,
    pub frontmatter: Option<Frontmatter>,
    /// 1-based line where the Markdown body starts.
    pub body_start_line: usize,
    /// The file exactly as read (BOM and CRLF kept), for consumers that need the body text.
    /// Not part of the JSON output.
    #[serde(skip)]
    pub source: String,
}

impl Inventory {
    /// Reads the tree under `root` (read-only). Per-file failures become [`Problem`]s; only
    /// ambiguous configuration is an error.
    pub fn build(root: &Path, config: &Config, limits: &Limits) -> Result<Self, OverlapError> {
        let discovered = discovery::discover(root, &config.paths, limits)?;
        let mut inventory = Self {
            schema: SCHEMA_VERSION,
            agents: Vec::new(),
            skills: Vec::new(),
            rules: Vec::new(),
            problems: discovered.problems,
        };
        for file in discovered.files {
            match frontmatter::parse(&file.source) {
                Ok(parsed) => {
                    let resource = Resource {
                        name: file.name,
                        path: file.path,
                        bytes: file.source.len(),
                        frontmatter: parsed.frontmatter,
                        body_start_line: parsed.body_start_line,
                        source: file.source,
                    };
                    match file.kind {
                        Kind::Agent => inventory.agents.push(resource),
                        Kind::Skill => inventory.skills.push(resource),
                        Kind::Rule => inventory.rules.push(resource),
                    }
                }
                Err(error) => inventory.problems.push(Problem {
                    path: file.path,
                    line: Some(error.line),
                    kind: ProblemKind::Frontmatter,
                    message: error.message,
                }),
            }
        }
        inventory.problems.sort();
        Ok(inventory)
    }

    /// Pretty-printed JSON with a trailing newline. Deterministic: resources are sorted by path
    /// and metadata keys are sorted.
    pub fn to_json(&self) -> serde_json::Result<String> {
        let mut json = serde_json::to_string_pretty(self)?;
        json.push('\n');
        Ok(json)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::fs;

    fn build(files: &[(&str, &[u8])]) -> Inventory {
        let dir = tempfile::tempdir().unwrap();
        for (path, bytes) in files {
            let full = dir.path().join(path);
            fs::create_dir_all(full.parent().unwrap()).unwrap();
            fs::write(full, bytes).unwrap();
        }
        let config = Config::parse(
            "schema = 1\n[project]\nname = \"x\"\n",
            Path::new("agenvil.toml"),
        )
        .unwrap();
        Inventory::build(dir.path(), &config, &Limits::default()).unwrap()
    }

    #[test]
    fn parses_every_kind_and_keeps_source_and_lines() {
        let source = "\u{feff}---\r\nname: rev\r\nmodel: opus\r\n---\r\nBody\r\n";
        let inv = build(&[
            ("agents/rev.md", source.as_bytes()),
            ("skills/tdd/SKILL.md", b"no frontmatter\n"),
            ("rules/style.md", b"---\n---\n"),
        ]);
        assert!(inv.problems.is_empty(), "{:?}", inv.problems);
        let agent = &inv.agents[0];
        assert_eq!(agent.source, source);
        assert_eq!(agent.bytes, source.len());
        assert_eq!(agent.body_start_line, 5);
        let fm = agent.frontmatter.as_ref().unwrap();
        assert_eq!((fm.start_line, fm.end_line), (1, 4));
        assert_eq!(fm.get("model"), Some(&json!("opus")));
        assert_eq!(inv.skills[0].frontmatter, None);
        assert_eq!(inv.skills[0].body_start_line, 1);
        assert!(
            inv.rules[0]
                .frontmatter
                .as_ref()
                .unwrap()
                .metadata
                .is_empty()
        );
    }

    #[test]
    fn malformed_frontmatter_is_a_problem_not_a_resource() {
        let inv = build(&[
            ("agents/bad.md", b"---\nname: [oops\n---\n"),
            ("agents/good.md", b"---\nname: ok\n---\n"),
            ("rules/open.md", b"---\nnever closed\n"),
        ]);
        assert_eq!(inv.agents.len(), 1);
        assert_eq!(inv.agents[0].name, "good");
        assert!(inv.rules.is_empty());
        let problems: Vec<_> = inv
            .problems
            .iter()
            .map(|p| (p.path.as_str(), p.line, p.kind))
            .collect();
        assert_eq!(
            problems,
            [
                ("agents/bad.md", Some(2), ProblemKind::Frontmatter),
                ("rules/open.md", Some(1), ProblemKind::Frontmatter),
            ]
        );
    }

    #[test]
    fn json_is_stable_sorted_and_omits_source() {
        let files: &[(&str, &[u8])] = &[
            ("agents/b.md", b"---\nzeta: 1\nalpha: 2\n---\nSECRET-BODY\n"),
            ("agents/a.md", b"x\n"),
        ];
        let json = build(files).to_json().unwrap();
        assert_eq!(json, build(files).to_json().unwrap());
        assert!(json.ends_with("}\n"));
        assert!(!json.contains("SECRET-BODY"));
        assert!(json.find("agents/a.md").unwrap() < json.find("agents/b.md").unwrap());
        assert!(json.find("alpha").unwrap() < json.find("zeta").unwrap());
        let value: serde_json::Value = serde_json::from_str(&json).unwrap();
        assert_eq!(value["schema"], 1);
        assert_eq!(value["agents"][1]["frontmatter"]["metadata"]["alpha"], 2);
        assert_eq!(value["problems"], json!([]));
    }
}
