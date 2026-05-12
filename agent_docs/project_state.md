# Project State

Last updated: 2026-05-12

## Currently building

Between tasks after `M4-004`. Deny-list enforcement is shipped: `ProjectConfig` now carries a configurable `DenyList` (with sensible defaults), `commands::ingest_git` filters out denied paths before inserting `files`/`commit_files` rows and reports `denied_files_skipped` in its summary, and `commands::search` post-filters file-history results so denied paths already in the DB never surface. Invalid patterns fail closed: ingestion and search refuse to run until the bad pattern is fixed. Matching uses the `globset` crate and walks path segments so `config/server.pem` is denied by `*.pem` even though the pattern is unrooted.

## Next up

1. Plan the last remaining Milestone 4 slice: confidence scoring and staleness handling (PRD §11.4). Open design questions on continuous decay vs. simple stale-flag at 90 days, whether to add a confidence column to `commands`, and how update-on-success/failure should look.
2. Plan `M5-001` K9 Claude Framework integration per `docs/roadmap/k9-integration.md`. Preconditions are met across the board: M4-001/002/003/004 are shipped, and the `list_pending_writes` MCP tool plus the review CLI are the surface K9 `/wrap-up` was designed for.
3. Decide whether MCP needs broader indexed retrieval over facts, tasks, or command history beyond the current narrow paths.

## Last session

2026-05-12 - Completed `M4-004` by adding the `globset` crate, a new `src/config/deny.rs` module with `DenyList`/`PathMatcher`/`default_patterns`, ingest-git skip logic with `denied_files_skipped` reporting, search-side post-filtering for both prefixed and inferred file lookups, and a `deny_patterns` count in `memhub status` and the MCP `status` tool. Added 5 integration tests in `tests/deny_list.rs` plus 4 unit tests in `src/config/deny.rs`. Updated README with a "Deny list" section. Verified with `cargo fmt`, `cargo build`, and `cargo test` (59 tests across all suites).

2026-05-12 - Completed `M4-003` by adding migration `0005_pending_write_reviewed_at`, the `commands::review` module with `list/show/accept/reject/expire`, the `memhub review` CLI subcommand, and the read-only `list_pending_writes` MCP tool. Promotion delegates to `fact::add`/`decision::add` (which regenerate FTS chunks and run sync-md when enabled). Added 9 integration tests in `tests/review.rs` plus 1 MCP test. Updated README with a "Reviewing staged MCP proposals" section. Verified with `cargo fmt`, `cargo build`, and `cargo test` (50 tests across all suites).

2026-05-12 - Completed `M4-002` by adding `MemhubError::MissingDatabase`, gating `db::open_project` and `db::init_project` on an existing `project.sqlite`, exposing `db::init_project_for_recovery`, and wiring `memhub init --from-backup <path>` through CLI parsing, `commands::init::run_with_backup`, and the existing import path. Refreshed the README backup/restore section with a "Recover when the database is missing or corrupted" subsection. Verified with `cargo fmt`, `cargo build`, and `cargo test` (40 tests across all suites, including 5 new tests in `tests/export_import.rs`).

2026-05-12 - Completed `M4-001` by adding `memhub export` / `memhub import`, version-tagged JSON format types under `src/export/v1`, and the wipe-and-restore import path with `PRAGMA defer_foreign_keys = ON`, decision-chunk regeneration, audit logging, and post-restore `sync-md`. Shipped `docs/reference/export-format.md` and a README backup/restore section. Verified with `cargo fmt`, `cargo build`, and `cargo test` (35 tests across all suites).

## Open questions

- For confidence/staleness: continuous decay function or simple stale flag at 90 days? Add a `confidence` column to `commands` or keep success/fail counts only?
- Should `memhub` ship a future `gc` slice that purges already-ingested denied paths after a pattern change, or is filter-on-read sufficient indefinitely?
- Which additional `clientInfo.name` values do Codex and Claude Code send in real handshakes beyond the initial alias map?
- Should `memhub migrate` remain implicit-on-open or become explicit once external users adopt the tool?
