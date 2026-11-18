# Agenvil

`agenvil` validates and builds AI agent, skill and rule source trees. One Rust binary, one crate.

> **Status: v0.1, in progress.** Configuration loading, exit codes, `inventory`, `check`, `rules`,
> `explain` and `build --target claude` are implemented (see [`docs/rules.md`](docs/rules.md) for
> which rules). Drift checks (`build --check`), cleanup and managed Claude installation are also available.

## Quickstart

Create `agenvil.toml` alongside `agents/` and `skills/`:

```toml
schema = 1
[project]
name = "my-agents"
```

Add agent and skill files as shown in [the build guide](docs/build.md), then run
`agenvil check` and `agenvil build --target claude`. No TAK files are needed.
See the [standalone configuration example](examples/agenvil.toml) for optional keys.

For existing TAK-style configurations, see [migration guidance](docs/compatibility.md#migrating-from-tak-style-config).

## Commands

| Command | Kind | Purpose |
| --- | --- | --- |
| `agenvil check [--fast] [--format human\|json\|github] [--only IDS] [--skip IDS] [--fail-on error\|warning] [--quiet]` | read-only, offline | validate the source tree |
| `agenvil inventory [--format json]` | read-only, offline | list discovered resources as JSON |
| `agenvil rules` | read-only | list validation rules and their status |
| `agenvil explain <ID>` | read-only | explain one rule: rationale, failing and passing example, fix |
| `agenvil build --target claude --check [--out DIR]` | read-only, offline | compare the staging tree with what the sources generate; never creates it ([`docs/build.md`](docs/build.md#drift-checks-build---check)) |
| `agenvil build --target claude --clean [--apply]` | dry run unless `--apply` | remove stale generated files, only those whose digests still match ([`docs/build.md`](docs/build.md#cleanup---clean)) |
| `agenvil build --target claude [--out DIR]` | writes `.generated/claude/` | build a Claude-native staging tree from the sources; never installs, never touches `~/.claude` ([`docs/build.md`](docs/build.md)) |
| `agenvil sync --target claude [--dry-run\|--apply] [--out DIR] [--format human\|json]` | dry run unless `--apply`, offline | reconcile user-level Claude agents, skills and rules from approved staging and the private layer ([`docs/managed-install.md`](docs/managed-install.md)) |
| `agenvil settings --target claude (--fragment FILE\|--remove) [--dry-run\|--apply] [--format human\|json]` | dry run unless `--apply`, offline | merge managed values and hooks into the user-level `settings.json`, with ownership, backups and conflicts ([`docs/managed-settings.md`](docs/managed-settings.md)) |
| `agenvil doctor --target claude [--out DIR] [--format human\|json]` | read-only, offline | diagnose Claude install, settings and prerequisites ([lifecycle guide](docs/lifecycle.md#doctor)) |
| `agenvil uninstall --target claude [--dry-run\|--apply] [--format human\|json]` | dry run unless `--apply`, offline | detach recorded Claude links and settings ([lifecycle guide](docs/lifecycle.md#uninstall)) |
| `agenvil restore --target claude (--list\|--backup NAME [--dry-run\|--apply]) [--format human\|json]` | read-only unless restoring with `--apply` | list or preview a settings backup, or apply it with `--apply` ([lifecycle guide](docs/lifecycle.md#backups-and-restore)) |

`--root <DIR>` selects the project root; the default is the current directory. The config file is
`<root>/agenvil.toml` unless `--config <FILE>` overrides it (no search of parent directories). It must be a regular file of at
most 1 MiB; a symlinked `agenvil.toml` is rejected without being read, so the config cannot point
outside the root.

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | success |
| 1 | policy findings at or above `--fail-on` (`check`), sources `build` cannot turn into valid output, drift (`build --check`), or lifecycle findings (see [lifecycle guide](docs/lifecycle.md)) |
| 2 | usage or configuration error, including unknown rule IDs, severities and policy keys, a disabled target, an unsafe `--out` or an unsafe generated tree (symlink, escaping manifest path) |
| 3 | internal error (including panics) |
| 4 | unable to verify: an explicitly selected rule that is not implemented yet, or an inventory that is incomplete because of [problems](#problems); wins over `1` |

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

### For the validator and the builder

`inventory::Inventory::build(root, &config, &Limits)` is the shared entry point: it returns
`Resource`s (`name`, `path`, `frontmatter`, `body_start_line`, `source`) and `Problem`s, and
parses frontmatter only through `frontmatter::parse`. Consumers should treat `Problem`s as "input
unusable" and keep policy findings separate. `Frontmatter::metadata` is a
`serde_json::Map<String, Value>`, so no Claude field is hard-coded in a schema.

## `agenvil build`

`agenvil build --target claude` turns an `agenvil.toml`, `agents/` and `skills/` into a Claude Code
staging tree in `.generated/claude/`. The output is **staging only**: nothing is installed, and
`~/.claude` is never written. Use `agenvil sync --target claude` to plan a managed install. See
[`docs/build.md`](docs/build.md) for a minimal project, skill references, model/effort mapping,
the manifest and the safety rules.

### Project instructions

Add a `[paths] instructions` entry to include portable project-wide instructions in the build.
The Claude target copies the file byte for byte to `CLAUDE.md` at the root of the staging tree:

```
my-agents/
  agenvil.toml
  instructions.md
  agents/reviewer.md
  skills/tdd/SKILL.md
```

```toml
schema = 1

[project]
name = "my-agents"

[paths]
instructions = "instructions.md"

[targets.claude]
enabled = true
```

Resulting tree: `.generated/claude/{manifest.json, CLAUDE.md, .claude/{agents,skills,rules}}`.

```sh
agenvil check                                    # validate agents, skills, rules and configured instructions
agenvil build --target claude                    # build the staging tree
agenvil build --target claude --check            # compare staging tree with sources (read-only)
```

Both `agenvil check` and `agenvil build` validate the configured instructions source.

See [`docs/build.md#project-instructions-claudemd`](docs/build.md#project-instructions-claudemd)
for source validation rules, manifest format, `--check`/`--clean` behaviour, and the distinction
between staging and installing. See [`docs/instructions.md`](docs/instructions.md) for the
portability model and future target-specific extension points.

## `agenvil sync`

`agenvil sync --target claude` checks the staging tree and previews which user-level Claude agents,
skills and rules it would link, update or remove. Pass `--dry-run` to preview explicitly or
`--apply` to install the plan. Sync tracks
individual links in an ownership ledger and preserves user-owned entries. Installed links belong
to the checkout that applied them; sync from another checkout is refused even under the same
`[project].name`. Sync does not touch settings. See [managed install](docs/managed-install.md)
for paths, private overrides and recovery, and the [Claude lifecycle guide](docs/lifecycle.md)
for doctor, uninstall and settings restore.

## `agenvil check`

Runs the [rules](docs/rules.md) over the inventory and prints findings (`id`, `severity`, `path`,
`line`, `column`, `message`, `fix`) in a deterministic order. `--format json` prints
`{"schema":1,"findings":[…],"summary":{"errors":0,"warnings":0}}`; `github` prints workflow
annotations. Incomplete input (including an unreadable file, or a missing configured policy
file when agents exist) goes to stderr and exits `4`. The default check runs implemented,
applicable rules and notes deferred rules on stderr; they are not counted as passed. Explicitly
selecting a deferred rule with `--only` or a non-`off` `[rules]` entry exits `4`.
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
| `[paths] instructions` | path | unset | portable project instructions; Claude target copies to `CLAUDE.md` |
| `[paths] policy` | path | unset | enables PO001 |
| `[paths] defaults` | path | unset | enables CF001 |
| `[paths] bin` | path | unset | enables HY002 |
| `[targets.claude] enabled` | boolean | `true` | Claude target |
| `[policy.models.claude] models, efforts, pairs` | lists | unset | optional authoritative allowlists; see [model policy](docs/model-policy.md) |
| `[[targets.claude.map]] source, target` | pairs | none | explicit target mapping |
| `[rules] <ID>` | `"error"`, `"warning"`, `"off"` | rule default | severity override |
| `[[ignore]] rule, path, reason` | strings | none | suppress one rule in one file; reason required |
| `[hygiene] personal_patterns` | regex list | unset | enables HY001 when non-empty |
| `[compat] profile` | `"tak"` | unset | opt in to [TAK defaults](docs/compatibility.md#migrating-from-tak-style-config) |

- The model policy is validated when the file loads (duplicates, pairs outside `models`/`efforts`,
  unsupported mapping targets): exit `2`.
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
| `test` | Ubuntu, macOS, Windows (stable Rust) | `cargo fmt --all -- --check`, `cargo clippy --locked --all-targets -- -D warnings`, `cargo test --locked --all-targets` (includes the golden, fixture compatibility and build end-to-end suites, also run as named steps), `cargo build --locked` |
| `audit` | Ubuntu | `cargo audit --deny warnings` (cargo-audit 0.22.2, public RustSec database) |
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

### Build goldens and the drift flow

`tests/fixtures/build/<case>/` are synthetic standalone projects (no TAK, no private data) and
`tests/golden-build/<case>/` the exact staging trees they must produce: paths, bytes, manifest
(the generator version is stored as `@VERSION@`), sha256, order and transforms. `tests/build_golden.rs`
runs the CI flow on every OS: build into a temp dir, validate the generated tree, compare with the
golden, `build --check` passes, mutate a generated file, `build --check` fails, and the tree is
unchanged. `tests/stage.rs` holds the adversarial cases (forged manifests, symlinks, foreign
files, cleanup safety, scan limits).

```sh
cargo test --test build_golden --test stage
AGENVIL_BLESS=1 cargo test --test build_golden goldens_match   # intentional update, then review the diff
```

Blessing is refused when `CI` is set and never happens implicitly; `--check` never rewrites a
golden.

### Testing and compatibility

Validation behaviour is pinned by fixtures: `tests/fixtures/validation/<category>/<name>/` holds a
`project/` tree and an `expect.toml` (exit code, every finding with its location, stderr
expectations). `tests/golden/` holds byte-exact snapshots of `check --format json|github|human`
and the stderr summary, and `tests/golden/coverage.md` reports fixture and rule coverage.

```sh
cargo test --test compatibility   # fixtures, rule matrix and coverage report checks
cargo test --test golden          # committed output snapshots
AGENVIL_BLESS=1 cargo test --test golden --test compatibility   # rewrite snapshots, then review the diff
```

Blessing is refused when `CI` is set, so a changed snapshot always arrives as a reviewed diff.
Fixtures run in a copy of the tree with an empty HOME, and the tests assert that nothing was
written, no repository script ran and the real HOME was never read.

**These baselines are Agenvil's own, not the legacy shell validator's.** The shell was not
available, so [`docs/compatibility.md`](docs/compatibility.md) lists every rule with its parity
status (none `verified`), the known differences awaiting approval and the proposed follow-ups.
A green run means the implemented rules behave as the fixtures say; 19 of 30 rules are still
deferred. Default `check` notes that they were not run; explicitly selecting one exits `4`.
