---
name: catch-up
description: Compare this repo with its synced memhub snapshot and adopt it only after explicit approval.
---

Prefer the attached memhub MCP server's `sync_status` then `sync_check`; if
unavailable use `memhub sync status --json` and `memhub sync check --json`.
Use the resolved path, not a guessed or manually assembled one. Stop if sync
is disabled, required paths are unresolved, the project ID differs, or the
snapshot schema is too new. Nothing to adopt on no-remote, up-to-date or
local-ahead.

On drive-ahead or diverged, show the incoming machine/time and any baseline
warning. Counters do not establish ancestry. Explain the risk of losing local
changes and preserve both histories before replacing either. Wait for explicit
approval to overwrite local memory. Then call `sync_adopt(confirm=true)` or
`memhub sync adopt --yes`. These commands enforce project, schema, and checksum
guards. On success render with MCP or `memhub render --actor omp:catch-up` and
report the resulting PROJECT.md path. Never adopt on an unavailable verdict.
