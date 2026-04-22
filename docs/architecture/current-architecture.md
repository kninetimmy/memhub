# Current Architecture

## What Exists Now

The current repository implements a single-binary Rust CLI with embedded SQLite migrations, git ingestion, indexed search, hardened markdown sync, and an on-demand stdio MCP server. A memhub-managed repo stores runtime state in `.memhub/`, specifically:

- `.memhub/project.sqlite` for durable project records
- `.memhub/config.toml` for per-repo config

The binary resolves the nearest ancestor containing `.memhub/`, opens the SQLite database, applies pending embedded migrations, and executes CLI commands against that local store.

## Implemented Subsystems

- CLI parsing via `clap`
- Config load/save via `serde` and `toml`
- SQLite access via `rusqlite`
- MCP server wiring via `rmcp`
- Schema bootstrap and migration tracking
- CRUD handlers for facts, decisions, tasks, command history verification, git ingestion, and markdown sync
- Staged pending-write handling for agent-originated fact and decision proposals
- FTS5-backed search chunks for decision text plus exact file-history queries
- Managed-block generation for `AGENTS.md` and `CLAUDE.md`, with optional auto-sync after writes
- Stdio MCP tools for status, search, task listing, recent decision listing, latest-command lookup, explicit verified command recording, and staged fact/decision proposals
- Audit logging through `writes_log`

## Layout

- `src/main.rs` wires logging, CLI parsing, and process exit behavior.
- `src/cli/` defines the command surface and output formatting.
- `src/db/` handles path discovery, `.memhub/` bootstrap, connection setup, migrations, and `.gitignore` updates.
- `src/commands/` holds the actual CLI-useful operations.
- `src/config/`, `src/logging/`, and `src/errors/` keep infrastructure concerns narrow.
- `migrations/` contains the SQL schema applied by the embedded migration runner.

## Important Limits

- Search routing is still intentionally narrow: exact file-path history and decision-text FTS only.
- No review or promotion flow exists yet for staged agent-originated writes.
- No confidence decay, review queue, or deny-list enforcement exists yet.

Future docs should describe those pieces only after they are implemented.
