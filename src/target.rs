//! Source-to-target model/effort resolution.
//!
//! A source `(model, effort)` pair is checked against the project's allowed pairs, then either
//! passed through unchanged or replaced by an explicitly configured override. Nothing is ever
//! guessed: an unsupported pair is a [`Diagnostic`], never a silent downgrade, a different effort
//! or a fallback model. (Claude Code itself falls back to the highest supported effort; Agenvil
//! reports instead, so the generated artifact says what the source says.)
//!
//! [`resolve`] is target-independent. A target adapter supplies what it supports (a
//! [`Supports`] function) and its `[[targets.<name>.map]]` overrides; [`claude`] is the first one.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::config::Config;

/// `[model, effort]`, the same shape as `[policy.models.claude].pairs`.
pub type Pair = [String; 2];

/// What a target accepts: `Ok` for a directly supported pair, `Err` with the reason otherwise.
pub type Supports = fn(&Pair) -> Result<(), String>;

/// One `[[targets.<name>.map]]` entry: use `target` wherever the source declares `source`.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PairMap {
    pub source: Pair,
    pub target: Pair,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum Via {
    /// The source pair is directly supported and used as is.
    Same,
    /// An explicit `[[targets.<name>.map]]` entry chose the target pair.
    Override,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
#[serde(tag = "status", rename_all = "lowercase")]
pub enum Resolution {
    /// Neither `model` nor `effort` is set (or `model: inherit`): the target inherits both, as
    /// Claude Code documents. There is nothing to map.
    Inherit,
    Resolved {
        source: Pair,
        target: Pair,
        via: Via,
    },
}

/// Why a pair cannot be resolved. `code` is stable; the message is for people.
#[derive(Clone, Debug, PartialEq, Eq, Serialize)]
pub struct Diagnostic {
    pub code: &'static str,
    pub message: String,
}

impl fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code, self.message)
    }
}

impl std::error::Error for Diagnostic {}

fn diagnostic(code: &'static str, message: String) -> Diagnostic {
    Diagnostic { code, message }
}

fn show(pair: &Pair) -> String {
    format!("{}/{}", pair[0], pair[1])
}

/// Resolves `source` for one target.
///
/// 1. `source` must be one of `allowed` (the project policy).
/// 2. A matching entry in `maps` decides the target pair, which must be `supports`-ed.
/// 3. Otherwise `source` itself must be supported; there is no fallback.
pub fn resolve(
    source: &Pair,
    allowed: &[Pair],
    maps: &[PairMap],
    supports: Supports,
) -> Result<Resolution, Diagnostic> {
    if !allowed.contains(source) {
        return Err(diagnostic(
            "source-not-allowed",
            format!(
                "{} is not an allowed pair; add it to [policy.models.claude].pairs or change the source",
                show(source)
            ),
        ));
    }
    if let Some(map) = maps.iter().find(|m| &m.source == source) {
        return match supports(&map.target) {
            Ok(()) => Ok(Resolution::Resolved {
                source: source.clone(),
                target: map.target.clone(),
                via: Via::Override,
            }),
            Err(reason) => Err(diagnostic(
                "unsupported-target",
                format!(
                    "the mapping for {} targets {}, which is not supported: {reason}",
                    show(source),
                    show(&map.target)
                ),
            )),
        };
    }
    match supports(source) {
        Ok(()) => Ok(Resolution::Resolved {
            source: source.clone(),
            target: source.clone(),
            via: Via::Same,
        }),
        Err(reason) => Err(diagnostic(
            "missing-mapping",
            format!(
                "{} is allowed but the target does not support it ({reason}) and no mapping is configured; add a [[targets.<name>.map]] entry or change the source",
                show(source)
            ),
        )),
    }
}

/// Native Claude Code frontmatter values, checked against the Claude Code docs (2026-11).
pub mod claude_values {
    pub const ALIASES: [&str; 4] = ["haiku", "sonnet", "opus", "fable"];
    pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
}

/// Whether Claude Code accepts `pair` as frontmatter `model` + `effort`: an alias or a full
/// `claude-*` model ID, and one of the documented effort levels.
///
/// shortcut: which levels a given model supports (e.g. no `xhigh` on Opus/Sonnet 4.6) is not
/// modelled; aliases move with releases. Pin the pair in the policy, or map it, until Agenvil
/// carries a model table.
pub fn claude_supports(pair: &Pair) -> Result<(), String> {
    let [model, effort] = pair;
    let full_id = model.strip_prefix("claude-").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '.'))
    });
    if !claude_values::ALIASES.contains(&model.as_str()) && !full_id {
        return Err(format!(
            "`{model}` is not a Claude model alias ({}) or a full `claude-*` model ID",
            claude_values::ALIASES.join(", ")
        ));
    }
    if !claude_values::EFFORTS.contains(&effort.as_str()) {
        return Err(format!(
            "`{effort}` is not a Claude effort level ({})",
            claude_values::EFFORTS.join(", ")
        ));
    }
    Ok(())
}

/// Resolves the `model` and `effort` frontmatter values of one agent or skill for `target =
/// claude`. Agents and skills go through the same function; neither is required to declare them.
///
/// Both unset (or `model: inherit` alone) means native inheritance. Anything else must be a
/// complete pair, because a lone value cannot be checked against the pair policy.
pub fn claude(
    config: &Config,
    model: Option<&Value>,
    effort: Option<&Value>,
) -> Result<Resolution, Diagnostic> {
    let text = |field: &str, value: Option<&Value>| match value {
        None => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(diagnostic(
            "not-a-string",
            format!("`{field}` must be a string, found {other}"),
        )),
    };
    let (model, effort) = (text("model", model)?, text("effort", effort)?);
    let (model, effort) = match (model, effort) {
        (None, None) => return Ok(Resolution::Inherit),
        (Some(m), None) if m == "inherit" => return Ok(Resolution::Inherit),
        (Some(m), Some(e)) if m != "inherit" => (m, e),
        _ => {
            return Err(diagnostic(
                "incomplete-pair",
                "`model` and `effort` must be set together so the pair can be checked against the policy; omit both to inherit".to_owned(),
            ));
        }
    };
    resolve(
        &[model, effort],
        &config.policy.models.claude.pairs,
        &config.targets.claude.map,
        claude_supports,
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    use std::path::Path;

    fn config(extra: &str) -> Config {
        let text = format!("schema = 1\n[project]\nname = \"x\"\n{extra}");
        Config::parse(&text, Path::new("agenvil.toml")).unwrap()
    }

    fn run(config: &Config, model: &str, effort: &str) -> Result<Resolution, Diagnostic> {
        claude(config, Some(&json!(model)), Some(&json!(effort)))
    }

    fn pair(model: &str, effort: &str) -> Pair {
        [model.to_owned(), effort.to_owned()]
    }

    #[test]
    fn allowed_native_pairs_pass_through() {
        let config = config("");
        for (m, e) in [("opus", "high"), ("sonnet", "medium")] {
            assert_eq!(
                run(&config, m, e),
                Ok(Resolution::Resolved {
                    source: pair(m, e),
                    target: pair(m, e),
                    via: Via::Same
                })
            );
        }
    }

    #[test]
    fn forbidden_pair_is_rejected_not_downgraded() {
        let error = run(&config(""), "haiku", "high").unwrap_err();
        assert_eq!(error.code, "source-not-allowed");
        assert!(error.message.contains("haiku/high"));
    }

    #[test]
    fn explicit_override_wins_and_nothing_else_does() {
        let config = config(
            "[[targets.claude.map]]\nsource = [\"opus\", \"xhigh\"]\ntarget = [\"opus\", \"max\"]\n",
        );
        assert_eq!(
            run(&config, "opus", "xhigh"),
            Ok(Resolution::Resolved {
                source: pair("opus", "xhigh"),
                target: pair("opus", "max"),
                via: Via::Override
            })
        );
        // Other sources are untouched by the override.
        assert_eq!(
            run(&config, "opus", "high").unwrap(),
            Resolution::Resolved {
                source: pair("opus", "high"),
                target: pair("opus", "high"),
                via: Via::Same
            }
        );
    }

    #[test]
    fn allowed_but_unsupported_pair_without_mapping_is_a_missing_mapping() {
        let config = config(
            "[policy.models.claude]\nmodels = [\"gpt-x\"]\nefforts = [\"high\"]\npairs = [[\"gpt-x\", \"high\"]]\n",
        );
        let error = run(&config, "gpt-x", "high").unwrap_err();
        assert_eq!(error.code, "missing-mapping");
        assert!(
            error
                .message
                .contains("`gpt-x` is not a Claude model alias")
        );
    }

    #[test]
    fn mapping_makes_a_non_native_source_resolvable() {
        let config = config(
            "[policy.models.claude]\nmodels = [\"gpt-x\"]\nefforts = [\"high\"]\npairs = [[\"gpt-x\", \"high\"]]\n[[targets.claude.map]]\nsource = [\"gpt-x\", \"high\"]\ntarget = [\"claude-opus-5-5\", \"high\"]\n",
        );
        assert_eq!(
            run(&config, "gpt-x", "high").unwrap(),
            Resolution::Resolved {
                source: pair("gpt-x", "high"),
                target: pair("claude-opus-5-5", "high"),
                via: Via::Override
            }
        );
    }

    #[test]
    fn resolve_rejects_an_unsupported_override_target() {
        let source = pair("opus", "high");
        let maps = [PairMap {
            source: source.clone(),
            target: pair("opus", "ultra"),
        }];
        let error = resolve(
            &source,
            std::slice::from_ref(&source),
            &maps,
            claude_supports,
        )
        .unwrap_err();
        assert_eq!(error.code, "unsupported-target");
        assert!(error.message.contains("`ultra`"));
    }

    #[test]
    fn omission_and_inherit_are_native_inheritance() {
        let config = config("");
        assert_eq!(claude(&config, None, None), Ok(Resolution::Inherit));
        assert_eq!(
            claude(&config, Some(&json!("inherit")), None),
            Ok(Resolution::Inherit)
        );
        for (m, e) in [
            (None, Some(json!("high"))),
            (Some(json!("opus")), None),
            (Some(json!("inherit")), Some(json!("high"))),
        ] {
            let error = claude(&config, m.as_ref(), e.as_ref()).unwrap_err();
            assert_eq!(error.code, "incomplete-pair");
        }
        let error = claude(&config, Some(&json!(3)), Some(&json!("high"))).unwrap_err();
        assert_eq!(error.code, "not-a-string");
    }

    #[test]
    fn claude_supports_documented_values_only() {
        for model in ["haiku", "sonnet", "opus", "fable", "claude-opus-5-5"] {
            for effort in claude_values::EFFORTS {
                assert_eq!(claude_supports(&pair(model, effort)), Ok(()));
            }
        }
        for model in ["inherit", "Opus", "claude-", "claude-Opus", "gpt-5", ""] {
            assert!(claude_supports(&pair(model, "high")).is_err(), "{model}");
        }
        assert!(claude_supports(&pair("opus", "ultra")).is_err());
    }

    #[test]
    fn serialization_is_stable() {
        let resolved = Resolution::Resolved {
            source: pair("opus", "xhigh"),
            target: pair("opus", "max"),
            via: Via::Override,
        };
        assert_eq!(
            serde_json::to_string(&resolved).unwrap(),
            r#"{"status":"resolved","source":["opus","xhigh"],"target":["opus","max"],"via":"override"}"#
        );
        assert_eq!(
            serde_json::to_string(&Resolution::Inherit).unwrap(),
            r#"{"status":"inherit"}"#
        );
        let error = run(&config(""), "haiku", "high").unwrap_err();
        assert_eq!(
            serde_json::to_string(&error).unwrap(),
            r#"{"code":"source-not-allowed","message":"haiku/high is not an allowed pair; add it to [policy.models.claude].pairs or change the source"}"#
        );
    }
}
