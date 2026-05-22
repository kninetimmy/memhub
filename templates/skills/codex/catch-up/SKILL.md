---
name: catch-up
description: Pull this repo's memhub DB from the Google Drive sync folder, compare it to local, and (with user approval) adopt the newer state so this machine has memory from sessions on other machines.
framework: memhub
framework_version: 1.0.0
last_updated: 2026-05-22
---

Bring this machine's memhub memory up to date with work done on another
machine. memhub is offline — Codex is the courier: you move one file
down from Google Drive, and memhub does the compare and the gated
replace on local files. Run at the **start** of a session on a repo
synced across machines; the matching push is the tail of `/wrap-up`.

This is the Codex counterpart to the Claude Code `/catch-up` skill;
both drive the same offline `memhub sync` CLI. See
`docs/reference/memhub-prd-addendum-m10-drive-sync.md` for the model
(whole-DB snapshot, last-writer-wins, operator-gated divergence).

## Detection

**Check 1 — `.memhub/` exists.**
`test -d .memhub && echo present || echo absent` — `absent` → stop, tell
the user to run `memhub init` (or `/init-project`).

**Check 2 — memhub on PATH.**
`command -v memhub >/dev/null 2>&1 && echo present || echo absent` —
`absent` → stop, ask the user to put `memhub` on PATH.

**Check 3 — sync enabled.**
`memhub sync status --json`.
- `enabled == false` → stop: "Run `memhub sync enable`, then re-run
  `/catch-up`."
- `project_id` null with a `project_id_error` (no git remote) → stop,
  ask the user to set `[sync] project_id` in `.memhub/config.toml`.
- Otherwise keep `project_id` and `drive_subpath`.

## Download from Drive

Using your Google Drive access (memhub has no network), download into a
fresh temp dir (e.g. `/tmp/memhub-catchup-<project_id>/`):

- `<drive_subpath>/memhub/<project_id>/project.sqlite`
- `<drive_subpath>/memhub/<project_id>/manifest.json`

If empty `drive_subpath`, ask the user once where the memhub sync folder
lives. If the files aren't on Drive yet → stop: "No remote snapshot for
this project yet — nothing to catch up; the next `/wrap-up` pushes the
first." Expected first-run state, not an error.

## Compare

`memhub sync check <temp-dir> --json`. Honor the guard flags first:

- `project_id_mismatch` set → STOP, wrong-folder snapshot; do not adopt.
- `schema_blocks_adopt` true → STOP, snapshot is from a newer memhub;
  tell the user to run `memhub upgrade`, then retry.

Then on `verdict`: `up-to-date` → nothing; `local-ahead` → nothing to
pull (wrap-up will push); `drive-ahead` → recommend adopting (safe
fast-forward); `diverged` → both changed, gated decision below.

## Adopt (gated)

Adopt only on `drive-ahead` or `diverged`, only after explicit user
confirmation.

- `drive-ahead`: summarize incoming machine id + timestamp, confirm,
  then `memhub sync adopt <temp-dir> --yes`.
- `diverged`: the lossy case. State clearly that adopting Drive
  **discards local-only changes**; show local vs drive logical versions;
  require an explicit "yes, overwrite local" before
  `memhub sync adopt <temp-dir> --yes`. If the user prefers local,
  do nothing — `/wrap-up` pushes local up instead.

`adopt` backs up the replaced DB under `.memhub/backups/sync/` and
self-refuses on bad checksum / schema / project id.

## After adopting

Run `memhub render`, then tell the user what changed and point them at
`.memhub/rendered/PROJECT.md`.

## Notes

- No `git` operations, no commits — the Drive snapshot is only memhub's
  gitignored local DB.
- If Drive is unreachable, say so and stop; never fabricate a verdict.
- Manual, one file each way: `/catch-up` pulls at the start, `/wrap-up`
  pushes at the end. No background sync.
