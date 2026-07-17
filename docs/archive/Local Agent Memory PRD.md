# PRD: Local Agent Memory Hub

**Author:** Elswick
**Status:** Draft v2
**Last updated:** 2026-04-21

---

## 1. One-liner

A local, per-repo memory database that Codex and Claude Code both read from and write to, so switching between agents (or resuming after a gap) doesn't cost you context.

## 2. Why this exists

Codex has `AGENTS.md`. Claude Code has `CLAUDE.md`. Both tools have their own auto-memory systems. When you use both on the same project — which is the actual real-world workflow — you end up manually keeping two sets of notes in sync, or you don't, and they drift. Session wrap-ups are manual. Project state lives in chat history you can't search.

The fix is a single local database per repo that both agents read from through MCP. Both tools see the same facts, same decisions, same file history, same learned commands. The markdown files stay as the entry point — but their "durable knowledge" section is generated from the DB instead of hand-maintained in two places.

This is the only real value prop. Everything else in this doc is implementation.

## 3. Design principles

These are load-bearing. Anything that violates them gets cut, even if it would be cool.

1. **No productivity theater.** If a feature sounds good in a demo but doesn't actually reduce friction during real coding, it doesn't ship. No hive-mind agent swarms, no cloud backends, no "AI-powered dashboards." This is a database with a tool interface.
2. **Local first, local only by default.** No mandatory network calls. No telemetry. Runs offline on an airgapped machine if needed.
3. **Agents are untrusted writers.** Anything an agent wants to save is treated as a claim, not a fact, until there's a verifiable signal (exit code, test pass, git diff) or explicit user confirmation.
4. **Retrieval is cheap, writes are skeptical.** It's fine for the DB to grow. It's not fine for it to grow with garbage.
5. **Boring tech.** SQLite, FTS5, git CLI, MCP. No vector DBs, no graph DBs, no services until they solve a problem I've actually hit.
6. **One DB file = one repo.** Portable, backup-able, delete-able. No hidden state anywhere else on the machine.

## 4. Non-goals

- Multi-user sync or team features
- Replacing git, GitHub, the agents themselves, or existing skill systems
- Becoming a general knowledge base, note-taking app, or second brain
- Embedding-based semantic search in v1 (deferred to v2, may never ship)
- Auto-compacting session logs in v1 (deferred until I know what "good" compaction looks like)
- Any cloud component, ever, unless I explicitly change my mind on this in a future doc

## 5. Users

Me. Primarily. Published to GitHub so other people can use it if they want, but scope and feature decisions are driven by my workflow, not a hypothetical user base.

My workflow, concretely:
- Multiple hobby repos at any given time (Free-AI-SSD is current primary)
- Switching between Codex CLI and Claude Code depending on the task
- Long gaps between sessions on any given project (sometimes weeks)
- Windows primary, Linux secondary (CCNA study, WSL), macOS occasional

## 6. Architecture overview

### 6.1 Components

```
┌─────────────────────────────────────────────────────────┐
│  Agent (Codex or Claude Code)                           │
│  reads: CLAUDE.md / AGENTS.md  →  calls: MCP tools      │
└─────────────────────────────────────────────────────────┘
                          │ MCP
                          ▼
┌─────────────────────────────────────────────────────────┐
│  memhub (Rust binary, local)                            │
│                                                         │
│  ┌──────────┐  ┌──────────┐  ┌──────────┐  ┌─────────┐ │
│  │ MCP tool │  │  Query   │  │  Writer  │  │   MD    │ │
│  │  layer   │→ │  router  │→ │ (policy) │→ │ syncer  │ │
│  └──────────┘  └──────────┘  └──────────┘  └─────────┘ │
│                          │                              │
│                          ▼                              │
│  ┌────────────────────────────────────────────────┐    │
│  │  SQLite (rusqlite) + FTS5                      │    │
│  │  .memhub/project.sqlite                        │    │
│  └────────────────────────────────────────────────┘    │
│                          │                              │
│                          ▼                              │
│  ┌────────────────────────────────────────────────┐    │
│  │  git CLI (shelled out) for history ingestion   │    │
│  └────────────────────────────────────────────────┘    │
└─────────────────────────────────────────────────────────┘
```

### 6.2 Layout per repo

```
<repo-root>/
├── .memhub/
│   ├── project.sqlite         # source of truth
│   ├── config.toml            # per-repo settings, deny lists
│   └── migrations/            # schema versions applied
├── CLAUDE.md                  # has a <!-- memhub:managed --> section
└── AGENTS.md                  # has a <!-- memhub:managed --> section
```

`.memhub/` is added to `.gitignore` by default. The user opts in if they want to commit it (useful for teams, but not the default).

### 6.3 Global DB

Minimal scope. Stored at:

- Windows: `%APPDATA%\memhub\global.sqlite`
- Linux/macOS: `~/.config/memhub/global.sqlite`

Holds only:
- User preferences (name, default editor, preferred commit message style, etc.)
- Reusable patterns flagged as "this applies to all my projects" (e.g., "I always use Conventional Commits")
- No cross-project search, no shared decisions, no analytics.

If the global DB turns out to carry its weight, it expands in v2. If not, it stays minimal or gets killed.

### 6.4 Tech stack

| Layer          | Choice                     | Why |
|----------------|----------------------------|-----|
| Language       | Rust 2024 edition          | Tauri-ready, single binary deploy, good SQLite bindings, no Python packaging pain |
| Storage        | SQLite via `rusqlite`      | Serverless, portable, FTS5 built in, one-file backups |
| Search         | SQLite FTS5                | Built in, fast enough, no extra dependency |
| MCP            | `rmcp` crate (or equivalent current-maintained crate — verify at build time) | Official-ish Rust MCP SDK |
| Git            | Shell out to `git` CLI     | `git2` crate works but adds libgit2 dep; CLI is universally available and simpler to reason about |
| Config         | TOML via `serde`           | Matches Cargo and Codex conventions |
| CLI            | `clap` v4                  | Standard |
| Desktop (v3+)  | Tauri                      | Deferred, noted for future |

### 6.5 Project boundary rule

No auto-discovery magic. Rule is explicit:

1. Run `memhub init` in a repo root. Creates `.memhub/`.
2. When MCP server starts, it walks up from CWD looking for the nearest ancestor containing `.memhub/`. That's the project.
3. If none found, MCP server responds with a clear error ("no memhub project above CWD; run `memhub init`"). Never silently creates one.

Monorepos and multi-repo workspaces: one `.memhub/` at the level you want memory to be scoped to. You decide. No heuristics.

## 7. The CLAUDE.md / AGENTS.md integration

This is the part that makes the whole thing worth building.

### 7.1 Managed sections

Both files get a managed block that memhub owns and rewrites:

```markdown
<!-- memhub:managed:start -->
<!-- DO NOT EDIT BELOW THIS LINE. Generated by memhub. -->
<!-- To change, use `memhub fact add` / `memhub decision add` or edit the DB. -->

## Project state (auto-generated)

**Build:** `cargo build --release` (last verified: 2026-04-18)
**Test:** `cargo test` (last verified: 2026-04-18)
**Active tasks:** 3 open, 1 blocked — see `memhub tasks`

### Durable decisions
- Use SQLite FTS5 for text search; no external search engine (2026-04-15)
- Shell out to git CLI instead of git2 crate (2026-04-16)

### Known quirks
- Windows builds require MSVC toolchain, not GNU

<!-- memhub:managed:end -->
```

Above and below the managed block, the user writes whatever they want by hand. That content is never touched.

### 7.2 What goes where

| Content type                                    | Lives in         |
|-------------------------------------------------|------------------|
| Hand-authored behavioral rules                  | CLAUDE.md / AGENTS.md (outside managed block) |
| "Always run the linter before committing"       | CLAUDE.md / AGENTS.md (outside managed block) |
| Last known working build command                | DB → managed block |
| Architectural decisions                         | DB → managed block |
| Open tasks                                      | DB → managed block (summary) + `memhub tasks` CLI |
| Git history / file relationships                | DB only, queried via MCP tools |
| Full session logs                               | DB only |

### 7.3 Sync behavior

`memhub sync-md` regenerates the managed block. Runs:
- On `memhub init`
- On explicit user command
- After any write operation that affects managed content (flag-controlled: `auto_sync_md = true` in config)

Agents never edit the managed block directly. If an agent has new info, it writes to the DB via MCP, and the syncer rewrites the markdown.

## 8. Data model

Core tables. Not exhaustive — schema will evolve — but this is the starting point.

```sql
-- Core
projects(id, root_path, created_at, schema_version)
sessions(id, project_id, agent, started_at, ended_at, summary)

-- Knowledge
facts(id, project_id, key, value, confidence, source, verified_at, created_at)
decisions(id, project_id, title, rationale, status, decided_at, superseded_by)
tasks(id, project_id, title, status, notes, created_at, updated_at)
commands(id, project_id, kind, cmdline, last_exit_code, last_run_at, success_count, fail_count)

-- Code structure
files(id, project_id, path, last_seen_commit, language)
symbols(id, file_id, name, kind, line_start, line_end)

-- History
commits(sha, project_id, author, committed_at, message)
commit_files(commit_sha, file_id, change_type)

-- Text search
chunks(id, project_id, source_type, source_id, text, created_at)
chunk_fts  -- FTS5 virtual table over chunks.text

-- Audit
writes_log(id, project_id, actor, table, row_id, action, reason, at)
```

Notes:
- Every `facts` / `decisions` / `commands` row has a `confidence` score (0.0–1.0) and a `source` (`agent:claude-code`, `agent:codex`, `user`, `git`, `observed`).
- `confidence` starts low for agent-written rows and increases when re-confirmed (command runs again successfully, decision referenced in subsequent session, etc.).
- `writes_log` is the audit trail. Every write goes through it. Used for debugging, rollback, and "show me what memhub changed in the last week."

## 9. Indexing principle (load-bearing)

**The whole point of this tool is that agents don't have to scan the database to find things.** They look things up by index, the same way a filesystem looks up a file by its entry in the allocation table instead of reading every block on disk.

This is stated here because it's easy to accidentally write a query that does a full table scan — SQLite will happily run it — and the system will get measurably slower as the DB grows. Every implementer (human or agent) needs to understand this rule and follow it.

### The rule

Every query path must be backed by an index appropriate to the query shape:

| Query shape                              | Index type required         | Example |
|------------------------------------------|-----------------------------|---------|
| Exact match on a column                  | B-tree on that column       | `WHERE kind='build'` needs `INDEX ON commands(kind)` |
| Exact match on a hot subset              | Partial B-tree              | `WHERE status='open'` → `INDEX ON tasks(status) WHERE status='open'` |
| Ordered recent lookup                    | Composite B-tree            | `ORDER BY last_run_at DESC` with predicate → `INDEX ON commands(kind, last_run_at DESC)` |
| Join on foreign key                      | B-tree on FK column         | `commit_files.file_id` needs an index |
| Full-text / phrase search                | FTS5 inverted index         | `chunk_fts MATCH 'router decision'` |
| Prefix / path match                      | B-tree on normalized column | Path prefix queries use `dir_prefix` column, not `LIKE '%foo%'` |

### What's forbidden

- `SELECT * FROM <table>` with no `WHERE` or `LIMIT` in production code paths. (OK for debug CLI, never in MCP tool paths.)
- `LIKE '%foo%'` on large tables — leading wildcard can't use an index. Use FTS5 instead.
- Text search implemented as `LIKE` instead of `MATCH`. FTS5 exists for a reason.
- Any query where the query plan (`EXPLAIN QUERY PLAN`) shows a full table scan on a table that could grow unboundedly.

### How this is enforced

- A test suite runs `EXPLAIN QUERY PLAN` on every query the MCP tool layer issues and asserts no full scans on unbounded tables.
- Schema migrations must declare the indexes the migration depends on.
- The CLI has a `memhub explain <query>` command for manual verification during development.

### Why this is non-negotiable

A database without a discipline around indexing is just a slow text file. Exact lookups should return in under 100ms and FTS lookups in under 500ms on a normal-size project DB — achievable only if every hot path hits an index. The moment an agent has to wait 2 seconds to find the last build command, the tool is worse than just re-reading `CLAUDE.md`, and it fails at its only real job.

---

## 10. Query router

This was hand-waved in v1. Fleshed out here. The goal: rule-based routing that covers ~90% of queries with zero LLM calls and sub-100ms latency. LLM fallback is a v2 idea.

### 10.1 Router is a pipeline of matchers

Each matcher either claims the query and returns a result set, or passes. First match wins.

```
Query → [pattern matchers] → [keyword matchers] → [FTS fallback] → result
```

### 10.2 Matchers in order

1. **Exact-key lookup.** Queries matching known patterns go straight to indexed SQL.
   - `"last build command"` → `SELECT cmdline FROM commands WHERE kind='build' ORDER BY last_run_at DESC LIMIT 1`
   - `"open tasks"` → `SELECT * FROM tasks WHERE status='open'`
   - `"who wrote X.rs"` → git log via commit_files join
   - Maintained as a list of `(regex, sql_template)` pairs. Easy to extend.

2. **Entity extraction.** Queries containing a file path, symbol name, or commit SHA route to entity-specific lookups.
   - `"history of src/router.rs"` → file history via commit_files
   - `"decisions about the router"` → decisions table + FTS on `title` and `rationale`

3. **FTS fallback.** Anything else hits `chunk_fts` with a BM25 ranking, filtered by source_type if the query contains hints ("decision", "task", "command").

4. **Nothing found.** Returns `{results: [], hint: "no matches; try: memhub search --fts <term>"}`. Never hallucinates results. Never makes up a "best guess."

### 10.3 Why no LLM classifier in v1

- Adds latency (50–500ms per query) and tokens
- Non-deterministic; hard to debug
- The 10% of queries it would handle better are ones I can handle by adding more rules as I find them
- LLM routing is the kind of feature that feels productive but is usually just moving the failure mode

If after real use the rule-based router clearly falls short, v2 can add an LLM fallback *after* all rule-based matchers have passed.

### 10.4 Result format

Every MCP tool response includes:

```json
{
  "results": [...],
  "provenance": {
    "matcher": "exact-key:last_command",
    "rows_scanned": 12,
    "elapsed_ms": 3
  },
  "confidence": 0.85,
  "verified_at": "2026-04-18T14:22:00Z"
}
```

Provenance is non-optional. Agents should never get a result without knowing where it came from and how stale it is.

## 11. Write-back policy

This is the other place v1 was hand-wavy. Fleshed out:

### 11.1 Write categories

| Signal type        | Examples                                                    | Action |
|--------------------|-------------------------------------------------------------|--------|
| **Verified**       | Command that exited 0, test that passed, git diff that landed | Auto-write, high initial confidence (0.7) |
| **Observed**       | Git history, file structure, file contents                  | Auto-write, high confidence (0.9) — these are facts about reality |
| **Self-reported**  | Agent says "I decided X" or "this should be a rule"         | Stage to a review queue, require user confirmation before promoting to `decisions` / `facts` |
| **User-authored**  | User runs `memhub fact add` / `decision add`                | Direct write, confidence 1.0 |

### 11.2 Verifiable signals — concrete list

- **Command success:** process exit code 0, AND (if applicable) the command produced expected output. Recorded with the exact cmdline, cwd, and timestamp.
- **Test pass:** test runner invocation returned 0 AND stdout contains a recognizable pass indicator (framework-specific patterns; configurable).
- **Git diff landed:** commit made, hash known. Used for "X file was changed to fix Y bug" claims.
- **User said yes:** MCP tool prompted user through agent, user replied affirmatively. Logged in `writes_log` with the exact prompt and response.

Agent claiming "that worked" in chat is **not** a verifiable signal.

### 11.3 Review queue

Self-reported writes go to `pending_writes` with full context (session, agent, prompt, proposed change). User reviews via:

```
memhub review              # interactive review of pending writes
memhub review --auto-accept --confidence-above 0.8   # batch accept (v2)
```

Pending writes older than 30 days auto-expire. Better to lose a maybe-fact than to accumulate garbage.

### 11.4 Confidence decay

Facts have a `verified_at` timestamp. Queries can filter by freshness. A fact not re-verified in 90 days gets flagged stale in results. Commands that fail get their confidence dropped; commands that succeed get it raised.

## 12. MCP tool surface

Minimum viable set. Each tool is a thin wrapper over the router or writer.

```
memhub.search(query: string, kind?: string, limit?: int)
    → {results, provenance, confidence}

memhub.get_fact(key: string)
memhub.get_command(kind: "build" | "test" | "run" | "lint" | "other")
memhub.list_tasks(status?: string)
memhub.get_decisions(topic?: string)
memhub.file_history(path: string, limit?: int)

memhub.record_command(kind, cmdline, exit_code, stdout_tail?, cwd)
memhub.propose_fact(key, value, rationale)    // goes to pending_writes
memhub.propose_decision(title, rationale)     // goes to pending_writes
memhub.log_session_note(text)                 // free-form, no promotion

memhub.stats()   // for agents that want to know DB size, last sync, etc.
```

Note the asymmetry: read tools are direct, write tools either require verifiable signals (`record_command` wants an exit code) or stage to the review queue (`propose_*`). There's no `memhub.write_fact_directly` — that only exists as a CLI command for the user.

## 13. CLI surface

```
memhub init                          # create .memhub/ in current repo
memhub status                        # summary of what's stored
memhub sync-md                       # regenerate managed blocks in CLAUDE.md/AGENTS.md
memhub ingest-git [--since <ref>]    # pull git history into DB
memhub search <query>                # same router, from CLI
memhub tasks [list|add|done|block]
memhub fact [add|list|rm]
memhub decision [add|list|supersede]
memhub command [list|verify]
memhub review                        # review pending writes
memhub log <text>                    # manual session note
memhub export <path>                 # dump DB to portable format
memhub import <path>                 # restore from export
memhub migrate                       # run schema migrations
memhub serve                         # start MCP server
```

## 14. Migrations and export

Schema will change. Planning for it from day one:

- Every schema change ships as a numbered SQL file in `src/migrations/`.
- `memhub migrate` applies any missing migrations, backing up the DB first to `.memhub/backup-<timestamp>.sqlite`.
- `memhub export` dumps to a version-tagged JSON format (not a SQL dump) so future versions can re-import even after breaking schema changes.
- Old backups older than 30 days get cleaned up by `memhub gc`.

## 15. Security / privacy

- Default deny-list for sensitive patterns: `.env*`, `*.pem`, `*.key`, `id_rsa*`, `secrets/*`, anything matching common AWS/GCP credential patterns. Configurable per-repo.
- Agents reading memory can't read these. They're not ingested in the first place.
- `writes_log` is never purged — it's small and useful.
- No telemetry. No network calls unless user explicitly invokes one (e.g., future `memhub ingest-github-pr` feature, which is not in v1).

## 16. Milestones

Instead of MVP/Phase 1/Phase 2, break into milestones where each one produces a usable tool. You can stop at any milestone and have something that works.

### Milestone 1: "DB + CLI"
- Rust project scaffolded, CI green
- SQLite schema + migrations
- Config loading
- `memhub init`, `status`, `fact`, `decision`, `task`, `command` CLI subcommands
- `writes_log` working
- No MCP server yet, no agent integration, no git ingestion
- **Done criteria:** I can track project state by hand from the CLI and it's already useful.

### Milestone 2: "Git + search"
- `memhub ingest-git` — walks git log, populates `commits`, `commit_files`, `files`
- FTS5 index over `chunks`
- `memhub search` with rule-based router
- **Done criteria:** I can answer "when did I last touch this file" and "find decisions about X" from the CLI.

### Milestone 3: "MCP + Markdown sync"
- MCP server exposing the read + write-back tools
- `memhub sync-md` — managed block generation in CLAUDE.md / AGENTS.md
- Write-back policy enforced (verifiable signals + review queue)
- **Done criteria:** Claude Code and Codex both see the same project state through MCP and their markdown files.

### Milestone 4: "Quality"
- Confidence scoring + decay
- Stale-fact flagging
- `memhub review` flow
- Export / import
- Deny-list enforcement
- **Done criteria:** I trust the DB enough to stop keeping duplicate notes elsewhere.

### Milestone 5+ (speculative)
- Global DB features beyond prefs
- Manual session compaction tooling (`memhub log` + `memhub session close` that prompts for a summary — still *not* automatic)
- Optional embeddings for fuzzy search
- File watcher for live updates
- Tauri desktop inspector
- GitHub PR metadata ingestion (opt-in, network feature)
- LLM-based router fallback

Anything in milestone 5+ gets its own mini-PRD when it's time to build it, not before.

## 17. Success metric

**The only real one:** am I still using this in 3 months on at least one active project? If yes, it works. If no, it's productivity theater.

Supporting telemetry (local, for me, not phoned home):
- Query count per week (via `writes_log` and a simple read counter) — is usage growing, steady, or dying?
- Pending-writes review rate — if I never review, the queue feature isn't working.
- Stale-fact ratio — if most facts are stale, I'm not using it during real sessions.

No dashboards. A `memhub stats` command that prints these is plenty.

## 18. Risks

| Risk | Mitigation |
|------|------------|
| Schema churn breaks my existing DB | Migrations mandatory from day one; export format stable |
| Router too dumb, agents get bad results | Provenance always returned; agents can ignore low-confidence results |
| Managed-block overwrites user edits | Strict markers, backup before rewrite, never touch outside markers |
| I lose interest after milestone 2 | Each milestone is independently useful — that's the design |
| MCP spec changes | Rust ecosystem is young, so pin MCP crate version; revisit at each milestone |
| "Just one more feature" scope creep | Milestone 5+ requires its own mini-PRD — hard gate |

## 19. Resolved decisions (previously open)

1. **Managed-block location:** Hardcoded to **bottom** of CLAUDE.md / AGENTS.md. No config option. User's hand-authored rules stay at the top where they're most visible; managed block is appendix-style reference data. If it becomes annoying later, it's a five-line change.
2. **Agent identification:** Use `clientInfo.name` from the MCP initialize handshake. This is the protocol-designed mechanism — MCP clients announce themselves at connection time. Normalize known aliases (e.g., `claude-ai` → `claude-code`). Store raw value on session row if unrecognized, log it, add to alias map over time. Falls back to `"unknown"` if no `clientInfo` provided.
3. **`.memhub/` gitignore default:** Ignored by default. A post-init message tells the user how to commit it instead if they want the DB to travel with the repo. Opt-in, not opt-out — prevents noisy diffs on every write.

## 20. Still open

1. **MCP Rust crate choice.** `rmcp` is the leading candidate but verify current maintenance status and MCP spec alignment (target 2025-06-18 or newer) at build time.
2. **Chunk sources for FTS.** Initial set: decision rationales, session notes, command stdout tails. Expand as real use reveals gaps.
3. **Client alias normalization map.** Needs initial population with Claude Code and Codex identifiers as they exist at build time. Driven by observed `clientInfo.name` values in real use.

## 21. Next artifact

The point where this stops being a PRD and becomes a repo plan is the concrete schema + MCP tool contract. Next doc to write:

**`memhub-v1-spec.md`** — full SQL schema with all constraints, full MCP tool JSON schemas with examples, full CLI help output. Roughly 2–3x the length of this PRD. That's the doc Claude Code and Codex get handed when it's time to start building.