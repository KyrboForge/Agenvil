# `agenvil build --target claude`

Builds a **Claude Code staging tree** from your sources. It is staging only: nothing is
installed, and the real HOME and `~/.claude` are never written. Copy or link the result into a
project (`.claude/`) or `~/.claude` yourself.

Agenvil is an independent agent configuration manager. The builder needs no `POLICY.md`, no
`kit:model-table`, no company layout and does not depend on `agenvil check` passing.

## A minimal project

```
my-project/
├── agenvil.toml
├── agents/reviewer.md
└── skills/tdd/
    ├── SKILL.md
    ├── references/cycle.md
    └── scripts/run.sh
```

`agenvil.toml` needs only:

```toml
schema = 1

[project]
name = "my-project"
```

A complete project lives in [`examples/standalone/`](../examples/standalone).

### Declare an agent

`agents/<name>.md`, with the native Claude Code frontmatter. `name` and `description` are
required by Claude Code and by the builder; every other field is kept as written.

```markdown
---
name: reviewer
description: Reviews a diff for correctness bugs and missing tests.
tools: Read, Grep, Glob
model: opus
effort: high
skills:
  - tdd
---
You review code changes.
```

### Declare a skill

`skills/<name>/SKILL.md` plus any supporting files. `name` in the frontmatter is optional but, if
present, must equal the directory name. `model` and `effort` are optional: a standard skill needs
neither. The whole directory is copied byte for byte (references, scripts, assets, binaries,
nested directories); there is no templating.

### Reference skills from agents

List skill names under the agent's `skills:` key. Claude Code preloads them natively; Agenvil adds
no orchestration. Claude Code silently ignores an unknown skill, so the build turns these into
errors instead: a name that is not a skill of this project, a duplicate, a non-string entry, a
`skills:` that is not a YAML list, and more than the 32 skills Claude Code preloads.

### Model and effort

Every agent and skill goes through the resolver described in [model-policy.md](model-policy.md):

| Source | Output |
| --- | --- |
| no `model`/`effort`, or `model: inherit` | copied; the session's model is inherited |
| only `model` or only `effort` (partial) | checked against the policy lists, copied as written |
| a pair Claude Code runs as written | copied as written |
| a pair with an explicit `[[targets.claude.map]]` entry | the `model` and `effort` lines are rewritten to the mapped pair, nothing else changes |
| anything else | the build fails with the resolver's diagnostic; there is never a silent fallback |

```toml
[[targets.claude.map]]
source = ["opus", "xhigh"]
target = ["opus", "high"]
```

The source pair must also be allowed by `[policy.models.claude]` (the defaults are the five pairs
listed in the README). A mapping only rewrites plain one-line `model: x` / `effort: y` entries; any
other shape (flow style, duplicates) is refused rather than guessed, and the rewritten file is
parsed again before it is written.

## Running it

```
agenvil build --target claude [--out DIR] [--root DIR] [--config FILE]
```

`--out` defaults to `.generated/claude` under the project root (add `.generated/` to `.gitignore`;
this repository does). Only `claude` is a supported target, and it must not be disabled with
`[targets.claude] enabled = false`.

| Exit | Meaning |
| --- | --- |
| 0 | built |
| 1 | the sources are invalid (missing description, unknown skill, unsupported model/effort, case collision, ...), nothing written |
| 2 | usage or config error: unknown target, disabled target, unsafe `--out` |
| 3 | I/O error while writing |
| 4 | the input is incomplete (an unreadable, oversized, symlinked or non-regular file; see the README's [problems](../README.md#problems)); an incomplete inventory is never a successful build |

## Output

```
.generated/claude/
├── manifest.json
└── .claude/
    ├── agents/<name>.md
    ├── skills/<name>/SKILL.md (+ every supporting file)
    └── rules/<name>.md
```

These are the locations Claude Code reads. Emitted kinds: agents, skills and rules (`rules/*.md`,
copied as they are; Claude Code reads only their `paths` frontmatter). **Gap:** `CLAUDE.md` project
instructions have no source in the Agenvil schema yet, so none is generated and none is invented.

Unknown agent fields (for example `x-team:`) are kept untouched: Claude Code ignores them, so
there is nothing to drop or warn about. Executable bits of skill scripts are kept on Unix; no other
mode bit is copied.

### `manifest.json`

```json
{
  "schema": 1,
  "target": "claude",
  "owner": "agenvil",
  "generator": { "name": "agenvil", "version": "0.1.0" },
  "files": [
    {
      "path": ".claude/agents/reviewer.md",
      "source": "agents/reviewer.md",
      "sha256": "…",
      "bytes": 252,
      "transform": "copy"
    }
  ]
}
```

`files` is sorted by `path`; paths are relative to the output directory and `/`-separated.
`sha256` and `bytes` describe the written file, `transform` is `copy` or `model-effort-override`.
Output is byte-for-byte deterministic: no timestamps, and nothing is injected into the generated
files. `manifest.json` itself is not listed. Drift detection and cleanup (`build --check`) are
separate work and read this file.

## Ownership and safety

- Sources are untrusted data: never executed, no network. Symlinks, special files, `..`, names
  with `\` or `:` and control characters are rejected; scans are bounded (16 MiB per file by default
  for the build, plus the inventory's total and entry limits).
- Output paths that differ only by case collide on macOS and Windows and fail the build.
- The build is staged in a temporary sibling directory and renamed into place only after every
  file was written, so a failure leaves no partial output and keeps a previous good build.
- An existing `--out` is replaced only if it is empty or holds an earlier Agenvil build: its
  `manifest.json` has `owner: "agenvil"` and `target: "claude"` and every file in it is listed. A
  file you added, another tool's directory or a non-directory makes the build fail untouched
  (exit 2).
- `--out` may not contain `..`, point inside `~/.claude`, `$CLAUDE_CONFIG_DIR` or `%USERPROFILE%\.claude`
  (symlinks resolved), or contain the project root.

## Not covered

Native-Claude checks are limited to: agent `name`/`description`, unique agent names, the `skills:`
list, skill name equals directory, model/effort. Claude Code's other frontmatter fields are not
validated. Not in this command: `build --check`, cleanup of removed sources from an installed copy,
installation, other targets, a template language, private skill isolation.
