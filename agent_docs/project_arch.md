# Project Architecture

## Purpose

`memhub` is a local-first per-repo memory CLI that aims to give Codex and Claude Code one shared durable source of project context. The current implementation now covers the core of Milestone 2 plus the first markdown-sync slice of Milestone 3 while still avoiding speculative subsystems.

## Stack and Versions

- Rust 2024 edition
- `clap` for CLI parsing
- `rusqlite` with bundled SQLite
- `serde` + `toml` for config
- `env_logger` + `log` for lightweight logging

## Layout

- `src/cli/` - top-level CLI command definitions and output formatting
- `src/commands/` - core command handlers
- `src/config/` - per-repo config model and read/write helpers
- `src/db/` - path discovery, connection bootstrap, migrations, `.gitignore` handling
- `src/models/` - small data structs used by the CLI layer
- `src/mcp/` - explicit placeholder for future MCP work
- `src/sync_md/` - markdown managed-block rendering and file rewrite logic
- `migrations/` - numbered SQL files embedded into the binary
- `docs/` - preserved PRD, implementation notes, architecture, roadmap

## Key Subsystems

- Project bootstrap resolves or creates `.memhub/` in a repository root.
- The DB layer applies migrations and maintains a single `projects` row.
- Command handlers perform real writes for facts, decisions, tasks, explicit command verification, and git ingestion, and log those writes to `writes_log`.
- Search uses SQLite FTS5 over `chunks` for decision text and exact indexed lookups for file history through `files` and `commit_files`.
- Markdown sync rewrites only explicit managed sections in `AGENTS.md` and `CLAUDE.md`, validates that each file has at most one well-formed managed block pair, creates timestamped backups for changed existing files under `.memhub/backups/markdown/`, and uses temp-file replacement writes. It can run explicitly or after writes when `auto_sync_md` is enabled.

## Security Invariants

- Runtime state stays local to the repository under `.memhub/`.
- No network behavior is part of the product runtime in this scaffold.
- Agent-authored automation features are deferred until write policy is implemented.

## Runtime Layout

Single local CLI process with an embedded SQLite database. No background services, ports, or external APIs.

## Known Gaps / Out of Scope

- MCP server
- Search coverage beyond exact file history plus decision FTS
- Confidence decay, review queue, export/import, and deny-list enforcement
