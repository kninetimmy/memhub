# Project State

Last updated: 2026-04-22

## Currently building

Between tasks after `M3-003`. The current codebase now has the hardened markdown sync path plus a local stdio MCP server exposed through `memhub serve`, with thin adapters for status, search, task listing, decision listing, latest-command lookup, explicit verified command recording, and staged fact/decision proposals backed by `pending_writes`. Exact raw client names are preserved from the initialize handshake, log output is sanitized, and staged writes now store the available MCP request/init provenance in JSON form.

## Next up

1. Start `M4-001` with portable `memhub export` / `memhub import` as the supported recovery path.
2. Decide whether the first import slice should also restore per-repo config and run markdown sync automatically.
3. After `M4-001`, implement `M4-002` missing-DB safety so an existing `.memhub/` without `project.sqlite` is treated as recovery, not silent re-init.
4. Decide whether MCP needs broader indexed retrieval over facts, tasks, or command history beyond the current narrow paths.

## Last session

2026-04-22 - Tightened MCP proposal provenance handling by preserving exact raw `clientInfo.name`, sanitizing client names before logging, and storing available MCP request/init provenance JSON on `pending_writes` via migration `0004_pending_write_provenance`. Verified with `cargo fmt`, `cargo test`, and `cargo build`.

2026-04-22 - Completed `M3-003` by adding a `pending_writes` table, staged MCP `propose_fact` / `propose_decision` tools, pending-write visibility in CLI and MCP status, and `clientInfo.name` alias normalization with raw-value preservation. Verified with `cargo fmt`, `cargo test`, and `cargo build`.

2026-04-22 - Completed `M3-002` by adding `memhub serve`, selecting the official `rmcp` crate for the stdio MCP server, and wiring thin MCP tools over existing services for project status, indexed search, task listing, recent decisions, latest command lookup, and explicit verified command recording. Verified with `cargo fmt`, `cargo test`, and `cargo build`.
## Open questions

- Should the next retrieval expansion index facts, tasks, or command history first now that MCP reads exist?
- Which additional `clientInfo.name` values do Codex and Claude Code send in real handshakes beyond the initial alias map?
- Should `memhub migrate` remain implicit-on-open or become explicit once external users adopt the tool?
- Should import also restore per-repo config and immediately run markdown sync, or should some parts stay opt-in during the first recovery slice?
