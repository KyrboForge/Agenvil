# `agenvil sync --target claude`

Sync reconciles Claude Code's user-level agents, skills and rules from an approved staging tree
and the local private layer. It is the low-level, offline reconcile command and does no Git work.
Future `init` and `update` commands are the user-facing onboarding and update path that will
consume this reconcile step. Doctor, uninstall, restore, `settings.json` merging and a Codex
adapter are future work.

## Running it

```sh
agenvil build --target claude
agenvil sync --target claude --dry-run
agenvil sync --target claude --apply
```

[Build](build.md) produces `.generated/claude/` under the project root. Sync reads that tree, or
the one selected by `--out DIR`, and checks that it exactly matches the current build inputs.
Any drift or unsafe staging entry stops sync. `--root DIR` and `--config FILE` select the project
and config as in `build`. A staged root `CLAUDE.md` is [project-scoped](instructions.md) and is
excluded from the plan and global installation.

The Claude config root is the non-empty `CLAUDE_CONFIG_DIR` when set, otherwise `.claude` in
the runtime user's home directory. A root symlink must resolve to an existing directory. Sync requires
plain resource directories below that root. The Claude config root and `~/.agents.local/`
must be disjoint, including filesystem aliases. State lives under `~/.agents.local/state/`.

## Ownership and symlinks

Agenvil owns a destination only when **the ledger and filesystem agree**. The ledger must record
the destination and the filesystem entry must be a symlink whose raw target text exactly matches
the recorded target. A pending update can also match its recorded previous target. An occupied pending
create is ambiguous, even when its target matches the journal. Filename
matches, matching file contents and equivalent resolved paths do not establish ownership. Unowned
files, directories and links always survive.

Each agent or rule gets its own file symlink under `agents/` or `rules/`. Each skill gets its own
directory symlink under `skills/`, including its supporting files. Sync never replaces any of the
three parent directories.

Team links point to absolute paths under `~/.agents.local/state/snapshots/<resource_sha256>/`.
Sync publishes each snapshot from verified bytes and reuses it for the same resource digest. The
digest covers the resource kind and name, file paths, contents and executable bits. Issue #15
requires this boundary because a link into the mutable checkout could expose changes after
validation. Rebuilding or moving the checkout leaves installed snapshot links intact.

Agenvil treats published snapshots as immutable. Same-user processes can still modify them, so
planning re-verifies digests and reports corruption as a conflict. On Unix, group- or
other-writable snapshot directories also cause a conflict. Superseded snapshots are retained.
Pruning is future work.

## Private layer

Sync discovers `~/.agents.local/agents/*.md`, `~/.agents.local/skills/*/SKILL.md` and
`~/.agents.local/rules/*.md` by inspecting filesystem metadata. It never reads or modifies the
private resource contents. The `state/` sibling holds Agenvil's ledger and snapshots.

Every valid private resource is eligible for linking directly to its private path. A private
resource wins over a team resource of the same kind and name. Existing unowned Claude entries
still block installation. The ledger tracks the installed link as `layer: private`. Removing
the private source makes sync switch its managed link to the team snapshot if one exists, or
remove the link otherwise. The plan lists unsafe names and entries of the wrong type in `ignored`.

## Dry run and apply

The default is a **dry run**. You can pass `--dry-run` explicitly. It conflicts with `--apply`.
A dry run verifies staging, reads the ledger, inspects destinations and snapshots, and prints
the plan without creating directories, taking a lock or writing files. Sync never executes
staged skill files.

`--format json` prints the dry-run plan with `schema` (currently `1`), `harness`, `scope`, `root`,
`kit`, `operations`, `summary` (operation counts), `snapshots` and `ignored`. Operations are sorted
by kind and name. Each has `operation`, `kind`, `name`, `layer`, `destination`, `source`,
`link_target`, `ownership`, `observed` and `reason`. The operation is `create`, `update`, `remove`,
`noop` or `conflict`. Ownership is `managed`, `unmanaged`, `absent` or `mismatch`.

`--apply` refuses the entire plan if it has any conflict. On a supported platform it takes the
state lock, checks that the ledger and observed destinations still match the plan, and publishes
any missing snapshots from verified bytes. It records pending intents before changing links.
Apply creates links without overwriting existing entries. It journals each retirement name before
moving the destination and verifies the moved link before proceeding.
A successful run clears the pending intents and records the applied build provenance.

## Ownership ledger

`~/.agents.local/state/ownership.json` uses a versioned envelope with a separate namespace for
each harness. A missing file means an empty ledger. Invalid records, duplicate resources and
unsupported schema versions stop sync. Agenvil preserves the raw JSON of other harness namespaces and Claude scopes.

```json
{
  "schema": 1,
  "generator": { "name": "agenvil", "version": "0.1.0" },
  "harnesses": {
    "claude": {
      "user": {
        "root": "/home/lee/.claude",
        "applied": {
          "kit": "my-agents",
          "manifest_sha256": "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
          "generator": { "name": "agenvil", "version": "0.1.0" }
        },
        "entries": [
          {
            "kind": "agent",
            "name": "reviewer",
            "destination": "agents/reviewer.md",
            "link_target": "/home/lee/.agents.local/agents/reviewer.md",
            "layer": "private",
            "kit": null,
            "resource_sha256": null,
            "pending": null
          }
        ]
      }
    }
  }
}
```

- `root` binds ownership to one resolved Claude config directory. Each `destination` is relative
  to that root.
- `applied` records the kit (`[project].name`), manifest digest and generator of the last
  published commit. It is `null` until the first commit. Sync refuses a different kit.
- A team entry records its kit and resource digest. A private entry tracks the link, so both
  fields are `null` in the example.
- `pending` is `null` for a settled link. A journaled change records an `operation` (`create`,
  `update` or `remove`), a `previous_target` (a path or `null`) and optional `quarantines`
  with retirement names and expected link information. Ledger publication uses a lock and
  atomic exchange or no-replace rename. Sync verifies the displaced generation against the
  ledger digest and attempts to restore a mismatch.

## Conflicts

| Reason | What to do |
| --- | --- |
| A destination is occupied without a ledger entry | Keep the existing entry and rename the incoming resource in its source, or move the existing entry somewhere safe if you want Agenvil to install at that path. Rebuild if the team source changed. |
| A recorded link was redirected or replaced | Review the occupant and, if you want sync to manage the destination again, move the occupant somewhere safe. Agenvil treats it as user-owned. |
| An unsafe root or resource parent blocks access | Check `CLAUDE_CONFIG_DIR` and the reported path. Resolve the obstruction while preserving existing contents, then run the dry run again. |
| A snapshot is corrupt or missing and cannot be restored from verified bytes | Check the reported snapshot path. Verify that the expected kit is staged and rebuild if needed. If the conflict persists, preserve the snapshot and ledger for investigation. |
| The resolved Claude root differs from the ledger's root | Run sync against the recorded Claude config root. Ownership does not carry over to a different root. |
| A pending destination matches neither recorded target and is still occupied | Review the apply report and preserve the occupant. If you intend to resume at that destination, move the occupant somewhere safe before retrying. |
| An occupied pending create | Move the occupant somewhere safe before retrying. A matching target does not establish ownership. |
| Two destination spellings resolve to the same entry | Resolve the case-only collision before applying. The existing link stays in place. |
| The requested kit differs from the ledger's kit | Run sync with the recorded kit. |
| A journaled retirement name holds an unexpected object | Preserve the object at the path reported by sync. Move it somewhere safe before retrying and review the original destination. |

A broken managed link still has its recorded target text. Apply can restore a missing snapshot
when it has the verified bytes. If the recorded link itself is missing, sync plans to recreate it
when still desired or drop its ledger entry when obsolete. For an invalid or busy ledger, follow
the diagnostic. Sync never auto-repairs an invalid ledger.

## Recovery after an interrupted or failed apply

Apply attempts to roll back completed link changes in reverse order and restore the prior
ledger. An interruption or failed rollback can leave pending entries. Sync checks the journaled
retirement paths on retry and reports the location of unexpected objects for manual recovery.

If the final ledger was published but syncing its directory and restoring the journal both fail,
apply reports `commit durability unconfirmed` and the visible ledger state. The same outcome
covers a publication sync cannot verify or restore. Sync leaves the links in place.
A visible `Committed` ledger already records the new provenance. If it is still visible
on retry, a dry run reports all operations as `noop` and apply has nothing to change. A visible
`Pending` ledger requires normal journal recovery. `Unchanged` means the original ledger is
visible. `Unknown` means sync could not identify the visible generation.

1. Read the apply report if one was emitted. It identifies operations completed, failed,
   skipped or rolled back and includes any recovery error.
2. Run `agenvil sync --target claude --dry-run` with the same root and staging options.
   Sync can reconcile vacant destinations and links matching pending update or remove records.
   Resolve any conflicts before retrying.
3. Re-run `agenvil sync --target claude --apply` with the same options. Leave the journal in place
   so sync can recover it.

Retirement leaves hidden `.tmp-agenvil-retired-link-*` entries beside Claude resources. Ledger
publication can leave `.tmp-agenvil-ledger-*` generations in state. Sync keeps these entries because
the filesystem offers no identity-conditional unlink. Temporary snapshot directories also stay
inert. An interruption between a ledger exchange and verification can leave the candidate at
`ownership.json` and the displaced generation under its temporary name. Preserve both if that
interruption is suspected, and preserve any temporary entry named in an error until recovery is
complete.

## Supported platforms

| Platform | Support |
| --- | --- |
| macOS arm64 and x64 | Planning and apply with native symlinks. |
| Linux x86_64 and aarch64 | Planning and apply with native symlinks. |
| Windows | Compiles and supports read-only planning. Apply probes symlink capability and fails closed with an explicit error even if the probe succeeds. There is no copy fallback. |

Apply also fails closed if the filesystem lacks the required atomic exchange or no-replace rename.
