# memhub — Codex CLI instructions

Local-first Rust CLI for durable per-repo project memory shared between Codex and Claude Code. Treat [docs/reference/memhub-prd.md](docs/reference/memhub-prd.md) as the product authority and do not silently diverge from it.

This file is the Codex counterpart to `CLAUDE.md`. The two exist so Codex CLI and Claude Code sessions get the same orientation when they open this repo. Where they diverge it is intentional (different agent identifiers, different skill paths).

## Session Continuity

This repo is **memhub-primary** as of M7-002 (2026-05-13). The DB at `.memhub/project.sqlite` is the source of truth; rendered markdown is a local human-readable view under `.memhub/rendered/`.

At session start, read `.memhub/rendered/PROJECT.md` if present — it carries the "currently building / next up / open questions" state plus the architecture narrative plus recent session notes, all rendered from the DB. If it is missing, use `memhub recall` / `memhub status` and run `memhub render` when a local view is useful.

**Mid-session, prefer `memhub.recall` (or `/recall`) over reading `.memhub/rendered/PROJECT_LEDGER.md`.** Recall is the SQL+RAG hybrid query surface over facts, decisions, and tasks; it returns a focused evidence bundle for the question you actually have, instead of you re-scanning the full ledger. Read `PROJECT_LEDGER.md` only as a fallback when recall comes up empty for something you suspect is recorded, or when the user explicitly asks for the full ledger.

If recall returns a `warnings[].kind == "stale_embeddings"` entry, surface it and ask the user before invoking `/reindex`. Recall results stay usable in the meantime — the warning means hybrid scoring may be undercounting some rows, not that retrieval is broken.

Re-render after wrap-up with `memhub render`.

The four legacy K9 files (`agent_docs/project_state.md`, `project_arch.md`, `project_decisions.md`, `project_backlog.md`) are historical archive — last accurate at commit `366cc1c`. Do not write to them; they are no longer authoritative. K9 integration is disabled in `.memhub/config.toml`.

## Cross-machine workflow

memhub state is **per-machine**. Each machine has its own `.memhub/project.sqlite`, its own embeddings, and its own rendered markdown under `.memhub/rendered/`. None of that is committed to git — only code, migrations, and the static tracked `CLAUDE.md` / `AGENTS.md` guardrails are.

**After `git pull` on a fresh or existing machine:**

```bash
cargo build --release
cargo run --release -- status   # first call auto-applies pending migrations from migrations/*.sql
```

`db::open_project` runs `migrations::apply_all` on every invocation; migrations are idempotent against `schema_migrations`, so no manual step is needed even if the schema bumped on another machine.

**To carry memory between machines (e.g. continue on Windows what you started on Mac):**

```bash
# on the source machine
memhub export ~/transfer/memhub-<repo>-<date>.json

# move the file via Drive / USB / scp — memhub itself stays offline

# on the target machine, with an existing memhub project
memhub import ~/transfer/memhub-<repo>-<date>.json          # refuses if target has data
memhub import ~/transfer/memhub-<repo>-<date>.json --force  # overwrite

# or to bootstrap a target that has no DB yet
memhub init --from-backup ~/transfer/memhub-<repo>-<date>.json
```

After import, the target's embeddings are not yet built (only the rows). Run `memhub index` to populate them — the import output prints this hint. Until then, recall falls back to FTS-only and may miss vector-similar matches.

If recall later surfaces a `stale_embeddings` warning (most likely after an embedding-model upgrade on either machine), follow the same rule as everywhere else: surface it and ask before invoking `/reindex`.

The export format is JSON v1, additive: older exports import cleanly into newer builds via `#[serde(default)]` on later-added fields. The format is defined in `src/export/v1.rs`.

**Config baseline travels with the repo.** The canonical defaults live in `.memhub/config.example.toml`, which is the **only** file inside `.memhub/` that is tracked by git. A fresh `memhub init` (or the first `open_project` call on a machine with no local config) copies the example verbatim into `.memhub/config.toml`. The local file stays gitignored and per-machine; edit the example to change the baseline for every machine. Fields that should not drift (deny_list, retrieval weights, render output dir, integrations) are documented at the top of the example as commit-back-here fields.

## Agent attribution (Codex-specific)

When you write to memhub from the CLI, identify yourself so the row gets attributed correctly. Two flags matter:

- `--source` — origin of the claim. Pass `--source user+agent:codex` on `memhub fact add` / `memhub decision add` writes that go through `/wrap-up` (agent surfaced, user approved). For direct CLI writes the user typed themselves, omit the flag and take the `user` default.
- `--actor` — who performed the write. Pass `--actor codex:wrap-up` from the wrap-up skill; `--actor codex:<skill-name>` from other skills.

See [docs/reference/memhub-prd-source-vocabulary-addendum.md](docs/reference/memhub-prd-source-vocabulary-addendum.md) for the full vocabulary (`user`, `agent:<id>`, `user+agent:<id>`, `git`, `observed`).

When you write via MCP (`memhub serve` registered in `~/.codex/config.toml`), attribution is automatic — the server reads `clientInfo.name` from `initialize` and tags writes as `codex` / `codex:wrap-up` without you needing to pass anything.

## Project Guardrails

- Local-first, offline-capable, and intentionally boring.
- Milestone 1 stays lean: CLI, DB, migrations, config, logging, and real CRUD for facts, decisions, and tasks.
- Agents are untrusted writers; do not promote agent claims to durable truth without a concrete signal or explicit user action.
- Prefer narrow milestones and explicit TODOs over speculative subsystems.
- Do not pretend MCP, markdown sync, git ingestion, routing, or confidence decay are implemented before they exist.
- Keep `docs/reference/memhub-prd.md` verbatim. PRD changes land as addendum files in `docs/reference/`.

## Build / Test / Run

```bash
cargo build
cargo test
cargo run -- init
cargo run -- status
```

## Retrieval

Hybrid recall (the default) combines FTS5 BM25 and BGE-small embeddings,
then by default runs a bundled cross-encoder re-ranker
(ms-marco-MiniLM-L-6-v2) over the top `[retrieval] rerank_candidate_pool`
candidates before truncating to `max_results`. The re-ranker adds
~275 ms per recall at pool=20 in exchange for ~+17 percentage points of
Recall@1 on memhub's own golden set (decision 68).

Toggle per-call with `memhub recall <query> --no-rerank`, or globally
with `[retrieval] use_reranker = false` in `.memhub/config.toml`. The
re-ranker is bundled into the binary unconditionally (no Cargo feature
flag) — turning it off in config skips the inference cost but doesn't
strip the model from disk. FTS-only mode bypasses the re-ranker
entirely.

Candidates whose cross-encoder logit falls below
`[retrieval.scoring] min_rerank_score` (default 2.0) are dropped
after re-ranking, primarily to keep gibberish queries returning empty
bundles. This replaces the legacy `min_vector_score` cosine floor
(decisions 70, 71). Override per-call with `--min-rerank-score=<F>`
(use the `=` form for negative values).

Decisions support an optional `summary` field (migration 0011,
decision 72). When set, the summary is prepended to both the embed
text and the cross-encoder rerank input, so jargon-titled decisions
surface for plain-English queries. Set with `memhub decision add
--summary "..."` or backfill with `memhub decision set-summary <ID>
"..."`. Decision 72 covers the empirical lift this produced.

For A/B testing in any repo: `memhub eval retrieval` vs
`memhub eval retrieval --no-rerank`.

## Token Accounting

Off by default. Opt in per machine with `memhub metrics enable` — this
auto-detects the Claude Code transcript directory and writes the resolved
path into `.memhub/config.toml`. Disable with `memhub metrics disable`.

Two independent sub-switches under `[metrics]`:
- `recall_proxy = true` (component A) — logs one row to `recall_metrics`
  per `memhub recall` call: actual bundle size vs a full-ledger
  counterfactual, tokenised with tiktoken cl100k.
- `session_accounting = true` (component B) — scrapes Claude Code
  transcript JSONL into `session_metrics` for real input/output/cache
  token totals. Scraping is incremental and never fatal; bad lines are
  skipped.

**Codex note:** component B currently scrapes Claude Code transcripts
only. `codex_transcripts_dir` in `[metrics]` is a stub until the Codex
transcript format is confirmed; until then a Codex session contributes
component-A proxy rows (every `memhub recall` is logged regardless of
host) but no real input/output token totals.

**Proxy contract:** `bundle_tokens` is the token count of the recall bundle
actually returned. `ledger_tokens` (per row in `recall_metrics`) is the size
of `PROJECT_LEDGER.md` at recall time, measured in cl100k tokens. The
counterfactual is **session-scoped**: for each session that had at least one
non-empty recall, charge one ledger load (the minimum `ledger_tokens` across
that session's recalls, as a proxy for session-start size) and subtract all
non-empty bundle tokens. Empty-bundle recalls (no results returned) are not
savings events and contribute nothing to the offset. The rendered label is
"context offset vs full-ledger baseline" — not "tokens saved" — because the
agent would not necessarily have loaded the full ledger anyway.

**Tokenizer caveat:** tiktoken cl100k is ±10% off Anthropic's real
tokenizer. Ratios stay sound because both sides of every comparison use the
same yardstick; treat absolute token counts as estimates, not ground truth.

Dashboard surfaces: `memhub metrics status` (CLI) · `memhub.metrics` (MCP
tool) · `/metrics` (skill). `memhub render` appends a 7-day digest to
`PROJECT.md` when enabled and ≥1 row exists; the section is omitted
entirely when disabled or when no data has been captured yet.

## Doc Ingestion

External markdown reference docs (design specs, API contracts) can be
ingested into `.memhub/project.sqlite` as opt-in retrieval material
(decision 86). The file is chunked by heading — fenced code blocks kept
intact — and each chunk is embedded, so it is retrievable through the
same SQL+RAG hybrid recall as facts, decisions, and tasks.

**Default after first ingest (decision 90, extends 86).** Docs are
opt-in by default; the first successful `memhub doc add` in a repo
flips `[retrieval] include_docs_in_default` on in that repo's local
config, so the user-pointed write that establishes docs also wires up
retrieval. After that, plain `memhub recall` surfaces a doc chunk only
when it clears the cross-encoder relevance boundary
(`[retrieval.scoring] doc_min_rerank_score`, default 0.0) — strong
topical matches in, off-topic docs out, so a UI style guide stays
silent on a backend query while a code style guide surfaces. The
doc floor is deliberately *below* `min_rerank_score`: doc chunks
rerank in a lower band than facts/decisions (an on-topic doc ≈ +1.6,
off-topic ≈ −11), so a higher floor would filter relevant docs.
Scope to docs alone with `memhub recall <query> --source-type doc`
(CLI) or `memhub.recall(query=..., source_types=["doc"])` (MCP);
explicit scoping keeps the normal floor and is unaffected by the
flag. Plain recall still returns `available_docs` — now the count of
ingested chunks that did *not* surface this call — so a doc-scoped
follow-up for the long tail stays a judgment call. Set
`include_docs_in_default = false` to revert to strict opt-in.

Surfaces: `memhub doc add|ls|show|rm` (CLI) · `memhub.doc_add` (MCP,
direct write — a doc is a user-pointed artifact, not an agent claim) ·
`/doc` (skill). Re-ingesting an unchanged file is a no-op; changed
content replaces every chunk and refreshes embeddings/FTS.

Doc content is **excluded from `memhub export`** — it is a disk-backed,
re-ingestable cache. On another machine, re-run `doc add` against the
same file. Embeddings populate only in `hybrid` mode; `fts` mode
ingests chunks + FTS and vector recall for docs starts after
`memhub index rebuild`.

## Machine-global memory

Milestone 9 (design anchor:
[docs/reference/memhub-prd-addendum-m9-machine-global-memory.md](docs/reference/memhub-prd-addendum-m9-machine-global-memory.md))
adds an optional second store at `~/.memhub/global.sqlite`,
structurally identical to a repo DB (same embedded migrations;
`project_id = 1` is per-database, so zero new SQL migrations). It is
the global-vs-repo `AGENTS.md` idea made retrievable.

Off by default and **per-repo**: a repo opts in with
`memhub global enable` (mirrors `memhub metrics enable|disable`).
`enabled` lives in `.memhub/config.toml` `[global]`; the tracked
`.memhub/config.example.toml` baseline ships `false`. When disabled or
the store is absent, recall is byte-identical to a pre-M9 build (the
eval-regression guarantee).

When enabled, `recall` merges global hits with repo hits; every hit
carries `scope: "repo" | "global"`. **Precedence is
provenance-tag-only** — recall never drops a global hit and does no
automatic conflict resolution. Apply repo-overrides-global yourself
(exactly as repo `AGENTS.md` overrides global `AGENTS.md`).

Promotable to global: **facts, decisions, docs** only — machine/
toolchain truths, standing engineering policy, broadly-applicable
guides. Never global: tasks, rendered narrative, anything naming a
repo-specific path/symbol. Routing is **user-gated and never
agent-automatic** (one bad global write poisons every repo). Surfaces:
`memhub global enable|disable|status`, `--global` on
`fact|decision|doc add` and `doc ls|rm|show`,
`fact|decision promote <id> --global` (CLI) ·
`memhub.propose_fact|propose_decision(global=true)` (MCP — staged into
the repo's `pending_writes`, durable only on `memhub review accept`;
no `global` on MCP `doc_add`) · `/global` (skill). Global memory is
**not** exported by `memhub export` (per-machine; re-add on another
machine).

Onboarding exposes two explicit toggles — `[retrieval] mode` (fts vs
hybrid) and the machine-global store — plus two auto-followers with a
manual override: `[retrieval] use_reranker` (auto-on with hybrid) and
`[retrieval] include_docs_in_default` / its `[global]` mirror
(auto-flips true on the first `doc add` / `doc add --global`).

## Machine-wide upgrade

`memhub upgrade` (decision 96; resolves task 48, subsumes the
recurring stale-PATH-binary problem) is the one dependable command to
bring **every** memhub install on a machine to a coherent state after
a code change — the binary on PATH, each known repo DB, the global
store, and the installed agent skill wrappers — not just whichever
repo you rebuilt from. Run it **from the memhub source repo**; it
errors elsewhere.

Flow: `cargo install --path . --force` → one-time, order-independent
PATH-shadow fix (a regular-file `~/.local/bin/memhub` shadowing
`~/.cargo/bin/memhub` is replaced **once** with a symlink so future
installs always take effect; already-a-symlink is an idempotent no-op;
a non-symlink shadow is replaced only after a y/N confirm or `--yes`,
otherwise the manual `ln -sf` is printed) → installed-skill resync
(decision 97; below) → **re-exec the freshly installed binary** for the
migrate+verify pass so migrations run under new code → per-instance
`ready/migrated/skipped/ERROR` table plus a per-agent skills line
(`--json` carries a `skills` array). `--dry-run` reports the plan
(including would-sync skill counts) and changes nothing.

Skill resync (decision 97; resolves task 50, internalizes the fact-10
manual `cp`): the same `memhub upgrade` also refreshes the installed
slash-command wrappers so they never lag the binary. For each agent
dir that **already exists** — `~/.claude/commands/` (flat `*.md`),
`~/.codex/skills/` (dir-per-skill) — it copies from the source repo's
`templates/skills/{claude,codex}/`. It runs in the orchestrate phase
(the old binary, where `templates/` lives) and the result is rendered
by the re-exec'd child in one table. The copy is **additive**: a skill
removed/renamed in `templates/` leaves a harmless installed orphan —
settled against mirror-with-prune because pruning shared user-global
dirs risks a user's own same-named skill, while an orphan is just a
stale slash-command. Idempotent, best-effort (a partial/permission
error degrades to a `warn` row, never fails the upgrade — same posture
as the registry/metrics writes). `--no-skills` skips the step; the
binary + DB migrate still run. The manual `cp` recipe (fact 10) is now
a fallback only.

Instances are enumerated from a **self-maintaining registry**, never a
filesystem scan: every `db::open_project` does a single guarded,
debounced UPSERT into `known_projects` in `~/.memhub/global.sqlite`,
but **only if that store already exists** — the common
repo-with-no-global path pays one `stat`. A repo memhub has never
opened since this landed is absent from the first run but
self-migrates on its next open; seed it explicitly with `memhub
upgrade --also <path>` (repeatable; also persists it). Migration
`0015_known_projects` adds the table to the shared MIGRATIONS list; it
is read only from the global store.

Hard invariants: registry membership is **not** M9 global-memory
opt-in — recall never reads `known_projects` and stays gated on each
repo's own `[global] enabled` (a populated registry must not change
recall output: the eval-regression guarantee, tested in
`tests/upgrade_registry.rs`). `upgrade` migrates the global store only
if it already exists; it never creates it (opting in stays the
explicit `memhub global enable` choice). `known_projects` is
machine-local and **not** exported by `memhub export`. Skill resync
likewise only ever writes into an agent dir that already exists — it
never creates `~/.claude/commands` or `~/.codex/skills`, and a
non-directory at that path is a clean skip, not a clobber (mirrors the
PATH-shadow and global-store "only act on what exists" rule).

**Build-artifact GC (`memhub gc`).** Cargo's `target/<profile>/deps/`
is append-only — every rebuild writes a new hash-suffixed artifact and
never reclaims the old one; with memhub's `include_bytes!`'d ONNX
models each stale `libmemhub-<hash>.rlib` / test binary is ~1 GB, so a
few weeks of `cargo test` strands 100+ GB. `memhub gc` keeps only the
newest-mtime hash per **memhub-owned** stem (`memhub`, `libmemhub`,
each top-level `tests|benches|examples/*.rs` basename) and deletes the
superseded hashes plus their `.fingerprint/<stem>-<hash>` dirs.
Third-party dependency rlibs carry one hash, never balloon, and are
structurally never considered; `incremental/` is left alone (deleting
it only slows the next build). Worst case of pruning a superseded hash
is one rebuilt test binary — Cargo recovers, the current set is never
touched, so it cannot corrupt a tree. Runs **automatically inside
`memhub upgrade`** (best-effort, never fatal — same posture as the
skill/registry writes; `--no-gc` skips it, `--dry-run` reports it) and
standalone as `memhub gc [--dry-run] [--json]`. Pure `std::fs`,
OS-agnostic (macOS + Windows). Intentionally not a skill — ops
housekeeping like `upgrade` itself, surfaced via the CLI and the
upgrade flow.

## Current Build Focus

The repository currently provides Milestone 1 scaffolding and a usable local CLI foundation. Future work should extend from these boundaries instead of replacing them.

## Project state

Current project state (active tasks, durable decisions, known quirks)
is machine-local and lives in `.memhub/rendered/PROJECT.md`. Use
`memhub recall` mid-session and `memhub render` to refresh the local
view. Nothing under this section is committed to git — each machine
maintains its own DB and its own rendered view.
