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
  Status: completed
  Scope: `src/cli/`, `src/commands/export.rs`, `src/commands/import.rs`, `src/export/v1.rs`, `docs/reference/export-format.md`, README backup/restore section, `tests/export_import.rs`
  Notes: Shipped `memhub export <path>` writing a version-tagged JSON file (`memhub_export_version = 1`) covering facts, decisions, tasks, commands, pending_writes, and writes_log. Shipped `memhub import <path>` with `--force` flag; wipe-and-restore semantics in a single transaction using `PRAGMA defer_foreign_keys = ON`; preserves row IDs; regenerates decision chunks via `search::sync_decision_chunks`; logs an audit entry for the restore; runs `sync_md::sync_project` after commit. Import requires the target to already be initialized; the missing-DB recovery case is `M4-002`. Merge semantics and CLI restore convenience UX explicitly deferred.

- `M4-002` - Add recovery-safe missing-DB handling and follow-on init UX.
  Status: completed
  Scope: `src/db/`, `src/commands/init.rs`, `src/cli/mod.rs`, `src/errors/mod.rs`, README, `tests/export_import.rs`
  Notes: Shipped `MemhubError::MissingDatabase` and gated both `db::open_project` and `db::init_project` so an existing `.memhub/` without `project.sqlite` returns the new error instead of silently rebuilding the database. Exposed `db::init_project_for_recovery` as the explicit recovery-mode entry point. Added `memhub init --from-backup <path>` (CLI flag plus `commands::init::run_with_backup`) which refuses when a database already exists, then runs the existing import flow. README "Recover when the database is missing or corrupted" section explains the recovery path. Plain `memhub init` stays non-interactive and refuses the missing-DB case per the prior decision.

- `M5-001` - K9 Claude Framework integration: optional DB writes from `/wrap-up`.
  Status: triaged
  Scope: `docs/roadmap/k9-integration.md`, install scripts, `src/cli/init`, `src/config/`, K9 repo `/wrap-up.md`
  Notes: Full design in `docs/roadmap/k9-integration.md`. `M4-001` and `M4-002` are now complete, so the recovery preconditions for this milestone are in place. Key requirements: memhub install must detect K9 and configure accordingly without modifying K9 files; K9 `/wrap-up` shells out to existing memhub CLI commands after human approval; no bidirectional sync; standalone modes for both systems remain fully supported.
