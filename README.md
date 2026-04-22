# memhub

`memhub` is a local-first Rust CLI for durable per-repo project memory shared across Codex and Claude Code. This scaffold implements a real Milestone 1 foundation: SQLite storage, migrations, config loading, logging, and a usable set of CLI commands for facts, decisions, tasks, and status.

## Product Authority

The authoritative product reference is [docs/reference/memhub-prd.md](docs/reference/memhub-prd.md). This repository intentionally narrows that PRD to a buildable Milestone 1 and leaves later milestones explicit instead of implied.

## Current Scope

Implemented now:

- `memhub init`
- `memhub status`
- `memhub fact add|list`
- `memhub decision add|list`
- `memhub task add|list|done`
- `memhub command list`
- Embedded SQLite migrations and per-repo config
- Audit logging through `writes_log`

Deferred for later milestones:

- MCP server
- Git history ingestion and search router
- Managed block sync for `AGENTS.md` / `CLAUDE.md`
- Review queue, confidence decay, export/import, deny-list enforcement

## Build

```bash
cargo build
```

## Run

Initialize the current repository for memhub:

```bash
cargo run -- init
```

Check status:

```bash
cargo run -- status
```

Add and list durable facts:

```bash
cargo run -- fact add build-command "cargo build"
cargo run -- fact list
```

Add and list decisions:

```bash
cargo run -- decision add "Use rusqlite bundled mode" --rationale "Avoid system SQLite setup friction during early development."
cargo run -- decision list
```

Track tasks:

```bash
cargo run -- task add "Implement MCP server" --notes "Milestone 3"
cargo run -- task list
cargo run -- task done 1
```

List stored commands:

```bash
cargo run -- command list
```

## Repository Layout

- `src/` - CLI, config, DB, command handlers, logging, error types
- `migrations/` - embedded SQL schema migrations
- `docs/reference/` - preserved PRD and implementation notes
- `docs/architecture/` - actual current architecture
- `docs/roadmap/` - milestone execution plan
- `agent_docs/` - durable continuity files for future agent sessions

## Notes

- The project stores runtime state in `.memhub/` inside the repository you initialize.
- The current code auto-applies embedded migrations when the CLI opens an initialized project. An explicit `memhub migrate` command is deferred.
- `command list` is present because the schema includes command history, but command verification/recording is intentionally deferred.
