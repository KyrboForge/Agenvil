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
agenvil build --target claude --check [--out DIR]
agenvil build --target claude --clean [--apply] [--out DIR]
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

## Drift checks (`build --check`)

```
agenvil build --target claude --check                      # compare .generated/claude
agenvil build --target claude --check --out ./my-stage     # compare another staging directory
```

`--check` is **fully read-only**: it never creates the output directory, never writes, renames or
deletes anything, and never touches a golden snapshot. It loads the config, plans the output in
memory with the same code a normal build uses (`build::expected`), and compares the plan with the
existing staging tree. A **missing staging directory is drift**, not an invitation to generate it.

```
$ agenvil build --target claude --check
changed          .claude/agents/full.md: 98 bytes, expected 91
extra            notes.txt: not generated and not in the manifest
drift: 2 finding(s) in ./.generated/claude
error: the staging tree is out of date: 2 finding(s)
$ echo $?
1
```

A match prints `ok: <out> matches the sources` and exits `0`. The report is sorted by drift class
then path, so it is deterministic. Files are compared by size and SHA-256.

| Class | Meaning |
| --- | --- |
| `missing` | the plan generates it but it is not on disk (or the whole staging directory is absent) |
| `changed` | on disk but its size or SHA-256 differs from the plan |
| `stale` | listed by the previous build's manifest, no longer generated |
| `extra` | on disk, not generated and not in the manifest: yours, never deleted |
| `manifest-invalid` | `manifest.json` is absent, unparsable, an unsupported `schema`, an unexpected `owner` or `target`, has duplicate or case-colliding paths, Windows-invalid paths, a file/directory conflict, or sizes that exceed the limits |
| `unsafe` | a symlink, special file or non-UTF-8 name in the tree, a manifest path that is absolute, contains `..` or leaves the staging root, or an unsafe `--out` |

| Exit | Meaning |
| --- | --- |
| 0 | the tree matches |
| 1 | drift (any class except `unsafe`), or the sources are invalid |
| 2 | invalid config, or an unsafe state: an unsafe `--out`, or an `unsafe` finding |
| 3 | internal or filesystem error |
| 4 | cannot verify: the inputs are incomplete, or the tree exceeds the scan limits (depth 24, 30 000 entries, the build's byte limits) |

**Malformed data is drift (1); an unsafe state is exit 2.** A manifest that is merely wrong (bad
JSON, duplicate paths, wrong owner) says the tree cannot be trusted but is not dangerous to
inspect. A symlink, a path that escapes the root or an `--out` that points at personal Claude
config is something no command should act on, so it gets the usage-error code and blocks cleanup.

### Generated-tree validation

`--check` and every build also validate the staging tree **by itself**, independent of the plan:
layout (`.claude/agents|skills|rules`), parseable frontmatter, Agent Skills metadata (skill `name`
equals its directory), supporting files present, agent `skills:` entries that resolve to a
generated skill, model and effort already resolved to native values, and manifest size/SHA-256
agreeing with the bytes on disk. Source-only rules (PO001, the deferred legacy rules, policy on
what an agent may declare) are **not** applied to output: those belong to `agenvil check` on the
sources. Failures show up as `manifest-invalid`/`changed` or, when the file itself is malformed,
as `invalid` findings in the report.

## Cleanup (`--clean`)

Removing files the sources no longer generate is **opt-in and a dry run by default**:

```
$ agenvil build --target claude --clean
would remove  .claude/agents/inherits.md
dry run: nothing was changed; pass --apply to do this
$ agenvil build --target claude --clean --apply
removed  .claude/agents/inherits.md
```

Only the staging tree is touched. The candidates are the *stale* set (previous manifest minus the
current plan). A file is deleted only if it is a regular file whose size and SHA-256 still match
the manifest, re-verified immediately before each delete. A stale file you modified is *kept* (and
reported as `kept`); unknown files, symlinks and directories with foreign content are never
deleted, directories are removed only when cleanup emptied them, and nothing outside `--out` (so
no `~/.claude` and no HOME data) is ever a candidate. An `unsafe` finding or an untrusted manifest
**blocks** the whole cleanup with nothing deleted (`cleanup blocked`). The manifest is rewritten
atomically afterwards. A manifest entry is an ownership claim, never authorization to delete.

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
files. `manifest.json` itself is not listed. `build --check` and `--clean` read this file, but
only as a claim: see [Drift checks](#drift-checks-build---check).

## Ownership and safety

- Sources are untrusted data: never executed, no network. Symlinks, special files, `..`, names
  with `\` or `:` and control characters are rejected; scans are bounded (16 MiB per file by default
  for the build, plus the inventory's total and entry limits).
- Output paths that differ only by case collide on macOS and Windows and fail the build.
- The build is staged in a temporary sibling directory and renamed into place only after every
  file was written, so a failure leaves no partial output and keeps a previous good build.
- An existing `--out` is replaced only if it is empty or holds an earlier Agenvil build: its
  `manifest.json` has `owner: "agenvil"` and `target: "claude"` and every file in it is listed with
  its recorded size and sha256. A file you added or edited, another tool's directory or a
  non-directory makes the build fail untouched (exit 2).
- Ownership is checked again on the old output after it is moved aside, right before anything is
  deleted, so a directory that changed in between is put back untouched. Names that are not UTF-8
  and a `manifest.json` that is not a regular file count as foreign.
- `--out` may not contain `..`, be inside or contain `~/.claude`, `$CLAUDE_CONFIG_DIR` or
  `%USERPROFILE%\.claude` (so not HOME either; symlinks resolved), contain the project root, or be
  inside the agents, skills or rules source directories.
- Skill directories share one budget of entries and bytes (the inventory's total limits).

## Not covered

Native-Claude checks are limited to: agent `name`/`description`, unique agent names, the `skills:`
list, skill name equals directory, model/effort. Claude Code's other frontmatter fields are not
validated. Not in this command: cleanup of an installed copy, installation, other targets, a template
language, private skill isolation.

Manifest limitations: it is not signed, so anyone who can write the staging directory can forge a
matching manifest; `--check` stays correct because it compares against the plan, not the manifest,
but `--clean` only trusts a manifest for the files whose digests still match. Hash and size mean
modification is detected, not attributed. Mode bits other than the Unix executable bit are not
tracked.
