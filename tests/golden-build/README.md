# Build golden fixtures

Each standalone source project in `tests/fixtures/build/<case>/` has a complete expected
staging tree in `tests/golden-build/<case>/`. `tests/build_golden.rs` compares paths and
exact bytes, validates manifest digests, ordering and transforms, and checks reproducible
builds and read-only drift detection. Only the generator version is normalized to `@VERSION@`.

| Case | Coverage |
|---|---|
| `models` | Model/effort mappings, inheritance and unchanged metadata |
| `skills-rules` | Agents, skills, nested supporting files, binary assets and rules |
| `instructions` | Opt-in root `CLAUDE.md`, Unicode and CRLF bytes without a final newline, copy provenance, and one agent, skill and rule |

The instructions source and expected `CLAUDE.md` deliberately contain CRLF line endings;
do not normalize them. `.gitattributes` preserves all fixture and golden bytes.

For intentional output changes, run `AGENVIL_BLESS=1 cargo test --test build_golden`
outside CI and review the complete diff. Never bless merely to silence a failing test.
