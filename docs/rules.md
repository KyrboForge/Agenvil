# Validation rules

`agenvil check` runs the rules in `src/validate/rules.rs` over the parsed inventory and the typed
config. `agenvil rules` lists them, `agenvil explain <ID>` prints one rule's rationale, a failing
and a passing example, and the fix. Rule semantics come from the Team Agents Kit rule table; where
the kit does not define something, this page says so instead of guessing (see
[Unspecified](#unspecified-in-the-kit)).

## Status

| ID | Severity | `--fast` | Status | Checks |
| --- | --- | --- | --- | --- |
| AG001 | error | yes | implemented | an agent has `name`, `description`, `model`, `effort`, `tools`; `name` equals the file name |
| AG002 | error | yes | implemented | `model` is in `[policy.models.claude] models` |
| AG003 | error | yes | implemented | `effort` is in `[policy.models.claude] efforts` |
| AG004 | error | yes | implemented | `(model, effort)` is in `[policy.models.claude] pairs` |
| AG005 | error | yes | implemented | `tools` is a non-empty YAML list of non-empty strings |
| SK001 | error | yes | implemented | `SKILL.md` exists, `name` equals the directory, `description` is not empty |
| SK002 | warning | **no** | implemented | a skill sets `model` and `effort` (an explicit `inherit` counts) |
| CF001 | error | yes | implemented | `config/defaults.toml` has only known sections/keys with valid values |
| PO001 | error | yes | implemented | the generated model table in `POLICY.md` matches the agents, both ways |
| HY001 | error | yes | implemented | no `[hygiene] personal_patterns` match in agents, skills and rules |
| HY002 | error | yes | implemented | files in `bin/` start with `#!` and (Unix) have an executable bit |

Deferred (not implemented; tracked in [`compatibility.md`](compatibility.md)), all `Check::Deferred` in the catalog: AG006, AG007,
CF002, CF003, CF004, CN001, FM001, HY003, HY005, SK003–SK006, ST001, ST002, TL001, TL002, TP001,
WF001.

**A deferred rule is never a pass.** Any selected deferred rule, including those in the default
selection, makes the run exit `4` and names them on stderr (`cannot verify …: not implemented`,
shown even with `--quiet`). To check only the implemented rules, opt out explicitly: pass
`--skip` with the listed IDs, or set them to `"off"` under `[rules]`.

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
a symlink, an oversized or non-UTF-8 file, an unreadable directory, a missing `POLICY.md` while
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

## Known differences from the kit

- **CF003 is not in `--fast`.** The kit lists it as fast, but it needs `git` (tracked files), and
  `--fast` must not spawn processes. The rule is deferred and, once implemented, non-fast.
- **SK002** is a warning outside `--fast`. The kit's tier-based exemptions are deferred with
  SK003; until then every skill is held to the model/effort rule.
- **AG004 pairs come from config.** The defaults match the kit's table
  (`haiku+low`, `sonnet+medium|high`, `opus+high|xhigh`).
- **CF001's key list is the kit's** (update, install, hooks, tools, headroom, connectors, packs)
  and is only enforced when `config/defaults.toml` exists, so a project without it passes.
- **HY001 patterns** default to `/Users/` and `~/.agents/` and live in `[hygiene]`.
- **HY002** needs the executable bit only on Unix; on Windows only the shebang is checked.
- **FM001** is not a rule here: malformed frontmatter is an input problem (exit `4`), which keeps
  it distinguishable from a violation.
- A skill directory without `SKILL.md` is reported by SK001; with SK001 skipped it is an input
  problem again.

## Unspecified in the kit

Documented rather than guessed:

- The format of an *allowed pairs* block inside `POLICY.md` (pairs come from `agenvil.toml`).
- Valid tool names for AG005 (only the list shape is checked).
- A file format for hygiene patterns (config only).
- Tier-based SK002 rules and the valid range of `headroom.port` (CF001 requires an integer).
