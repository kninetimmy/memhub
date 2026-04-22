# memhub

`memhub` is a local-first Rust CLI for durable per-repo project memory shared across Codex and Claude Code. The current codebase now covers the Milestone 1 foundation plus the core of Milestone 2: git ingestion and indexed search on top of the local SQLite store.

## Product Authority

The authoritative product reference is [docs/reference/memhub-prd.md](docs/reference/memhub-prd.md). This repository intentionally implements the PRD in narrow, usable milestones instead of implying later subsystems early.

## Current Scope

Implemented now:

- `memhub init`
- `memhub status`
- `memhub sync-md`
- `memhub ingest-git [--since <ref>]`
- `memhub search <query>`
- `memhub fact add|list`
- `memhub decision add|list`
- `memhub task add|list|done`
- `memhub command list|verify`
- Embedded SQLite migrations and per-repo config
- Audit logging through `writes_log`
- SQLite FTS5-backed decision search plus exact file-history lookups

Deferred for later milestones:

- MCP server
- Review queue, confidence decay, export/import, deny-list enforcement
- MCP server

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
cargo run -- command verify build "cargo build" --exit-code 0
```

Ingest git history and search it:

```bash
cargo run -- ingest-git
cargo run -- search src/lib.rs
cargo run -- search "decisions about sqlite"
```

Regenerate managed markdown blocks:

```bash
cargo run -- sync-md
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
- `memhub search` currently stays narrow on purpose: exact file-history lookups and FTS-backed decision retrieval only.
- `memhub init` creates or refreshes the managed block in `AGENTS.md` and `CLAUDE.md`, and later writes can auto-sync it when `auto_sync_md = true`.
