# Project State

Last updated: 2026-05-12

## Currently building

Between tasks after `M4-002`. Missing-database safety is shipped: `memhub::db::open_project` and `memhub::db::init_project` both refuse to silently re-create `project.sqlite` inside an existing `.memhub/`, returning a new `MemhubError::MissingDatabase` that points at the supported recovery path. The convenience UX is shipped as `memhub init --from-backup <path>`, which creates `.memhub/` if needed, initializes the schema via `init_project_for_recovery`, then runs the existing import flow. The flag refuses when a database already exists and tells the user to use `memhub import --force` to overwrite a live database instead.

## Next up

1. Decide whether MCP needs broader indexed retrieval over facts, tasks, or command history beyond the current narrow paths.
2. Begin the review and promotion flow for staged MCP `pending_writes` proposals.
3. Plan confidence/staleness handling and deny-list enforcement for Milestone 4.
4. After M4 closes, plan `M5-001` K9 Claude Framework integration per `docs/roadmap/k9-integration.md`.

## Last session

2026-05-12 - Completed `M4-002` by adding `MemhubError::MissingDatabase`, gating `db::open_project` and `db::init_project` on an existing `project.sqlite`, exposing `db::init_project_for_recovery`, and wiring `memhub init --from-backup <path>` through CLI parsing, `commands::init::run_with_backup`, and the existing import path. Refreshed the README backup/restore section with a "Recover when the database is missing or corrupted" subsection. Verified with `cargo fmt`, `cargo build`, and `cargo test` (40 tests total across all suites, including 5 new tests in `tests/export_import.rs`).

2026-05-12 - Completed `M4-001` by adding `memhub export` / `memhub import`, version-tagged JSON format types under `src/export/v1`, and the wipe-and-restore import path with `PRAGMA defer_foreign_keys = ON`, decision-chunk regeneration, audit logging, and post-restore `sync-md`. Shipped `docs/reference/export-format.md` and a README backup/restore section. Verified with `cargo fmt`, `cargo build`, and `cargo test` (35 tests across all suites).

2026-04-22 - Tightened MCP proposal provenance handling by preserving exact raw `clientInfo.name`, sanitizing client names before logging, and storing available MCP request/init provenance JSON on `pending_writes` via migration `0004_pending_write_provenance`. Verified with `cargo fmt`, `cargo test`, and `cargo build`.

2026-04-22 - Completed `M3-003` by adding a `pending_writes` table, staged MCP `propose_fact` / `propose_decision` tools, pending-write visibility in CLI and MCP status, and `clientInfo.name` alias normalization with raw-value preservation. Verified with `cargo fmt`, `cargo test`, and `cargo build`.

## Open questions

- Should the next retrieval expansion index facts, tasks, or command history first now that MCP reads exist?
- Which additional `clientInfo.name` values do Codex and Claude Code send in real handshakes beyond the initial alias map?
- Should `memhub migrate` remain implicit-on-open or become explicit once external users adopt the tool?
- What is the right shape for the eventual review/promotion flow over `pending_writes`?
