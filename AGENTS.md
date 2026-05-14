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

## Current Build Focus

The repository currently provides Milestone 1 scaffolding and a usable local CLI foundation. Future work should extend from these boundaries instead of replacing them.

## Project state

Current project state (active tasks, durable decisions, known quirks)
is machine-local and lives in `.memhub/rendered/PROJECT.md`. Use
`memhub recall` mid-session and `memhub render` to refresh the local
view. Nothing under this section is committed to git — each machine
maintains its own DB and its own rendered view.
