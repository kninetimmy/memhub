---
name: catch-up
description: Pull this repo's memhub DB from the Google Drive sync folder at session start and adopt newer cross-machine state with user approval.
compatibility: opencode
---

# Skill: catch-up

Bring this machine's memhub memory up to date with another machine. memhub is offline; you are the courier moving one file down from Google Drive.

Workflow:
- Gate: run `memhub sync status --json`; if `enabled` is false, tell the user to run `memhub sync enable` and stop. Keep `project_id` and `drive_subpath`.
- Using your Google Drive access (not memhub), download `<drive_subpath>/memhub/<project_id>/project.sqlite` and `manifest.json` into a temp dir. If absent on Drive, say "nothing to catch up yet" and stop.
- Run `memhub sync check <temp-dir> --json`. If `project_id_mismatch` is set or `schema_blocks_adopt` is true, stop and explain (the latter needs `memhub upgrade` first).
- On verdict `drive-ahead`, recommend adopting; on `diverged`, warn that adopting discards local-only changes and require explicit approval; on `up-to-date`/`local-ahead`, do nothing.
- After approval run `memhub sync adopt <temp-dir> --yes`, then `memhub render`, and report what changed.
- Never run git or commit; the snapshot is only memhub's gitignored local DB.
