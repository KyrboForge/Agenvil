# Agenvil

`agenvil` validates and builds AI agent, skill and rule source trees. One Rust binary, one crate.

> **Status: v0.1 scaffold.** The CLI, configuration loading and exit codes exist. No command is
> implemented yet: every subcommand loads `agenvil.toml` and then exits `4` ("unable to verify")
> instead of reporting success.

## Commands

| Command | Kind | Purpose |
| --- | --- | --- |
| `agenvil check` | read-only, offline | validate the source tree |
| `agenvil inventory` | read-only, offline | list discovered resources |
| `agenvil rules` | read-only | list validation rules |
| `agenvil explain <ID>` | read-only | explain one finding ID |
| `agenvil build` | mutating | build staged output (never `~/.claude`) |

`--root <DIR>` selects the project root; the default is the current directory. The config file is
always `<root>/agenvil.toml` (no search of parent directories).

## Exit codes

| Code | Meaning |
| --- | --- |
| 0 | success |
| 1 | policy findings |
| 2 | usage or configuration error |
| 3 | internal error (including panics) |
| 4 | unable to verify (for example, a command that is not implemented yet) |

## `agenvil.toml` schema (version 1)

See [`examples/agenvil.toml`](examples/agenvil.toml); it is parsed by a unit test.

| Key | Type | Default | Notes |
| --- | --- | --- | --- |
| `schema` | integer | **required** | must be `1`; anything else is a configuration error |
| `[project] name` | string | **required** | |
| `[paths] agents` | path | `"agents"` | `agents/*.md` |
| `[paths] skills` | path | `"skills"` | `skills/*/SKILL.md` |
| `[paths] rules` | path | `"rules"` | `rules/*.md` |
| `[targets.claude] enabled` | bool | `true` | the only target in v0.1 |
| `[policy.models.claude]` | table | empty | accepted but has no keys yet |

- Unknown keys are rejected everywhere, so typos and not-yet-supported options fail loudly.
- Paths are relative to the project root, non-empty, and cannot be absolute or contain `..`.
  This check is lexical; symlinks are not resolved.
- **Version policy:** `schema` is bumped only for incompatible changes. A file with a different
  `schema` is rejected as unsupported before any other key is validated.

## Development

Minimum supported Rust version: **1.85** (Rust 2024 edition; declared as `rust-version`).

```sh
cargo fmt --check
cargo clippy --all-targets -- -D warnings
cargo test
```
