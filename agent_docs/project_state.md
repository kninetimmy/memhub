# Project State

Last updated: 2026-05-12

## Currently building

Between tasks after `M4-003`. The staged MCP proposal loop is closed: `memhub review list|show|accept|reject|expire` promotes pending fact/decision proposals into their durable tables via the existing `fact::add` / `decision::add` paths, marks the `pending_writes` row `accepted`/`rejected`/`expired`, and stamps a new `reviewed_at` column added in migration `0005_pending_write_reviewed_at`. The MCP server now exposes a read-only `list_pending_writes` tool so K9 wrap-up can surface staged proposals during its human-approval gate. Promotion runs at `source = "user"` and `confidence = 1.0`, preserving the original-actor provenance in `pending_writes` and `writes_log` rather than encoding it on `facts.source`.

## Next up

1. Decide between deny-list enforcement (PRD §15) and confidence/staleness handling (PRD §11.4) as the next Milestone 4 slice.
2. Plan `M5-001` K9 Claude Framework integration per `docs/roadmap/k9-integration.md`. Preconditions are met: `M4-001`, `M4-002`, and `M4-003` are all shipped, and the new `list_pending_writes` MCP tool plus the review CLI are exactly the surface K9 `/wrap-up` was designed to call.
3. Decide whether MCP needs broader indexed retrieval over facts, tasks, or command history beyond the current narrow paths.

## Last session

2026-05-12 - Completed `M4-003` by adding migration `0005_pending_write_reviewed_at`, the `commands::review` module with `list/show/accept/reject/expire`, the `memhub review` CLI subcommand, and the read-only `list_pending_writes` MCP tool. Promotion delegates to `fact::add`/`decision::add` (which regenerate FTS chunks and run sync-md when enabled). Added 9 integration tests in `tests/review.rs` plus 1 MCP test. Updated README with a "Reviewing staged MCP proposals" section. Verified with `cargo fmt`, `cargo build`, and `cargo test` (50 tests across all suites).

2026-05-12 - Completed `M4-002` by adding `MemhubError::MissingDatabase`, gating `db::open_project` and `db::init_project` on an existing `project.sqlite`, exposing `db::init_project_for_recovery`, and wiring `memhub init --from-backup <path>` through CLI parsing, `commands::init::run_with_backup`, and the existing import path. Refreshed the README backup/restore section with a "Recover when the database is missing or corrupted" subsection. Verified with `cargo fmt`, `cargo build`, and `cargo test` (40 tests across all suites, including 5 new tests in `tests/export_import.rs`).

2026-05-12 - Completed `M4-001` by adding `memhub export` / `memhub import`, version-tagged JSON format types under `src/export/v1`, and the wipe-and-restore import path with `PRAGMA defer_foreign_keys = ON`, decision-chunk regeneration, audit logging, and post-restore `sync-md`. Shipped `docs/reference/export-format.md` and a README backup/restore section. Verified with `cargo fmt`, `cargo build`, and `cargo test` (35 tests across all suites).

2026-04-22 - Tightened MCP proposal provenance handling by preserving exact raw `clientInfo.name`, sanitizing client names before logging, and storing available MCP request/init provenance JSON on `pending_writes` via migration `0004_pending_write_provenance`. Verified with `cargo fmt`, `cargo test`, and `cargo build`.

## Open questions

- Which milestone slice comes next: deny-list enforcement (smaller surface) or confidence/staleness (larger but central to PRD §11.4)?
- Should `review accept` ever support a `--confidence <n>` override, or is that always deferred until confidence decay exists?
- Which additional `clientInfo.name` values do Codex and Claude Code send in real handshakes beyond the initial alias map?
- Should `memhub migrate` remain implicit-on-open or become explicit once external users adopt the tool?
