---
name: catch-up
description: Pull this repo's memhub DB from the Google Drive synced folder, compare it to local, and (with user approval) adopt the newer state so this machine has memory from sessions on other machines.
framework: memhub
framework_version: 1.0.0
last_updated: 2026-05-22
---

Bring this machine's memhub memory up to date with work done on another
machine. The transport is an **OS-level synced folder** — Google Drive
for Desktop (macOS/Windows) or an rclone mount (Linux) — mirroring a
Drive folder to a local path. memhub stays fully offline: it reads and
writes that local path, and Google's app syncs the bytes. No network,
no base64. Run at the **start** of a session on a synced repo; the
matching push is the tail of `/wrap-up`.

This is the Codex counterpart to the Claude Code `/catch-up` skill.
Prefer the **`memhub.sync_*` MCP tools** — they default the Drive
folder to the canonical `<drive_subpath>/memhub/<project_id>`, so you
never build that path by hand; the CLI is the fallback (end). See
`docs/reference/memhub-prd-addendum-m10-drive-sync.md` (whole-DB
snapshot, last-writer-wins, operator-gated divergence).

## Detection

Call **`memhub.sync_status`** — it resolves enablement and the remote
dir in one shot (no `test -d`, no path math). Stop when:

- `enabled == false` → "Run `memhub sync enable`, then re-run
  `/catch-up`."
- `project_id_error` set (no git remote) → ask the user to set
  `[sync] project_id` in `.memhub/config.toml`.
- `remote_dir_error` set (usually an empty `drive_subpath`) → ask the
  user to set `[sync] drive_subpath` to the absolute path of the synced
  Drive folder on this machine.

Otherwise `remote_dir` is the resolved snapshot folder.

## Compare

Call **`memhub.sync_check`** (no args — it targets `remote_dir`). Honor
the guard flags first:

- `project_id_mismatch` set → STOP, wrong-project snapshot; do not adopt.
- `schema_blocks_adopt` true → STOP, snapshot is from a newer memhub;
  tell the user to run `memhub upgrade`, then retry.

Then on `verdict`: `no-remote` → nothing to catch up (first-run, or the
Drive app is still syncing); `up-to-date` → nothing; `local-ahead` →
nothing to pull (wrap-up will push); `drive-ahead` → recommend adopting
(safe fast-forward); `diverged` → both changed, gated decision below.

## Adopt (gated)

Adopt only on `drive-ahead` or `diverged`, only after explicit user
confirmation. **`memhub.sync_adopt` is gated by `confirm`**: calling it
without `confirm: true` returns the would-change verdict and changes
nothing (a built-in dry run). Pass `confirm: true` only after the user
says yes.

- `drive-ahead`: summarize incoming `remote_machine_id` +
  `remote_created_at`, confirm, then `memhub.sync_adopt(confirm=true)`.
- `diverged`: the lossy case. State clearly that adopting Drive
  **discards local-only changes**; show local vs drive logical versions;
  require an explicit "yes, overwrite local" before
  `memhub.sync_adopt(confirm=true)`. If the user prefers local, do
  nothing — `/wrap-up` pushes local up instead.

`adopt` backs up the replaced DB under `.memhub/backups/sync/` and
self-refuses on bad checksum / schema / project id.

## After adopting

Call **`memhub.render`**, then tell the user what changed and point them
at `.memhub/rendered/PROJECT.md`.

## CLI fallback

No MCP server connected? Drive the same flow from the CLI — each
command defaults to the canonical folder when the path is omitted:

```bash
memhub sync status        # enablement + resolved remote dir
memhub sync check         # verdict + guard flags
memhub sync adopt --yes   # --yes is the confirm gate
memhub render
```

## Notes

- No `git` operations, no commits — the snapshot is only memhub's
  gitignored local DB.
- If the synced folder isn't present (Drive app absent or still
  syncing), `sync_check` reports `no-remote` — say so and stop; never
  fabricate a verdict.
- Manual, one snapshot each way: `/catch-up` pulls at the start,
  `/wrap-up` pushes at the end. No background memhub sync.
