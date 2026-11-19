# Claude managed lifecycle

Agenvil manages Claude Code's user-level agents, skills, rules and selected settings. The Claude
root is `CLAUDE_CONFIG_DIR` when set to a non-empty value, otherwise `~/.claude`. macOS and
Linux support apply. Windows is plan-only.

## Install and reconcile

```sh
agenvil build --target claude
agenvil sync --target claude --dry-run
agenvil sync --target claude --apply
```

Run the same sequence after changing sources. Build stages the kit, and sync verifies that
staging matches the current inputs before reconciling links. Pass the same `--out DIR` to both
commands when using custom staging. Sync also reads local private resources, which override
matching team resources. See [managed install](managed-install.md) for paths and conflict
recovery.

Sync does not touch settings. To install selected values and hook groups, preview and apply a
[settings fragment](managed-settings.md#fragments) separately.

```sh
agenvil settings --target claude --fragment fragment.json
agenvil settings --target claude --fragment fragment.json --apply
```

## Agenvil owns vs the user owns

| Agenvil owns | The user owns |
| --- | --- |
| Individual Claude resource symlinks whose raw target and file identity agree with `state/ownership.json` | Claude config directories, untracked files or links, and replacements for recorded symlinks |
| Published team snapshots under `~/.agents.local/state/snapshots/` | Kit sources and private resources under `~/.agents.local/` |
| Recorded settings values and hook groups while they still match `state/managed.json` | Other settings and hooks, user edits, and settings at project or enterprise scope |
| Its records, journals and settings backups under `~/.agents.local/state/` | Claude credentials, installed tools, shell and Git configuration |

## Doctor

```sh
agenvil doctor --target claude
agenvil doctor --target claude --format json
```

Doctor can report an invalid project config. It reads local files and searches `PATH` for
prerequisites without executing them. It creates no files or locks and makes no network calls.
Use `--out DIR` if the kit was built into custom staging. Link checks require approved staging.
Settings, journal and prerequisite checks still run when the source config or staging blocks
link checks.

The report uses these statuses in JSON and labels in human output.

| Status | Label |
| --- | --- |
| `healthy` | `ok` |
| `missing` | `MISS` |
| `authentication_required` | `LOGIN` |
| `user_action_required` | `CHECK` |
| `drifted` | `DRIFT` |
| `unsupported` | `OFF` |

| ID | Statuses | Meaning | Remediation |
| --- | --- | --- | --- |
| `source.config` | healthy, missing, user_action_required | Project configuration can be loaded | Run from the intended checkout or specify the correct root or config. Fix any reported config error. |
| `build.staging` | healthy, missing, drifted, user_action_required, unsupported | Claude staging is checked against valid source config | Build again for missing or stale output. Resolve an unsafe path or entry first. |
| `links.ownership` | healthy, drifted, user_action_required, unsupported | Compares link ownership and actual destinations with the desired install | Check the ledger and resolved Claude root. Preview sync from the recorded checkout. |
| `links.missing` | missing | A desired link is absent or a managed link target is broken | Verify the source or snapshot, then preview and apply sync. |
| `links.conflict` | drifted, user_action_required | An occupant, broken ownership or an unsafe path blocks reconciliation | Preserve the reported entry and resolve the [link conflict](managed-install.md#conflicts) before applying. |
| `settings.consistency` | healthy, drifted, user_action_required | Checks for edited or missing records, type clashes and settings read errors | Review the reported edit, missing entry or invalid file. Follow the [merge rules](managed-settings.md#merge-rules) before applying again. |
| `operations.interrupted` | healthy, user_action_required | Checks for pending link intents or a settings journal | Fix reported state errors. For pending changes, preview the interrupted operation and retry with `--apply` once conflicts are resolved. |
| `prereq.claude_cli` | healthy, missing | Optional `claude` executable is discoverable on `PATH` | Install Claude Code or update `PATH` if you need the CLI. |
| `prereq.git` | healthy, missing | Optional `git` executable is discoverable on `PATH` | Install Git or update `PATH` if you need Git operations. |
| `auth.claude` | unsupported | Claude authentication is unchecked | Run `claude` and sign in if prompted. |
| `integration.rtk` | unsupported | The RTK adapter is deferred | Check RTK directly if you use it. |
| `integration.headroom` | unsupported | The Headroom adapter is deferred | Check Headroom directly if you use it. |

Doctor prints the report before returning `0` for healthy or unsupported checks and missing
optional prerequisites, or `1` for a missing required check, drift, required user action or
required authentication. Schema `1` JSON includes an overall `status`, a `summary` of check
counts, and a stable-order `checks` array. Each check has `id`, `status`, `required` and
`summary`, with `detail`, `remediation` and `subject` when applicable. The same inputs
produce the same JSON. The overall status is the highest-priority required finding, ordered
`user_action_required`, `authentication_required`, `drifted`, then required `missing`. With
no findings it is `healthy`. Preview settings changes separately.

A human report for a missing managed rule includes this check.

```text
MISS  links.missing Link or its managed target is missing (rules/example.md)
      managed link was removed
      Review the plan and re-run sync --apply to repair the link.
```

The corresponding object in the JSON report's `checks` array:

```json
{
  "id": "links.missing",
  "status": "missing",
  "summary": "Link or its managed target is missing",
  "required": true,
  "detail": "managed link was removed",
  "remediation": "Review the plan and re-run sync --apply to repair the link.",
  "subject": "rules/example.md"
}
```

## Uninstall

```sh
agenvil uninstall --target claude
agenvil uninstall --target claude --apply
```

Uninstall uses recorded ownership for the resolved Claude root. It can run without the kit
checkout, config or staging tree. The default dry run reports what can be removed without taking
locks or writing files. Use `--format json` for a machine-readable plan or apply report.

- Apply removes verifiably managed symlinks and drops records for absent links. It keeps
  untracked entries, private source files, staging, snapshots, backups and the Claude config
  directories. A redirected or replaced recorded link stays in place and is reported as a
  conflict. Its ledger record remains until the conflict is resolved.
  Once the last Claude user link record is removed, sync can install at a different
  `CLAUDE_CONFIG_DIR`.
- Settings removal follows the [merge rules](managed-settings.md#merge-rules). It restores
  previous values or removes installed values and matching hook groups. Other settings survive.
  Agenvil backs up the previous file before rewriting it. A settings conflict blocks the settings
  phase. Verified links can still be removed, and link conflicts can coexist with settings cleanup.
- Apply processes links first, then settings. A conflict returns exit `1` in either mode. Inspect
  the report and resolve conflicts before retrying. A retry uses the existing journals and resumes
  from the current state. After a clean uninstall, another run has nothing to remove. Leave journals
  and any temporary paths named in errors in place until recovery completes.

## Backups and restore

```sh
agenvil restore --target claude --list
agenvil restore --target claude --backup 01700000000000000000-abcdef01.json
agenvil restore --target claude --backup 01700000000000000000-abcdef01.json --apply
```

Replace the example name with one from `--list`. Listing is read-only and reports matching
regular backup files newest first from `~/.agents.local/state/backups/claude-settings/`.
The filename contains a zero-padded UTC Unix timestamp in nanoseconds and an eight-character
hex digest prefix. The list omits backups whose bytes do not match the digest prefix and
reports the name, timestamp, byte size and full SHA-256 for the remaining backups. Use
`--format json` for structured output. Writes retain the newest ten settings backups.

Restore replaces the whole user-level `settings.json` with a named backup. Use a filename from
the backup directory. The backup must contain a JSON object and match the digest prefix in its
filename. A digest mismatch returns exit `2`. Dry run is the default. The preview
shows the current and backup digests and which managed records would be retained or dropped. Restore requires the live file's digest to match Agenvil's last write,
even if a user edit only changed formatting. Apply rechecks `settings.json` and `managed.json`
against the plan and refuses a detected concurrent change with exit `1`. Agenvil backs up the
current bytes before replacing settings.

Restore retains current value records that match the restored document and hook records whose
group occurs exactly once in its recorded event. It drops other records and records the restored
digest as the last write. This settling of `managed.json` addresses
[issue #61](https://github.com/KyrboForge/Agenvil/issues/61). Restore can bring back settings
content without re-creating old ownership records. To recreate links, build and sync again.

After interruption, preserve the journal and any files named in the error. Run a fresh preview
before retrying apply. The journal can settle if the live file matches either recorded digest.
If neither matches, or the file is no longer Agenvil's last write, preserve your current file and
review the backup before reconciling settings or manually recovering the desired content. See
[settings apply and recovery](managed-settings.md#dry-run-and-apply).

## Lifecycle API

Future consumer `init` and `update` commands can compose `install::lifecycle` using the same
`install::Locations` for planning and apply:

- `sync_plan` and `sync_apply` reconcile the approved staging tree and private layer.
- `settings_plan` and `settings_apply` handle a reviewed settings fragment independently.
- `uninstall_plan` and `uninstall_apply` detach the recorded install.
- `restore_backups`, `restore_plan` and `restore_apply` list and restore settings backups.
- `pending_operations` inspects link intents and the settings journal without locking.
- `doctor::diagnose` uses these APIs for read-only checks with the same locations.

Desired state comes from verified staging and the private layer for links, and a fragment for
settings. Managed state lives in the ownership and settings records. Actual state is the live
filesystem and `settings.json`. Plan against all three and apply the reviewed plan through the
facade so ownership checks, revalidation and recovery stay in the existing engines.

## Deferred to M3

RTK and Headroom adapters, authentication detection, Git update, tool installation, consumer
`init` and `update`, snapshot pruning, and Windows apply.
