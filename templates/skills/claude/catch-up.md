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
offline: it just reads and writes that local path, and Google's app
syncs the bytes in the background. No network calls, no MCP, no
base64. Run this at the **start** of a session on a repo you sync
across machines; the matching push is the tail of `/wrap-up`.

See `docs/reference/memhub-prd-addendum-m10-drive-sync.md` for the
model: whole-DB snapshot, last-writer-wins, divergence is detected and
operator-gated.

## Detection

**Check 1 — `.memhub/` exists.**
`test -d .memhub && echo present || echo absent`
- `absent` → stop. Tell me to run `memhub init` first.

**Check 2 — memhub on PATH.**
`command -v memhub >/dev/null 2>&1 && echo present || echo absent`
- `absent` → stop. Tell me to put `memhub` on PATH.

**Check 3 — sync enabled, and resolve the synced folder.**
Run `memhub sync status --json`.
- `enabled == false` → stop. Tell me: "Cross-machine sync isn't enabled
  for this repo. Run `memhub sync enable`, then re-run `/catch-up`."
- `project_id` null with a `project_id_error` (no git remote) → stop;
  tell me to set `[sync] project_id` in `.memhub/config.toml`.
- `drive_subpath` empty → stop; tell me to set `[sync] drive_subpath`
  to the absolute path of the synced Drive folder on this machine
  (e.g. `~/Library/CloudStorage/GoogleDrive-<me>/My Drive/memhub-sync`
  on macOS, `G:\My Drive\memhub-sync` on Windows).

The remote snapshot dir is `REMOTE = <drive_subpath>/memhub/<project_id>`.
Confirm it exists: `test -d "<REMOTE>" && echo present || echo absent`.
- `absent` → stop and tell me: "No remote snapshot for this project
  yet — nothing to catch up. Your next `/wrap-up` will push the first
  one." Expected first-run state, not an error. (If the Drive app is
  still syncing, the folder may not have appeared yet — give it a
  moment.)

## Compare

Run `memhub sync check "<REMOTE>" --json`. Read the `verdict` and the
guard flags:

- **`project_id_mismatch` is set** → STOP. The snapshot in that folder
  belongs to a different project. Do not adopt; tell me what id it
  carries.
- **`schema_blocks_adopt` is true** → STOP. The snapshot is from a
  newer memhub than this machine. Tell me to run `memhub upgrade`
  first, then re-run `/catch-up`.

Then act on `verdict`:

| verdict | meaning | action |
|---|---|---|
| `up-to-date` | local already matches Drive | nothing to do — say so |
| `drive-ahead` | another machine pushed newer state | recommend adopting; this is a safe fast-forward |
| `local-ahead` | this machine is ahead of Drive | nothing to pull; `/wrap-up` will push |
| `diverged` | **both** sides changed since last sync | requires an explicit decision — see below |

## Adopt (gated)

Only adopt on `drive-ahead` or `diverged`, and only after I confirm.

- **`drive-ahead`**: summarize what's incoming (the `remote` machine id
  and timestamp from the check output) and ask me to confirm. On yes,
  run `memhub sync adopt "<REMOTE>" --yes`.
- **`diverged`**: the lossy case. Tell me plainly that both this machine
  and Drive changed since the last sync, so adopting the Drive copy
  **discards the local-only changes** made here. Show the local vs
  drive logical versions. Require an explicit "yes, overwrite local"
  before running `memhub sync adopt "<REMOTE>" --yes`. If I'd rather
  keep local, do nothing — `/wrap-up` will push local up and Drive
  becomes the one that's behind.

`adopt` makes a single safety copy of the replaced DB under
`.memhub/backups/sync/` before swapping, and refuses on its own if the
checksum, schema, or project id don't check out — so a half-synced or
wrong file can't corrupt local.

## After adopting

1. Run `memhub render` so the local `PROJECT.md` / `PROJECT_LEDGER.md`
   reflect the adopted state.
2. Briefly tell me what changed (e.g. "pulled 3 newer decisions and a
   session note from <machine>"), then suggest reading
   `.memhub/rendered/PROJECT.md` for the freshly-synced state.

## Notes

- Never run `git` operations or commits. The snapshot carries only
  memhub's local DB, which is gitignored.
- If the synced folder isn't present (Drive app not installed/signed
  in, or still syncing), say so and stop — do not fabricate a verdict.
- Manual, one snapshot each way: `/catch-up` pulls at the start,
  `/wrap-up` pushes at the end. There is no background memhub sync —
  only Google's app moving the file you wrote.
