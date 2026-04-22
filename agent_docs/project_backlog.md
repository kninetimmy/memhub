# Project Backlog

## How to pull from this file

When working an item from this backlog:
1. Read the item and any files it references before changing code.
2. Re-check `docs/reference/memhub-prd.md` and `docs/reference/prd-implementation-notes.md` if the scope seems to have drifted.
3. Check `agent_docs/project_decisions.md` for locked constraints.
4. Keep milestones narrow and avoid speculative architecture.

Each item should capture scope, affected files, status, and explicit deferrals.

## Items

- `M1-001` - Add command recording through explicit CLI verification.
  Status: completed
  Scope: `src/cli/`, `src/commands/`, `tests/`, continuity docs
  Notes: Implemented as `memhub command verify` using exit-code recording against the existing `commands` table. Richer verification metadata and automated capture remain deferred.

- `M2-001` - Add git ingestion for commits, files, and commit-file relationships.
  Status: completed
  Scope: `src/commands/`, `src/db/`, `migrations/`, `docs/architecture/current-architecture.md`
  Notes: Implemented as `memhub ingest-git` using the git CLI, with schema support for `commits`, `files`, and `commit_files`.

- `M2-002` - Add FTS-backed `memhub search` with indexed query paths only.
  Status: completed
  Scope: `src/commands/`, `src/db/`, `migrations/`, tests for query plans
  Notes: Implemented with FTS5-backed decision search plus exact indexed file-history lookups and query-plan tests that guard against full scans on hot tables.

- `M3-001` - Implement markdown managed-block sync for `AGENTS.md` and `CLAUDE.md`.
  Status: in progress
  Scope: `src/sync_md/`, CLI surface, docs, backup behavior
  Notes: Core `memhub sync-md` generation is implemented, including init-time sync and optional auto-sync after writes. Backup behavior and any broader managed-content coverage remain open.

- `M3-002` - Implement MCP read/write tools as thin adapters over existing services.
  Status: triaged
  Scope: `src/mcp/`, write policy wiring, tests
