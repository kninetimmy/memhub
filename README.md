# memhub

A local-first, per-repo project memory that **Claude Code and Codex CLI share**.

You write a fact, decision, or task from either agent — or directly from the terminal — and it lands in one SQLite database at `.memhub/project.sqlite`. Both agents read the same rows. Switching tools, or coming back to a project after a month, doesn't cost you context.

## What it is, in plain language

Claude Code and Codex each have their own notes systems, and they don't talk to each other. If you use both on the same project, you either keep two sets of notes in sync by hand, or you don't, and they drift.

memhub is a small Rust CLI that puts one structured store per repo. Facts, decisions, tasks, recent commands, session notes — all in SQLite, queryable, with attribution.

Rendered markdown at `agent_docs/PROJECT.md` and `agent_docs/PROJECT_LEDGER.md` is the human-readable view (and what you commit). The database is the source of truth.

**Three things matter:**

- **Offline.** No cloud, no account, no daemon. Just a binary and a `.sqlite` file.
- **Per-repo.** Different projects, different databases. No global state to coordinate.
- **Attributed.** Every write records both *where the claim came from* (`user`, `agent:codex`, `user+agent:claude-code`, …) and *who performed the write* (`cli:user`, `claude:wrap-up`, …). You can always tell whether a fact came from you typing it, or from an agent surfacing it during a wrap-up that you approved.

---

## Quickstart

### One-line summary

```bash
git clone https://github.com/kninetimmy/memhub.git ~/src/memhub \
  && cargo install --path ~/src/memhub --force \
  && cd /path/to/your/project \
  && memhub init && memhub status
```

That builds the binary, installs it on PATH, and initializes memhub in a project. Everything below is polish.

### Install with Claude Code

Open Claude Code in the repo you want memhub to track. Paste this:

```
Please install memhub for me.

1. Clone https://github.com/kninetimmy/memhub.git into ~/src/memhub if it
   isn't already there (`git pull` if it is). Stop if the Rust toolchain
   (1.85+) is missing.
2. Run `cargo install --path ~/src/memhub --force` so `memhub` ends up on
   PATH (~/.cargo/bin must be on PATH; warn me if it isn't).
3. Run `memhub --version` to verify.
4. (Optional, ask me first) Copy the user-level skills so /wrap-up,
   /check-init, and /init-project work as slash commands:
       cp ~/src/memhub/templates/skills/claude/*.md ~/.claude/commands/
5. Then cd back to this repo and run `memhub init` followed by
   `memhub status`. Tell me what `status` reports.

Don't touch any files in this repo other than what `memhub init` writes
(.memhub/ and a .gitignore line).
```

### Install with Codex CLI

Open Codex in the repo you want memhub to track. Paste this:

```
Please install memhub for me.

1. Clone https://github.com/kninetimmy/memhub.git into ~/src/memhub if it
   isn't already there (`git pull` if it is). Stop if the Rust toolchain
   (1.85+) is missing.
2. Run `cargo install --path ~/src/memhub --force` so `memhub` ends up on
   PATH (~/.cargo/bin must be on PATH; warn me if it isn't).
3. Run `memhub --version` to verify.
4. (Optional, ask me first) Register memhub as an MCP server so you can
   call it as a structured tool. Append this to ~/.codex/config.toml:

       [mcp_servers.memhub]
       command = "memhub"
       args = ["serve"]

5. (Optional, ask me first) Copy the user-level skills so /wrap-up,
   /check-init, and /init-project work as commands:
       cp -R ~/src/memhub/templates/skills/codex/* ~/.codex/skills/
6. Then cd back to this repo and run `memhub init` followed by
   `memhub status`. Tell me what `status` reports.

Don't touch any files in this repo other than what `memhub init` writes
(.memhub/ and a .gitignore line).
```

### Install by hand

```bash
# 1. Build + install the binary
git clone https://github.com/kninetimmy/memhub.git ~/src/memhub
cargo install --path ~/src/memhub --force

# 2. Verify
memhub --version

# 3. Initialize in your project
cd /path/to/your/project
memhub init
memhub status

# 4. Optional: agent skills
cp ~/src/memhub/templates/skills/claude/*.md ~/.claude/commands/
cp -R ~/src/memhub/templates/skills/codex/*  ~/.codex/skills/

# 5. Optional: register MCP for Codex (in ~/.codex/config.toml)
#   [mcp_servers.memhub]
#   command = "memhub"
#   args = ["serve"]
```

---

## A typical session

memhub doesn't auto-track anything. You decide what's worth remembering.

```bash
# Start of session — orient yourself
memhub status                       # open tasks, stale facts, pending writes
memhub task list --status open

# While working — capture the things you'd lose to chat history
memhub fact add build-command "cargo build"
memhub decision add "Use rusqlite bundled mode" \
  --rationale "Avoid system SQLite setup friction."
memhub task add "Wire up MCP server" --notes "Milestone 3"

# After a verified command — record it so future agents trust the recipe
cargo test
memhub command verify test "cargo test" --exit-code 0

# Closing out
memhub task done 7
memhub render                       # regenerate agent_docs/PROJECT.md
```

**End-of-session wrap-up.** If you installed the skills, run `/wrap-up` and the agent walks you through:

- New facts / decisions / tasks since the last wrap-up
- Pending MCP proposals to accept or reject
- A short session summary written to `session_notes`

Each item gets your individual approval before it lands in the DB.

---

## Compatibility

### Claude Code

- Reads `CLAUDE.md` at session start.
- User-level slash commands at `~/.claude/commands/`: `/wrap-up`, `/check-init`, `/init-project`.
- Skill writes are attributed `actor=claude:wrap-up`, `source=user+agent:claude-code`.

### Codex CLI

- Reads `AGENTS.md` at session start (same role as CLAUDE.md).
- User-level skills at `~/.codex/skills/`: `/wrap-up`, `/check-init`, `/init-project`.
- MCP server registered in `~/.codex/config.toml` as `[mcp_servers.memhub]`. Codex's MCP client identifies as `codex`; memhub auto-attributes writes accordingly.
- Skill writes are attributed `actor=codex:wrap-up`, `source=user+agent:codex`.

### Both at once

Same DB, same rows. Every write is tagged. `memhub fact list` and `memhub decision list` show the `source` column on every row, so you always know who surfaced it.

```text
source                       Meaning
─────────────────────────────────────────────────────────────────────
user                         You typed `memhub fact add` directly
agent:codex                  Codex proposed it (still in pending_writes)
agent:claude-code            Claude proposed it
user+agent:codex             Codex surfaced via /wrap-up, you approved
user+agent:claude-code       Same, Claude-side
git                          Reserved for git ingestion
observed                     Reserved for observed signals
```

`memhub stats --window 7d` breaks down writes by actor and table.

---

## What's in the box

| Command | What it does |
|---|---|
| `memhub init` | Set up `.memhub/` in a repo |
| `memhub status` | Open tasks, stale facts, pending writes, schema version |
| `memhub fact add/list` | Durable key-value facts (build commands, MSRV, etc.) |
| `memhub decision add/list` | Decisions with rationale, FTS-searchable |
| `memhub task add/list/done` | Lightweight task tracking |
| `memhub command verify` | Record verified command outcomes; derives confidence |
| `memhub note add/list` | Session notes (low-stakes scratch) |
| `memhub state set/show` | The "current state" narrative |
| `memhub arch set/show` | The architecture narrative |
| `memhub ingest-git` | Pull commit + file history into the DB |
| `memhub search <query>` | Indexed search over decisions and file history |
| `memhub review list/accept/reject` | Triage agent-proposed writes |
| `memhub render` | Emit `agent_docs/PROJECT.md` and `PROJECT_LEDGER.md` from the DB |
| `memhub stats --window 7d` | Write activity by actor, review rate, stale-fact counts |
| `memhub export/import` | Portable JSON backup; cross-machine restore |
| `memhub serve` | Stdio MCP server for Claude Code / Codex |

Run any command with `--help` for flags.

---

## Deeper dive

### One database, one truth, two views

```
.memhub/project.sqlite        ← source of truth (durable, gitignored)
agent_docs/PROJECT.md         ← rendered narrative (committed)
agent_docs/PROJECT_LEDGER.md  ← rendered structured view (committed)
```

`memhub render` regenerates the markdown from the DB. The markdown is one-way output: there's no parser that reads human edits back into the DB. To change durable content, use the CLI (or have an agent do it via `/wrap-up`).

### How attribution works

Two columns split the work:

- `source` on `facts` and `decisions` — *origin of the claim*. One of `user`, `agent:<id>`, `user+agent:<id>`, `git`, `observed`. Full vocabulary: [`docs/reference/memhub-prd-source-vocabulary-addendum.md`](docs/reference/memhub-prd-source-vocabulary-addendum.md).
- `actor` on `writes_log` and `pending_writes` — *who performed the write*. Free-form, e.g. `cli:user`, `claude:wrap-up`, `codex:wrap-up`.

When you accept a pending MCP proposal via `memhub review accept`, the durable row's `source` becomes `user+agent:<actor>` automatically — both signals preserved without you passing anything.

### MCP server

`memhub serve` starts a stdio MCP server. Tools:

- **Read:** `status`, `search`, `list_tasks`, `list_decisions`, `list_pending_writes`, `get_command`
- **Write (staged):** `propose_fact`, `propose_decision`, `log_session_note`, `record_command`

Proposals don't hit `facts` / `decisions` directly — they queue in `pending_writes` until you approve them with `memhub review accept`. Session notes are write-only (no promotion path).

Client identity is read from `clientInfo.name` at `initialize` and normalized: Claude Code → `claude-code`, Codex → `codex`. That value lands in `actor` columns automatically.

### Review flow

```bash
memhub review list                              # default: pending, last 25
memhub review show <id>
memhub review accept <id>                       # promote to facts/decisions
memhub review reject <id> --reason "untrusted"
memhub review expire --older-than-days 30       # batch age-out
```

Accept derives `source = user+agent:<actor>` from the pending row's actor. Re-reviewing an already-reviewed row errors.

### Staleness and confidence

- **Facts** go stale after 90 days without re-verification. `memhub fact list` flags them; `memhub status` reports the count.
- **Commands** carry a derived confidence: `success_count / (success_count + fail_count)`. `memhub command verify <kind> <cmdline> --exit-code N` updates the counters.

Continuous decay is deliberately not implemented. Decisions don't have confidence.

### Deny list

`.memhub/config.toml` ships with defaults blocking `.env*`, `*.pem`, `*.key`, `secrets/**`, `.aws/credentials`, etc. The list filters both `ingest-git` writes and `search` reads. Invalid patterns fail closed.

Path-based only; no content scanning. Already-ingested paths are not purged when the list changes.

### Backup and restore

```bash
memhub export ./memhub-backup.json     # portable, version-tagged JSON
memhub init --from-backup <path>       # init + restore in one shot
memhub import <path>                   # restore into an existing repo
memhub import <path> --force           # overwrite live data
```

Export covers facts, decisions, tasks, commands, pending writes, and writes_log. Git ingestion and FTS chunks are derived state and regenerate on demand. Session notes are not in the v1 export format.

---

## How it's built

Single Rust binary over an embedded-migration SQLite database. The MCP server reuses the same command layer.

```text
memhub CLI / MCP
   ├── src/commands/    fact / decision / task / command / review / ...
   ├── src/db/          path discovery, migrations, audit log
   ├── src/config/      per-repo TOML
   ├── src/mcp/         stdio MCP server, client identity normalization
   ├── src/sync_md/     managed markdown rewrite
   ├── src/render/      PROJECT.md and PROJECT_LEDGER.md emit
   └── src/export/      v1 portable JSON
       │
       └── SQLite (.memhub/project.sqlite) + git CLI
```

Schema is at migration `0008_decisions_source` as of 2026-05-13. Architecture details in [`docs/architecture/current-architecture.md`](docs/architecture/current-architecture.md).

---

## Project status

**Active core (shipping):**

- CLI for facts / decisions / tasks / commands / state / arch / notes / review / stats
- Stdio MCP server with client-identity auto-attribution
- `memhub render` emits `agent_docs/PROJECT.md` + `PROJECT_LEDGER.md`
- Portable JSON export/import with single-step `init --from-backup`
- Compound source vocabulary for multi-agent attribution
- Per-repo deny list for sensitive paths
- Claude Code and Codex CLI bridge: shared DB, parity skills, MCP registration

**Not implemented yet:**

- Continuous confidence decay
- Embeddings / fuzzier semantic search
- Cross-repo / global memory layer
- File watcher
- Desktop inspector

PRD authority: [`docs/reference/memhub-prd.md`](docs/reference/memhub-prd.md) (kept verbatim; changes land as addenda).

---

## Principles

- **Local-first.** No network, no daemon, no account.
- **One per repo.** Project boundaries are repo boundaries.
- **Boring tech.** SQLite, Rust, glob patterns, FTS5. Nothing speculative.
- **Agents are untrusted writers.** Agent proposals stage in `pending_writes` until a human approves. The schema enforces it.
- **Narrow milestones.** Ship usable slices; defer speculative work until a real workflow demands it.

---

## Further reading

- [Product PRD (verbatim)](docs/reference/memhub-prd.md)
- [Source vocabulary addendum](docs/reference/memhub-prd-source-vocabulary-addendum.md)
- [K9 deprecation addendum](docs/reference/memhub-prd-deprecation-addendum.md) — memhub used to coexist with the K9 markdown framework; that integration is retired as of 2026-05-13. K9 markdown files in older repos are historical archive.
- [Current architecture](docs/architecture/current-architecture.md)
- [Project state (rendered)](agent_docs/PROJECT.md)
- [Project ledger (rendered)](agent_docs/PROJECT_LEDGER.md)
