# PRD Implementation Notes

## Core Value Proposition

`memhub` exists to keep one local, per-repo source of durable project memory that both Codex and Claude Code can rely on. The value is shared context with less manual note drift, not feature count.

## Actual V1 Target

This scaffold targets Milestone 1 from the PRD:

- Rust CLI
- Per-repo SQLite database in `.memhub/project.sqlite`
- Embedded SQL migrations
- Per-repo config in `.memhub/config.toml`
- Lean logging and error handling
- Usable commands for `init`, `status`, `fact`, `decision`, `task`, and `command list`
- Audit logging for durable writes

## Explicitly Deferred

- MCP server and tool surface
- Git ingestion, file history, FTS, and the query router
- Managed-block sync into `AGENTS.md` / `CLAUDE.md`
- Review queue, stale/confidence logic, and advanced write-back policy
- Export/import, garbage collection, and deny-list enforcement

## Narrowing Decisions In This Scaffold

- The schema keeps a `projects` table and `project_id` foreign keys to stay close to the PRD, even though each DB only serves one repo.
- Migrations run automatically when the CLI opens an initialized project. The explicit `memhub migrate` command is deferred to keep Milestone 1 lean.
- The `commands` table exists now, but command recording and verification are deferred. `command list` is intentionally read-only in this scaffold.

## Open Decisions To Revisit

- Rust MCP crate choice and server shape at Milestone 3
- Exact FTS chunk sources and indexing rules once search is built
- Managed-block sync behavior and backup semantics for markdown files
- Review queue UX for untrusted agent writes
