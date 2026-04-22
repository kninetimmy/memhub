# Milestones

## Current Scaffold

This repository now covers the Milestone 1 foundation, the core Milestone 2 retrieval path, and the first Milestone 3 markdown sync slice:

- Rust CLI scaffold
- SQLite schema and migrations
- Config loading and persistence
- Logging and error handling
- `init`, `status`, `sync-md`, `ingest-git`, `search`, `fact add|list`, `decision add|list`, `task add|list|done`, `command list|verify`
- Git ingestion into `commits`, `files`, and `commit_files`
- FTS-backed decision search plus exact indexed file-history lookup
- Managed-block generation for `AGENTS.md` and `CLAUDE.md`
- Audit logging for writes

## Milestone 2: Git + Search

- Add git ingestion into `commits`, `commit_files`, and `files`
- Add FTS-backed text chunks
- Add a rule-based search path from the CLI
- Add query-plan-aware tests for hot queries
- Status: core complete in the current codebase

## Milestone 3: MCP + Markdown Sync

- Add an MCP server with thin wrappers over read/write services
- Add explicit markdown managed-block sync for `AGENTS.md` and `CLAUDE.md`
- Start enforcing write-back policy boundaries for agent-originated data
- Status: markdown sync core is in the current codebase; MCP is still pending

## Milestone 4: Trust and Maintenance

- Add review queue flows
- Add confidence scoring and stale data handling
- Add export/import and maintenance commands
- Add deny-list enforcement for sensitive paths and patterns

## Milestone 5+

Treat everything beyond Milestone 4 as speculative until a new mini-PRD exists. That includes embeddings, desktop UI, file watchers, richer global DB behavior, and network-backed ingestion.
