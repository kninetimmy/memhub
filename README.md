# memhub

`memhub` is a local-first Rust CLI for durable per-repo project memory shared across Codex and Claude Code. It gives a repository one structured source of truth for facts, decisions, tasks, command history, git-derived context, and managed project-state blocks in `AGENTS.md` and `CLAUDE.md`.

The long-term product direction is a shared memory layer that both agents can read and write through a local interface. The current implementation already provides a usable local database, CLI workflows, git ingestion, indexed search, hardened markdown sync, and a narrowed local MCP server/write-policy slice, while later milestones add broader trust and recovery tooling.

## Development Status

`memhub` has completed PRD §16 Milestones 1–4 and the K9 Claude Framework interop slice of Milestone 5 (the memhub-side of K9 `/wrap-up` integration). Milestone 5 K9-repo consumer edits live outside this repo. Milestone 6+ is speculative future work.

Current state:

- Shipped: Milestone 1 foundations, Milestone 2 git ingestion and indexed search, the narrowed Milestone 3 slice covering markdown sync, stdio MCP access, staged proposal writes, and client alias normalization, all of Milestone 4 (portable `export` / `import`, missing-DB safety with `memhub init --from-backup <path>` recovery, the `memhub review` flow for promoting or rejecting staged MCP proposals, path-based deny-list enforcement, and the fact-staleness + derived command-confidence pass), and the memhub side of Milestone 5 K9 interop (detection + config + status surfacing, the v1 K9 `/wrap-up` contract, and machine-readable `--json` on every read and mutating command K9 needs)
- Current focus: between numbered tasks; PRD §16 v1 milestones are done
- Implemented now: local SQLite storage, embedded migrations, per-repo config (including a configurable deny list and K9 integration section), audit logging, facts/decisions/tasks/commands CRUD, explicit command verification, derived command confidence and 90-day fact staleness flag, git ingestion with path-based deny-list filtering, indexed search with deny-list filtering, managed-block sync for `AGENTS.md` / `CLAUDE.md`, `memhub serve` for stdio MCP access, staged MCP fact/decision proposals, MCP `list_pending_writes` read tool, `memhub review list|show|accept|reject|expire` CLI flow with `--json` on `list` / `show` / `accept` / `reject`, pending-write visibility in status, portable `memhub export` / `memhub import`, missing-DB detection, `memhub init --from-backup <path>` single-step recovery, K9 detection on `memhub init`, `memhub integrations status | enable-k9 | disable-k9 | check-k9`, and `--actor` attribution on every K9-targeted mutating command
- Not implemented yet: continuous confidence decay over time, `memhub.log_session_note` and `memhub stats` PRD surfaces, and broader indexed retrieval beyond current narrow paths

Milestone status:

| Milestone | Status | Notes |
|-----------|--------|-------|
| Milestone 1: DB + CLI | Complete | Core repo bootstrap, schema, CRUD, config, logging |
| Milestone 2: Git + search | Complete | `ingest-git`, FTS-backed decision search, exact file-history lookups |
| Milestone 3: MCP + markdown sync | Complete | Markdown sync, stdio MCP reads, verified command recording, staged proposal writes, and client alias normalization are shipped under the current narrowed plan |
| Milestone 4: Quality | Complete | Portable `export` / `import` (`M4-001`), missing-DB safety with `init --from-backup` (`M4-002`), `memhub review` flow for staged proposals (`M4-003`), deny-list filtering of git ingestion and search (`M4-004`), and fact staleness + derived command confidence (`M4-005`) |
| Milestone 5: K9 framework interop | Memhub side complete | K9 detection + config (`M5-001`), v1 wrap-up contract + machine-readable mutating commands (`M5-002`), and `--json` read surfaces on `review list` / `review show` (`M5-003`). K9 repo `/wrap-up.md` consumer edits live outside this repo. |
| Milestone 6+ | Planned | Speculative future expansions only after separate validation |

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
- See staged proposal count in `memhub status`
- Ingest git history with `memhub ingest-git [--since <ref>]`
- Search current indexed memory with `memhub search <query>`
- Regenerate managed blocks in `AGENTS.md` and `CLAUDE.md` with `memhub sync-md`
- Serve the current repository over stdio MCP with `memhub serve`, including staged `propose_fact` and `propose_decision` MCP tools
- Back up the project with `memhub export <path>` and restore it on another machine or a clean checkout with `memhub import <path>` (use `--force` to overwrite existing data)
- Recover from a deleted or corrupted database in a single step with `memhub init --from-backup <path>`
- Review and promote staged MCP fact/decision proposals with `memhub review list|show|accept|reject|expire`
- Keep sensitive paths (`*.pem`, `.env*`, `secrets/**`, etc.) out of ingestion and search via a configurable deny list in `.memhub/config.toml`

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

## Backup and Restore

`memhub` ships a portable, version-tagged JSON export as the supported
recovery path. The export captures durable user data — facts, decisions,
tasks, commands, pending writes, and the writes log — and excludes
derived state (git ingestion, FTS chunks, schema migrations) that can be
regenerated.

### Back up the current project

```bash
cargo run -- export ./memhub-backup.json
```

The destination is a path you choose. Parent directories are created if
needed. Keep the file somewhere outside `.memhub/` — for example, a
cloud-synced folder or another backup target.

### Restore onto a fresh machine or clean checkout

```bash
cd /path/to/repo
cargo run -- init
cargo run -- import /path/to/memhub-backup.json
```

The target must already be initialized (`memhub init`). The import wipes
durable tables and restores rows with their original IDs so cross-table
references (e.g. decision supersession chains) stay intact.

If the target already has data, `memhub import` refuses unless `--force`
is passed:

```bash
cargo run -- import /path/to/memhub-backup.json --force
```

After import, re-run `memhub ingest-git` to repopulate commit/file
history from the local git repository — git history is not part of the
export.

### Recover when the database is missing or corrupted

If `.memhub/` exists but `project.sqlite` is gone — for example after a
disk fault, an accidental deletion, or a partial sync — `memhub` will
not silently rebuild an empty database. Every command except the
recovery entry point refuses with a `MissingDatabase` error pointing at
the recovery flow.

The supported single-step recovery is:

```bash
cd /path/to/repo
cargo run -- init --from-backup /path/to/memhub-backup.json
```

`init --from-backup` creates `.memhub/` if needed, runs migrations to
the current schema, then imports the supplied backup. It works for both
a clean clone (no `.memhub/`) and the missing-database case (existing
`.memhub/` without `project.sqlite`). It refuses to run when a database
already exists at `.memhub/project.sqlite`; use `memhub import --force`
to overwrite a live database instead.

If you intentionally want to start over with no data, remove `.memhub/`
manually and run `memhub init` again.

The on-disk shape of the export file is documented in
[docs/reference/export-format.md](docs/reference/export-format.md).

## Reviewing staged MCP proposals

Agent-originated MCP writes never land directly in durable `facts` or
`decisions`. They stage in `pending_writes` and wait for a human review.
`memhub review` is the CLI surface for that review.

```bash
cargo run -- review list                       # default: --status pending --limit 25
cargo run -- review list --status all
cargo run -- review show <id>
cargo run -- review accept <id>
cargo run -- review reject <id> --reason "untrusted source"
cargo run -- review expire                     # default: --older-than-days 30
```

`accept` promotes the staged row to its durable table:

- A staged `fact` becomes a row in `facts` with `source = "user"` and
  `confidence = 1.0`, upserted by `(project_id, key)` like
  `memhub fact add`.
- A staged `decision` becomes an active row in `decisions` with the
  original rationale, regenerating the FTS chunk so it is immediately
  searchable.

In both cases the `pending_writes` row is marked `accepted` and a
`reviewed_at` timestamp is recorded. `reject` marks the row `rejected`
and stores any user-provided reason in `writes_log`. `expire` is an
explicit batch operation that ages pending proposals older than
`--older-than-days` (default 30) into `expired`. Nothing auto-expires
on read.

Re-reviewing an already-reviewed row is an error, so re-acceptance must
go through `memhub fact add` / `memhub decision add` directly.

## Deny list

`memhub` ships a configurable path-based deny list so sensitive files
never enter the database. The list is matched as glob patterns against
each file path that `memhub ingest-git` would otherwise insert, and
also against `memhub search` results before they are returned. A
denied direct path lookup returns a normal "no matches" — there is no
signal indicating *why* the result is empty.

Defaults cover the patterns the PRD calls out:

```toml
[deny_list]
patterns = [
  ".env", ".env.*",
  "*.pem", "*.key", "*.p12", "*.pfx",
  "id_rsa", "id_rsa.*",
  "id_dsa", "id_dsa.*",
  "id_ecdsa", "id_ecdsa.*",
  "id_ed25519", "id_ed25519.*",
  "secrets/**",
  ".aws/credentials",
  ".gcloud/credentials*",
  ".gnupg/**",
]
```

Edit `.memhub/config.toml` to add or remove patterns. Existing configs
without a `[deny_list]` section automatically fall back to the
defaults at load time.

Matching is glob-based via the `globset` crate and supports `**` for
recursive directory matches. The matcher checks each path segment, so
`config/server.pem` is denied by `*.pem` even though the pattern is
unrooted. Invalid patterns fail closed: `memhub ingest-git` and
`memhub search` refuse to run until the bad pattern is fixed, rather
than silently allowing sensitive paths through.

The current scope is path-based only. Content scanning for credential
strings (e.g. AWS access key IDs in file contents) is out of scope for
this slice, as is purging already-ingested paths from the database
after a pattern change. `memhub status` prints the current deny pattern
count, and `memhub ingest-git` prints how many paths were skipped on
each run.

## Confidence and staleness

`memhub` surfaces two derived freshness signals on read paths, matching
PRD §11.4.

**Facts** carry a `verified_at` timestamp that is refreshed every time
`memhub fact add` writes the row (insert or upsert) or `memhub review
accept` promotes a staged proposal. A fact whose `verified_at` is more
than 90 days old — or null entirely — is flagged stale.

```
$ memhub fact list
[1] build-command = cargo build (source: user, confidence: 1.00, verified: 2026-05-12 14:22:00, created: 2026-05-12 14:22:00)
[2] router-style = manual [stale] (source: user, confidence: 1.00, verified: 2026-01-02 09:00:00, created: 2026-01-02 09:00:00)
```

`memhub status` also reports the total count of stale facts so you can
see at a glance how much of the store has decayed without re-verification.

**Commands** carry a derived confidence equal to
`success_count / (success_count + fail_count)`. `memhub command verify`
already bumps the counters on every recorded run, so confidence rises
on successful re-runs and falls on failures with no schema change. A
command that has never been verified reports `confidence: n/a` rather
than a fabricated value.

```
$ memhub command list
[1] build => cargo build (last_exit: 0, last_run: 2026-05-12 14:23:00, success: 3, fail: 1, confidence: 0.75)
```

The 90-day threshold is intentionally hardcoded in this slice; promote
it to config if a real workflow needs it. Continuous decay, a persisted
confidence column on `commands`, and confidence on `decisions` are
explicitly deferred.

## Project usage stats

`memhub stats` prints the dogfood metrics that PRD §17 names as the
success signal: are facts/decisions/tasks growing, is the review queue
actually being reviewed, is the store going stale?

```
cargo run -- stats                          # default: --window 30d
cargo run -- stats --window 7d
cargo run -- stats --window 90d
cargo run -- stats --window all
cargo run -- stats --json                   # machine-readable
```

The human-readable output prints totals, windowed write activity
grouped by actor and table (sourced from `writes_log`), pending-write
review rate over the window, all-time `pending_writes` status counts,
the top five commands by run count, and the five most recently verified
facts. `--json` emits the same data as a structured envelope.

Deliberate limitation: this slice tracks **write** activity only, via
`writes_log`. PRD §17 also mentions a "simple read counter"; instrumenting
every read path is deferred until there's a real workflow demanding it.
The output explicitly notes the deviation so the omission is never
silent.

## Session notes

Agents talking to `memhub` over MCP often want to record low-stakes
scratch — *"tried X, no observable effect"* — without polluting durable
facts or clogging the review queue. `log_session_note` is that path:
write-only, never promoted, never surfaced as truth, but durable in the
local database for later inspection.

Write surface (MCP only):

```
log_session_note({ "text": "tried router rewrite, no measurable diff" })
  → { "id": 17, "actor": "claude-code", "actor_raw": "claude-ai", "created_at": "2026-05-12 14:55:00" }
```

The actor identity is taken from `clientInfo.name` exactly like the
`propose_*` tools — both the normalized alias (e.g. `claude-code`) and
the raw value are persisted on the row, plus a `writes_log` audit entry
with `table_name = "session_notes"`.

Read surface (CLI only):

```
cargo run -- note list
cargo run -- note list --limit 50
cargo run -- note list --actor claude-code
cargo run -- note list --since-days 7
cargo run -- note list --json
```

Notes are intentionally not promotable — they never become facts or
decisions. Notes are also intentionally not yet covered by `memhub
export` (the v1 export format is locked); they live in the local
database only and are lost on `import`. Promote that to a v2 export if
notes start carrying durable value.

## K9 Claude Framework integration

`memhub` runs standalone, but if the [K9 Claude
Framework](https://github.com/anthropics/claude-code) is in use in a
repo, `memhub init` detects it automatically and records the
integration in `.memhub/config.toml`. The detection probe is a single
file: `agent_docs/project_state.md`.

When detected during a fresh `memhub init`, a new section appears in
the config:

```toml
[integrations.k9]
enabled = true
agent_docs_path = "agent_docs"
```

If K9 is not present at init time, the section is omitted entirely —
the default config stays minimal. `memhub init` is idempotent and
never modifies an existing config, so to toggle the integration after
the fact use the explicit subcommand:

```bash
memhub integrations status
memhub integrations enable-k9 [--agent-docs-path docs/k9] [--force]
memhub integrations disable-k9
```

`enable-k9` refuses to run when no K9 marker is detected unless
`--force` is supplied, so the config can't quietly drift away from
reality. `disable-k9` flips `enabled = false` but keeps the section as
a record of past configuration.

`memhub status` reports the integration state on every invocation:

```
K9 detected: yes
K9 integration: enabled (agent_docs_path: agent_docs)
```

If the config says `enabled = true` but the marker disappeared (drift),
`status` surfaces a `note: K9 enabled in config but
agent_docs/project_state.md is missing` line. The reverse case — K9
detected but not yet enabled — produces a `note: K9 detected; run
\`memhub integrations enable k9\` to enable` hint instead.

The MCP `status` tool exposes the same booleans (`k9_detected`,
`k9_enabled`, `k9_agent_docs_path`, `k9_drift`) so MCP clients can
condition behavior on the integration state without a separate
endpoint.

### K9 `/wrap-up` shell-out contract

`memhub` ships a stable v1 CLI contract that K9 `/wrap-up` shells out
to after the human-approval gate. The full contract — gating
semantics, JSON output schemas, actor convention, exit codes — lives
in [`docs/reference/k9-wrap-up-contract.md`](docs/reference/k9-wrap-up-contract.md).

Quick reference for the supporting affordances `memhub` provides:

- **Pre-flight gate.** `memhub integrations check-k9` exits 0 when
  the integration is enabled (and a `.memhub/project.sqlite` exists),
  exit 1 otherwise. Zero stdout. K9 should run this once at the top
  of `/wrap-up` and short-circuit when it returns non-zero.
- **Machine-readable mutating commands.** `fact add`, `decision add`,
  `task add`, `task done`, `review accept`, and `review reject` all
  accept `--json`. When set, they emit a single JSON object on stdout
  (no trailing decoration) and suppress the human-readable line. The
  per-command schema is locked by the contract doc — there is no
  in-payload `schema_version` field; a `v2` contract will bump the
  doc instead.
- **Actor attribution.** The same commands accept `--actor <name>`
  (defaults to `cli:user`, max 64 characters, non-empty). K9 passes
  `--actor k9:wrap-up` so the `writes_log` audit trail differentiates
  K9-mediated writes from manual CLI use.
- **Machine-readable read surfaces.** `review list` and `review show`
  also accept `--json`. `review list --json` emits
  `{"status": <filter|null>, "pending_writes": [...]}` so K9 can fold
  staged proposals into the draft alongside Markdown updates without
  parsing human-readable output.

Example end-to-end shell session:

```bash
memhub integrations check-k9 || exit 0

memhub fact add build-command "cargo build" --json --actor k9:wrap-up
# {"id":12,"key":"build-command","value":"cargo build","source":"user","created":true}

memhub review accept 4 --json --actor k9:wrap-up
# {"pending_id":4,"kind":"fact","durable_table":"facts","durable_id":12}
```

Any non-zero exit from a mutating call is a hard abort signal: K9
should not touch `agent_docs/*.md` if the DB write phase failed.

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
- `list_pending_writes`
- `get_command`
- `record_command`
- `propose_fact`
- `propose_decision`

Proposal tools stage writes in `pending_writes`; they do not write directly to durable `facts` or `decisions`, and they do not replace the later review flow. Each staged row now also stores the MCP request/init provenance that `rmcp` exposes today, such as request ID, protocol version, client version, and optional MCP metadata. Prompt/session context remains deferred until the transport surface exposes or requires it.

### Local-first trust model

The product guardrails matter:

- runtime state stays local to the repository
- there is no cloud backend
- agents are treated as untrusted writers in the product design
- current Milestone 1-3 implementation focuses on explicit CLI actions, verifiable stored state, and staged proposals instead of direct agent promotion

The fuller review flow, promotion path, confidence handling, and recovery tooling described in the PRD are still planned work, not shipped behavior yet.

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
- stdio MCP tools for status, search, task listing, recent decisions, latest command lookup, explicit verified command recording, staged fact/decision proposals, and read-only `list_pending_writes`
- `memhub review` CLI for promoting, rejecting, and expiring staged MCP proposals
- path-based deny list filtering git ingestion writes and search reads

### Planned architecture

The intended product architecture from the PRD is now partially implemented: `memhub` has a local stdio MCP layer, but not the full later write-policy and trust model yet.

Planned later architecture includes:

- stricter write policy for verified versus proposed writes
- review queue and confidence handling
- broader retrieval and quality tooling

Those pieces should be described as planned only until the implementation lands.

## Repository Layout

- `src/cli/` - top-level CLI command definitions and output formatting
- `src/commands/` - command handlers for facts, decisions, tasks, commands, search, git ingestion, status, portable export/import, and review of staged MCP proposals
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

Status: Complete

Delivered:

- `memhub sync-md`
- managed-block generation for `AGENTS.md` and `CLAUDE.md`
- backup-before-rewrite behavior
- strict marker validation
- safer temp-file replacement writes
- `memhub serve`
- thin stdio MCP tools for status, search, task listing, recent decisions, latest command lookup, explicit verified command recording, staged fact/decision proposals, and read-only `list_pending_writes`
- pending-write visibility in status output
- client identification and alias normalization from MCP `clientInfo.name`

Deferred to Milestone 4:

- review and promotion flow for staged writes
- confidence and staleness handling

### Milestone 4: Quality

Status: Complete

Shipped:

- portable `memhub export` / `memhub import` as the supported backup and restore path (`M4-001`)
- readable README backup/restore instructions
- missing-DB safety: every command refuses with a clear recovery error when `.memhub/` exists without `project.sqlite` instead of silently rebuilding an empty database (`M4-002`)
- `memhub init --from-backup <path>` single-step recovery that initializes and restores in one command (`M4-002`)
- `memhub review list|show|accept|reject|expire` to promote, reject, or age out staged MCP proposals, plus a read-only `list_pending_writes` MCP tool (`M4-003`)
- path-based deny list with sensible defaults, filtering both `ingest-git` writes and `search` reads (`M4-004`)
- stale-flag on facts (90-day threshold) and derived confidence on commands, surfaced in CLI list output, `memhub status`, and MCP responses (`M4-005`)

Remaining scope:

- none — Milestone 4 is complete.

### Milestone 5: K9 framework interop

Status: Memhub side complete

Shipped:

- K9 detection on `memhub init`; `[integrations.k9]` config section auto-populated when `agent_docs/project_state.md` is present (`M5-001` phase 1)
- `memhub integrations status | enable-k9 | disable-k9` subcommands for explicit toggling on already-initialized repos (`M5-001` phase 1)
- Drift detection and surfacing in `memhub status` and the MCP `status` tool (`M5-001` phase 1)
- v1 K9 wrap-up contract documented at `docs/reference/k9-wrap-up-contract.md` (`M5-002`, additively amended by `M5-003`)
- `memhub integrations check-k9` exit-code gate for K9 to short-circuit on disabled repos (`M5-002`)
- `--json` and `--actor` flags on `fact add`, `decision add`, `task add`, `task done`, `review accept`, `review reject` (`M5-002`)
- `--json` read surfaces on `memhub review list` and `memhub review show` so K9 can fold staged proposals into draft assembly without parsing human-readable output (`M5-003`)

Remaining scope (separate slices, outside this repo):

- K9 repo `/wrap-up.md` consumer edits (lives in K9 repo; consumes the v1 contract end-to-end — gate + read + mutate)

### Milestone 6+

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
