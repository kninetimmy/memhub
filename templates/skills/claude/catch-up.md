---
name: catch-up
description: Pull this repo's memhub DB from the Google Drive synced folder, compare it to local, and (with your approval) adopt the newer state so this machine has memory from sessions on your other machines.
framework: memhub
framework_version: 1.0.0
last_updated: 2026-05-22
---

Bring this machine's memhub memory up to date with what you did on
another machine. The transport is an **OS-level synced folder** —
Google Drive for Desktop (macOS/Windows) or an rclone mount (Linux) —
that mirrors a Drive folder to a local path. memhub stays fully
offline: it reads and writes that local path, and Google's app syncs
the bytes in the background. No network calls, no base64. Run this at
the **start** of a session on a repo you sync across machines; the
matching push is the tail of `/wrap-up`.

Prefer the **`memhub.sync_*` MCP tools** throughout — they default the
Drive folder to the canonical `<drive_subpath>/memhub/<project_id>`, so
you never construct that path by hand. The CLI is the fallback when the
MCP server isn't connected (see the end). See
`docs/reference/memhub-prd-addendum-m10-drive-sync.md` for the model:
whole-DB snapshot, last-writer-wins, divergence detected and
operator-gated.

## Detection

Call **`memhub.sync_status`**. It resolves enablement and the remote
dir in one shot — no `test -d`, no path math. Stop and tell me when:

- `enabled == false` → "Cross-machine sync isn't enabled for this repo.
  Run `memhub sync enable`, then re-run `/catch-up`."
- `project_id_error` is set (no git remote) → tell me to set
  `[sync] project_id` in `.memhub/config.toml`.
- `remote_dir_error` is set (usually an empty `drive_subpath`) → tell me
  to set `[sync] drive_subpath` to the absolute path of the synced
  Drive folder on this machine (e.g.
  `~/Library/CloudStorage/GoogleDrive-<me>/My Drive/memhub-sync` on
  macOS, `G:\My Drive\memhub-sync` on Windows).

Otherwise `remote_dir` is the resolved snapshot folder; carry on.

## Compare

Call **`memhub.sync_check`** (no args — it targets `remote_dir`). Read
`verdict` and the guard flags:

- **`project_id_mismatch` is set** → STOP. The snapshot in that folder
  belongs to a different project. Do not adopt; tell me what id it
  carries.
- **`schema_blocks_adopt` is true** → STOP. The snapshot is from a
  newer memhub than this machine. Tell me to run `memhub upgrade`
  first, then re-run `/catch-up`.

Then act on `verdict`:

| verdict | meaning | action |
|---|---|---|
| `no-remote` | no snapshot in the folder yet | nothing to catch up — say so. Expected first-run state; your next `/wrap-up` pushes the first. (Or the Drive app is still syncing — give it a moment.) |
| `up-to-date` | local already matches Drive | nothing to do — say so |
| `drive-ahead` | another machine pushed newer state | recommend adopting; this is a safe fast-forward |
| `local-ahead` | this machine is ahead of Drive | nothing to pull; `/wrap-up` will push |
| `diverged` | **both** sides changed since last sync | requires an explicit decision — see below |

## Adopt (gated)

Only adopt on `drive-ahead` or `diverged`, and only after I confirm.
**`memhub.sync_adopt` is gated by `confirm`** — calling it *without*
`confirm: true` returns the would-change verdict and changes nothing,
so you can use it as a dry run. Only pass `confirm: true` after I say
yes.

- **`drive-ahead`**: summarize what's incoming (the `remote_machine_id`
  and `remote_created_at` from the check output) and ask me to confirm.
  On yes, call `memhub.sync_adopt(confirm=true)`.
- **`diverged`**: the lossy case. Tell me plainly that both this machine
  and Drive changed since the last sync, so adopting the Drive copy
  **discards the local-only changes** made here. Show the local vs
  drive logical versions. Require an explicit "yes, overwrite local"
  before calling `memhub.sync_adopt(confirm=true)`. If I'd rather keep
  local, do nothing — `/wrap-up` will push local up and Drive becomes
  the one that's behind.

`adopt` makes a single safety copy of the replaced DB under
`.memhub/backups/sync/` before swapping, and refuses on its own if the
checksum, schema, or project id don't check out — so a half-synced or
wrong file can't corrupt local.

## After adopting

1. Call **`memhub.render`** so the local `PROJECT.md` /
   `PROJECT_LEDGER.md` reflect the adopted state.
2. Briefly tell me what changed (e.g. "pulled 3 newer decisions and a
   session note from <machine>"), then suggest reading
   `.memhub/rendered/PROJECT.md` for the freshly-synced state.

## CLI fallback

When the MCP server isn't connected, drive the same flow from the CLI —
each command defaults to the canonical folder when you omit the path:

```bash
memhub sync status            # enablement + resolved remote dir
memhub sync check             # verdict + guard flags
memhub sync adopt --yes       # the --yes is the confirm gate
memhub render
```

## Notes

- Never run `git` operations or commits. The snapshot carries only
  memhub's local DB, which is gitignored.
- If the synced folder isn't present (Drive app not installed/signed
  in, or still syncing), `sync_check` reports `no-remote` — say so and
  stop; do not fabricate a verdict.
- Manual, one snapshot each way: `/catch-up` pulls at the start,
  `/wrap-up` pushes at the end. There is no background memhub sync —
  only Google's app moving the file you wrote.
