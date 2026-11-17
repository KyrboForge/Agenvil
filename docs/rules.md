# Validation rules

`agenvil check` runs the rules in `src/validate/rules.rs` over the parsed inventory and the typed
config. `agenvil rules` lists them, `agenvil explain <ID>` prints one rule's rationale, a failing
and a passing example, and the fix. Rule semantics come from the Team Agents Kit rule table; where
the kit does not define something, this page says so instead of guessing (see
[Unspecified](#unspecified-in-the-kit)).

## Status

| ID | Severity | `--fast` | Status | Checks and applicability |
| --- | --- | --- | --- | --- |
| AG001 | error | yes | implemented | Agent requires `name` and `description`; name equals filename. TAK also requires `model`, `effort`, `tools`. |
| AG002 | error | yes | implemented | Declared model must be native and satisfy any model allowlist; `inherit` is valid outside TAK. |
| AG003 | error | yes | implemented | Declared effort must be native and satisfy any effort allowlist. |
| AG004 | error | yes | implemented | Declared pair satisfies any pair allowlist and Claude target support or an explicit mapping. Missing fields are allowed. |
| AG005 | error | yes | implemented | Optional `tools` must be a non-empty list or comma-separated string of non-empty names. TAK requires a list. |
| SK001 | error | yes | implemented | Skill requires non-empty `name` matching its directory and `description`; Agenvil is stricter than Claude Code, where skill name is optional. |
| SK002 | warning | no | implemented | Requires skill `model` and `effort` (including `inherit`); off unless TAK, `--only`, or a `[rules]` entry enables it. |
| CF001 | error | yes | implemented | Checks defaults file when `[paths] defaults` is set. |
| PO001 | error | yes | implemented | Checks the generated model table when `[paths] policy` is set. |
| HY001 | error | yes | implemented | Checks files when `[hygiene] personal_patterns` is non-empty. |
| HY002 | error | yes | implemented | Checks scripts when `[paths] bin` is set; executable bit checked on Unix. |

The 19 deferred rules are listed in [`compatibility.md`](compatibility.md) and `agenvil rules`.
A default check notes on stderr that they were not run; they are never counted as passed.
Explicit selection with `--only` or a non-`off` `[rules]` entry exits `4` as unverifiable.

## Output

Findings are sorted by `(path, line, column, id, message)`, bytewise, so `POLICY.md` precedes
`agents/…`. Paths are relative to the root and use `/`. A finding about a missing key or file has
`line` and `column` `null`: positions are never invented. JSON is pretty-printed with keys in this
order and nothing else:

```json
{"schema": 1, "findings": [{"id": "AG002", "severity": "error", "path": "agents/a.md",
  "line": 4, "column": 1, "message": "…", "fix": "…"}], "summary": {"errors": 1, "warnings": 0}}
```

`--format github` prints one `::error`/`::warning` annotation per finding; `--format human` prints
`path:line:col: severity[ID] message` plus a `fix:` line and a summary. `--quiet` drops the fix
lines and the summary from the human format.

## Exit codes and precedence

| Code | When |
| --- | --- |
| 0 | no finding at or above `--fail-on` (default `error`) |
| 1 | at least one finding at or above `--fail-on` |
| 2 | usage or configuration error: unknown rule ID (`--only`, `--skip`, `[rules]`, `[[ignore]]`), bad severity, empty ignore `reason`, bad `personal_patterns` regex, `--fast --only <non-fast rule>` |
| 3 | internal error |
| 4 | the verdict is incomplete |

Exit `4` wins over `1`. It means a selected rule could not read its input: malformed frontmatter,
a symlink, an oversized or non-UTF-8 file, an unreadable directory, a missing configured policy file while
agents exist, or a selected deferred rule. Findings found so far are still printed, but
a parse failure is never reported as a policy finding. Problems go to stderr, findings to stdout.

## Selection and severity

- `--only`/`--skip` take comma-separated IDs and can be repeated; IDs are exact (no case folding or
  prefixes). `--skip` wins over `--only`.
- `[rules] ID = "error" | "warning" | "off"` overrides the default severity. `--only ID` runs a
  rule the config turned `off`.
- `[[ignore]]` (`rule`, `path`, `reason`) suppresses one rule in one file; `reason` is mandatory.
- `--fast` runs only the rules marked fast above: offline, no subprocess, read-only, never
  executes a script found in the tree. SK002 is excluded to keep the kit's `--fast` list
  (AG001–AG005, SK001, PO001, CF001–CF004, HY001, HY002).
- `--config <FILE>` replaces `<root>/agenvil.toml`; `--root` still selects the project.

## Known differences from kit

TAK conventions are opt-in via `[compat] profile = "tak"`; see
[compatibility and migration](compatibility.md#migrating-from-tak-style-config). CF003 is
excluded from `--fast` because it needs git and remains deferred. HY002 checks executable bits
only on Unix. Malformed frontmatter is an input problem (exit `4`), not an FM001 finding.
A skill directory without `SKILL.md` is reported by SK001; other input problems are not
reported again as SK001 findings.

## Unspecified in the kit

Documented rather than guessed:

- The format of an *allowed pairs* block inside `POLICY.md` (pairs come from `agenvil.toml`).
- Valid tool names for AG005 (only the list shape is checked).
- A file format for hygiene patterns (config only).
- Tier-based SK002 rules and the valid range of `headroom.port` (CF001 requires an integer).
