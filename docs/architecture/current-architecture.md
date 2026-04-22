# Current Architecture

## What Exists Now

The current repository implements a single-binary Rust CLI with embedded SQLite migrations. A memhub-managed repo stores runtime state in `.memhub/`, specifically:

- `.memhub/project.sqlite` for durable project records
- `.memhub/config.toml` for per-repo config

The binary resolves the nearest ancestor containing `.memhub/`, opens the SQLite database, applies pending embedded migrations, and executes CLI commands against that local store.

## Implemented Subsystems

- CLI parsing via `clap`
- Config load/save via `serde` and `toml`
- SQLite access via `rusqlite`
- Schema bootstrap and migration tracking
- CRUD handlers for facts, decisions, tasks, and command history verification
- Audit logging through `writes_log`
- Placeholder modules for future MCP and markdown sync work

## Layout

- `src/main.rs` wires logging, CLI parsing, and process exit behavior.
- `src/cli/` defines the command surface and output formatting.
- `src/db/` handles path discovery, `.memhub/` bootstrap, connection setup, migrations, and `.gitignore` updates.
- `src/commands/` holds the actual CLI-useful operations.
- `src/config/`, `src/logging/`, and `src/errors/` keep infrastructure concerns narrow.
- `migrations/` contains the SQL schema applied by the embedded migration runner.

## Important Limits

- No MCP server exists yet.
- No git ingestion or search router exists yet.
- No markdown managed-block sync exists yet.
- No confidence decay, review queue, or deny-list enforcement exists yet.

Future docs should describe those pieces only after they are implemented.
