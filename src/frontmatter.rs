//! Splits Markdown into YAML frontmatter and body. Pure text in, no I/O.
//!
//! The frontmatter is kept as a generic map: Claude-native fields (`model`, `effort`, `skills`, ...)
//! and unknown keys alike are preserved verbatim for later stages instead of being forced into a
//! schema here. Whether a field is allowed or required is policy and belongs to the validator.

use serde_json::{Map, Value};
use serde_saphyr::{Options, SnippetMode, render_options};

#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct Frontmatter {
    /// 1-based line of the opening `---`.
    pub start_line: usize,
    /// 1-based line of the closing `---`.
    pub end_line: usize,
    /// Every top-level key. Sorted by key (not source order) so output is deterministic.
    pub metadata: Map<String, Value>,
}

impl Frontmatter {
    pub fn get(&self, key: &str) -> Option<&Value> {
        self.metadata.get(key)
    }
}

#[derive(Debug, PartialEq)]
pub struct Parsed {
    /// `None` when the file does not start with a `---` line; that is valid Markdown.
    pub frontmatter: Option<Frontmatter>,
    /// 1-based line of the first body line (it may lie past the end of a file that has no body).
    pub body_start_line: usize,
}

/// A malformed frontmatter block. This is a parse error, never a policy finding.
#[derive(Debug, PartialEq, thiserror::Error)]
#[error("{message}")]
pub struct ParseError {
    /// 1-based line in the file.
    pub line: usize,
    pub message: String,
}

/// Parses `source`. A leading UTF-8 BOM and CRLF line endings are accepted.
pub fn parse(source: &str) -> Result<Parsed, ParseError> {
    let text = source.strip_prefix('\u{feff}').unwrap_or(source);
    let mut lines = text.split_inclusive('\n');
    let opener = lines.next().filter(|line| is_delimiter(line));
    let Some(opener) = opener else {
        return Ok(Parsed {
            frontmatter: None,
            body_start_line: 1,
        });
    };

    let yaml_start = opener.len();
    let mut yaml_end = yaml_start;
    for (index, line) in lines.enumerate() {
        if is_delimiter(line) {
            // `index` counts lines after the opener, which is line 1.
            let end_line = index + 2;
            let metadata = parse_yaml(&text[yaml_start..yaml_end])?;
            return Ok(Parsed {
                frontmatter: Some(Frontmatter {
                    start_line: 1,
                    end_line,
                    metadata,
                }),
                body_start_line: end_line + 1,
            });
        }
        yaml_end += line.len();
    }
    Err(ParseError {
        line: 1,
        message: "frontmatter is opened with `---` but never closed with `---`".to_owned(),
    })
}

fn is_delimiter(line: &str) -> bool {
    line.trim_end() == "---"
}

/// `yaml` starts on file line 2, directly after the opening delimiter.
fn parse_yaml(yaml: &str) -> Result<Map<String, Value>, ParseError> {
    const LINES_BEFORE_YAML: u64 = 1;
    let mut options = Options::default();
    // YAML 1.2 booleans only, so `skills: [x, y, n]` stays strings like it does in Claude's parser.
    options.strict_booleans = true;
    // The file is untrusted: keep the parser's alias/anchor/depth budgets, only tighter.
    if let Some(budget) = options.budget.as_mut() {
        budget.max_depth = 32;
        budget.max_aliases = 100;
        budget.max_anchors = 100;
        budget.max_events = 100_000;
        budget.max_nodes = 50_000;
    }
    match serde_saphyr::from_str_with_options::<Option<Map<String, Value>>>(yaml, options) {
        // An empty block is YAML `null`, and means "no fields".
        Ok(metadata) => {
            // serde_json keeps insertion order (for settings merging); metadata stays sorted.
            let mut metadata = Value::Object(metadata.unwrap_or_default());
            metadata.sort_all_objects();
            let Value::Object(metadata) = metadata else {
                unreachable!("an object stays an object")
            };
            Ok(metadata)
        }
        Err(error) => {
            let render = render_options! {
                line_offset: LINES_BEFORE_YAML,
                snippets: SnippetMode::Off,
            };
            Err(ParseError {
                line: error
                    .location()
                    .map_or(2, |at| at.line() as usize + LINES_BEFORE_YAML as usize),
                message: format!(
                    "invalid YAML frontmatter: {}",
                    error.render_with_options(render)
                ),
            })
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn ok(source: &str) -> Parsed {
        parse(source).unwrap()
    }

    fn err(source: &str) -> ParseError {
        parse(source).unwrap_err()
    }

    #[test]
    fn without_delimiter_there_is_no_frontmatter() {
        for source in [
            "",
            "# Title\n",
            "text\n---\na: 1\n---\n",
            "--- x\na: 1\n---\n",
        ] {
            let parsed = ok(source);
            assert_eq!(parsed.frontmatter, None, "{source:?}");
            assert_eq!(parsed.body_start_line, 1);
        }
    }

    #[test]
    fn reads_fields_and_reports_line_numbers() {
        let parsed = ok("---\nname: demo\nmodel: opus\n---\n# Body\n");
        let frontmatter = parsed.frontmatter.unwrap();
        assert_eq!(frontmatter.start_line, 1);
        assert_eq!(frontmatter.end_line, 4);
        assert_eq!(parsed.body_start_line, 5);
        assert_eq!(frontmatter.get("model"), Some(&json!("opus")));
    }

    #[test]
    fn empty_block_and_missing_body_are_valid() {
        let parsed = ok("---\n---");
        assert_eq!(parsed.frontmatter.unwrap().metadata, Map::new());
        assert_eq!(parsed.body_start_line, 3);
        assert!(ok("---\n# just a comment\n---\n").frontmatter.is_some());
    }

    #[test]
    fn native_claude_fields_and_unknown_keys_are_preserved() {
        let parsed = ok(
            "---\nname: reviewer\nmodel: claude-opus-5-5\neffort: high\nskills:\n  - tdd\n  - y\nx-custom:\n  nested: [1, true]\n---\n",
        );
        let metadata = parsed.frontmatter.unwrap().metadata;
        assert_eq!(metadata["effort"], json!("high"));
        // `y` is a skill name, not a YAML 1.1 boolean.
        assert_eq!(metadata["skills"], json!(["tdd", "y"]));
        assert_eq!(metadata["x-custom"], json!({"nested": [1, true]}));
    }

    #[test]
    fn block_scalars_and_multiline_values() {
        let parsed = ok(
            "---\ndescription: |\n  line one\n  line two\nfolded: >-\n  a\n  b\nquoted: \"x\n  y\"\n---\nbody\n",
        );
        let metadata = parsed.frontmatter.unwrap().metadata;
        assert_eq!(metadata["description"], json!("line one\nline two\n"));
        assert_eq!(metadata["folded"], json!("a b"));
        assert_eq!(metadata["quoted"], json!("x y"));
    }

    #[test]
    fn crlf_and_bom_are_equivalent_to_lf() {
        let lf = ok("---\nname: demo\ndescription: |\n  a\n  b\n---\nbody\n");
        let crlf = ok("---\r\nname: demo\r\ndescription: |\r\n  a\r\n  b\r\n---\r\nbody\r\n");
        let bom = ok("\u{feff}---\nname: demo\ndescription: |\n  a\n  b\n---\nbody\n");
        let metadata = |p: &Parsed| p.frontmatter.as_ref().unwrap().metadata["name"].clone();
        assert_eq!(metadata(&crlf), metadata(&lf));
        assert_eq!(metadata(&bom), metadata(&lf));
        assert_eq!(crlf.body_start_line, lf.body_start_line);
        assert_eq!(bom.body_start_line, lf.body_start_line);
        // Block scalars keep the `\n` they were written with after line-ending normalisation.
        assert_eq!(
            crlf.frontmatter.unwrap().metadata["description"],
            json!("a\nb\n")
        );
    }

    #[test]
    fn delimiter_tolerates_trailing_whitespace_only() {
        assert!(ok("---  \r\na: 1\n--- \t\n").frontmatter.is_some());
        assert_eq!(err("---\na: 1\n----\n").line, 1);
    }

    #[test]
    fn unterminated_frontmatter_points_at_the_opener() {
        let error = err("\u{feff}---\nname: x\n\nbody never closes\n");
        assert_eq!(error.line, 1);
        assert!(error.message.contains("never closed"));
    }

    #[test]
    fn invalid_yaml_reports_the_file_line() {
        let error = err("---\nname: ok\nlist: [1, 2\n---\nbody\n");
        assert_eq!(error.line, 3, "{error:?}");
        assert!(error.message.starts_with("invalid YAML frontmatter"));
        // The line quoted in the message is a file line too, not relative to the YAML block.
        assert!(error.message.contains("line 3"), "{}", error.message);
        // Reported at the same place with CRLF line endings.
        assert_eq!(err("---\r\nname: ok\r\nlist: [1, 2\r\n---\r\n").line, 3);
    }

    #[test]
    fn frontmatter_must_be_a_mapping_with_unique_keys() {
        for yaml in ["- a\n- b\n", "just text\n", "a: 1\na: 2\n"] {
            let error = err(&format!("---\n{yaml}---\n"));
            assert!(error.line >= 2, "{yaml:?} -> {error:?}");
        }
    }

    #[test]
    fn yaml_bombs_hit_the_budget_instead_of_exhausting_memory() {
        let mut bomb = String::from("---\na: &a [x, x, x, x, x, x, x, x, x]\n");
        for (prev, next) in "abcdefghij".chars().zip("bcdefghijk".chars()) {
            bomb.push_str(&format!(
                "{next}: &{next} [{}]\n",
                vec![format!("*{prev}"); 9].join(", ")
            ));
        }
        bomb.push_str("---\n");
        assert!(parse(&bomb).is_err());

        let deep = format!("---\na: {}1{}\n---\n", "[".repeat(5_000), "]".repeat(5_000));
        assert!(parse(&deep).is_err());
        let deep = format!("---\na:\n  {}x\n---\n", "- ".repeat(5_000));
        assert!(parse(&deep).is_err());
    }

    #[test]
    fn tags_are_data_not_instructions() {
        let parsed = ok("---\nx: !!python/object/apply:os.system [echo hi]\n---\n");
        assert!(parsed.frontmatter.is_some() || parse("---\nx: !!python/x y\n---\n").is_err());
    }
}
