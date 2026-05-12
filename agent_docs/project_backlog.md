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

- `M4-004` - Add path-based deny-list enforcement.
  Status: completed
  Scope: `Cargo.toml`, `src/config/deny.rs`, `src/config/mod.rs`, `src/commands/ingest_git.rs`, `src/commands/search.rs`, `src/commands/status.rs`, `src/models/mod.rs`, `src/cli/mod.rs`, `src/mcp/mod.rs`, `tests/deny_list.rs`, README
  Notes: `ProjectConfig::deny_list` (serde-defaulted) holds the per-repo glob patterns. `commands::ingest_git` skips denied paths and surfaces `denied_files_skipped`; `commands::search` post-filters file-history results both for prefixed `file:` lookups and inferred path lookups. Invalid patterns fail closed via `MemhubError::InvalidInput`. Matching uses `globset` and walks path segments so `config/server.pem` is denied by `*.pem`. Content scanning and historical-data cleanup explicitly deferred.

- `M4-003` - Add review and promotion flow for staged MCP `pending_writes`.
  Status: completed
  Scope: `migrations/0005_pending_write_reviewed_at.sql`, `src/commands/review.rs`, `src/commands/mod.rs`, `src/cli/mod.rs`, `src/mcp/mod.rs`, `src/models/mod.rs`, `tests/review.rs`, README
  Notes: Added `memhub review list|show|accept|reject|expire`. `accept` reuses `fact::add` / `decision::add` so promoted rows go through existing audit, FTS chunk regeneration, and sync-md plumbing; promoted facts land at `source = "user"` with `confidence = 1.0`. `pending_writes` gained a nullable `reviewed_at` column set on any transition out of `pending`. `expire` is explicit only (no auto-expire on read), defaulting to 30 days per PRD §11.3. Added a read-only `list_pending_writes` MCP tool so K9 `/wrap-up` can surface staged proposals during its human-approval gate. Confidence-override flags and batch auto-accept remain deferred until confidence decay exists.

- `M4-002` - Add recovery-safe missing-DB handling and follow-on init UX.
  Status: completed
  Scope: `src/db/`, `src/commands/init.rs`, `src/cli/mod.rs`, `src/errors/mod.rs`, README, `tests/export_import.rs`
  Notes: Shipped `MemhubError::MissingDatabase` and gated both `db::open_project` and `db::init_project` so an existing `.memhub/` without `project.sqlite` returns the new error instead of silently rebuilding the database. Exposed `db::init_project_for_recovery` as the explicit recovery-mode entry point. Added `memhub init --from-backup <path>` (CLI flag plus `commands::init::run_with_backup`) which refuses when a database already exists, then runs the existing import flow. README "Recover when the database is missing or corrupted" section explains the recovery path. Plain `memhub init` stays non-interactive and refuses the missing-DB case per the prior decision.

- `M5-001` - K9 Claude Framework integration: optional DB writes from `/wrap-up`.
  Status: triaged
  Scope: `docs/roadmap/k9-integration.md`, install scripts, `src/cli/init`, `src/config/`, K9 repo `/wrap-up.md`
  Notes: Full design in `docs/roadmap/k9-integration.md`. `M4-001`, `M4-002`, and `M4-003` are now complete; the recovery preconditions and the staged-write promotion surface that K9 `/wrap-up` was designed against both exist. Key requirements: memhub install must detect K9 and configure accordingly without modifying K9 files; K9 `/wrap-up` shells out to existing memhub CLI commands (`memhub review list/accept/reject`, plus `decision add` / `task add` / `fact add`) after human approval; no bidirectional sync; standalone modes for both systems remain fully supported.
