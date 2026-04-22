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
  Status: completed
  Scope: `src/sync_md/`, CLI surface, docs, backup behavior
  Notes: Completed by adding strict marker validation, timestamped backups for changed existing markdown files, temp-file replacement writes, richer sync reporting, and regression coverage for failure/no-op/manual-content paths. Broader managed-content coverage remains deferred.

- `M3-002` - Implement MCP read/write tools as thin adapters over existing services.
  Status: completed
  Scope: `src/mcp/`, write policy wiring, tests
  Notes: Completed as a narrow stdio MCP slice using the official `rmcp` crate. The server is exposed through `memhub serve` and currently supports status, search, task listing, recent decision listing, latest-command lookup, and explicit verified command recording. Broader agent-originated write policy remains deferred.

- `M3-003` - Expand MCP write-policy boundaries and client identity handling.
  Status: completed
  Scope: `src/mcp/`, write-policy plumbing, continuity docs, tests
  Notes: Completed by adding `pending_writes`, staged MCP `propose_fact` / `propose_decision` tools, status visibility for pending writes, and `clientInfo.name` alias normalization with raw-value preservation. Review and promotion flows remain deferred to Milestone 4.

- `M4-001` - Add portable export/import for repo recovery and machine moves.
  Status: triaged
  Scope: `src/cli/`, `src/commands/`, `src/db/`, format docs, tests
  Notes: Implement `memhub export` / `memhub import` as the supported recovery path using a version-tagged portable format. Import should restore data into the repo-local `.memhub/` layout, reconcile project metadata with the current repo root, run migrations as needed, and regenerate managed markdown after restore.

- `M4-002` - Add recovery-safe missing-DB handling and follow-on init UX.
  Status: triaged
  Scope: `src/db/`, `src/commands/`, `src/cli/`, README, tests
  Notes: If `.memhub/` exists but `project.sqlite` is missing, fail as an explicit recovery case instead of silently creating a fresh database. After `M4-001`, add the narrowest convenience UX around restore entry points without making plain `memhub init` depend on prompts. Ship README backup/restore instructions with this slice.
