---
name: catch-up
description: Pull this repo's memhub DB from the Google Drive synced folder at session start and adopt newer cross-machine state with user approval.
compatibility: opencode
---

# Skill: catch-up

Bring this machine's memhub memory up to date with another machine. The transport is an OS-level synced folder (Google Drive for Desktop on macOS/Windows, rclone mount on Linux) mirrored to a local path; memhub stays offline and just reads/writes that path.

Workflow:
- Gate: run `memhub sync status --json`; if `enabled` is false, tell the user to run `memhub sync enable` and stop. Need a non-empty `project_id` and `drive_subpath` (the absolute synced-folder path); if either is missing, stop and ask the user to set it in `.memhub/config.toml`.
- `REMOTE = <drive_subpath>/memhub/<project_id>`. If that directory is absent, say "nothing to catch up yet" and stop (it may also just be mid-sync).
- Run `memhub sync check "<REMOTE>" --json`. If `project_id_mismatch` is set or `schema_blocks_adopt` is true, stop and explain (the latter needs `memhub upgrade` first).
- On verdict `drive-ahead`, recommend adopting; on `diverged`, warn that adopting discards local-only changes and require explicit approval; on `up-to-date`/`local-ahead`, do nothing.
- After approval run `memhub sync adopt "<REMOTE>" --yes`, then `memhub render`, and report what changed.
- Never run git or commit; the snapshot is only memhub's gitignored local DB.
