# memhub

*Your AI agents forget things. memhub doesn't.*

---

memhub is a small, offline, per-repo memory system for AI coding assistants. It gives Claude Code and Codex CLI one shared, searchable store of project facts — built on SQLite, with semantic search bundled right into the binary.

No cloud. No account. No daemon. No model download at runtime. Just a `.sqlite` file that lives next to your code and a binary on your PATH.

<br>

<p align="center">
  <img src="docs/images/system-overview.svg" alt="memhub system overview" width="920"/>
</p>

---

## How it works

### 1. The store (SQL)

Everything lands in a SQLite database at `.memhub/project.sqlite`. Facts, decisions, tasks, session notes — all structured rows with full-text search indexes built in (FTS5), plus timestamps and source attribution on every write.

SQLite means no server, no setup, and the whole thing is just a file you can inspect, back up, or export whenever you want. Migrations apply automatically on startup; you never need to think about schema versions.

### 2. The semantic layer (RAG)

Keyword search is great when you remember the exact term you used. It's less great three months in when you're asking about "compile settings" and the relevant fact is filed under `release_build`.

memhub ships with a bundled embedding model (BGE-small-en-v1.5, ~130 MB, compiled into the binary at build time). Every fact and decision you write also gets embedded as a vector. When you recall something, memhub runs both a keyword search and a semantic similarity search in parallel, blends the scores, then runs a cross-encoder re-ranker over the top candidates — all locally, no network call.

<br>

<p align="center">
  <img src="docs/images/hybrid-recall.svg" alt="how hybrid recall works" width="840"/>
</p>

The result is a ranked, cited evidence bundle. You get `title`, `body`, `score`, `source`, and a staleness flag for every hit. The agent gets crisp context; you keep a record of where it came from.

### 3. The agent bridge (MCP + skills)

memhub speaks [MCP](https://modelcontextprotocol.io/) (Model Context Protocol), the standard tool-call interface that Claude Code and Codex both use. When the agent needs context, it calls `memhub.recall` — a structured tool call, not a file read. It gets back a ranked bundle, not a wall of markdown.

When the agent wants to *write* something, it calls `propose_fact` or `propose_decision`. The proposal lands in `pending_writes` — a staging area — and waits there until you review it. Nothing an agent proposes becomes durable fact until you've said yes.

Tasks and session notes are different: those write directly, because they're low-stakes (intent and scratch, not claims).

---

## What gets saved (and when)

| Type | What it's for | Who writes it | Goes straight to DB? |
|---|---|---|---|
| **facts** | Key project knowledge: build commands, MSRV, env vars, naming conventions | You or agent | Agent: no (staged). You: yes. |
| **decisions** | Design choices with rationale and context | You or agent | Agent: no (staged). You: yes. |
| **tasks** | Lightweight to-dos and in-flight work | You or agent | Yes — low-stakes |
| **session notes** | Observations and scratch thoughts during a session | Agent | Yes — scratchpad only, not recalled |
| **commands** | Verified shell commands with success/fail tracking | You or agent | Yes — observational |
| **state / arch** | The "currently building" and architecture narratives | Agent at wrap-up | Yes — agent-authored but explicit |
| **reference docs** | External markdown you point it at: specs, contracts, style guides | You (you hand it a file) | Yes — user-pointed, opt-in to recall |

The rule behind the table: things that could be *wrong* — facts that might be outdated, decisions that might be misattributed — need a human in the loop. Things that are clearly ephemeral or observational write directly. Reference docs are a fourth case — not a claim, not observational, just material you explicitly handed to memhub — so they write directly but stay out of the default recall bundle (see [Point it at your design docs](#point-it-at-your-design-docs)).

When an agent proposal gets staged in `pending_writes`, the source is recorded as `agent:claude-code` or `agent:codex`. When you accept it at `/wrap-up`, that source upgrades to `user+agent:claude-code` — both signals preserved, so you can always tell later what was verified.

---

## Agent-driven vs. driving it yourself

### The usual way: let the agent handle it

In a normal session, you talk to your agent and memhub runs in the background. The agent reads `PROJECT.md` at session start for the narrative context, calls `memhub.recall` when it needs a specific fact mid-session, and stages any new knowledge it wants to record.

At the end of a session, you run `/wrap-up` (a slash command in Claude Code, a skill in Codex). The agent walks you through everything it wants to commit — new facts, decisions, task updates, a short session summary — one item at a time. You approve or skip each one. Then it re-renders the local `PROJECT.md` and `PROJECT_LEDGER.md` from the database.

```
You: "What did we decide about the authentication flow?"
  → memhub.recall "authentication flow"  (returns cited evidence bundle)

You: "Add a task to refactor the cache layer."
  → task_add "refactor cache layer"  (direct write, done)

You: "We're going to use rusqlite bundled mode because X."
  → propose_decision "use rusqlite bundled mode" --rationale "X"  (staged)

You: "/wrap-up"
  → agent reviews staged proposals with you, one by one
  → you say yes or no to each
  → session note written, PROJECT.md re-rendered
```

The `/wrap-up` gate is the whole point. The agent is helpful for surfacing and structuring knowledge; you're the one who decides what's true.

### If you'd rather type

The CLI is a first-class interface. Everything the agent does has a terminal equivalent:

```bash
memhub status
memhub recall "auth flow"
memhub task add "Refactor cache layer"
memhub task done 7
memhub fact add build-command "cargo build --release"
memhub decision add "use rusqlite bundled mode" \
  --rationale "Avoid system SQLite setup friction."
memhub note add "Tried the router rewrite — no measurable diff."
memhub render
```

The two flows write to the same database. The only difference is the `source` column on each row — `user` for what you typed directly, `user+agent:<id>` for what you approved through a `/wrap-up`. `memhub stats --window 7d` shows a breakdown of writes by actor.

---

## One machine, many projects

memhub is per-repo by design. Every project gets its own `.memhub/project.sqlite` — completely isolated. There's no global database, no coordination between repos, no leakage between projects.

```
~/code/
├── my-web-app/
│   └── .memhub/project.sqlite   ← web app memory
├── my-cli-tool/
│   └── .memhub/project.sqlite   ← CLI tool memory
└── my-library/
    └── .memhub/project.sqlite   ← library memory
```

The `memhub` binary is installed once at `~/.cargo/bin/memhub`. Each project's database is independent. To add memhub to a new project, just `cd` into it and run `memhub init`. The recall surface, the dashboard, the stats — everything reads whichever repo's database is in your current working directory.

This also means you can try memhub on one project without any risk to your others. If you decide it's not for you, `rm -rf .memhub/` is the entire uninstall.

---

## Moving between machines

memhub state is machine-local by default — the database, embeddings, and rendered markdown are all gitignored. Only code and migrations travel with the repo. To carry your memory to another machine:

```bash
# on your current machine
memhub export ~/memhub-myproject-backup.json

# move the file however you like (Drive, USB, scp)

# on the new machine, after cloning the repo + installing memhub
memhub init --from-backup ~/memhub-myproject-backup.json
memhub index rebuild   # re-generate embeddings from the imported rows
```

The export format is versioned JSON. It covers facts, decisions, tasks, commands, pending writes, writes log, session notes, and both narrative tables. Embeddings are not included — the target machine re-derives them via `memhub index rebuild`.

---

## The web dashboard

Run `memhub viz` in your project directory (or `/viz` in Claude Code) to open a local read-only dashboard in your browser. It serves from localhost, reads the current repo's database, and never writes anything.

Six tabs:

- **Overview** — open tasks, recent decisions, pending writes, and current project state at a glance
- **Embedding Map** — a 2D PCA projection of your semantic memory space; points are facts and decisions, clustered by meaning
- **Recall Inspector** — type any query and see per-row scores in real time: FTS score, vector score, and final re-ranked position
- **Activity** — a write-history feed with actor attribution
- **Audit** — the full `writes_log`, every write ever, with source and actor
- **Token Metrics** — input/output/cache token totals from your Claude Code sessions, a cumulative per-turn burn-up chart, and a context-offset estimate comparing targeted recall bundles vs. loading the full project ledger

<!-- TODO: add screenshot of Overview tab here -->
<!-- TODO: add screenshot of Embedding Map tab here -->
<!-- TODO: add screenshot of Token Metrics tab here -->

---

## Point it at your design docs

Sometimes the thing the agent needs to know isn't a fact you wrote down — it's sitting in a spec file on your desktop. A design system, an API contract, a style guide: long external markdown you don't want to paste into every prompt and don't want to hand-transcribe into facts.

Point memhub at it:

```bash
memhub doc add ~/specs/design-system.md
```

memhub splits the file into section-aware chunks — heading breadcrumbs preserved, so a hit knows it came from *Typography > Design Tokens*, not just "somewhere in a 14 KB file" — embeds each one, and makes the whole thing searchable through the exact same hybrid recall path as everything else.

The catch — and it's deliberate — is that ingested docs are **opt-in**. They never show up in the default recall bundle. A design spec is reference material, not durable project knowledge, so it doesn't get to crowd out your facts and decisions. Instead, when docs exist, recall returns an `available_docs` count alongside the normal results. The agent sees that signal and, when the question is design- or spec-flavored, runs one follow-up doc-scoped query:

```bash
memhub recall "how should color tokens be named" --source-type doc
```

You can scope to docs explicitly any time with `--source-type doc`. Docs are file-backed and re-ingestable — so they're excluded from `memhub export`, and re-running `memhub doc add` after the source file changes replaces every chunk in place. Use `memhub doc ls / show / rm` to manage what's ingested.

---

## Why bother?

The honest answer: if you work on a project for more than a few days with an AI assistant, you'll notice the difference.

- **Context doesn't evaporate.** "What's the build command again?" gets a real answer on day 90, not just day 1. The agent looks it up; you don't repeat yourself.
- **Decisions stay explained.** Six months from now you'll know *why* a call was made, not just that it was made. That's the difference between a decision and a fact.
- **Small context, relevant content.** A targeted recall bundle is much smaller than pasting the full project README into every prompt. The [Token Metrics tab](#the-web-dashboard) estimates how much context you're saving.
- **Agent proposals are reviewable.** You see exactly what the agent wants to commit, and you can say no. Nothing sneaks into your project memory.
- **Your specs are searchable too.** Point memhub at an external design doc or API contract and the agent pulls the relevant section on demand — no pasting the whole file into the prompt, no polluting normal recall.
- **Both agents, same memory.** Claude Code and Codex share the same rows. Switching tools doesn't cost you context.
- **It's just a file.** SQLite, gitignored, in your repo. No accounts to manage, no services to stay online, no vendor lock-in. Back it up, move it, or delete it whenever you want.

---

## Quickstart

### Install via Claude Code (recommended)

Open Claude Code in the repo you want memhub to track and paste:

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
   /recall, /reindex, /eval-recall, /doc, /metrics, and /viz all work
   as slash commands:

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
8. Tell me memhub can also ingest long reference docs (design specs,
   API contracts) as opt-in, RAG-searchable material that never
   pollutes normal recall. Ask whether I want to ingest one now — if I
   give you a path, run `memhub doc add "<path>" --json` and report the
   chunk count; if not, just note `/doc` is available anytime.

Don't touch any files in this repo other than what `memhub init` writes
(.memhub/ and the generated-output .gitignore entries) and the
.memhub/config.toml edit in step 6.
```

### Install via Codex CLI

Open Codex in the repo you want to track and paste:

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
   /recall, /reindex, /eval-recall, /doc, /metrics, and /viz all work:

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
9. Tell me memhub can also ingest long reference docs (design specs,
   API contracts) as opt-in, RAG-searchable material that never
   pollutes normal recall. Ask whether I want to ingest one now — if I
   give you a path, run `memhub doc add "<path>" --json` and report the
   chunk count; if not, just note `/doc` is available anytime.

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

# 5. MCP for Codex — append to ~/.codex/config.toml:
#   [mcp_servers.memhub]
#   command = "memhub"
#   args = ["serve"]

# 6. (Recommended) Turn on hybrid recall
#    Add to .memhub/config.toml:
#       [retrieval]
#       mode = "hybrid"
#    Then backfill embeddings for existing rows:
memhub index rebuild --actor cli:user
memhub index status   # confirm Missing: 0

# 7. (Optional) Ingest a long reference doc as opt-in, RAG-searchable
#    material — it never pollutes normal recall. /doc wraps this too.
memhub doc add path/to/design-spec.md --json
```

---

## Reference

### Commands

| Command | What it does |
|---|---|
| `memhub init` | Set up `.memhub/` in a repo |
| `memhub status` | Open tasks, stale facts, pending writes, schema version |
| `memhub recall <query>` | Hybrid ranked bundle of facts/decisions/tasks |
| `memhub fact add/list` | Durable key-value facts (build commands, MSRV, etc.) |
| `memhub decision add/list` | Decisions with rationale, FTS-indexed and embedded |
| `memhub task add/list/done` | Lightweight task tracking |
| `memhub command verify` | Record verified command outcomes; derives confidence |
| `memhub note add/list` | Session notes (low-stakes scratch; not in recall) |
| `memhub state set/show` | The "current state" narrative |
| `memhub arch set/show` | The architecture narrative |
| `memhub ingest-git` | Pull commit + file history into the DB |
| `memhub doc add/ls/rm/show` | Ingest external markdown reference docs; recall with `--source-type doc` |
| `memhub review list/accept/reject` | Triage agent-proposed writes |
| `memhub render` | Emit local `PROJECT.md` and `PROJECT_LEDGER.md` from the DB |
| `memhub index status/rebuild` | Embedding coverage; one-shot backfill for `fts → hybrid` migrations |
| `memhub eval retrieval` | Run the Recall@K harness against `tests/retrieval_golden.json` |
| `memhub stats --window 7d` | Write activity by actor, review rate, stale-fact counts |
| `memhub metrics enable/status` | Opt-in token accounting (Claude Code transcript scraping) |
| `memhub viz` | Open the local read-only web dashboard |
| `memhub export/import` | Portable JSON backup; cross-machine restore |
| `memhub serve` | Stdio MCP server for Claude Code / Codex |

Run any command with `--help` for flags.

### Two retrieval modes

memhub ships with two retrieval modes. Both are first-class; the install prompt asks you to pick one.

| | **`fts`** (default) | **`hybrid`** (recommended) |
|---|---|---|
| Scoring | FTS5 BM25 over title + body | 0.5 × FTS + 0.5 × cosine − 0.3 × stale_penalty, then re-ranked |
| What it catches | Exact terms, stemmed variants | Exact + paraphrases (`"compile a release"` → `release_build` fact) |
| Per-write cost | 0 ms | ~50 ms eager-embed inside the source-write transaction |
| Per-recall cost | <10 ms | <100 ms (brute-force cosine + ~275 ms re-ranker at pool=20) |
| Disk footprint | None beyond source rows | ~1.5 KB per row (384-dim f32 vector) |
| Network | Never | Never. Model is bundled. |
| Best for | Small projects, scripted use | Multi-month projects where you forget exact wording |

**Switching modes is non-destructive.** `fts → hybrid` requires one `memhub index rebuild`. `hybrid → fts` just stops consulting embeddings; nothing is deleted.

The `[retrieval]` block in `.memhub/config.toml`:

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

### Compatibility

**Claude Code**

- Reads `CLAUDE.md` at session start.
- User-level slash commands at `~/.claude/commands/`: `/wrap-up`, `/check-init`, `/init-project`, `/recall`, `/reindex`, `/eval-recall`, `/viz`, `/doc`.
- Skill writes are attributed `actor=claude:wrap-up`, `source=user+agent:claude-code`.

**Codex CLI**

- Reads `AGENTS.md` at session start (same role as `CLAUDE.md`).
- User-level skills at `~/.codex/skills/`: same set as above.
- MCP server registered in `~/.codex/config.toml` as `[mcp_servers.memhub]`. Codex's MCP client identifies as `codex`; memhub auto-attributes writes accordingly.
- Skill writes are attributed `actor=codex:wrap-up`, `source=user+agent:codex`.

**Both at once**

Same DB, same rows. Every write is tagged. `memhub fact list` and `memhub decision list` show the `source` column, so you always know who surfaced it.

```text
source                      Meaning
────────────────────────────────────────────────────────────────────
user                        You typed `memhub fact add` directly
agent:codex                 Codex proposed it (still in pending_writes)
agent:claude-code           Claude proposed it
user+agent:codex            Codex surfaced via /wrap-up, you approved
user+agent:claude-code      Same, Claude-side
git                         Reserved for git ingestion
observed                    Reserved for observed signals
```

### Attribution in depth

Two columns split the work:

- `source` on `facts` and `decisions` — *origin of the claim*. One of `user`, `agent:<id>`, `user+agent:<id>`, `git`, `observed`.
- `actor` on `writes_log` and `pending_writes` — *who performed the write*. Free-form, e.g. `cli:user`, `claude:wrap-up`, `codex:wrap-up`.

When you accept a pending MCP proposal via `memhub review accept`, the durable row's `source` becomes `user+agent:<actor>` automatically — both signals preserved without you passing anything.

### MCP server

`memhub serve` starts a stdio MCP server. Tools:

- **Read:** `status`, `search`, `recall`, `list_tasks`, `list_decisions`, `list_facts`, `list_pending_writes`, `get_command`
- **Write (direct):** `task_add`, `task_done`, `record_command`, `log_session_note`, `render`
- **Write (staged for review):** `propose_fact`, `propose_decision`

### Token accounting

Off by default. Opt in with `memhub metrics enable` — this auto-detects the Claude Code transcript directory and writes the resolved path into `.memhub/config.toml`. Disable with `memhub metrics disable`.

Two independent sub-switches under `[metrics]`:
- `recall_proxy = true` — logs one row to `recall_metrics` per `memhub recall` call: actual bundle size vs a full-ledger counterfactual.
- `session_accounting = true` — scrapes Claude Code transcript JSONL into `session_metrics` for real input/output/cache token totals.

The dashboard Token Metrics panel and `memhub metrics status` surface both components. `memhub render` appends a 7-day digest to `PROJECT.md` when enabled.

### Backup and restore

```bash
memhub export ./memhub-backup.json     # portable, version-tagged JSON
memhub init --from-backup <path>       # init + restore in one shot
memhub import <path>                   # restore into an existing repo
memhub import <path> --force           # overwrite live data
```

Export covers facts, decisions, tasks, commands, pending writes, writes_log, session notes, and both narrative tables. Embeddings are local derived state and are excluded; run `memhub index rebuild` after import.

### Staleness and confidence

- **Facts** go stale after 90 days without re-verification. `memhub fact list` flags them; `memhub status` reports the count.
- **Commands** carry a derived confidence: `success_count / (success_count + fail_count)`.
- **Embeddings** go stale when the source body changes (detected by `content_hash` drift) or when the binary upgrades to a different bundled model. Recall surfaces a warning; you decide whether to run `/reindex`.

### Deny list

`.memhub/config.toml` ships with defaults blocking `.env*`, `*.pem`, `*.key`, `secrets/**`, `.aws/credentials`, and similar. The list filters both `ingest-git` writes and `search` reads. Invalid patterns fail closed.

### Eval harness

`tests/retrieval_golden.json` ships 12 starter queries for testing `Recall@K`. Run the harness with:

```bash
memhub eval retrieval                  # markdown summary
memhub eval retrieval --json           # structured output
memhub eval retrieval --mode fts       # A/B compare modes
```

---

## How it's built

Single Rust binary over an embedded-migration SQLite database. The MCP server reuses the same command layer. The embedding model is bundled at build time via `build.rs` (downloads from Hugging Face, SHA256-pinned); no model download at runtime.

```text
memhub CLI / MCP
   ├── src/commands/    fact / decision / task / command / review / eval / index / ...
   ├── src/db/          path discovery, migrations, audit log
   ├── src/config/      per-repo TOML (incl. [retrieval] and [metrics] blocks)
   ├── src/mcp/         stdio MCP server, client identity normalization
   ├── src/retrieval/   BGE-small bi-encoder + ms-marco cross-encoder, hybrid recall
   ├── src/dashboard/   read-only local web UI (viz feature flag)
   ├── src/metrics/     opt-in token accounting + session scraper
   ├── src/render/      PROJECT.md and PROJECT_LEDGER.md emit
   └── src/export/      v1 portable JSON
       │
       └── SQLite (.memhub/project.sqlite) + bundled BGE-small + ms-marco ONNX
```

---

## Principles

- **Local-first.** No network, no daemon, no account, no runtime model download.
- **One per repo.** Project boundaries are repo boundaries.
- **Boring tech.** SQLite, Rust, FTS5, brute-force cosine. No vector DB, no extension loading, no Python.
- **Agents are untrusted writers.** Agent proposals stage in `pending_writes` until a human approves. The schema enforces it.
- **Recall is read-only.** Retrieval never writes to `writes_log` or any durable table.
- **Narrow milestones.** Ship usable slices; defer speculative work until a real workflow demands it.

---

## Further reading

- [Product PRD (verbatim)](docs/reference/memhub-prd.md)
- [M8 hybrid retrieval addendum](docs/reference/memhub-prd-addendum-m8-retrieval.md)
- [Source vocabulary addendum](docs/reference/memhub-prd-source-vocabulary-addendum.md)
- [K9 deprecation addendum](docs/reference/memhub-prd-deprecation-addendum.md)
- Local project state: run `memhub render`, then read `.memhub/rendered/PROJECT.md`
