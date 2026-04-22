# Milestones

## Current Scaffold

This repository now covers the Milestone 1 foundation, the core Milestone 2 retrieval path, and the shipped markdown sync plus narrowed MCP/write-policy slices of Milestone 3:

- Rust CLI scaffold
- SQLite schema and migrations
- Config loading and persistence
- Logging and error handling
- `init`, `status`, `sync-md`, `ingest-git`, `search`, `fact add|list`, `decision add|list`, `task add|list|done`, `command list|verify`
- Git ingestion into `commits`, `files`, and `commit_files`
- FTS-backed decision search plus exact indexed file-history lookup
- Managed-block generation for `AGENTS.md` and `CLAUDE.md`
- Local stdio MCP server through `memhub serve`
- Thin MCP tools for status, search, task listing, recent decisions, latest command lookup, explicit verified command recording, and staged fact/decision proposals
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
- Status: core complete in the current codebase under the narrowed repo plan, with staged proposal writes and client alias normalization shipped; review/promotion flow remains deferred to Milestone 4

## Milestone 4: Trust and Maintenance

- Add review queue flows
- Add confidence scoring and stale data handling
- Add portable export/import as the supported repo backup and restore path
- Add missing-DB safety handling so an existing `.memhub/` without `project.sqlite` is treated as a recovery case
- Add follow-on restore UX around `init` or adjacent commands after export/import is stable
- Add readable README backup/restore instructions when recovery features ship
- Add deny-list enforcement for sensitive paths and patterns

## Milestone 5+

Treat everything beyond Milestone 4 as speculative until a new mini-PRD exists. That includes embeddings, desktop UI, file watchers, richer global DB behavior, and network-backed ingestion.
