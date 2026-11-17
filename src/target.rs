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
    /// Only one of `model` and `effort` is set. Claude Code inherits the other one separately;
    /// Agenvil keeps the set one as is and does not guess the inherited value. Mappings are
    /// keyed by pairs, so they never apply here.
    Partial {
        #[serde(skip_serializing_if = "Option::is_none")]
        model: Option<String>,
        #[serde(skip_serializing_if = "Option::is_none")]
        effort: Option<String>,
    },
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
/// 1. `source` must be one of `allowed` (the project policy), when the policy lists pairs.
/// 2. A matching entry in `maps` decides the target pair, which must be `supports`-ed.
/// 3. Otherwise `source` itself must be supported; there is no fallback.
pub fn resolve(
    source: &Pair,
    allowed: Option<&[Pair]>,
    maps: &[PairMap],
    supports: Supports,
) -> Result<Resolution, Diagnostic> {
    if allowed.is_some_and(|allowed| !allowed.contains(source)) {
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

/// Native Claude Code frontmatter values, from the Claude Code model configuration docs and the
/// Claude models overview (2026-11).
pub mod claude_values {
    pub const ALIASES: [&str; 4] = ["haiku", "sonnet", "opus", "fable"];
    pub const EFFORTS: [&str; 5] = ["low", "medium", "high", "xhigh", "max"];
    const NO_XHIGH: &[&str] = &["low", "medium", "high", "max"];
    /// Full model IDs whose effort support is documented, with the levels each accepts. Claude
    /// Code silently runs an unsupported level as a lower one, so only these are verifiable.
    pub const MODELS: [(&str, &[&str]); 12] = [
        ("claude-fable-5-2", &EFFORTS),
        ("claude-fable-5-1", &EFFORTS),
        ("claude-fable-5", &EFFORTS),
        ("claude-opus-5-5", &EFFORTS),
        ("claude-sonnet-5-5", &EFFORTS),
        ("claude-haiku-5-5", &EFFORTS),
        ("claude-opus-5", &EFFORTS),
        ("claude-sonnet-5", &EFFORTS),
        ("claude-opus-4-8", &EFFORTS),
        ("claude-opus-4-7", &EFFORTS),
        ("claude-opus-4-6", NO_XHIGH),
        ("claude-sonnet-4-6", NO_XHIGH),
    ];
}

/// Whether `model` is a value Claude Code accepts in frontmatter: an alias or a full `claude-*`
/// model ID. Says nothing about whether that ID exists or what it supports.
pub fn native_model(model: &str) -> Result<(), String> {
    let full_id = model.strip_prefix("claude-").is_some_and(|rest| {
        !rest.is_empty()
            && rest
                .chars()
                .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '.'))
    });
    if claude_values::ALIASES.contains(&model) || full_id {
        Ok(())
    } else {
        Err(format!(
            "`{model}` is not a Claude model alias ({}) or a full `claude-*` model ID",
            claude_values::ALIASES.join(", ")
        ))
    }
}

pub fn native_effort(effort: &str) -> Result<(), String> {
    if claude_values::EFFORTS.contains(&effort) {
        Ok(())
    } else {
        Err(format!(
            "`{effort}` is not a Claude effort level ({})",
            claude_values::EFFORTS.join(", ")
        ))
    }
}

/// Whether `pair` is literal Claude frontmatter. Sources must be; mapping cannot launder an
/// invented value (a tier name, another vendor's model) into a Claude one.
pub fn claude_native(pair: &Pair) -> Result<(), String> {
    native_model(&pair[0])?;
    native_effort(&pair[1])
}

/// The effort levels `model` is documented to support.
///
/// shortcut: aliases are taken as resolved on the Anthropic API (every current model accepts
/// all levels); a provider that resolves `opus` to Opus 4.6 would run `xhigh` as `high`. Pin a
/// full ID to have it checked; model a per-provider alias table if that becomes common.
fn claude_efforts(model: &str) -> Result<&'static [&'static str], String> {
    native_model(model)?;
    if claude_values::ALIASES.contains(&model) {
        return Ok(&claude_values::EFFORTS);
    }
    claude_values::MODELS
        .iter()
        .find(|(id, _)| *id == model)
        .map(|(_, efforts)| *efforts)
        .ok_or_else(|| {
            format!(
                "`{model}` is not a model ID with documented effort support in this Agenvil version; use an alias or one of: {}",
                claude_values::MODELS.map(|(id, _)| id).join(", ")
            )
        })
}

/// Whether Claude Code runs `pair` exactly as written: a known model, and an effort level that
/// model supports (not one Claude Code would silently lower).
pub fn claude_supports(pair: &Pair) -> Result<(), String> {
    let [model, effort] = pair;
    let efforts = claude_efforts(model)?;
    native_effort(effort)?;
    if efforts.contains(&effort.as_str()) {
        Ok(())
    } else {
        Err(format!(
            "`{model}` does not support effort `{effort}` (it supports {}); Claude Code would silently run a lower level",
            efforts.join(", ")
        ))
    }
}

/// Resolves the `model` and `effort` frontmatter values of one agent or skill for `target =
/// claude`. Agents and skills go through the same function; neither is required to declare them.
///
/// The fields are independent, as in Claude Code: an unset (or `null`) field, or `model:
/// inherit`, is inherited. A complete pair goes through [`resolve`]; a lone value is kept as is.
/// Policy lists are optional: an unset list allows every native value, a set one is authoritative.
pub fn claude(
    config: &Config,
    model: Option<&Value>,
    effort: Option<&Value>,
) -> Result<Resolution, Diagnostic> {
    let text = |field: &str, value: Option<&Value>| match value {
        None | Some(Value::Null) => Ok(None),
        Some(Value::String(s)) => Ok(Some(s.clone())),
        Some(other) => Err(diagnostic(
            "not-a-string",
            format!("`{field}` must be a string, found {other}"),
        )),
    };
    let not_native = |reason| diagnostic("not-native", reason);
    let model = text("model", model)?.filter(|m| m != "inherit");
    let effort = text("effort", effort)?;
    if let Some(m) = &model {
        native_model(m).map_err(not_native)?;
    }
    if let Some(e) = &effort {
        native_effort(e).map_err(not_native)?;
    }
    let policy = &config.policy.models.claude;
    let not_allowed = |kind: &str, value: &str| {
        diagnostic(
            "source-not-allowed",
            format!(
                "{kind} `{value}` is not allowed; add it to [policy.models.claude].{kind}s or change the source"
            ),
        )
    };
    match (model, effort) {
        (None, None) => Ok(Resolution::Inherit),
        (Some(m), Some(e)) => resolve(
            &[m, e],
            policy.pairs.as_deref(),
            &config.targets.claude.map,
            claude_supports,
        ),
        (Some(m), None) => {
            if policy
                .models
                .as_ref()
                .is_some_and(|list| !list.contains(&m))
            {
                return Err(not_allowed("model", &m));
            }
            claude_efforts(&m).map_err(|reason| {
                diagnostic(
                    "unsupported-target",
                    format!("{reason}; a lone model cannot be mapped, so set `effort` and map the pair, or change the source"),
                )
            })?;
            Ok(Resolution::Partial {
                model: Some(m),
                effort: None,
            })
        }
        (None, Some(e)) => {
            if policy
                .efforts
                .as_ref()
                .is_some_and(|list| !list.contains(&e))
            {
                return Err(not_allowed("effort", &e));
            }
            Ok(Resolution::Partial {
                model: None,
                effort: Some(e),
            })
        }
    }
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

    fn tak(extra: &str) -> Config {
        config(&format!("[compat]\nprofile = \"tak\"\n{extra}"))
    }

    fn run(config: &Config, model: &str, effort: &str) -> Result<Resolution, Diagnostic> {
        claude(config, Some(&json!(model)), Some(&json!(effort)))
    }

    fn pair(model: &str, effort: &str) -> Pair {
        [model.to_owned(), effort.to_owned()]
    }

    #[test]
    fn allowed_native_pairs_pass_through() {
        let config = tak("");
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
        let error = run(&tak(""), "haiku", "high").unwrap_err();
        assert_eq!(error.code, "source-not-allowed");
        assert!(error.message.contains("haiku/high"));
    }

    #[test]
    fn without_a_policy_every_native_pair_is_allowed() {
        let config = config("");
        for (m, e) in [
            ("haiku", "high"),
            ("opus", "max"),
            ("claude-opus-5-5", "xhigh"),
        ] {
            assert_eq!(
                run(&config, m, e),
                Ok(Resolution::Resolved {
                    source: pair(m, e),
                    target: pair(m, e),
                    via: Via::Same
                })
            );
        }
        // Native capability is still enforced: no silent substitution, no invented values.
        let error = run(&config, "claude-opus-4-6", "xhigh").unwrap_err();
        assert_eq!(error.code, "missing-mapping");
        assert_eq!(
            run(&config, "gpt-5", "high").unwrap_err().code,
            "not-native"
        );
        assert_eq!(
            run(&config, "opus", "ultra").unwrap_err().code,
            "not-native"
        );
    }

    #[test]
    fn a_user_allowlist_is_authoritative() {
        let config =
            config("[policy.models.claude]\nmodels = [\"opus\"]\npairs = [[\"opus\", \"high\"]]\n");
        assert!(run(&config, "opus", "high").is_ok());
        assert_eq!(
            run(&config, "opus", "max").unwrap_err().code,
            "source-not-allowed"
        );
        let sonnet = json!("sonnet");
        assert_eq!(
            claude(&config, Some(&sonnet), None).unwrap_err().code,
            "source-not-allowed"
        );
        // `efforts` is unset, so a lone effort is only held to the native levels.
        assert!(claude(&config, None, Some(&json!("max"))).is_ok());
    }

    #[test]
    fn explicit_override_wins_and_nothing_else_does() {
        let config = tak(
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

    const PINNED: &str = "[policy.models.claude]\nmodels = [\"claude-opus-4-6\", \"claude-not-a-real-model\"]\nefforts = [\"high\", \"xhigh\"]\npairs = [[\"claude-opus-4-6\", \"xhigh\"], [\"claude-not-a-real-model\", \"high\"]]\n";

    #[test]
    fn allowed_but_unsupported_pair_without_mapping_is_a_missing_mapping() {
        let config = config(PINNED);
        let error = run(&config, "claude-opus-4-6", "xhigh").unwrap_err();
        assert_eq!(error.code, "missing-mapping");
        assert!(
            error
                .message
                .contains("`claude-opus-4-6` does not support effort `xhigh`"),
            "{error}"
        );
        let error = run(&config, "claude-not-a-real-model", "high").unwrap_err();
        assert_eq!(error.code, "missing-mapping");
        assert!(
            error.message.contains("documented effort support"),
            "{error}"
        );
    }

    #[test]
    fn mapping_resolves_an_unsupported_native_pair() {
        let config = config(&format!(
            "{PINNED}[[targets.claude.map]]\nsource = [\"claude-opus-4-6\", \"xhigh\"]\ntarget = [\"claude-opus-4-6\", \"high\"]\n"
        ));
        assert_eq!(
            run(&config, "claude-opus-4-6", "xhigh").unwrap(),
            Resolution::Resolved {
                source: pair("claude-opus-4-6", "xhigh"),
                target: pair("claude-opus-4-6", "high"),
                via: Via::Override
            }
        );
    }

    #[test]
    fn non_native_sources_are_rejected_even_when_allowed() {
        let config = config(
            "[policy.models.claude]\nmodels = [\"gpt-x\", \"strong\"]\nefforts = [\"high\", \"deep\"]\npairs = [[\"gpt-x\", \"high\"]]\n",
        );
        for (model, effort, needle) in [
            (
                Some("gpt-x"),
                Some("high"),
                "`gpt-x` is not a Claude model alias",
            ),
            (Some("strong"), None, "`strong` is not a Claude model alias"),
            (None, Some("deep"), "`deep` is not a Claude effort level"),
        ] {
            let error = claude(
                &config,
                model.map(|m| json!(m)).as_ref(),
                effort.map(|e| json!(e)).as_ref(),
            )
            .unwrap_err();
            assert_eq!(error.code, "not-native");
            assert!(error.message.contains(needle), "{error}");
        }
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
            Some(std::slice::from_ref(&source)),
            &maps,
            claude_supports,
        )
        .unwrap_err();
        assert_eq!(error.code, "unsupported-target");
        assert!(error.message.contains("`ultra`"));
    }

    #[test]
    fn omission_null_and_inherit_are_native_inheritance() {
        let config = config("");
        let null = json!(null);
        let inherit = json!("inherit");
        for (m, e) in [
            (None, None),
            (Some(&null), Some(&null)),
            (Some(&inherit), None),
            (Some(&inherit), Some(&null)),
        ] {
            assert_eq!(
                claude(&config, m, e),
                Ok(Resolution::Inherit),
                "{m:?} {e:?}"
            );
        }
        let error = claude(&config, Some(&json!(3)), Some(&json!("high"))).unwrap_err();
        assert_eq!(error.code, "not-a-string");
    }

    #[test]
    fn a_lone_field_is_kept_and_the_other_inherited() {
        let config = tak("");
        let (sonnet, high) = (json!("sonnet"), json!("high"));
        let model_only = Resolution::Partial {
            model: Some("sonnet".to_owned()),
            effort: None,
        };
        let effort_only = Resolution::Partial {
            model: None,
            effort: Some("high".to_owned()),
        };
        assert_eq!(claude(&config, Some(&sonnet), None), Ok(model_only.clone()));
        assert_eq!(
            claude(&config, Some(&sonnet), Some(&json!(null))),
            Ok(model_only)
        );
        assert_eq!(claude(&config, None, Some(&high)), Ok(effort_only.clone()));
        assert_eq!(
            claude(&config, Some(&json!("inherit")), Some(&high)),
            Ok(effort_only)
        );
        // A lone value is still held to the policy lists and to what the target supports.
        assert_eq!(
            claude(&config, None, Some(&json!("max"))).unwrap_err().code,
            "source-not-allowed"
        );
        let pinned = self::config(PINNED);
        let error = claude(&pinned, Some(&json!("claude-not-a-real-model")), None).unwrap_err();
        assert_eq!(error.code, "unsupported-target");
        assert_eq!(
            serde_json::to_string(&Resolution::Partial {
                model: Some("sonnet".to_owned()),
                effort: None
            })
            .unwrap(),
            r#"{"status":"partial","model":"sonnet"}"#
        );
    }

    #[test]
    fn claude_supports_documented_combinations_only() {
        for model in ["haiku", "sonnet", "opus", "fable", "claude-opus-5-5"] {
            for effort in claude_values::EFFORTS {
                assert_eq!(claude_supports(&pair(model, effort)), Ok(()));
            }
        }
        for model in ["claude-opus-4-6", "claude-sonnet-4-6"] {
            assert_eq!(claude_supports(&pair(model, "max")), Ok(()));
            assert!(claude_supports(&pair(model, "xhigh")).is_err(), "{model}");
        }
        for model in [
            "inherit",
            "Opus",
            "claude-",
            "claude-Opus",
            "claude-not-a-real-model",
            "gpt-5",
            "",
        ] {
            assert!(claude_supports(&pair(model, "high")).is_err(), "{model}");
        }
        assert!(claude_supports(&pair("opus", "ultra")).is_err());
        // Native shape is a separate, weaker check: it is what sources need.
        assert_eq!(
            claude_native(&pair("claude-not-a-real-model", "xhigh")),
            Ok(())
        );
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
        let error = run(&tak(""), "haiku", "high").unwrap_err();
        assert_eq!(
            serde_json::to_string(&error).unwrap(),
            r#"{"code":"source-not-allowed","message":"haiku/high is not an allowed pair; add it to [policy.models.claude].pairs or change the source"}"#
        );
    }
}
