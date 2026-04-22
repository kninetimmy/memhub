# Project Decisions

Append-only. Superseding decisions should be added as new dated entries rather than rewriting old ones.

---

## 2026-04-21 - Initialized project_docs framework

- Adopted `AGENTS.md`, `CLAUDE.md`, and `agent_docs/` as durable continuity files for future agent runs.

## 2026-04-21 - Milestone 1 stays intentionally narrow

- The initial scaffold implements CLI, SQLite, migrations, config, logging, and basic CRUD for facts, decisions, tasks, and status.
- MCP, git ingestion, markdown sync, router logic, confidence decay, and advanced write-back remain deferred.

## 2026-04-21 - One-database-per-repo schema still keeps `project_id`

- Even though each `.memhub/project.sqlite` only serves one repo, the schema keeps a `projects` row plus `project_id` foreign keys to stay close to the PRD and reduce future migration churn.

## 2026-04-21 - Migrations auto-apply when the CLI opens a project

- An explicit `memhub migrate` command is deferred.
- This keeps the current scaffold usable while still preserving numbered SQL migrations in the repository.

## 2026-04-21 - Milestone 1 command verification uses explicit exit-code recording

- The CLI records command history through `memhub command verify` instead of adding automated capture or a review queue early.
- Exit code is the Milestone 1 verification signal for command history in the existing schema; richer verification metadata remains deferred with the broader write-policy work.

## 2026-04-21 - Initial Milestone 2 search indexes decision text plus exact file history

- `memhub search` uses exact indexed file-path lookups for history queries and SQLite FTS5 over decision text for free-text retrieval.
- Additional chunk sources can be added later, but the first implementation stays narrow so the router remains explainable and the query-plan tests stay simple.

## 2026-04-22 - README should be refreshed at each milestone completion

- When a milestone or a major milestone slice is completed, update `README.md` so the public project description, current capabilities, and roadmap status reflect the new state of the codebase.
- Treat README maintenance as part of milestone completion rather than a later cleanup task.

## 2026-04-22 - Milestone 3 MCP work uses the official `rmcp` Rust SDK

- `memhub` uses `rmcp` for the first MCP server slice because it is the official Rust SDK and supports the stdio server transport needed for the local-first CLI workflow.
- The initial MCP surface stays narrow: thin adapters over existing services plus explicit verified command recording, while broader agent-originated write policy remains deferred.

## 2026-04-22 - Milestone 4 recovery work ships in two slices

- The supported recovery path should start with portable `memhub export` / `memhub import`, not a raw database-file copy workflow.
- `memhub init` should stay non-interactive at first; any convenience recovery UX should layer on after export/import exists and is stable.
- If `.memhub/` exists but `.memhub/project.sqlite` does not, `memhub` should treat that as an explicit recovery/safety case instead of silently creating a fresh database.
- When recovery features ship, `README.md` should gain a clear backup/restore section with readable step-by-step instructions.

## 2026-04-22 - M3-003 stages agent-originated MCP writes instead of promoting them directly

- Agent-originated MCP fact and decision writes land in `pending_writes`, not `facts` or `decisions`.
- `memhub status` and the MCP `status` tool expose pending-write count so staged proposals are visible before a review UX exists.
- MCP client identity is derived from `clientInfo.name`, normalized for known Codex and Claude Code aliases, and stored alongside the raw observed value.
