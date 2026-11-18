# `agenvil settings --target claude`

`agenvil settings` merges a small set of managed values and hooks into the user-level Claude
settings file, `<claude_root>/settings.json`. The Claude root is `CLAUDE_CONFIG_DIR`, else
`~/.claude`, resolved as for [sync](managed-install.md). It never reads or writes
`settings.local.json`, project settings or enterprise-managed settings, and it enforces nothing
organizational: a user can always edit or remove what it wrote. The command needs no
`agenvil.toml`.

```sh
agenvil settings --target claude --fragment managed.json            # dry run (the default)
agenvil settings --target claude --fragment managed.json --apply
agenvil settings --target claude --remove [--apply]                 # uninstall what is recorded
agenvil settings --target claude --remove --format json             # JSON plan
```

## Fragments

A fragment is a JSON file of at most 8 MiB:

```json
{
  "schema": 1,
  "values": { "/model": "opus", "/permissions/defaultMode": "plan" },
  "hooks": [
    {
      "id": "guard",
      "event": "PreToolUse",
      "group": { "matcher": "Bash", "hooks": [{ "type": "command", "command": "guard.sh" }] }
    }
  ]
}
```

`values` maps RFC 6901 JSON pointers to values. `hooks` adds matcher groups to `hooks.<event>`,
each under a stable `id`. These pointers and ids are the only things Agenvil manages. A fragment is
rejected before anything is read or written if it has unknown fields, duplicate or overlapping
pointers, or duplicate hook ids. It is also rejected if a pointer is at or below `/env` or
`/hooks`, or names a credential helper (`apiKeyHelper`, `awsAuthRefresh`, `awsCredentialExport`).

## Merge rules

Values are compared as parsed JSON, so key order and formatting do not matter.

| Current value | Install | Uninstall (`--remove` or dropped from the fragment) |
| --- | --- | --- |
| absent | add it, recording any parent objects created | drop the record |
| equal to the desired value but not recorded | adopt it: the record keeps the user's value as the previous value | the value stays |
| equal to what Agenvil recorded | change it to the desired value, or leave it if unchanged | restore the previous value, or delete the key and prune now-empty parents Agenvil created |
| anything else | conflict | conflict |

A new hook group is appended to its event array. Agenvil finds its own group again by exact
equality with the recorded group. It updates the group in place, or moves it when the event
changes. A group with the recorded commands but other content is a user modification, and so is a
conflict. If the commands of a new hook already appear in the event array, that is also a
conflict. Unmanaged keys and groups keep their order. No Agenvil metadata is written into
`settings.json`.

`settings.json` is rewritten with two-space indentation and a trailing newline only when its
parsed value changes, so a rewrite can reformat the file. A no-op leaves the file, its mtime and
`managed.json` untouched. Invalid JSON, a non-object root, a symlink or a non-regular file is
refused, and so is a pointer through a non-object (a type-clash conflict). Duplicate keys in
`settings.json` collapse to the last value on rewrite.

## Ownership file

Ownership lives beside the [sync ledger](managed-install.md#ownership-ledger) in
`~/.agents.local/state/managed.json`. It is a versioned envelope with a `harnesses.claude.user`
namespace that is bound to the resolved Claude root, and other namespaces are kept as raw JSON.
For each value it records the pointer, the owned value, the previous value and the created parent
objects. For each hook it records the id, the event, the exact group and its SHA-256. It also
records the digest of the last `settings.json` that Agenvil wrote. `ownership.json` is not touched.

## Dry run and apply

Planning only reads: it creates no directories, locks or files. Each operation reports
`operation` (add, change, remove, noop, conflict), `kind` (value, hook), `target` (a pointer or
hook id), `event` for hooks, `ownership` (managed, unmanaged, absent, mismatch) and `reason`. The
plan also reports `files` (settings.json and managed.json: create, modify or unchanged) and a
`summary`.

`--apply` refuses the whole plan if any operation conflicts. Otherwise it takes the
`ownership.lock` state lock and checks that the roots, the identity and digest of `settings.json`,
and the digest of `managed.json` still match the plan. It then makes these changes:

1. It backs up the current `settings.json` to
   `~/.agents.local/state/backups/claude-settings/<utc-unix-nanoseconds>-<sha8>.json`. The backup
   has mode 0600, is created exclusively and is fsynced. Only the newest 10 files matching that
   name pattern are kept.
2. It records a pending journal in `managed.json`, holding the digests before and after.
3. It publishes `settings.json` from a temporary file in the same directory. A new file uses a
   no-replace rename with mode 0600. An existing file is replaced by an atomic exchange that keeps
   its mode. If the displaced file is not the one that was planned, it is put back and apply fails.
4. It publishes the settled records to `managed.json`.

If step 4 fails, `settings.json` is rolled back, but only while it still holds exactly what
Agenvil wrote. Otherwise it is left alone. If a run is interrupted, the next plan settles the
journal from whichever side `settings.json` matches. If it matches neither side, planning fails
rather than guess.

The library's `restore(locations, backup_name)` puts a named backup back, but only while
`settings.json` is still Agenvil's last write. The current content is backed up first.

Exit codes: 0 on success, 1 for conflicts, a busy lock, or files that changed since planning, and
2 for an invalid fragment or settings file, unsafe paths and other usage errors. Windows plans
read-only; apply fails closed.

## Future work

- Wire fragments from kit sources instead of a file passed on the command line.
- An RTK provenance hook is just another fragment hook (for example `rtk-pretooluse`); no adapter
  exists yet.
- A CLI for `restore`, and uninstall for symlinks managed by sync.
- `restore` leaves `managed.json` as is, so the next install reports the restored file as a user edit.
- Unmanaged numbers beyond 64-bit range or in exponent form are re-encoded when the file is
  rewritten (serde_json without `arbitrary_precision`).
- A managed hook group that gained extra hooks is not recognized as modified, so a reinstall appends
  the managed group again.
