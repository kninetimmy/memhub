# Milestones

## Current Scaffold

This repository delivers the Milestone 1 foundation and intentionally stops there:

- Rust CLI scaffold
- SQLite schema and migrations
- Config loading and persistence
- Logging and error handling
- `init`, `status`, `fact add|list`, `decision add|list`, `task add|list|done`, `command list`
- Audit logging for writes

## Milestone 2: Git + Search

- Add git ingestion into `commits`, `commit_files`, and `files`
- Add FTS-backed text chunks
- Add a rule-based search path from the CLI
- Add query-plan-aware tests for hot queries

## Milestone 3: MCP + Markdown Sync

- Add an MCP server with thin wrappers over read/write services
- Add explicit markdown managed-block sync for `AGENTS.md` and `CLAUDE.md`
- Start enforcing write-back policy boundaries for agent-originated data

## Milestone 4: Trust and Maintenance

- Add review queue flows
- Add confidence scoring and stale data handling
- Add export/import and maintenance commands
- Add deny-list enforcement for sensitive paths and patterns

## Milestone 5+

Treat everything beyond Milestone 4 as speculative until a new mini-PRD exists. That includes embeddings, desktop UI, file watchers, richer global DB behavior, and network-backed ingestion.
