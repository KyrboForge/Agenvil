# Model and effort policy

How Agenvil reads `model` and `effort` from sources and resolves them for a target. Today the only
target is Claude (`target = claude`). Nothing here needs Team Agents Kit (TAK) files: a project
with only `agenvil.toml`, `agents/` and `skills/` is complete.

## Native Claude behaviour

Checked against the Claude Code docs (sub-agents, skills, model configuration), 2026-11.

- Agents: `name` and `description` are required; `model` and `effort` are optional. `model` is an
  alias (`haiku`, `sonnet`, `opus`, `fable`), a full ID such as `claude-opus-5-5`, or `inherit`.
  Omitting it inherits the conversation's model. The two fields are independent: each one that is
  omitted (or empty) is inherited on its own. `effort` is `low`, `medium`, `high`, `xhigh` or
  `max`; which levels apply depends on the model (for example Opus 4.6 and Sonnet 4.6 have no
  `xhigh`).
- Skills: `model` and `effort` are **Claude extensions**, optional, and not part of the
  [Agent Skills](https://agentskills.io/specification) spec (`name`, `description`, `license`,
  `compatibility`, `metadata`, `allowed-tools`). A standard `SKILL.md` is valid without them.
- Claude Code itself falls back silently to the highest supported effort at or below the one
  requested. **Agenvil does not**: it never changes an effort or a model on its own.

Agenvil keeps frontmatter as written (`model`, `effort`, unknown keys, the Markdown body) and
invents no `strong`/`cheap`/`balanced` tiers.

## Project policy (source side)

```toml
[policy.models.claude]
models  = ["sonnet", "opus"]
efforts = ["medium", "high", "xhigh"]
pairs   = [["sonnet", "medium"], ["opus", "high"], ["opus", "xhigh"]]
```

`pairs` is the allowlist of `(model, effort)` combinations a source may declare (AG002-AG004
report violations). `agenvil.toml` is rejected with exit `2`, every problem listed, when `models` or
`efforts` have empty or duplicate values, or when `pairs` has duplicates or uses a model or effort
that is not in `models`/`efforts`. The policy values are free strings (legacy configs may hold
non-Claude names); when resolving, a source value that is not literal Claude frontmatter is
`not-native`, whatever the policy says.

## Target resolution

```toml
[[targets.claude.map]]
source = ["opus", "xhigh"]
target = ["opus", "max"]
```

`agenvil::target::claude` resolves one agent or skill (the same function for both):

| Source | Result |
| --- | --- |
| `model` or `effort` not a string | `not-a-string` |
| a value that is not an alias, a `claude-*` ID or an effort level | `not-native` |
| no `model` and no `effort` (omitted, empty, or `model: inherit`) | `inherit`, nothing to map |
| only `model` (in `models`, documented support) | `partial`, model kept, effort inherited |
| only `effort` (in `efforts`), or `model: inherit` with an `effort` | `partial`, effort kept, model inherited |
| a lone value not in `models`/`efforts` | `source-not-allowed` |
| a lone `model` without documented support | `unsupported-target` (maps are keyed by pairs) |
| pair not in `pairs` | `source-not-allowed` |
| pair has a `map` entry | `resolved`, `via: override` |
| pair is natively supported | `resolved`, `via: same` |
| pair allowed but not natively supported, no entry | `missing-mapping` |
| `map` entry whose target is not natively supported | `unsupported-target` |

A `map` entry is rejected at load time (exit `2`) if its `source` is not in `pairs`, is listed
twice or is not native Claude frontmatter, or its `target` is not supported by Claude. There is no fallback: every result is either
the source pair, an explicitly configured target pair, or a diagnostic with a stable `code`.
Results serialise as JSON (`{"status":"resolved","source":[..],"target":[..],"via":"override"}`)
with fixed key order.

"Supported" means Claude Code runs the pair exactly as written: an alias, or a full ID in
`target::claude_values::MODELS` (the documented model/effort table, e.g. `claude-opus-4-6` has no
`xhigh`), with an effort level that model accepts. A well-formed but unlisted `claude-*` ID cannot
be verified offline and is not supported; map it to a listed ID or an alias. Aliases are taken as
resolved on the Anthropic API, where every current model accepts all five levels; a provider that
resolves an alias to an older model may still lower the effort, so pin a full ID to have it checked.
A lone `effort` cannot be checked against the inherited model at all.

This is a library API; no CLI command or rule calls it yet. The Claude builder (#10) will.

## Adding a target

`target::resolve` is target independent. A new adapter supplies a `Supports` function (what it
accepts) and its own `[[targets.<name>.map]]` entries (the same `PairMap` shape). Nothing else
changes; no Codex adapter exists yet.

## Legacy TAK differences

Not changed by this document; tracked in #29 and in the
[known differences](compatibility.md#known-differences):

- PO001 compares agents with a `POLICY.md` model table (`kit:model-table` markers) and needs that
  file. The model policy itself does not.
- AG001 requires `model`, `effort` and `tools` on agents; SK002 warns when a skill omits
  `model`/`effort`. Claude documents these as optional.
- The default policy and `examples/agenvil.toml` are the TAK table.
