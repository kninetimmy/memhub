# Project State

Last updated: 2026-04-21

## Currently building

Milestone 1 foundation for `memhub`: a real Rust CLI with local SQLite storage, migrations, config, audit logging, and usable commands for facts, decisions, tasks, and status. The PRD is preserved verbatim in `docs/reference/memhub-prd.md`.

## Next up

1. Make command history useful by adding command recording and verification.
2. Add git ingestion plus indexed search for Milestone 2.
3. Add MCP and markdown managed-block sync only after the CLI foundation is stable.

## Last session

2026-04-21 - Initialized project docs and scaffolded the Milestone 1 repository foundation from the PRD.

## Open questions

- Which Rust MCP crate is the right fit when Milestone 3 starts?
- Which text sources should seed the first FTS chunk set in Milestone 2?
- Should `memhub migrate` remain implicit-on-open or become explicit once external users adopt the tool?
