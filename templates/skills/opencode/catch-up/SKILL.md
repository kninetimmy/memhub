---
name: catch-up
description: Pull this repo's memhub DB from the Google Drive synced folder at session start and adopt newer cross-machine state with user approval.
compatibility: opencode
---

# Skill: catch-up

Bring this machine's memhub memory up to date with another machine. The transport is an OS-level synced folder (Google Drive for Desktop on macOS/Windows, rclone mount on Linux) mirrored to a local path; memhub stays offline and just reads/writes that path. Prefer the `memhub.sync_*` MCP tools — they default the Drive folder to the canonical `<drive_subpath>/memhub/<project_id>`, so you never build that path by hand; CLI is the fallback.

Workflow:
- Gate: call `memhub.sync_status`. If `enabled` is false, tell the user to run `memhub sync enable` and stop. If `project_id_error` is set (no git remote) or `remote_dir_error` is set (usually an empty `drive_subpath`), stop and ask the user to set it in `.memhub/config.toml`. Otherwise `remote_dir` is the resolved snapshot folder.
- Call `memhub.sync_check` (no args — it targets `remote_dir`). If `project_id_mismatch` is set or `schema_blocks_adopt` is true, stop and explain (the latter needs `memhub upgrade` first).
- On verdict `no-remote`, say "nothing to catch up yet" and stop (first-run, or mid-sync). On `drive-ahead`, recommend adopting; on `diverged`, warn that adopting discards local-only changes and require explicit approval; on `up-to-date`/`local-ahead`, do nothing.
- `memhub.sync_adopt` is gated by `confirm`: without `confirm: true` it returns the would-change verdict and changes nothing (a dry run). After approval call `memhub.sync_adopt(confirm=true)`, then `memhub.render`, and report what changed.
- CLI fallback (no MCP server): `memhub sync status` → `memhub sync check` → `memhub sync adopt --yes` → `memhub render`; each defaults to the canonical folder when the path is omitted.
- Never run git or commit; the snapshot is only memhub's gitignored local DB.
