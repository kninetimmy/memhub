# memhub

A local-first, per-repo project memory that **Claude Code and Codex CLI share** — now with hybrid SQL + vector recall.

You write a fact, decision, or task from either agent — or directly from the terminal — and it lands in one SQLite database at `.memhub/project.sqlite`. Both agents read the same rows via the same MCP tools and slash commands. Switching tools, or coming back to a project after a month, doesn't cost you context. Asking *"why did we pick rusqlite bundled mode?"* mid-session pulls the actual decision back, not a paraphrase.

## What it is, in plain language

Claude Code and Codex each have their own notes systems, and they don't talk to each other. If you use both on the same project, you either keep two sets of notes in sync by hand, or you don't, and they drift.

memhub is a small Rust CLI that puts one structured store per repo. Facts, decisions, tasks, recent commands, session notes — all in SQLite, queryable, with attribution. As of M8, it also ships a bundled embedding model (`bge-small-en-v1.5`, ~130 MB inside the binary) so agents can do **semantic recall** without any network, account, or vector-DB infrastructure.

Rendered markdown is a local human-readable view generated from the DB, stored under `.memhub/rendered/` by default and ignored by Git. The database is the source of truth, but it is machine-local too; use export/import for intentional moves between machines rather than committing DB, embeddings, or render output.

**Four things matter:**

- **Offline.** No cloud, no account, no daemon, no model download at runtime. Just a binary and a `.sqlite` file. The embedding model is bundled into the binary at build time and runs locally.
- **Per-repo.** Different projects, different databases. No global state to coordinate.
- **Attributed.** Every write records both *where the claim came from* (`user`, `agent:codex`, `user+agent:claude-code`, …) and *who performed the write* (`cli:user`, `claude:wrap-up`, …). You can always tell whether a fact came from you typing it, or from an agent surfacing it during a wrap-up that you approved.
- **Hybrid recall.** Keyword (FTS5 BM25) + semantic (cosine over local embeddings), blended into one ranked evidence bundle. Stays FTS-only if you'd rather skip the model load; the install prompt asks.

---

## Quickstart

### One-line install + initialize

```bash
git clone https://github.com/kninetimmy/memhub.git ~/src/memhub \
  && cargo install --path ~/src/memhub --force \
  && cd /path/to/your/project \
  && memhub init && memhub status
```

That builds the binary, installs it on PATH, and initializes memhub. The first `cargo install` is the slow one (~2-3 minutes — it downloads + SHA-pins the BGE-small ONNX model and bundles it into the binary). Subsequent installs reuse the cache. Everything below is polish.

### Install via Claude Code (recommended — agent-driven)

Open Claude Code in the repo you want memhub to track. Paste this:

```
Please install memhub for me, then turn on hybrid recall.

1. Clone https://github.com/kninetimmy/memhub.git into ~/src/memhub if it
   isn't already there (`git pull` if it is). Stop if the Rust toolchain
   (1.85+) is missing.
2. Run `cargo install --path ~/src/memhub --force` so `memhub` ends up on
   PATH (~/.cargo/bin must be on PATH; warn me if it isn't). First build
   takes a couple of minutes — it downloads and bundles a ~130 MB
   embedding model into the binary.
3. Run `memhub --version` to verify.
4. Copy the user-level skills so /wrap-up, /check-init, /init-project,
   /recall, /reindex, and /eval-recall all work as slash commands:

       cp ~/src/memhub/templates/skills/claude/*.md ~/.claude/commands/

5. cd back to this repo and run `memhub init`, then `memhub status`.
   Tell me what status reports.
6. Ask me: hybrid recall (recommended — semantic + keyword) or FTS-only
   (lighter, keyword search only)?
     - If I say hybrid: append `[retrieval]\nmode = "hybrid"` to
       .memhub/config.toml, then run `memhub index rebuild --actor
       claude-code:reindex`. Report how many rows were embedded.
     - If I say FTS: nothing to do; the default is already FTS.
7. Run `memhub recall "<some keyword from my project>" --max-results 3`
   so I can see the recall surface working end-to-end.

Don't touch any files in this repo other than what `memhub init` writes
(.memhub/ and the generated-output .gitignore entries) and the
.memhub/config.toml edit in step 6.
```

### Install via Codex CLI (agent-driven)

Open Codex in the repo you want memhub to track. Paste this:

```
Please install memhub for me, then turn on hybrid recall.

1. Clone https://github.com/kninetimmy/memhub.git into ~/src/memhub if it
   isn't already there (`git pull` if it is). Stop if the Rust toolchain
   (1.85+) is missing.
2. Run `cargo install --path ~/src/memhub --force` so `memhub` ends up on
   PATH (~/.cargo/bin must be on PATH; warn me if it isn't). First build
   takes a couple of minutes — it downloads and bundles a ~130 MB
   embedding model into the binary.
3. Run `memhub --version` to verify.
4. Register memhub as an MCP server so you can call it as a structured
   tool. Append this to ~/.codex/config.toml:

       [mcp_servers.memhub]
       command = "memhub"
       args = ["serve"]

5. Copy the user-level skills so /wrap-up, /check-init, /init-project,
   /recall, /reindex, and /eval-recall all work:

       cp -R ~/src/memhub/templates/skills/codex/* ~/.codex/skills/

6. cd back to this repo and run `memhub init`, then `memhub status`.
   Tell me what status reports.
7. Ask me: hybrid recall (recommended — semantic + keyword) or FTS-only
   (lighter, keyword search only)?
     - If I say hybrid: append `[retrieval]\nmode = "hybrid"` to
       .memhub/config.toml, then run `memhub index rebuild --actor
       codex:reindex`. Report how many rows were embedded.
     - If I say FTS: nothing to do; the default is already FTS.
8. Run `memhub recall "<some keyword from my project>" --max-results 3`
   so I can see the recall surface working end-to-end.

Don't touch any files in this repo other than what `memhub init` writes
(.memhub/ and the generated-output .gitignore entries) and the
.memhub/config.toml edit in step 7.
```

### Install by hand

```bash
# 1. Build + install the binary (slow on first build; bundles BGE-small)
git clone https://github.com/kninetimmy/memhub.git ~/src/memhub
cargo install --path ~/src/memhub --force

# 2. Verify
memhub --version

# 3. Initialize in your project
cd /path/to/your/project
memhub init
memhub status

# 4. Agent skills (Claude + Codex)
cp ~/src/memhub/templates/skills/claude/*.md ~/.claude/commands/
cp -R ~/src/memhub/templates/skills/codex/*  ~/.codex/skills/

# 5. MCP for Codex (append to ~/.codex/config.toml)
#   [mcp_servers.memhub]
#   command = "memhub"
#   args = ["serve"]

# 6. (Recommended) Turn on hybrid recall
#    Add to .memhub/config.toml:
#       [retrieval]
#       mode = "hybrid"
#    Then backfill embeddings for the existing rows:
memhub index rebuild --actor cli:user
memhub index status   # confirm Missing: 0
```

---

## A typical session

The default mode is **agent-driven**: you talk to Claude Code or Codex, and the agent calls memhub via MCP tools or the CLI on your behalf. You rarely need to drop into the terminal.

### What you say → what the agent does

```
You: "What did we decide about authentication?"
  → memhub.recall "authentication" (hybrid ranked bundle, cited)

You: "What's in flight on this project?"
  → list_tasks, list_decisions, status (read tools)

You: "Add a task to refactor the cache layer."
  → task_add (durable write; tasks are intent, easy to delete)

You: "Mark task 7 as done."
  → task_done

You: "What's the build command for this repo?"
  → memhub.recall "build command" (often hits a fact at rank 1)

You: "Note: tried the router rewrite, no measurable diff."
  → log_session_note (write-only scratch)

You: "Remember the build command is cargo build."
  → propose_fact (stages in pending_writes for /wrap-up approval)

You: "We're going to use rusqlite bundled mode because <rationale>."
  → propose_decision (stages for /wrap-up approval)

You: "Re-render the local memhub docs."
  → render
```

Facts and decisions stage in `pending_writes` instead of going durable directly — that's the "agents are untrusted writers" guardrail. They become durable when you approve them at `/wrap-up`, where the source becomes `user+agent:<your-agent>`.

### Mid-session context: `/recall` over rendered files

When the agent needs project context, the rule is: read the local rendered `PROJECT.md` if it exists, then call `memhub.recall` mid-session for anything deeper. The full `PROJECT_LEDGER.md` is a fallback for when recall comes up empty.

Recall returns a cited evidence bundle — title, body, score, source, staleness flag — pulled from the durable tables. It's read-only and never logs to `writes_log`. Empty bundles are honest answers, not failures.

### End-of-session: `/wrap-up`

Run `/wrap-up` and the agent walks you through:

- New facts / decisions surfaced this session (accept or reject each)
- Pending MCP proposals to triage
- Tasks added or closed
- A short session summary written to `session_notes`
- An updated state narrative if anything material changed
- A re-render of the configured local `PROJECT.md` and `PROJECT_LEDGER.md`

Each item gets your individual approval before it lands.

### If you'd rather drive from the terminal

Everything the agent does has a CLI equivalent:

```bash
memhub status
memhub recall "auth flow"
memhub task add "Refactor cache layer"
memhub task done 7
memhub fact list
memhub fact add build-command "cargo build"
memhub decision add "Use rusqlite bundled mode" \
  --rationale "Avoid system SQLite setup friction."
memhub note add "Tried router rewrite, no measurable diff."
memhub render
```

CLI use is fine — sometimes faster, always available, and what you'll want for batch operations or scripting. The two flows write to the same database; the only difference is the `source` and `actor` columns on each row, which let you tell later who wrote what.

---

## Two retrieval modes

memhub ships with two retrieval modes. Both are first-class; the install prompt asks you to pick one.

| | **`fts`** (default) | **`hybrid`** (recommended) |
|---|---|---|
| Scoring | FTS5 BM25 over title + body | 0.5 × FTS + 0.5 × cosine − 0.3 × stale_penalty |
| What it catches | Exact terms, stemmed variants | Exact + paraphrases (`"compile a release"` → `release_build` fact) |
| Per-write cost | 0 ms | ~50 ms eager-embed inside the source-write transaction |
| Per-recall cost | <10 ms | <100 ms (brute-force cosine over current corpus) |
| Disk footprint | None beyond source rows | ~1.5 KB per row (384-dim f32 vector) |
| Network | Never | Never. Model is bundled. |
| Best for | Small projects, scripted use | Multi-month projects where you forget exact wording |

**Switching modes is non-destructive.** Going `fts → hybrid` requires one `memhub index rebuild` to backfill embeddings for the rows you already have. Going `hybrid → fts` just stops consulting the embeddings table; nothing is deleted.

**The `[retrieval]` block** in `.memhub/config.toml`:

```toml
[retrieval]
mode = "hybrid"                  # "fts" | "hybrid"
default_max_results = 6
accepted_only_by_default = true  # filter to source IN ('user', 'user+agent:%')
include_stale_by_default = false # hide stale facts unless asked

[retrieval.scoring]
fts_weight = 0.5
vector_weight = 0.5
stale_penalty = 0.3
```

Stale embeddings (model upgrade, content drift, or pre-existing rows that haven't been indexed yet) are detected per recall call and surfaced as a `warnings[]` entry. The agent asks before running `/reindex` — recall results stay usable in the meantime.

---

## What's in the box

| Command | What it does |
|---|---|
| `memhub init` | Set up `.memhub/` in a repo |
| `memhub status` | Open tasks, stale facts, pending writes, schema version |
| `memhub recall <query>` | Hybrid ranked bundle of facts/decisions/tasks (the M8 surface) |
| `memhub fact add/list` | Durable key-value facts (build commands, MSRV, etc.) |
| `memhub decision add/list` | Decisions with rationale, FTS-indexed and embedded |
| `memhub task add/list/done` | Lightweight task tracking |
| `memhub command verify` | Record verified command outcomes; derives confidence |
| `memhub note add/list` | Session notes (low-stakes scratch; not in recall) |
| `memhub state set/show` | The "current state" narrative |
| `memhub arch set/show` | The architecture narrative |
| `memhub ingest-git` | Pull commit + file history into the DB |
| `memhub search <query>` | Indexed search over decisions and file history (legacy; prefer recall) |
| `memhub review list/accept/reject` | Triage agent-proposed writes |
| `memhub render` | Emit local `PROJECT.md` and `PROJECT_LEDGER.md` from the DB |
| `memhub index status/rebuild` | Embedding coverage; one-shot backfill for `fts → hybrid` migrations |
| `memhub eval retrieval` | Run the Recall@K harness against `tests/retrieval_golden.json` |
| `memhub stats --window 7d` | Write activity by actor, review rate, stale-fact counts |
| `memhub export/import` | Portable JSON backup; cross-machine restore |
| `memhub serve` | Stdio MCP server for Claude Code / Codex |

Run any command with `--help` for flags.

---

## Compatibility

### Claude Code

- Reads `CLAUDE.md` at session start.
- User-level slash commands at `~/.claude/commands/`: `/wrap-up`, `/check-init`, `/init-project`, `/recall`, `/reindex`, `/eval-recall`.
- Skill writes are attributed `actor=claude:wrap-up`, `source=user+agent:claude-code`.

### Codex CLI

- Reads `AGENTS.md` at session start (same role as CLAUDE.md).
- User-level skills at `~/.codex/skills/`: same six as above.
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

## Deeper dive

### One database, one truth, two views

```
.memhub/project.sqlite          ← source of truth (durable, gitignored)
.memhub/rendered/PROJECT.md        ← local rendered narrative (gitignored)
.memhub/rendered/PROJECT_LEDGER.md ← local rendered structured view (gitignored)
```

`memhub render` regenerates the markdown from the DB. The markdown is one-way output: there's no parser that reads human edits back into the DB. To change durable content, use the CLI (or have an agent do it via `/wrap-up`). If you want render output committed for a specific repo, set `[render].output_dir` to an in-repo path and remove that path from `.gitignore`; that is opt-in.

### How attribution works

Two columns split the work:

- `source` on `facts` and `decisions` — *origin of the claim*. One of `user`, `agent:<id>`, `user+agent:<id>`, `git`, `observed`. Full vocabulary: [`docs/reference/memhub-prd-source-vocabulary-addendum.md`](docs/reference/memhub-prd-source-vocabulary-addendum.md).
- `actor` on `writes_log` and `pending_writes` — *who performed the write*. Free-form, e.g. `cli:user`, `claude:wrap-up`, `codex:wrap-up`.

When you accept a pending MCP proposal via `memhub review accept`, the durable row's `source` becomes `user+agent:<actor>` automatically — both signals preserved without you passing anything.

### Recall: hybrid scoring + the warnings channel

`memhub.recall` is the primary read tool in hybrid mode. It:

1. Runs an FTS5 lookup per source table (`facts_fts`, `decisions_fts`, `tasks_fts` — contentless virtual tables that point at the live source rows).
2. (Hybrid mode only) Embeds the query with the bundled BGE-small model and does brute-force cosine over the active-model `embeddings` table.
3. Blends both signals with the configured weights (default `0.5 × fts + 0.5 × vec − 0.3 × stale`), after min-max-normalizing FTS BM25 onto `[0, 1]`.
4. Applies filters (`source_type` allowlist, `include_stale`, `accepted_only`) before scoring.
5. Returns a ranked bundle with `score`, `fts_score`, `vector_score`, `confidence`, `stale`, and `source` for every hit.

Recall is **read-only**: it never writes to `writes_log`, never stages a pending write, never mutates durable rows. Safe to call as many times as you want.

If recall detects any candidate row whose embedding is missing or whose `content_hash` has drifted from the current source body, it surfaces a `warnings[].kind == "stale_embeddings"` entry. Results are still returned (with vector_score = 0 for the affected rows); the agent surfaces the warning and **asks before running `/reindex`**.

### MCP server

`memhub serve` starts a stdio MCP server. Tools:

- **Read:** `status`, `search`, `recall`, `list_tasks`, `list_decisions`, `list_facts`, `list_pending_writes`, `get_command`
- **Write (direct):** `task_add`, `task_done`, `record_command`, `log_session_note`, `render`
- **Write (staged for review):** `propose_fact`, `propose_decision`

Tasks and session notes are direct writes because they're low-stakes (intent and scratch, not claims). Facts and decisions stage in `pending_writes` and only become durable when you approve them — usually through `memhub review accept` during `/wrap-up`. `render` is a thin side-effect tool: it regenerates the configured local `PROJECT.md` and backs up the prior version.

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
- **Embeddings** go stale when the source body changes (detected by `content_hash` drift) or when the binary upgrades to a different bundled model. Recall surfaces a warning; the user decides whether to `/reindex`.

Continuous decay is deliberately not implemented. Decisions don't have confidence.

### Eval harness

`tests/retrieval_golden.json` ships with 12 starter queries used to baseline `Recall@K` for the memhub repo itself. `memhub eval retrieval` (and the `/eval-recall` slash command) run the harness:

```bash
memhub eval retrieval                # markdown summary
memhub eval retrieval --json         # structured for skill consumption
memhub eval retrieval --golden tests/my-other-set.json --k 5
memhub eval retrieval --mode fts     # compare modes A/B
```

The harness is read-only. For your own projects, write a `tests/retrieval_golden.json` that pulls real titles/keywords from your DB and pass `--golden <path>`. Matchers use `title_contains` / `body_contains` substring checks so the file survives row-ID shifts.

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

Export covers facts, decisions, tasks, commands, pending writes, and writes_log. Git ingestion, FTS chunks, embeddings, `.memhub/config.toml`, and rendered markdown are local or derived state and are not committed by default. Session notes are not in the v1 export format.

---

## How it's built

Single Rust binary over an embedded-migration SQLite database. The MCP server reuses the same command layer. The embedding model is bundled at build time via `build.rs` (downloads from Hugging Face, SHA256-pinned); no model download at runtime.

```text
memhub CLI / MCP
   ├── src/commands/    fact / decision / task / command / review / eval / index / ...
   ├── src/db/          path discovery, migrations, audit log
   ├── src/config/      per-repo TOML (incl. [retrieval] block)
   ├── src/mcp/         stdio MCP server, client identity normalization
   ├── src/retrieval/   bundled BGE-small wrapper, eager-embed, hybrid recall
   ├── src/sync_md/     managed markdown rewrite
   ├── src/render/      PROJECT.md and PROJECT_LEDGER.md emit
   └── src/export/      v1 portable JSON
       │
       └── SQLite (.memhub/project.sqlite) + git CLI + bundled BGE-small ONNX
```

Schema is at migration `0010_embeddings_delete_triggers` (FTS5 source-table indexes + `embeddings` table + cascade triggers). Run `memhub render` for a local `.memhub/rendered/PROJECT.md` architecture snapshot.

---

## Project status

**Active core (shipping):**

- CLI for facts / decisions / tasks / commands / state / arch / notes / review / stats
- Stdio MCP server with client-identity auto-attribution
- `memhub render` emits local `PROJECT.md` + `PROJECT_LEDGER.md` under `.memhub/rendered/` by default
- Portable JSON export/import with single-step `init --from-backup`
- Compound source vocabulary for multi-agent attribution
- Per-repo deny list for sensitive paths
- Claude Code and Codex CLI bridge: shared DB, parity skills, MCP registration
- **M8 hybrid recall:** bundled BGE-small model, eager-embed write path, FTS5 contentless virtual tables, brute-force cosine, `recall` CLI + MCP tool, `index rebuild`, `eval retrieval` harness, `/recall` + `/reindex` + `/eval-recall` skills

**Not implemented yet:**

- Continuous confidence decay
- Min-score threshold on recall (low-similarity hits can leak into hybrid-mode nonsense queries)
- Cross-repo / global memory layer
- File watcher
- Desktop inspector
- `sqlite-vec` for projects past ~10,000 embeddable rows (current brute-force cosine is fine well below that)

PRD authority: [`docs/reference/memhub-prd.md`](docs/reference/memhub-prd.md) (kept verbatim; changes land as addenda). M8 design is in [`docs/reference/memhub-prd-addendum-m8-retrieval.md`](docs/reference/memhub-prd-addendum-m8-retrieval.md).

---

## Principles

- **Local-first.** No network, no daemon, no account, no runtime model download.
- **One per repo.** Project boundaries are repo boundaries.
- **Boring tech.** SQLite, Rust, glob patterns, FTS5, brute-force cosine. No vector DB, no extension loading, no Python.
- **Agents are untrusted writers.** Agent proposals stage in `pending_writes` until a human approves. The schema enforces it.
- **Recall is read-only.** Retrieval never writes to `writes_log` or any durable table.
- **Narrow milestones.** Ship usable slices; defer speculative work until a real workflow demands it.

---

## Further reading

- [Product PRD (verbatim)](docs/reference/memhub-prd.md)
- [M8 hybrid retrieval addendum](docs/reference/memhub-prd-addendum-m8-retrieval.md)
- [Source vocabulary addendum](docs/reference/memhub-prd-source-vocabulary-addendum.md)
- [K9 deprecation addendum](docs/reference/memhub-prd-deprecation-addendum.md) — memhub used to coexist with the K9 markdown framework; that integration is retired as of 2026-05-13. K9 markdown files in older repos are historical archive.
- Local project state: run `memhub render`, then read `.memhub/rendered/PROJECT.md`
- Local project ledger: run `memhub render`, then read `.memhub/rendered/PROJECT_LEDGER.md`
