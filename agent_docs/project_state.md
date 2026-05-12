# Project State

Last updated: 2026-05-12

## Currently building

Between tasks after `M4-001`. Portable export/import is shipped: `memhub export <path>` writes a version-tagged JSON file covering durable tables, and `memhub import <path>` restores into a target that was initialized via `memhub init`, refusing non-empty targets without `--force`. Restore preserves row IDs, regenerates decision chunks, logs an audit entry, and re-syncs managed markdown. Derived state (git ingestion, FTS chunks, schema migrations) stays out of the export and is regenerated on import or by re-running `memhub ingest-git`.

## Next up

1. Start `M4-002` missing-DB safety so an existing `.memhub/` without `project.sqlite` is treated as an explicit recovery case instead of silent re-init.
2. After `M4-002`, add the narrowest convenience UX around restore entry points without making plain `memhub init` interactive.
3. Decide whether MCP needs broader indexed retrieval over facts, tasks, or command history beyond the current narrow paths.
4. After M4 closes, plan `M5-001` K9 Claude Framework integration per `docs/roadmap/k9-integration.md`.

## Last session

2026-05-12 - Completed `M4-001` by adding `memhub export` / `memhub import`, version-tagged JSON format types under `src/export/v1`, and the wipe-and-restore import path with `PRAGMA defer_foreign_keys = ON`, decision-chunk regeneration, audit logging, and post-restore `sync-md`. Shipped `docs/reference/export-format.md` and a README backup/restore section. Verified with `cargo fmt`, `cargo build`, and `cargo test` (35 tests across all suites).

2026-04-22 - Tightened MCP proposal provenance handling by preserving exact raw `clientInfo.name`, sanitizing client names before logging, and storing available MCP request/init provenance JSON on `pending_writes` via migration `0004_pending_write_provenance`. Verified with `cargo fmt`, `cargo test`, and `cargo build`.

2026-04-22 - Completed `M3-003` by adding a `pending_writes` table, staged MCP `propose_fact` / `propose_decision` tools, pending-write visibility in CLI and MCP status, and `clientInfo.name` alias normalization with raw-value preservation. Verified with `cargo fmt`, `cargo test`, and `cargo build`.

2026-04-22 - Completed `M3-002` by adding `memhub serve`, selecting the official `rmcp` crate for the stdio MCP server, and wiring thin MCP tools over existing services for project status, indexed search, task listing, recent decisions, latest command lookup, and explicit verified command recording. Verified with `cargo fmt`, `cargo test`, and `cargo build`.

## Open questions

- Should the next retrieval expansion index facts, tasks, or command history first now that MCP reads exist?
- Which additional `clientInfo.name` values do Codex and Claude Code send in real handshakes beyond the initial alias map?
- Should `memhub migrate` remain implicit-on-open or become explicit once external users adopt the tool?
- Should `memhub import` ever auto-init a missing `.memhub/`, or should that always require explicit `memhub init` first?
