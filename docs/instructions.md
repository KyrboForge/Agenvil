# Project instructions

This page explains the portability model behind `[paths] instructions` and what it means for a
harness-neutral agent configuration to include project-wide instructions.

## What "portable project instructions" means

`paths.instructions` names a single Markdown file that is meant to be **project-scoped**: it
describes how contributors, agents and tools should work in this project. It lives next to the
agents and skills it accompanies, is version-controlled with them, and travels with the project.

Agenvil treats the file as opaque text. No templating, interpolation, DSL, code execution or
import resolution is applied. Any `@path` imports that Claude Code's own tooling follows inside the
file are **not** followed, rewritten or validated by Agenvil; that content is the author's
responsibility.

## Target-specific output

The same source file can be mapped to different target-native locations by each adapter. Only the
Claude adapter is implemented today:

| Target | Native entrypoint | Status |
| --- | --- | --- |
| Claude Code | `CLAUDE.md` at the root of the staging tree | **Implemented** (issue #40) |
| Codex | `AGENTS.md` | **Not implemented** — no Codex adapter exists |

**No Codex support exists.** The table above is forward-looking; Agenvil produces no Codex output
today.

### Why the same source file is not guaranteed to be semantically correct for all targets

Each harness defines its own instruction format, scope, loading order and limits. Claude Code reads
`./CLAUDE.md` as project context; Codex loads global instructions from `~/.codex` separately,
then walks from the project root down to the working directory, selecting at most one file per
directory (an override file takes precedence over `AGENTS.md`) and capping the combined size at
32 KiB by default. Instructions outside the project, including above its root, are not
inherited. Discovery, precedence, scope and size semantics differ.

Copying Claude-specific text (slash commands, `@`-import paths, Claude-specific tool references)
into an `AGENTS.md` is therefore **not** assumed to be semantically correct. The source file should
contain only content that is meaningful and correct for every target you generate output for.
Target-specific guidance belongs in a per-target override (see [Future extension
point](#future-extension-point-per-target-override-sources) below).

## Project scope, not user scope

`paths.instructions` is **project-scoped**: it describes this project and ships with it.

User/global instructions — Claude Code's `~/.claude/CLAUDE.md`, Codex's
`~/.codex/AGENTS.md` — are HOME-state: per-user, per-machine, and not part of a project
repository. Agenvil never writes them. Installing user-level instructions is out of scope;
it is tracked separately as issue #12.

Building a staging tree with `agenvil build --target claude` writes `CLAUDE.md` into the staging
directory, **not** into any live repository or home directory. Copy or link the staging tree
yourself.

## Future extension point: per-target override sources

> **This feature is not implemented.** The section below documents a planned extension point only.
> No conditional DSL, no per-target sections in `agenvil.toml` and no Codex support exist today.

A future adapter could accept an optional per-target override source alongside the shared
`paths.instructions` file. When an override is present the adapter uses it instead of (or in
addition to) the shared file; when absent it falls back to the shared file. The override is
still a plain Markdown file — no templating — and is subject to the same source-validation rules.
No syntax for this is defined yet; it would look something like:

```toml
# Hypothetical — not implemented
[paths]
instructions = "instructions.md"

[targets.claude]
enabled = true
instructions = "instructions.claude.md"   # override for Claude only
```

This is a documentation placeholder. Do not rely on it; it will be designed on a dedicated issue
when a second adapter is added.
