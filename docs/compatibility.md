# Compatibility with the Team Agents Kit validator

This page records how far `agenvil check` matches the legacy Team Agents Kit shell verifier
("the shell"), rule by rule. It is the only place that claims parity, and it claims very little on
purpose.

> **Green CI does not mean full compatibility.** 19 of the 30 registry rules are deferred and make
> `check` exit `4`. The 11 implemented rules are tested against Agenvil's own fixtures; **no legacy
> shell output has been captured**, so none of them is `verified`.

## Legacy oracle status

**Unavailable.** The shell verifier and its fixtures live in the private Team Agents Kit, which public
CI must not access. When this was written the oracle was not found in this repository, in the
`KyrboForge` organisation, or on the author's machine. What Agenvil knows about the shell comes
from the rule table transcribed into [`docs/rules.md`](rules.md) (#7) and the decisions in ADR #21.
`kitcheck-requirements.md`, the compatibility reference named in ADR #21, is not part of this
repository either.

Consequences:

- Every golden under `tests/golden/` and every fixture under `tests/fixtures/validation/` is an
  **Agenvil regression baseline**. None is shell output, and none is attributed to the shell.
- No shell behaviour is reconstructed from issue text. Where the shell is silent, the matrix says
  `unspecified`.
- Nothing from the private kit is copied here; fixtures are synthetic.

### Evidence levels

| Level | Meaning |
| --- | --- |
| shell-verified | Shell was run on a committed fixture and its output and exit code are committed under `tests/oracle/`. **None today.** |
| doc | Taken from the kit rule table as transcribed in `docs/rules.md`; not run against the shell. |
| ADR #21 | Deliberately changed by an accepted architecture decision. |
| unverifiable | Not specified anywhere Agenvil can read. |

### Parity vocabulary

`verified` (matches the shell, oracle output committed; the test suite refuses this value while
`tests/oracle/` does not exist) · `approved deviation` (differs, and a maintainer approved it) ·
`deferred` (rule not implemented; selecting it exits `4`) · `unspecified` (no authoritative
behaviour) · `blocked` (implemented, parity cannot be checked until the oracle is available).

### Capturing oracle results later (F1)

Someone with access to the private kit runs the shell over the committed `project/` trees, stores
stdout, stderr and the exit code under `tests/oracle/<category>/<name>/`, and flips each rule to
`verified` or records the difference as a deviation. Public CI then compares Agenvil's output with
those committed files; it never needs the kit or credentials. The live comparison is a manual step on
a machine that has the kit, not a CI job.

## Rule matrix

Columns: stable ID · description (default severity) · legacy behaviour and evidence · Agenvil
implementation · expected exit code · `--fast` · fixture coverage (`failing` = a fixture expects the
rule to fire; the goldens and per-fixture detail are in
[`tests/golden/coverage.md`](../tests/golden/coverage.md)) · parity · follow-up. A test keeps this
table in step with `agenvil rules` and the fixtures.

| ID | Description | Legacy behaviour (evidence) | Implementation | Exit | Fast | Fixtures | Parity | Follow-up |
|---|---|---|---|---|---|---|---|---|
| AG001 | An agent has `name`, `description`, `model`, `effort`, `tools`; `name` equals the file name (error) | doc: kit rule table (via `docs/rules.md`) | implemented | 1 on a finding; 4 on an input problem | yes | failing | blocked | F1 |
| AG002 | `model` is an allowed model (error) | doc: kit rule table; allowed set is config here (known difference 6) | implemented | 1 on a finding; 4 on an input problem | yes | failing | blocked | F1 |
| AG003 | `effort` is an allowed effort (error) | doc: kit rule table; allowed set is config here (known difference 6) | implemented | 1 on a finding; 4 on an input problem | yes | failing | blocked | F1 |
| AG004 | `(model, effort)` is an allowed pair (error) | doc: kit pair table; pairs are config here (known difference 6) | implemented | 1 on a finding; 4 on an input problem | yes | failing | blocked | F1 |
| AG005 | `tools` is an explicit, non-empty list (error) | doc: kit rule table; valid tool names unspecified | implemented | 1 on a finding; 4 on an input problem | yes | failing | blocked | F1 |
| AG006 | Every skill in an agent's `skills:` exists (error) | doc: kit rule table | deferred | 4 when selected (fails closed) | yes | none | deferred | F2 |
| AG007 | A `skills:` entry is core or agent tier, never on-demand (error) | doc: kit rule table | deferred | 4 when selected (fails closed) | yes | none | deferred | F2 |
| CF001 | `config/defaults.toml` is valid TOML with known keys and valid values (error) | doc: kit key list; malformed TOML as a finding is an Agenvil choice | implemented | 1 on a finding; 4 on an input problem | yes | failing | blocked | F1 |
| CF002 | No secret-like keys in config (error) | doc: kit rule table; key patterns unspecified | deferred | 4 when selected (fails closed) | yes | none | deferred | F3 |
| CF003 | A user `config.toml` is not in the repo tree or tracked by git (error) | doc: kit lists it as fast; needs git (known difference 3) | deferred | 4 when selected (fails closed) | no | none | deferred | F3 |
| CF004 | `connectors.toml` and `third-party.toml` carry no `default` key (error) | doc: kit rule table | deferred | 4 when selected (fails closed) | yes | none | deferred | F3 |
| CN001 | Each connector has the required fields (error) | doc: kit rule table; field list unspecified | deferred | 4 when selected (fails closed) | yes | none | deferred | F3 |
| FM001 | Frontmatter parses as YAML (error) | doc: kit rule table; Agenvil reports it as an exit-4 input problem (known difference 4) | deferred | 4 when selected (fails closed) | yes | none | deferred | F7 |
| HY001 | No personal paths or emails in agents, skills and rules (error) | doc: kit rule table; patterns are config here | implemented | 1 on a finding; 4 on an input problem | yes | failing | blocked | F1 |
| HY002 | Files in `bin/` are executable and start with a shebang (error) | doc: kit rule table; exec bit checked on Unix only | implemented | 1 on a finding; 4 on an input problem | yes | failing | blocked | F1 |
| HY003 | Symlinks in the repo (warning) | doc: kit rule table; Agenvil treats every symlink as an input problem today | deferred | 4 when selected (fails closed) | yes | none | deferred | F6 |
| HY005 | CRLF line endings or a BOM in frontmatter files (warning) | doc: kit rule table; Agenvil accepts both today (fixture `adversarial/bom-and-crlf`) | deferred | 4 when selected (fails closed) | yes | none | deferred | F6 |
| PO001 | The model table in the policy file matches the agents, both ways (error) | doc: kit rule table; marker format in known difference 2 | implemented | 1 on a finding; 4 on an input problem | yes | failing | blocked | F1 |
| SK001 | A skill has `SKILL.md`, name equals the folder, description not empty (error) | doc: kit rule table | implemented | 1 on a finding; 4 on an input problem | yes | failing | blocked | F1 |
| SK002 | A skill sets `model` and `effort` (or `inherit`) (warning) | doc: kit rule table; tier exemptions deferred with SK003 | implemented | 1 on a finding; 4 on an input problem | no | failing | blocked | F1 |
| SK003 | Every skill has a tier and a pack (error) | doc: kit rule table | deferred | 4 when selected (fails closed) | yes | none | deferred | F2 |
| SK004 | No two enabled packs provide the same skill name (error) | doc: kit rule table | deferred | 4 when selected (fails closed) | yes | none | deferred | F2 |
| SK005 | The core tier's description size stays under the budget (error) | doc: kit rule table; budget value unspecified | deferred | 4 when selected (fails closed) | yes | none | deferred | F2 |
| SK006 | Skill name and description follow the Agent Skills spec limits (error) | doc: kit rule table; Agent Skills spec | deferred | 4 when selected (fails closed) | yes | none | deferred | F2 |
| ST001 | `team.d` fragments are valid JSON with only known keys (error) | doc: kit rule table | deferred | 4 when selected (fails closed) | yes | none | deferred | F4 |
| ST002 | Hook commands in `team.d` fragments exist in the repo (error) | doc: kit rule table | deferred | 4 when selected (fails closed) | yes | none | deferred | F4 |
| TL001 | Each Brewfile equals the brew subset of `tools.toml` (error) | doc: kit rule table; tools are a later milestone | deferred | 4 when selected (fails closed) | yes | none | deferred | F5 |
| TL002 | Core and company tools have an install method or manual steps (error) | doc: kit rule table; tools are a later milestone | deferred | 4 when selected (fails closed) | yes | none | deferred | F5 |
| TP001 | Third-party entries are pinned and reviewed (error) | doc: kit rule table; packages are a later milestone | deferred | 4 when selected (fails closed) | yes | none | deferred | F5 |
| WF001 | Workflow scripts parse and set a model per stage (error) | doc: kit rule table | deferred | 4 when selected (fails closed) | yes | none | deferred | F5 |

## Known differences

Each is **proposed**, not approved. Approval is a maintainer decision recorded on the follow-up
issue; until then the parity column stays `blocked` or `deferred` rather than `approved deviation`.

| # | Difference | Rationale | Approval |
| --- | --- | --- | --- |
| 1 | `agenvil.toml` replaces the earlier `kitcheck.toml`. | ADR #21 split configuration: `agenvil.toml` is the product schema; the kit's file was a compatibility reference. | proposed (ADR #21 already says the assumption changed) |
| 2 | PO001 reads and regenerates its model table between the `<!-- kit:model-table -->` / `<!-- /kit:model-table -->` markers; markers for other generated Markdown are not defined yet. | The markers keep existing kit `POLICY.md` files readable. Generated-file markers belong to the build issues (#9-#11); #11's generated-tree validation does not apply PO001 or other source-only rules to output. The shell's exact marker handling is unverified. | proposed; needs F1 |
| 3 | CF003 is excluded from `check --fast`. | It needs `git`, and `--fast` must be offline and spawn no process (ADR #21 decision 6). The kit lists CF003 as fast. The rule is also deferred. | proposed (ADR #21 allows a full-check-only rule if documented) |
| 4 | Malformed frontmatter is an exit-4 input problem, not a policy finding (no FM001 finding). | Keeps "could not read" distinct from "violates policy", so a parse failure is never a pass or a false violation. | proposed |
| 5 | Models and efforts are native Claude values, checked against an allowed-pair policy; no `cheap`/`strong` tiers. | ADR #21 decision 4. | proposed (decided in ADR #21) |
| 6 | The allowed models, efforts and pairs (AG002-AG004) and HY001 patterns come from `agenvil.toml`; the defaults match the kit's table. | Policy belongs to the project; the defaults keep kit behaviour. | proposed |
| 7 | Incomplete coverage: 19 rules are deferred. A default `check` exits `4` until they are implemented or explicitly skipped. | Failing closed beats a green result that skipped rules. | proposed (not a deviation to approve, a gap to close: F2-F7) |
| 8 | HY002 checks the executable bit on Unix only. | Windows has no such bit; CI runs on Windows. | proposed |
| 9 | A symlink is an input problem (exit 4); a BOM or CRLF is accepted. HY003 and HY005 would report these as warnings. | Symlinks are never followed (safety); BOM/CRLF handling follows #6. The shell's behaviour is unverified. | proposed; F6 |
| 10 | The model policy is validated when `agenvil.toml` loads, and `[[targets.claude.map]]` adds explicit overrides; neither needs `POLICY.md`. | #9: a standalone project must not need kit files. PO001 itself is unchanged. | proposed |
| 11 | PO001, AG001, SK002, the default pairs and the example config still follow kit conventions. | Not changed in #9 to avoid silent behaviour changes; corrections proposed in #29. | proposed; #29 |

## Proposed follow-up issues

None of these is created yet.

| # | Issue | Blocked by |
| --- | --- | --- |
| F1 | Capture shell results for the 11 implemented rules into `tests/oracle/` and settle each parity value. | access to the private kit |
| F2 | Implement AG006, AG007, SK003-SK006 (skill registry, tiers, packs, size budget). | tier/pack spec; the budget value is unspecified |
| F3 | Implement CF002, CF003, CF004, CN001 (secrets, git tracking, connectors). | secret-key patterns and connector fields are unspecified |
| F4 | Implement ST001, ST002 (`team.d` fragments). | fragment schema |
| F5 | Implement TL001, TL002, TP001, WF001. | later milestones (tools, packages, workflows) |
| F6 | Decide HY003 and HY005 versus the current input-problem and accepted behaviour. | F1 |
| F7 | Decide whether FM001 stays an input problem or becomes a finding. | F1 |

## How to add or change a fixture

A case is `tests/fixtures/validation/<category>/<name>/` with `project/` (what `check` sees) and
`expect.toml` (exit code, every finding in output order with `id`, `severity`, `path`, `line`,
`column`, stderr substrings, and which goldens to keep). Trees that git or Windows cannot carry
(symlinks, files over 1 MiB, 10 001 entries) are built by name at test time (`setup`). After a
deliberate change run `AGENVIL_BLESS=1 cargo test --test golden --test compatibility`, review the
diff, and update this matrix if a rule's status changed. Blessing is refused in CI.
