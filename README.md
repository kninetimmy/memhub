# memhub

`memhub` is a local-first Rust CLI for durable per-repo project memory shared across Codex and Claude Code. It gives a repository one structured source of truth for facts, decisions, tasks, command history, git-derived context, and managed project-state blocks in `AGENTS.md` and `CLAUDE.md`.

The long-term product direction is a shared memory layer that both agents can read and write through a local interface. The current implementation already provides a usable local database, CLI workflows, git ingestion, indexed search, hardened markdown sync, and a first local MCP server slice, while later milestones add broader write policy and trust tooling.

## Development Status

`memhub` is in active development and is currently in Milestone 3 after completing `M3-002`.

Current state:

- Shipped: Milestone 1 foundations, Milestone 2 git ingestion and indexed search, and Milestone 3 markdown sync plus the first stdio MCP server slice
- Current focus: broaden Milestone 3 safely without skipping agent write-policy boundaries
- Implemented now: local SQLite storage, embedded migrations, per-repo config, audit logging, facts/decisions/tasks/commands CRUD, explicit command verification, git ingestion, indexed search, managed-block sync for `AGENTS.md` / `CLAUDE.md`, and `memhub serve` for stdio MCP access
- Not implemented yet: review queue, confidence decay, export/import, deny-list enforcement, broader agent-originated write paths, and broader search coverage beyond current indexed paths

Milestone status:

| Milestone | Status | Notes |
|-----------|--------|-------|
| Milestone 1: DB + CLI | Complete | Core repo bootstrap, schema, CRUD, config, logging |
| Milestone 2: Git + search | Complete | `ingest-git`, FTS-backed decision search, exact file-history lookups |
| Milestone 3: MCP + markdown sync | Current | Markdown sync and the first MCP adapter slice are shipped; broader write-policy work remains |
| Milestone 4: Quality | Planned | Review flow, confidence/staleness, export/import, deny-list work |
| Milestone 5+ | Planned | Speculative future expansions only after separate validation |

## Why memhub exists

Codex and Claude Code each have their own project-context entry points, but real workflows often move between both tools on the same repository over long gaps. That creates duplicated notes, drift between `AGENTS.md` and `CLAUDE.md`, and project state trapped in chat history.

`memhub` solves that with one local database per repository. Instead of treating markdown notes as the primary store, it treats the database as the durable source of truth and uses managed markdown sections as a synced, human-readable summary.

## What You Can Do Today

The current codebase already supports a practical local workflow:

- Initialize a repository with `memhub init`
- Inspect project state with `memhub status`
- Store and list durable facts with `memhub fact add|list`
- Store and list architectural or workflow decisions with `memhub decision add|list`
- Track work with `memhub task add|list|done`
- Record verified command outcomes with `memhub command verify` and inspect them with `memhub command list`
- Ingest git history with `memhub ingest-git [--since <ref>]`
- Search current indexed memory with `memhub search <query>`
- Regenerate managed blocks in `AGENTS.md` and `CLAUDE.md` with `memhub sync-md`
- Serve the current repository over stdio MCP with `memhub serve`

## Install and Quick Start

`memhub` currently runs from source as a standard Rust CLI.

Build the project:

```bash
cargo build
```

Initialize the current repository for `memhub`:

```bash
cargo run -- init
```

Check the current project summary:

```bash
cargo run -- status
```

Add a fact and list stored facts:

```bash
cargo run -- fact add build-command "cargo build"
cargo run -- fact list
```

Record a decision and list decisions:

```bash
cargo run -- decision add "Use rusqlite bundled mode" --rationale "Avoid system SQLite setup friction during early development."
cargo run -- decision list
```

Track a task:

```bash
cargo run -- task add "Implement MCP server" --notes "Milestone 3"
cargo run -- task list
cargo run -- task done 1
```

Record and inspect a verified command:

```bash
cargo run -- command verify build "cargo build" --exit-code 0
cargo run -- command list
```

Ingest git history and search the indexed store:

```bash
cargo run -- ingest-git
cargo run -- search src/lib.rs
cargo run -- search "sqlite decisions"
```

Refresh managed markdown blocks:

```bash
cargo run -- sync-md
```

Start the local stdio MCP server:

```bash
cargo run -- serve
```

## How memhub works

### Per-repo source of truth

Running `memhub init` in a repository root creates a local `.memhub/` directory for that project. Today it contains:

- `.memhub/project.sqlite` as the durable database
- `.memhub/config.toml` for per-repo settings
- `.memhub/backups/markdown/` when managed markdown files are rewritten and need backups

The project database is the source of truth. Markdown is a synced view, not the canonical store.

### Command flow

Each CLI invocation walks up from the current working directory to find the nearest `.memhub/` ancestor, opens the local SQLite database, applies any pending embedded migrations, and executes the requested command against that local store. `memhub serve` uses that same project discovery rule, then exposes the current repository through a local stdio MCP server.

That keeps runtime behavior simple:

- one binary
- one per-repo database
- no required network access
- no background service
- MCP transport only when explicitly started

### Managed markdown sync

`memhub` treats `AGENTS.md` and `CLAUDE.md` as partially managed documents. It owns only the section between:

```markdown
<!-- memhub:managed:start -->
...
<!-- memhub:managed:end -->
```

`memhub sync-md` regenerates that section from database state. The implementation validates markers strictly, preserves hand-authored content outside the block, creates timestamped backups for changed existing files, and uses temp-file replacement writes to reduce the chance of partial-file corruption.

### Git ingestion and indexed search

`memhub ingest-git` shells out to the local `git` CLI and stores commit, file, and commit-file relationship data in SQLite. Search is intentionally narrow and index-driven:

- exact indexed file-history lookups
- SQLite FTS5-backed decision text search

The current search surface is deliberately smaller than the PRD end-state. It is designed to stay fast and predictable while later milestones expand retrieval.

### MCP server

`memhub serve` starts a local stdio MCP server built on the same command and database layer as the CLI. The current tool surface is intentionally narrow:

- `status`
- `search`
- `list_tasks`
- `list_decisions`
- `get_command`
- `record_command`

That keeps the first MCP slice useful without pretending the broader write-policy system already exists.

### Local-first trust model

The product guardrails matter:

- runtime state stays local to the repository
- there is no cloud backend
- agents are treated as untrusted writers in the product design
- current Milestone 1-3 implementation focuses on explicit CLI actions and verifiable stored state

The stricter write-back policy, review flow, and confidence handling described in the PRD are planned work, not shipped behavior yet.

## Architecture

### Current architecture

The current implementation is a single Rust CLI process over an embedded-migration SQLite database, with an on-demand stdio MCP server layered over the same local services.

```text
User or agent
    |
    v
memhub CLI
    |
    +-- src/commands/     fact, decision, task, command, search, ingest, sync
    +-- src/db/           bootstrap, path discovery, migrations, .gitignore updates
    +-- src/config/       per-repo config model and persistence
    +-- src/mcp/          stdio MCP server and tool adapters
    +-- src/sync_md/      managed markdown rendering and safe rewrite logic
    |
    +-- SQLite (.memhub/project.sqlite)
    |
    +-- git CLI (for history ingestion)
```

Implemented subsystems today:

- CLI parsing via `clap`
- SQLite access via `rusqlite`
- embedded numbered migrations
- CRUD for core project records
- audit logging through `writes_log`
- git ingestion into relational tables
- FTS5-backed decision search and indexed file-history lookup
- markdown managed-block generation and sync
- stdio MCP tools for status, search, task listing, recent decisions, latest command lookup, and explicit verified command recording

### Planned architecture

The intended product architecture from the PRD is now partially implemented: `memhub` has a local stdio MCP layer, but not the full later write-policy and trust model yet.

Planned later architecture includes:

- stricter write policy for verified versus proposed writes
- review queue and confidence handling
- broader retrieval and quality tooling

Those pieces should be described as planned only until the implementation lands.

## Repository Layout

- `src/cli/` - top-level CLI command definitions and output formatting
- `src/commands/` - command handlers for facts, decisions, tasks, commands, search, git ingestion, and status
- `src/config/` - per-repo config model and read/write helpers
- `src/db/` - path discovery, connection bootstrap, migrations, and `.gitignore` handling
- `src/models/` - small structs used by the CLI layer
- `src/mcp/` - stdio MCP server and thin tool adapters over existing services
- `src/sync_md/` - managed markdown rendering and file rewrite logic
- `migrations/` - embedded SQL schema migrations
- `docs/reference/` - preserved PRD and implementation notes
- `docs/architecture/` - current implemented architecture notes
- `docs/roadmap/` - milestone planning documents
- `agent_docs/` - project continuity files for future agent sessions

## Milestones and Roadmap

### Milestone 1: DB + CLI

Status: Complete

Delivered:

- Rust project scaffold
- SQLite schema and embedded migrations
- config loading
- `init`, `status`, `fact`, `decision`, `task`, and `command` command surface
- audit logging through `writes_log`

### Milestone 2: Git + search

Status: Complete

Delivered:

- `memhub ingest-git`
- relational git history storage
- indexed query paths for file history
- FTS5-backed decision search

### Milestone 3: MCP + markdown sync

Status: Current

Delivered so far:

- `memhub sync-md`
- managed-block generation for `AGENTS.md` and `CLAUDE.md`
- backup-before-rewrite behavior
- strict marker validation
- safer temp-file replacement writes
- `memhub serve`
- thin stdio MCP tools for status, search, task listing, recent decisions, latest command lookup, and explicit verified command recording

Next in this milestone:

- broader agent-originated write-policy work
- client identification and alias normalization
- possible retrieval expansion if MCP usage proves the current indexed paths are too narrow

### Milestone 4: Quality

Status: Planned

Expected scope:

- review flow for proposed writes
- confidence scoring and staleness handling
- export/import
- deny-list enforcement

### Milestone 5+

Status: Planned, speculative

Potential future work:

- global DB expansion beyond minimal preferences
- manual session compaction tooling
- optional embeddings for fuzzier search
- file watcher support
- desktop inspector
- opt-in network-backed metadata ingestion

## Project Principles

`memhub` follows a small set of product constraints that shape both the README and the codebase:

- local-first and offline-capable by default
- one database per repository
- boring, inspectable technology over speculative systems
- explicit state and verifiable writes over agent claims
- narrow, useful milestones instead of feature-heavy scaffolding

When the README and the code diverge, the product authority is the PRD in `docs/reference/memhub-prd.md`.

## Further Reading

- Product authority: [docs/reference/memhub-prd.md](docs/reference/memhub-prd.md)
- Current architecture: [docs/architecture/current-architecture.md](docs/architecture/current-architecture.md)
- Project state: [agent_docs/project_state.md](agent_docs/project_state.md)
- Implementation architecture summary: [agent_docs/project_arch.md](agent_docs/project_arch.md)
- Backlog and staged work: [agent_docs/project_backlog.md](agent_docs/project_backlog.md)
