# Project State

Last updated: 2026-04-22

## Currently building

Between Milestone 3 tasks after finishing `M3-001` markdown-sync hardening. The current codebase now has git ingestion, indexed search, and managed-block generation for `AGENTS.md` / `CLAUDE.md`, plus backup-before-rewrite semantics, strict marker validation, and safer temp-file replacement for managed markdown updates.

## Next up

1. Start `M3-002` with MCP read/write adapters over the existing CLI services.
2. Decide whether search should index facts, tasks, or command history before MCP depends on it.
3. Choose and validate the Rust MCP crate before wiring the server surface.

## Last session

2026-04-21 - Completed `M3-001` by hardening `memhub sync-md` with backup-before-rewrite behavior for existing markdown files, strict managed-marker validation, temp-file replacement writes, richer sync reporting, and regression tests for no-op, malformed-marker, and manual-content preservation paths. Verified with `cargo fmt`, `cargo test`, and `cargo build`.

2026-04-21 - Shipped `d8bcd05` by adding Milestone 2 git ingestion and indexed search plus the first Milestone 3 markdown-sync slice. The release adds `memhub ingest-git`, FTS-backed `memhub search`, `memhub sync-md`, init-time managed-block generation for `AGENTS.md` / `CLAUDE.md`, and optional auto-sync after managed-content writes. Verified with `cargo fmt`, `cargo test`, `cargo build`, then pushed `main` to `origin`.

2026-04-21 - Completed explicit command-history verification in `5689bb5` by adding `memhub command verify`, wiring command outcome writes into the existing `commands` table, and covering insert/update behavior with tests. Verified with `cargo test` and `cargo build`, then pushed `main` to `origin`.
## Open questions

- Which Rust MCP crate is the right fit when Milestone 3 starts?
- Should Milestone 2 search index facts, tasks, or command history before MCP depends on it?
- Should `memhub migrate` remain implicit-on-open or become explicit once external users adopt the tool?
