# Model and effort policy

How Agenvil reads `model` and `effort` from sources and resolves them for a target. Today the only
target is Claude (`target = claude`). Nothing here needs Team Agents Kit (TAK) files: a project
with only `agenvil.toml`, `agents/` and `skills/` is complete.

## Native Claude behaviour

Checked against the Claude Code docs (sub-agents, skills, model configuration), 2026-11.

- Agents: `name` and `description` are required; `model` and `effort` are optional. `model` is an
  alias (`haiku`, `sonnet`, `opus`, `fable`), a full ID such as `claude-opus-5-5`, or `inherit`.
  Omitting it inherits the conversation's model. `effort` is `low`, `medium`, `high`, `xhigh` or
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
that is not in `models`/`efforts`. The policy values are free strings; whether the target supports
them is decided when resolving.

## Target resolution

```toml
[[targets.claude.map]]
source = ["opus", "xhigh"]
target = ["opus", "max"]
```

`agenvil::target::claude` resolves one agent or skill (the same function for both):

| Source | Result |
| --- | --- |
| no `model` and no `effort`, or `model: inherit` alone | `inherit`, nothing to map |
| only one of them (or `inherit` with an `effort`) | `incomplete-pair` |
| pair not in `pairs` | `source-not-allowed` |
| pair has a `map` entry | `resolved`, `via: override` |
| pair is natively supported | `resolved`, `via: same` |
| pair allowed but not natively supported, no entry | `missing-mapping` |
| `map` entry whose target is not natively supported | `unsupported-target` |

A `map` entry is rejected at load time (exit `2`) if its `source` is not in `pairs`, is listed
twice, or its `target` is not supported by Claude. There is no fallback: every result is either
the source pair, an explicitly configured target pair, or a diagnostic with a stable `code`.
Results serialise as JSON (`{"status":"resolved","source":[..],"target":[..],"via":"override"}`)
with fixed key order.

"Natively supported" means an alias or a lowercase `claude-*` ID, and one of the five effort
levels. Per-model effort support is **not** modelled (aliases move with releases); pin the pair or
map it.

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
