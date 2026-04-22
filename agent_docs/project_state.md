# Project State

Last updated: 2026-04-22

## Currently building

`M3-003` is complete. The current codebase now has the hardened markdown sync path plus a local stdio MCP server exposed through `memhub serve`, with thin adapters for status, search, task listing, decision listing, latest-command lookup, explicit verified command recording, and staged fact/decision proposals backed by `pending_writes`. MCP client names are now normalized from `clientInfo.name` with raw values preserved for future alias cleanup.

## Next up

1. Start `M4-001` with portable `memhub export` / `memhub import` as the supported recovery path.
2. Decide whether the first import slice should also restore per-repo config and run markdown sync automatically.
3. After `M4-001`, implement `M4-002` missing-DB safety so an existing `.memhub/` without `project.sqlite` is treated as recovery, not silent re-init.
4. Decide whether MCP needs broader indexed retrieval over facts, tasks, or command history beyond the current narrow paths.

## Last session

2026-04-22 - Completed `M3-003` by adding a `pending_writes` table, staged MCP `propose_fact` / `propose_decision` tools, pending-write visibility in CLI and MCP status, and `clientInfo.name` alias normalization with raw-value preservation. Verified with `cargo fmt`, `cargo test`, and `cargo build`.

2026-04-22 - Completed `M3-002` by adding `memhub serve`, selecting the official `rmcp` crate for the stdio MCP server, and wiring thin MCP tools over existing services for project status, indexed search, task listing, recent decisions, latest command lookup, and explicit verified command recording. Verified with `cargo fmt`, `cargo test`, and `cargo build`.

2026-04-21 - Completed `M3-001` by hardening `memhub sync-md` with backup-before-rewrite behavior for existing markdown files, strict managed-marker validation, temp-file replacement writes, richer sync reporting, and regression tests for no-op, malformed-marker, and manual-content preservation paths. Verified with `cargo fmt`, `cargo test`, and `cargo build`.

2026-04-21 - Shipped `d8bcd05` by adding Milestone 2 git ingestion and indexed search plus the first Milestone 3 markdown-sync slice. The release adds `memhub ingest-git`, FTS-backed `memhub search`, `memhub sync-md`, init-time managed-block generation for `AGENTS.md` / `CLAUDE.md`, and optional auto-sync after managed-content writes. Verified with `cargo fmt`, `cargo test`, `cargo build`, then pushed `main` to `origin`.

2026-04-21 - Completed explicit command-history verification in `5689bb5` by adding `memhub command verify`, wiring command outcome writes into the existing `commands` table, and covering insert/update behavior with tests. Verified with `cargo test` and `cargo build`, then pushed `main` to `origin`.
## Open questions

- Should the next retrieval expansion index facts, tasks, or command history first now that MCP reads exist?
- Which additional `clientInfo.name` values do Codex and Claude Code send in real handshakes beyond the initial alias map?
- Should `memhub migrate` remain implicit-on-open or become explicit once external users adopt the tool?
- Should import also restore per-repo config and immediately run markdown sync, or should some parts stay opt-in during the first recovery slice?
