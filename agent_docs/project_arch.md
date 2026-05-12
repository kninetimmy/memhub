# Project Architecture

## Purpose

`memhub` is a local-first per-repo memory CLI that aims to give Codex and Claude Code one shared durable source of project context. By PRD §16 milestones the project is at v1: the current implementation covers the Milestone 1 foundation, Milestone 2 retrieval (git ingestion + indexed search), the shipped markdown-sync and narrowed MCP/write-policy slices of Milestone 3, all of Milestone 4 (export/import recovery, missing-DB safety with `init --from-backup`, the `memhub review` flow, path-based deny-list filtering, and the 90-day fact staleness + derived command confidence pass), and the memhub side of Milestone 5 K9 framework interop (detection + config, the v1 K9 `/wrap-up` shell-out contract, machine-readable `--json` on every read and mutating command K9 needs, plus PRD-§17 `memhub stats` dogfood tooling and PRD-§12 free-form session notes via the `log_session_note` MCP tool).

## Stack and Versions

- Rust 2024 edition
- `clap` for CLI parsing
- `rusqlite` with bundled SQLite
- `serde` + `toml` for config
- `globset` for deny-list glob matching
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
- Command handlers perform real writes for facts, decisions, tasks, explicit command verification, git ingestion, and staged pending writes, plus the `commands::review` flow that promotes/rejects/expires staged proposals, and log those writes to `writes_log`.
- Search uses SQLite FTS5 over `chunks` for decision text and exact indexed lookups for file history through `files` and `commit_files`.
- The MCP layer serves a local stdio server through `memhub serve` and currently exposes thin tool adapters for status, search, task listing, decision listing, latest-command lookup, explicit verified command recording, staged fact/decision proposals, and read-only `list_pending_writes` for staged-proposal review surfaces (notably K9 wrap-up). It preserves the exact raw `clientInfo.name`, normalizes aliases from a trimmed copy, sanitizes client names before logging, and stores available MCP request/init provenance JSON with staged writes.
- Markdown sync rewrites only explicit managed sections in `AGENTS.md` and `CLAUDE.md`, validates that each file has at most one well-formed managed block pair, creates timestamped backups for changed existing files under `.memhub/backups/markdown/`, and uses temp-file replacement writes. It can run explicitly or after writes when `auto_sync_md` is enabled.
- Export/import provides the supported recovery path. `memhub export` writes a version-tagged JSON file covering facts, decisions, tasks, commands, pending writes, and the writes log; derived data (git ingestion, FTS chunks, schema migrations) is excluded. `memhub import` validates the format version, refuses on non-empty targets unless `--force` is passed, wipes durable tables plus decision chunks in a single transaction with `PRAGMA defer_foreign_keys = ON`, restores rows with their original IDs, regenerates decision chunks, appends a `writes_log` entry for the restore event, and runs `sync-md` after commit.
- `memhub init --from-backup <path>` is the single-step recovery convenience UX. It refuses to run when `.memhub/project.sqlite` already exists, then uses `db::init_project_for_recovery` to create `.memhub/` (if missing) and run migrations before delegating to the existing `commands::import::run` path. This covers both the clean-clone case and the missing-database case without making plain `memhub init` interactive.
- `commands::review` provides the explicit review and promotion flow for staged MCP proposals. `accept` delegates to `fact::add` / `decision::add` so promoted rows reuse all existing audit, FTS, and sync-md plumbing; `reject` and `expire` only mutate the pending row and `writes_log`. The `pending_writes` table gained a `reviewed_at` column in migration `0005_pending_write_reviewed_at` that is stamped on any transition out of `pending`. Acceptance runs against fresh database connections (not a single transaction), so a failure between durable promotion and pending-row update leaves the pending row in `pending` for safe retry, and `fact::add`'s `(project_id, key)` upsert keeps that retry idempotent.
- `config::deny` defines the per-repo path deny list. `ProjectConfig::deny_list` is a `serde(default)` field so existing `config.toml` files without the section automatically pick up the shipped defaults. `PathMatcher::from_patterns` compiles the patterns through `globset` and fails closed on invalid globs. `commands::ingest_git` builds the matcher at the start of a run and skips denied paths before any `files`/`commit_files` insert, surfacing `denied_files_skipped` in the summary. `commands::search` builds the same matcher and post-filters file-history hits (both prefixed `file:` lookups and inferred path lookups); denied direct lookups return a normal empty result without leakage. The current scope is path-based only; content scanning and historical-data cleanup remain out of scope.
- `commands::stats` is a pure read-only aggregation surface for PRD §17 dogfood metrics. It joins counts across `facts` (incl. stale ratio), `decisions`, `tasks`, `commands`, `commits`, `files`, `chunks`, `pending_writes`, and `writes_log` and produces a single `StatsSummary` shaped around a `StatsWindow` (`Days(i64)` or `All`). Windowed activity is sourced from `writes_log` alone — PRD §17's "simple read counter" is intentionally not instrumented; the omission is surfaced in both human and JSON output rather than being silently incomplete. No schema change, no migration.
- `commands::session_note` plus the `session_notes` table (migration `0006_session_notes`) provide free-form agent scratch space per PRD §12. The shape is `(id, project_id, actor, actor_raw, text, created_at)` indexed by `created_at DESC`. Writes go through the MCP `log_session_note` tool, which pulls actor identity from `clientInfo.name` via the same `ClientIdentity` plumbing as `propose_fact` / `propose_decision` and emits a `writes_log` row for audit. Notes are intentionally write-only — no promotion path to facts or decisions, no FTS index, no `note add` CLI, and no inclusion in the v1 `memhub export` format. Reads land in `memhub note list [--limit] [--actor] [--since-days] [--json]`. If notes ever start carrying durable value, a `v2` export format ships separately to include them.

## Security Invariants

- Runtime state stays local to the repository under `.memhub/`.
- No network behavior is part of the product runtime in this scaffold.
- Agent-authored automation features are deferred until write policy is implemented.

## Runtime Layout

Single local CLI process with an embedded SQLite database plus an on-demand stdio MCP server. No background services, listening ports, or external APIs.

## Known Gaps / Out of Scope

- Broader indexed retrieval beyond exact file history, decision FTS, latest-command lookup, and the narrow MCP read tools (e.g. fact / task / session-note search) is deferred until a real workflow demands it
- Continuous confidence decay (exponential or otherwise); a persisted `commands.confidence` column; decision-level confidence; a configurable staleness threshold — all explicitly out of v1 scope per the 2026-05-12 decision
- Read-counter half of PRD §17 (`memhub stats` currently surfaces write activity from `writes_log` only)
- Content-scanning deny rules (current deny list is path-based only)
- Garbage collection of already-ingested denied paths after a pattern change
- Session notes in the export format (v1 export deliberately omits `session_notes`; a `v2` export would change this if notes become durable)
