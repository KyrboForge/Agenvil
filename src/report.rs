//! Output formats for validation results. Pure functions: no I/O, no clocks, no machine paths, so
//! the same findings always produce the same bytes.

use std::fmt::Write;

use serde::Serialize;

use crate::diagnostics::Problem;
use crate::validate::{Finding, Severity};

/// Version of the JSON shape below. Bumped on any breaking change to it.
pub const SCHEMA_VERSION: u32 = 1;

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Summary {
    pub errors: usize,
    pub warnings: usize,
}

impl Summary {
    pub fn of(findings: &[Finding]) -> Self {
        let count = |severity| findings.iter().filter(|f| f.severity == severity).count();
        Self {
            errors: count(Severity::Error),
            warnings: count(Severity::Warning),
        }
    }
}

#[derive(Serialize)]
struct Document<'a> {
    schema: u32,
    findings: &'a [Finding],
    summary: Summary,
}

/// Pretty-printed JSON with a trailing newline.
pub fn json(findings: &[Finding]) -> serde_json::Result<String> {
    let mut out = serde_json::to_string_pretty(&Document {
        schema: SCHEMA_VERSION,
        findings,
        summary: Summary::of(findings),
    })?;
    out.push('\n');
    Ok(out)
}

fn location(finding: &Finding) -> String {
    match (finding.line, finding.column) {
        (Some(line), Some(column)) => format!("{}:{line}:{column}", finding.path),
        (Some(line), None) => format!("{}:{line}", finding.path),
        _ => finding.path.clone(),
    }
}

/// One finding per line; `quiet` drops the fix hints and the summary line.
pub fn human(findings: &[Finding], quiet: bool) -> String {
    let mut out = String::new();
    for f in findings {
        let _ = writeln!(
            out,
            "{}: {}[{}] {}",
            location(f),
            f.severity.as_str(),
            f.id,
            f.message
        );
        if !quiet {
            let _ = writeln!(out, "    fix: {}", f.fix);
        }
    }
    if !quiet {
        let s = Summary::of(findings);
        let _ = writeln!(out, "{} error(s), {} warning(s)", s.errors, s.warnings);
    }
    out
}

/// GitHub Actions workflow commands, one annotation per finding.
pub fn github(findings: &[Finding]) -> String {
    let mut out = String::new();
    for f in findings {
        let mut properties = format!("file={}", escape(&f.path, true));
        if let Some(line) = f.line {
            let _ = write!(properties, ",line={line}");
            if let Some(column) = f.column {
                let _ = write!(properties, ",col={column}");
            }
        }
        let _ = write!(properties, ",title={}", f.id);
        let _ = writeln!(
            out,
            "::{} {properties}::{}",
            f.severity.as_str(),
            escape(&format!("{} (fix: {})", f.message, f.fix), false)
        );
    }
    out
}

/// Escapes a workflow command value; property values also escape `:` and `,`.
fn escape(value: &str, property: bool) -> String {
    let mut out = String::with_capacity(value.len());
    for c in value.chars() {
        match c {
            '%' => out.push_str("%25"),
            '\r' => out.push_str("%0D"),
            '\n' => out.push_str("%0A"),
            ':' if property => out.push_str("%3A"),
            ',' if property => out.push_str("%2C"),
            c => out.push(c),
        }
    }
    out
}

/// Input problems, for stderr: the verdict is incomplete, which is not a policy finding.
pub fn problems(problems: &[Problem]) -> String {
    let mut out = String::new();
    for p in problems {
        let kind = serde_json::to_value(p.kind)
            .ok()
            .and_then(|v| v.as_str().map(str::to_owned))
            .unwrap_or_default();
        let line = p.line.map(|l| format!(":{l}")).unwrap_or_default();
        let _ = writeln!(out, "problem: {}{line}: {kind}: {}", p.path, p.message);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn finding(severity: Severity, line: Option<usize>) -> Finding {
        Finding {
            id: "AG002",
            severity,
            path: "agents/a,b.md".to_owned(),
            line,
            column: line.map(|_| 1),
            message: "50% bad\nsecond line".to_owned(),
            fix: "fix it",
        }
    }

    #[test]
    fn json_has_the_documented_shape() {
        let out = json(&[
            finding(Severity::Error, Some(3)),
            finding(Severity::Warning, None),
        ])
        .unwrap();
        let value: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert_eq!(value["schema"], 1);
        assert_eq!(
            value["summary"],
            serde_json::json!({"errors": 1, "warnings": 1})
        );
        // Key order is part of the contract (serde_json's Value would sort the keys).
        let positions: Vec<_> = ["id", "severity", "path", "line", "column", "message", "fix"]
            .iter()
            .map(|key| out.find(&format!("\"{key}\":")).unwrap())
            .collect();
        assert!(positions.windows(2).all(|w| w[0] < w[1]), "{out}");
        assert!(value["findings"][1]["line"].is_null());
        assert!(out.ends_with("}\n"));
    }

    #[test]
    fn empty_json_is_valid() {
        let value: serde_json::Value = serde_json::from_str(&json(&[]).unwrap()).unwrap();
        assert_eq!(value["findings"], serde_json::json!([]));
    }

    #[test]
    fn github_escapes_and_omits_unknown_positions() {
        let out = github(&[
            finding(Severity::Error, Some(3)),
            finding(Severity::Warning, None),
        ]);
        assert_eq!(
            out,
            "::error file=agents/a%2Cb.md,line=3,col=1,title=AG002::50%25 bad%0Asecond line (fix: fix it)\n\
             ::warning file=agents/a%2Cb.md,title=AG002::50%25 bad%0Asecond line (fix: fix it)\n"
        );
    }

    #[test]
    fn human_quiet_drops_fix_and_summary() {
        let f = [finding(Severity::Error, Some(3))];
        assert_eq!(
            human(&f, true),
            "agents/a,b.md:3:1: error[AG002] 50% bad\nsecond line\n"
        );
        assert!(human(&f, false).ends_with("    fix: fix it\n1 error(s), 0 warning(s)\n"));
    }
}
