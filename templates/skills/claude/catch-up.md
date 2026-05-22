---
name: catch-up
description: Pull this repo's memhub DB from the Google Drive sync folder, compare it to local, and (with your approval) adopt the newer state so this machine has memory from sessions on your other machines.
framework: memhub
framework_version: 1.0.0
last_updated: 2026-05-22
---

Bring this machine's memhub memory up to date with what you did on
another machine. memhub itself is offline — **you** are the courier:
you move one file down from Google Drive, and memhub does the compare
and the (gated) replace on local files. Run this at the **start** of a
session on a repo you sync across machines. The matching push happens
at the end of `/wrap-up`.

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

**Check 3 — sync enabled.**
Run `memhub sync status --json`.
- `enabled == false` → stop. Tell me: "Cross-machine sync isn't enabled
  for this repo. Run `memhub sync enable`, then re-run `/catch-up`."
- If `project_id` is null and `project_id_error` is set (no git
  remote) → stop and tell me to set `[sync] project_id` in
  `.memhub/config.toml`.
- Otherwise keep the `project_id` and `drive_subpath` for the next step.

## Download from Drive

Using your Google Drive integration (do **not** ask memhub to do this —
it has no network access), download both files from the project's sync
folder into a fresh temp directory, e.g. `/tmp/memhub-catchup-<project_id>/`:

- `<drive_subpath>/memhub/<project_id>/project.sqlite`
- `<drive_subpath>/memhub/<project_id>/manifest.json`

(`drive_subpath` is the hint from config for where under my Drive the
memhub folder lives; if empty, ask me where the memhub sync folder is
the first time.)

If the folder or files don't exist on Drive yet → stop and tell me:
"No remote snapshot for this project yet — nothing to catch up. Your
next `/wrap-up` will push the first one." This is the expected first-run
state, not an error.

## Compare

Run `memhub sync check <temp-dir> --json`. Read the `verdict` and the
guard flags:

- **`project_id_mismatch` is set** → STOP. The downloaded snapshot
  belongs to a different project (wrong Drive folder). Do not adopt;
  tell me what id it carries.
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
  run `memhub sync adopt <temp-dir> --yes`.
- **`diverged`**: this is the lossy case. Tell me plainly that both this
  machine and Drive changed since the last sync, so adopting the Drive
  copy **discards the local-only changes** made here. Show the local vs
  drive logical versions. Require an explicit "yes, overwrite local"
  before running `memhub sync adopt <temp-dir> --yes`. If I'd rather
  keep local, do nothing — `/wrap-up` will push local up and Drive
  becomes the one that's behind.

`adopt` makes a single safety copy of the replaced DB under
`.memhub/backups/sync/` before swapping, and refuses on its own if the
checksum, schema, or project id don't check out — so a bad download
can't corrupt local.

## After adopting

1. Run `memhub render` so the local `PROJECT.md` / `PROJECT_LEDGER.md`
   reflect the adopted state.
2. Briefly tell me what changed (e.g. "pulled 3 newer decisions and a
   session note from <machine>"), then suggest reading
   `.memhub/rendered/PROJECT.md` for the freshly-synced state.

## Notes

- This never runs `git` operations and never commits anything. The
  Drive snapshot carries only memhub's local DB, which is gitignored.
- memhub stays offline by design; if you can't reach Drive, say so and
  stop — do not fabricate a verdict.
- One file each way, manual cadence: `/catch-up` to pull at the start,
  `/wrap-up` to push at the end. There is no background sync.
