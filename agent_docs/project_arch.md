# Project Architecture

## Purpose

`memhub` is a local-first per-repo memory CLI that aims to give Codex and Claude Code one shared durable source of project context. The current implementation covers Milestone 2's retrieval path, the shipped markdown-sync and narrowed MCP/write-policy slices of Milestone 3, and the Milestone 4 portable export/import recovery path plus missing-DB safety with `init --from-backup` recovery, while still avoiding speculative subsystems.

## Stack and Versions

- Rust 2024 edition
- `clap` for CLI parsing
- `rusqlite` with bundled SQLite
- `serde` + `toml` for config
- `env_logger` + `log` for lightweight logging
- `tokio` for the local async runtime used by the MCP server
- `rmcp` for the local stdio MCP server and tool wiring

## Layout

- `src/cli/` - top-level CLI command definitions and output formatting
- `src/commands/` - core command handlers, including `export` and `import` for portable recovery
- `src/export/` - version-tagged on-disk format types (`v1`) for export/import
- `src/config/` - per-repo config model and read/write helpers
- `src/db/` - path discovery, connection bootstrap, migrations, `.gitignore` handling
- `src/models/` - small data structs used by the CLI layer
- `src/mcp/` - stdio MCP server and thin tool adapters over existing services
- `src/sync_md/` - markdown managed-block rendering and file rewrite logic
- `migrations/` - numbered SQL files embedded into the binary
- `docs/` - preserved PRD, implementation notes, architecture, roadmap

## Key Subsystems

- Project bootstrap resolves or creates `.memhub/` in a repository root. `db::open_project` and `db::init_project` both refuse to silently rebuild `project.sqlite` inside an existing `.memhub/`, returning `MemhubError::MissingDatabase`; `db::init_project_for_recovery` is the explicit recovery-mode entry point used by `memhub init --from-backup`.
- The DB layer applies migrations and maintains a single `projects` row.
- Command handlers perform real writes for facts, decisions, tasks, explicit command verification, git ingestion, and staged pending writes, and log those writes to `writes_log`.
- Search uses SQLite FTS5 over `chunks` for decision text and exact indexed lookups for file history through `files` and `commit_files`.
- The MCP layer serves a local stdio server through `memhub serve` and currently exposes thin tool adapters for status, search, task listing, decision listing, latest-command lookup, explicit verified command recording, and staged fact/decision proposals. It preserves the exact raw `clientInfo.name`, normalizes aliases from a trimmed copy, sanitizes client names before logging, and stores available MCP request/init provenance JSON with staged writes.
- Markdown sync rewrites only explicit managed sections in `AGENTS.md` and `CLAUDE.md`, validates that each file has at most one well-formed managed block pair, creates timestamped backups for changed existing files under `.memhub/backups/markdown/`, and uses temp-file replacement writes. It can run explicitly or after writes when `auto_sync_md` is enabled.
- Export/import provides the supported recovery path. `memhub export` writes a version-tagged JSON file covering facts, decisions, tasks, commands, pending writes, and the writes log; derived data (git ingestion, FTS chunks, schema migrations) is excluded. `memhub import` validates the format version, refuses on non-empty targets unless `--force` is passed, wipes durable tables plus decision chunks in a single transaction with `PRAGMA defer_foreign_keys = ON`, restores rows with their original IDs, regenerates decision chunks, appends a `writes_log` entry for the restore event, and runs `sync-md` after commit.
- `memhub init --from-backup <path>` is the single-step recovery convenience UX. It refuses to run when `.memhub/project.sqlite` already exists, then uses `db::init_project_for_recovery` to create `.memhub/` (if missing) and run migrations before delegating to the existing `commands::import::run` path. This covers both the clean-clone case and the missing-database case without making plain `memhub init` interactive.

## Security Invariants

- Runtime state stays local to the repository under `.memhub/`.
- No network behavior is part of the product runtime in this scaffold.
- Agent-authored automation features are deferred until write policy is implemented.

## Runtime Layout

Single local CLI process with an embedded SQLite database plus an on-demand stdio MCP server. No background services, listening ports, or external APIs.

## Known Gaps / Out of Scope

- Search coverage beyond exact file history plus decision FTS
- Review/promotion flow for staged agent-originated writes
- Confidence decay, review queue, and deny-list enforcement
