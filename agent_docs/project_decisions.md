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
