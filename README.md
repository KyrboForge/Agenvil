# Agenvil

`agenvil` validates and builds AI agent, skill and rule source trees. One Rust binary, one crate.

> **Status: v0.1, in progress.** Configuration loading, exit codes, `inventory`, `check`, `rules`
> and `explain` are implemented (see [`docs/rules.md`](docs/rules.md) for which rules). `build`
> is not: it loads `agenvil.toml` and then exits `4` ("unable to verify") instead of reporting
> success.

## Commands

| Command | Kind | Purpose |
| --- | --- | --- |
| `agenvil check [--fast] [--format human\|json\|github] [--only IDS] [--skip IDS] [--fail-on error\|warning] [--quiet]` | read-only, offline | validate the source tree |
| `agenvil inventory [--format json]` | read-only, offline | list discovered resources as JSON |
| `agenvil rules` | read-only | list validation rules and their status |
| `agenvil explain <ID>` | read-only | explain one rule: rationale, failing and passing example, fix |
| `agenvil build` | mutating | build staged output (never `~/.claude`) |

`--root <DIR>` selects the project root; the default is the current directory. The config file is
`<root>/agenvil.toml` unless `--config <FILE>` overrides it (no search of parent directories). It must be a regular file of at
most 1 MiB; a symlinked `agenvil.toml` is rejected without being read, so the config cannot point
outside the root.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | success |
| 1 | policy findings at or above `--fail-on` (`check`) |
| 2 | usage or configuration error, including unknown rule IDs, severities and policy keys |
| 3 | internal error (including panics) |
| 4 | unable to verify: a command or rule that is not implemented yet, or an inventory that is incomplete because of [problems](#problems); wins over `1` |

## `agenvil inventory`

Discovers `agents/*.md`, `skills/*/SKILL.md` and `rules/*.md` (directories from `[paths]`), splits
each file into YAML frontmatter and Markdown body, and prints one JSON document on stdout. It only
reads files; it never executes, imports or follows anything found in the tree.

```json
{
  "schema": 1,
  "agents": [
    {
      "name": "reviewer",
      "path": "agents/reviewer.md",
      "bytes": 120,
      "frontmatter": { "start_line": 1, "end_line": 5, "metadata": { "model": "opus", "skills": ["tdd"] } },
      "body_start_line": 6
    }
  ],
  "skills": [],
  "rules": [],
  "problems": []
}
```

- **Deterministic:** resources are sorted by path, `metadata` keys are sorted, and no absolute
  paths, timestamps or machine data appear. Paths are relative to the root and use `/` on every OS.
- `name` is the file stem (agents, rules) or the directory name (skills). The frontmatter `name`
  is only another `metadata` field.
- `metadata` is every top-level frontmatter key, unvalidated: Claude fields such as `model`,
  `effort` and `skills`, and unknown keys, are passed through as plain JSON values. Whether a key
  is allowed or required is policy and belongs to `check`. YAML 1.2 booleans only, so
  `skills: [x, y, n]` stays a list of strings. `frontmatter` is `null` for a file without a leading
  `---` block. Line numbers are 1-based file lines (BOM and CRLF do not shift them).
- The Markdown body is not in the JSON (`body_start_line` locates it). Library consumers get the
  exact file text, BOM and CRLF included, in `Resource::source`.
- **Missing directories are not an error:** nothing is discovered there. Files and directories
  that do not match the layout (other extensions, deeper levels, extra files in a skill
  directory) are not resources and are ignored.
- **`.gitignore`:** `.gitignore` files inside the root are honored, with or without a git
  repository. Ignore files above the root, global git config and `.ignore` files are not read, so
  output does not depend on where the project is checked out.
- Two configured directories may not be the same or nested in each other (exit `2`).

### Problems

A file that cannot be read or parsed is reported in `problems` (`path`, 1-based `line` when known,
`kind`, `message`) instead of aborting the run, and the command exits `4` after printing the JSON,
because the inventory is incomplete. Kinds: `symlink`, `not-regular-file`, `too-large`,
`unreadable`, `invalid-utf8`, `invalid-path`, `missing-skill-file`, `not-a-directory`,
`limit-exceeded`, `frontmatter`. A file with malformed frontmatter appears only as a problem, never
as a half-parsed resource. Problems are parse/IO failures, not policy findings (exit `1`, `check`).

### Safety limits

- Symlinks are never followed or read, including in parent components of a configured directory;
  they are reported as `symlink` problems. Nothing outside the root is opened.
- At most 1 MiB per file, 64 MiB in total and 10 000 directory entries per run; the scan depth is
  fixed by the layout. FIFOs and other special files are never opened.
- YAML is parsed with a tight alias, anchor, node and nesting budget, so alias bombs and deeply
  nested documents become `frontmatter` problems instead of exhausting memory. YAML tags are
  data, never instantiated.

### Limitations

- Between checking a path and opening it a concurrent local attacker could swap in a symlink;
  `O_NOFOLLOW` opens are a possible hardening (marked `shortcut:` in the code).
- Not yet: registries, policy files, JSON/TOML resources, symlinks that stay inside the root,
  content hashes, other output formats than JSON.
- Platform: path handling is lexical and the walk uses `/`-normalised paths, so output is the same
  on macOS, Linux and Windows. CI runs on all three; the symlink and FIFO tests run on Unix only.

### For the validator and the Claude builder

`inventory::Inventory::build(root, &config, &Limits)` is the shared entry point: it returns
`Resource`s (`name`, `path`, `frontmatter`, `body_start_line`, `source`) and `Problem`s, and
parses frontmatter only through `frontmatter::parse`. Consumers should treat `Problem`s as "input
unusable" and keep policy findings separate. `Frontmatter::metadata` is a
`serde_json::Map<String, Value>`, so no Claude field is hard-coded in a schema.

## `agenvil check`

Runs the [rules](docs/rules.md) over the inventory and prints findings (`id`, `severity`, `path`,
`line`, `column`, `message`, `fix`) in a deterministic order. `--format json` prints
`{"schema":1,"findings":[…],"summary":{"errors":0,"warnings":0}}`; `github` prints workflow
annotations. Input problems (malformed frontmatter, symlinks, size limits, a missing `POLICY.md`)
go to stderr and exit `4`: they are never reported as policy violations or as a pass. Rules that
are not implemented yet are listed on stderr as "not checked".

`--fast` is offline: no subprocess, no network, no writes, and no script from the tree is ever
executed. It excludes SK002 and the (deferred) CF003, which needs git. `check` never modifies the
project or HOME.

## `agenvil.toml` schema (version 1)

See [`examples/agenvil.toml`](examples/agenvil.toml); it is parsed by a unit test.

| Key | Type | Default | Notes |
| --- | --- | --- | --- |
| `schema` | integer | **required** | must be `1`; anything else is a configuration error |
| `[project] name` | string | **required** | |
| `[paths] agents` | path | `"agents"` | `agents/*.md` |
| `[paths] skills` | path | `"skills"` | `skills/*/SKILL.md` |
| `[paths] rules` | path | `"rules"` | `rules/*.md` |
| `[paths] policy` | path | `"POLICY.md"` | file with the model table (PO001) |
| `[paths] defaults` | path | `"config/defaults.toml"` | validated by CF001 when present |
| `[paths] bin` | path | `"bin"` | scripts checked by HY002 when present |
| `[targets.claude] enabled` | bool | `true` | the only target in v0.1 |
| `[policy.models.claude] models` | string list | `haiku`, `sonnet`, `opus` | AG002 |
| `[policy.models.claude] efforts` | string list | `low`, `medium`, `high`, `xhigh` | AG003 |
| `[policy.models.claude] pairs` | list of `[model, effort]` | the kit's five pairs | AG004 |
| `[rules] <ID>` | `"error"`, `"warning"`, `"off"` | rule default | severity override |
| `[[ignore]] rule, path, reason` | strings | none | suppress one rule in one file; `reason` required |
| `[hygiene] personal_patterns` | regex list | `/Users/`, `~/\.agents/` | HY001 (Rust regex, linear time) |

- Unknown keys are rejected everywhere, so typos and not-yet-supported options fail loudly.
- Paths are relative to the project root, non-empty, use `/` as separator and cannot start with
  `/` or contain `..`, `\` or `:`. The same rules apply on every OS, so Windows drive and UNC paths
  are rejected everywhere. `.` and empty segments are dropped (`./agents//` is `agents`). The check
  is lexical; discovery separately refuses to follow symlinks.
- **Version policy:** `schema` is bumped only for incompatible changes. A file with a different
  `schema` is rejected as unsupported before any other key is validated.

## Development

Minimum supported Rust version: **1.89** (Rust 2024 edition; declared as `rust-version`). It was
1.85 before `inventory`: the `ignore` and `serde-saphyr` crates require 1.88 and 1.89.

### CI

[`.github/workflows/ci.yml`](.github/workflows/ci.yml) runs on every pull request to `main`, on
pushes to `main` and on manual dispatch. It has read-only permissions, uses no secrets and pins
third-party actions to commit SHAs. Pull requests only restore the Cargo cache; `main` saves it.

| Job | Runs on | Commands |
| --- | --- | --- |
| `test` | Ubuntu, macOS, Windows (stable Rust) | `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked --all-targets`, `cargo build --locked` |
| `msrv` | Ubuntu, Rust from `rust-version` in `Cargo.toml` | `cargo build --locked`, `cargo test --locked --all-targets` |

Run the same checks locally (`--locked` fails if `Cargo.lock` is out of date). Tests use temporary
directories and never read or write your real HOME.

```sh
cargo fmt --all -- --check
cargo clippy --locked --all-targets -- -D warnings
cargo test --locked --all-targets
cargo build --locked
cargo +1.89 test --locked --all-targets   # MSRV; needs `rustup toolchain install 1.89`
```
