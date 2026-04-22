# Project State

Last updated: 2026-04-21

## Currently building

Milestone 1 foundation for `memhub`: a real Rust CLI with local SQLite storage, migrations, config, audit logging, and usable commands for facts, decisions, tasks, status, and command verification. The PRD is preserved verbatim in `docs/reference/memhub-prd.md`.

## Next up

1. Add git ingestion plus indexed search for Milestone 2.
2. Add MCP and markdown managed-block sync only after the CLI foundation is stable.

## Last session

2026-04-21 - Added `memhub command verify` so command history can record explicit exit-code outcomes into the existing `commands` table and `writes_log`.

## Open questions

- Which Rust MCP crate is the right fit when Milestone 3 starts?
- Which text sources should seed the first FTS chunk set in Milestone 2?
- Should `memhub migrate` remain implicit-on-open or become explicit once external users adopt the tool?
