# Project State

Last updated: 2026-04-22

## Currently building

Milestone 3 is now in the MCP phase after completing `M3-002`. The current codebase now has the hardened markdown sync path plus a local stdio MCP server exposed through `memhub serve`, with thin adapters for status, search, task listing, decision listing, latest-command lookup, and explicit verified command recording.

## Next up

1. Decide whether MCP needs broader indexed retrieval over facts, tasks, or command history beyond the current narrow paths.
2. Design the next Milestone 3 write-policy slice for agent-originated fact/decision proposals without skipping review boundaries.
3. Add client identification and alias normalization around MCP initialization once real client handshakes are observed.

## Last session

2026-04-22 - Completed `M3-002` by adding `memhub serve`, selecting the official `rmcp` crate for the stdio MCP server, and wiring thin MCP tools over existing services for project status, indexed search, task listing, recent decisions, latest command lookup, and explicit verified command recording. Verified with `cargo fmt`, `cargo test`, and `cargo build`.

2026-04-21 - Completed `M3-001` by hardening `memhub sync-md` with backup-before-rewrite behavior for existing markdown files, strict managed-marker validation, temp-file replacement writes, richer sync reporting, and regression tests for no-op, malformed-marker, and manual-content preservation paths. Verified with `cargo fmt`, `cargo test`, and `cargo build`.

2026-04-21 - Shipped `d8bcd05` by adding Milestone 2 git ingestion and indexed search plus the first Milestone 3 markdown-sync slice. The release adds `memhub ingest-git`, FTS-backed `memhub search`, `memhub sync-md`, init-time managed-block generation for `AGENTS.md` / `CLAUDE.md`, and optional auto-sync after managed-content writes. Verified with `cargo fmt`, `cargo test`, `cargo build`, then pushed `main` to `origin`.

2026-04-21 - Completed explicit command-history verification in `5689bb5` by adding `memhub command verify`, wiring command outcome writes into the existing `commands` table, and covering insert/update behavior with tests. Verified with `cargo test` and `cargo build`, then pushed `main` to `origin`.
## Open questions

- Should the next retrieval expansion index facts, tasks, or command history first now that MCP reads exist?
- What is the narrowest safe MCP write surface for agent-originated facts and decisions before a review queue lands?
- Which `clientInfo.name` values do Codex and Claude Code actually send in real MCP handshakes for alias normalization?
- Should `memhub migrate` remain implicit-on-open or become explicit once external users adopt the tool?
