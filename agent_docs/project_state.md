# Project State

Last updated: 2026-04-21

## Currently building

Milestone 3 has started: the CLI now supports managed-block generation for `AGENTS.md` and `CLAUDE.md` through `memhub sync-md`, while git ingestion and indexed search remain the current retrieval base.

## Next up

1. Start Milestone 3 with markdown managed-block sync for `AGENTS.md` and `CLAUDE.md`.
2. Add MCP read/write adapters over the existing CLI services after the markdown sync shape is stable.
3. Tighten markdown sync backup semantics and decide whether more managed content belongs in the generated block.

## Last session

2026-04-21 - Started Milestone 3 in the working tree by adding `memhub sync-md`, managed-block generation for `AGENTS.md` / `CLAUDE.md`, init-time sync, and optional `auto_sync_md` rewrites after managed-content writes. Verified with `cargo test`.

2026-04-21 - Implemented Milestone 2 core in the working tree by adding `memhub ingest-git`, the `commits` / `files` / `commit_files` / `chunks` schema, FTS-backed `memhub search`, and query-plan-aware tests for exact file-history and decision search. Verified with `cargo test` and `cargo build`.

## Open questions

- Which Rust MCP crate is the right fit when Milestone 3 starts?
- Should Milestone 2 search index facts, tasks, or command history before MCP depends on it?
- What backup behavior should `sync-md` use before rewriting managed blocks?
- Should `memhub migrate` remain implicit-on-open or become explicit once external users adopt the tool?
