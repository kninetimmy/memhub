# memhub — operations reference

Operational detail for memhub's subsystems, moved out of the repo `CLAUDE.md` / `AGENTS.md` orientation files so those stay lean at session start (Wave 2 token diet, issue #30). Nothing here is load-bearing from turn one — the two orientation files keep the must-have-inline set (Session Continuity, Guardrails, Delegation, the `stale_embeddings` and `sync_adopt` safety gates, Build/Test/Run). Everything below is **memhub-recall-searchable**: this file is ingested with `memhub doc add`, so recall it on demand instead of loading it every session.

This document is content-preserving: the sections below are the full prose that used to live inline, verbatim, with every fact, decision number, and command name intact. When a subsystem changes, update it here and re-ingest.

## Cross-machine workflow

memhub state is **per-machine**. Each machine has its own
`.memhub/project.sqlite`, its own embeddings, and its own rendered
markdown under `.memhub/rendered/`. None of that is committed to git —
only code, migrations, and the static tracked `CLAUDE.md` / `AGENTS.md`
guardrails are.

**After `git pull` on a fresh or existing machine:**

```bash
cargo build --release
cargo run --release -- status   # first call auto-applies pending
                                # migrations from migrations/*.sql
```

`db::open_project` runs `migrations::apply_all` on every invocation;
migrations are idempotent against `schema_migrations`, so no manual
step is needed even if the schema bumped on another machine.

**To carry memory between machines (e.g. continue on Windows what you
started on Mac):**

```bash
# on the source machine
memhub export ~/transfer/memhub-<repo>-<date>.json

# move the file via Drive / USB / scp — memhub itself stays offline

# on the target machine, with an existing memhub project
memhub import ~/transfer/memhub-<repo>-<date>.json          # refuses
                                                            # if target
                                                            # has data
memhub import ~/transfer/memhub-<repo>-<date>.json --force  # overwrite

# or to bootstrap a target that has no DB yet
memhub init --from-backup ~/transfer/memhub-<repo>-<date>.json
```

After import, the target's embeddings are not yet built (only the
rows). Run `memhub index` to populate them — the import output
prints this hint. Until then, recall falls back to FTS-only and may
miss vector-similar matches.

If recall later surfaces a `stale_embeddings` warning (most likely
after an embedding-model upgrade on either machine), follow the same
rule as everywhere else: surface it and ask before invoking
`/reindex`.

The export format is JSON v1, additive: older exports import cleanly
into newer builds via `#[serde(default)]` on later-added fields. The
format is defined in `src/export/v1.rs`.

**Config baseline travels with the repo.** The canonical defaults
live in `.memhub/config.example.toml`, which is the **only** file
inside `.memhub/` that is tracked by git. A fresh `memhub init` (or
the first `open_project` call on a machine with no local config) copies
the example verbatim into `.memhub/config.toml`. The local file stays
gitignored and per-machine; edit the example to change the baseline
for every machine. Fields that should not drift (deny_list, retrieval
weights, render output dir, integrations) are documented at the top
of the example as commit-back-here fields.

## Cross-machine Drive sync

Milestone 10 (design anchor:
[docs/reference/memhub-prd-addendum-m10-drive-sync.md](docs/reference/memhub-prd-addendum-m10-drive-sync.md))
makes one user's repo memory follow them between their own machines
through a synced folder, **without memhub ever going online**. It is
the export/import flow above, automated and made fast-forward-aware.

**Model (decisions 102/103/104).** Whole-DB **snapshot**, not row
merge: each push writes a consistent single-file DB copy (`VACUUM
INTO`) plus a `manifest.json`. Divergence is decided from a **logical
version** (a digest of the durable content tables, never file bytes —
SQLite is byte-unstable), so `check` reports a git-style verdict:
`up-to-date` / `local-ahead` / `drive-ahead` / `diverged` /
`no-remote`. The single lossy case (both sides changed) is **operator-
gated**, never automatic. Scope is deliberately **single-user across
their own machines**: last-writer-wins on a diverged history is an
accepted cost; no snapshot-history/undo buffer, no multi-user plumbing
— do not re-propose either (decision 103).

**Transport is an OS-level synced folder, NOT the Drive MCP connector
(decision 104).** memhub stays fully offline and only reads/writes a
local path. Google Drive for Desktop (macOS/Windows) or an rclone
mount (Linux) does the byte movement out of band — so writing a
snapshot *into* the synced folder *is* the push. (The base64-over-MCP
courier framing in the addendum is superseded: a 2.8 MB snapshot is
~987K tokens per transfer.)

**Canonical remote path** is resolved in code (`sync::resolve_remote_dir`),
no longer hand-concatenated: `<drive_subpath>/memhub/<project_id>`,
where `drive_subpath` is the absolute synced-folder mount in `[sync]`
and `project_id` derives from the git remote (or an explicit `[sync]
project_id` override for a no-remote repo). CLI sync commands default
to it when the path arg is omitted.

**Per-repo opt-in** (mirrors `memhub global enable`): `memhub sync
enable`. `enabled` + `drive_subpath` live in `.memhub/config.toml`
`[sync]`; the tracked `.memhub/config.example.toml` baseline ships
`enabled = false`. When disabled, every sync command refuses.

Surfaces:
- CLI: `memhub sync enable|disable|status|snapshot|check|adopt|commit`.
  A push is just `snapshot`: writing to the resolved canonical remote
  dir records the push baseline itself, so a later `check` reads
  up-to-date on equal local/remote logical versions with no second
  step. (Snapshotting to a non-canonical destination — an inspection
  copy, a test fixture dir — leaves the marker untouched, the
  pre-existing fail-closed behavior.) `commit` is no longer part of
  the routine push; it exists to verify or repair a baseline after
  the fact. A pull is `check` then `adopt --yes`. `status` shows the
  resolved `remote dir`.
- MCP (the agent-first surface): `memhub.sync_status`,
  `memhub.sync_snapshot`, `memhub.sync_check`, `memhub.sync_commit`,
  and `memhub.sync_adopt`. All default the target to the canonical
  path; pass `remote` to override. **`sync_adopt` is gated**: it
  overwrites the local DB (the one destructive op), so without
  `confirm=true` it returns the would-change verdict and refuses —
  surface that to the user and only re-call with `confirm=true` after
  they approve. Hard refusals regardless of confirm: project-id
  mismatch, a snapshot schema newer than this binary (run `memhub
  upgrade`), or a checksum that disagrees with the manifest. (This
  gate is also kept inline in `CLAUDE.md` / `AGENTS.md` under Safety
  gates — it is the one destructive sync op.)
- Skill: `/catch-up` orchestrates the pull side (check → summarize →
  adopt with your approval).

`known_projects`/registry membership and the M9 global store are
**unrelated** to sync. Sync state (`[sync]`, the `sync_marker.json`
baseline) is per-machine and **not** exported by `memhub export`.

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
(decisions 70, 71) — the cosine band of nonsense overlapped borderline
semantic queries, so a vector-path floor had no safe sweet spot.
The rerank-score band is similarly noisy on memhub's own corpus, so
2.0 is a parity calibration rather than an improvement; override with
`memhub recall --min-rerank-score <F>` or `memhub eval retrieval
--min-rerank-score=<F>` (use the `=` form for negative values).

Decisions can carry an optional natural-language `summary` (migration
0011, decision 72). When set, the summary is prepended to BOTH the
bi-encoder's embed text and the cross-encoder's rerank input, letting
jargon-titled decisions surface for plain-English queries. On memhub's
own golden set, backfilling summaries on four jargon-titled decisions
lifted Recall@3 from 76.5% to 100% with the safety probe still
passing. Set at write time with `memhub decision add --summary "..."`
or backfill an existing row with `memhub decision set-summary <ID>
"..."` (empty string clears it back to NULL).

For A/B testing in any repo: `memhub eval retrieval` vs
`memhub eval retrieval --no-rerank`.

**Hermetic golden fixture (N28, issue #44).** `tests/retrieval_golden.json`'s
18 queries target memhub's own real decisions/facts/tasks (e.g. decision 34
"Agents prefer recall over reading PROJECT_LEDGER.md", decision 48 "recall is
read-only"), so running `memhub eval retrieval` from this repo's root scores
against *this machine's* live `.memhub/project.sqlite` — a corpus that drifts
as new rows land, making the golden-set contract a property of a given DB's
row population, not just of the code. `tests/retrieval_golden_hermetic.rs` is
the hermetic CI gate: it seeds a disposable tempdir project — switched to
hybrid mode *before* seeding so eager-embed (decision 27) actually fires —
whose rows reproduce the golden set's targets (copied verbatim from the live
decisions where one is cited, including the real backfilled `summary` text
on the four decisions that need it), then drives the compiled `memhub eval
retrieval --json` binary against it with `--golden` pointed at the real
shipped `tests/retrieval_golden.json`. That is the same pattern
`tests/locate_polyglot.rs` already established for `eval locate` — a fixture
seeded fresh per run, independent of live `.memhub` state — applied to the
retrieval golden.

Baseline recorded 2026-07-06 (issue #44): Recall@3 = 100% (17/17 match
queries, every one at rank 1), 0 safety failures, over the 18-query set
(hybrid mode, default rerank floor 2.0). This is the reference other
Wave 3 lifecycle PRs (L2 staleness, L3 supersession, L6 age decay) compare
their own hermetic re-run against. There is no persisted fixture DB to
regenerate — the corpus is defined entirely by the `fact::add` /
`decision::add` calls in that test's `seed_hermetic_corpus`, rebuilt from
scratch every run. When `tests/retrieval_golden.json` legitimately changes,
update `seed_hermetic_corpus` to match and re-run
`cargo test retrieval_golden_hermetic`. The live-DB run from the repo
root (what `/eval-recall` still drives by default) remains a self-hosted
calibration signal, not the enforced gate.

## Token Accounting

**Hibernated by default (Wave 7 Q30).** Normal builds preserve the metrics
schema, config, stored rows, and source implementation, but compile out all
collection, maintenance, rendering, CLI, MCP, calibration, dashboard, and
agent-skill surfaces. A pre-existing `metrics.enabled = true` is inert and is
not rewritten or deleted. Reactivation is explicit: build with
`--features metrics`; build with `--features viz` for the dashboard (`viz`
implies `metrics`). Default skill installation also skips `/metrics` and
`/viz`. Transcript archiving remains independent and available.

The retained feature behaves as follows when explicitly compiled in. Opt in
per machine with `memhub metrics enable` — this
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

**Empirical counterfactual baseline (task 64, decision 109).** The assumed
full-ledger baseline above is a guess. Task 64 adds a *measured* baseline
alongside it: `session_metrics.baseline_input_tokens` (migration 0017) records
the **full prompt of each session's first usage turn** — `input_tokens +
cache_read_input_tokens + cache_creation_input_tokens` — which approximates
everything loaded at session start (system prompt + CLAUDE.md + PROJECT.md +
any handoff md). It is `input + both cache fields`, **not `input_tokens`
alone**, because under prompt caching the bulk of startup context is billed as
cache_creation/cache_read; `input_tokens` alone undercounts startup ~10×. The
scraper sets it once per session, on the first usage line (`COALESCE` keeps the
earliest), so it pins the session-START cost. In `render_period_block` the
headline "Context offset" now prefers the **median `baseline_input_tokens`
across the window's no-recall sessions** (`recall_calls = 0`) as the
denominator (`recall_sessions × that median`), with the assumed full-ledger
percentage shown on an aligned line beneath so the gap is visible. The column
is **machine-local, not exported, and not applied retroactively** — existing
sessions are already past their first-turn offset, so the empirical baseline
accrues from new sessions forward (an uncalibrated/empty install renders only
the assumed line, byte-identical to before). The dashboard burn-up chart
(`query_series`) still uses the assumed ledger counterfactual; only the period
block changed.

**Tokenizer caveat:** tiktoken cl100k is ±10% off Anthropic's real
tokenizer. Ratios stay sound because both sides of every comparison use the
same yardstick; treat absolute token counts as estimates, not ground truth.

**Tokenizer calibration (task 63, decision 109).** The ±10% above is a
fixed multiplier, so it can be corrected once. `memhub metrics calibrate`
sends a **fixed bundled corpus** — never your project's content — to
Anthropic's `count_tokens` endpoint, measures the cl100k→real ratio, and
writes it back to `[metrics] calibration_factor`. `tokenizer::tokens_of`
then scales every estimate by it (default `1.0` = uncalibrated
passthrough, so an uncalibrated install and every unit test is
byte-identical to before). It corrects *absolute* counts and the
ledger-vs-bundle *offset*; the context-offset **percentage** is a ratio
of two equally-scaled numbers and is unchanged. **This is the only
command in all of memhub that touches the network** (via `ureq`,
compiled in but never reached otherwise) — offline-first holds because
the call is explicit and one-time. It is **CLI-only ops housekeeping like
`gc`/`upgrade`, deliberately not an MCP/agent surface** (an agent must
not reach the network on its own). The factor is **per-machine** (a
property of the local binary's tokenizer, not the repo) and **not
applied retroactively** — rows written before calibration keep their
earlier scaling; re-run after a binary/tokenizer change. Needs
`ANTHROPIC_API_KEY` in the environment; refuses cleanly without it.

**Cache churn (task 62).** Each rendered period block also carries a
`Cache churn:` line — the share of cache tokens that were *creation*
(rebuilt prefix) rather than *read* (reused prefix). At a 1M-token
window the real recurring cost is cumulative per-turn `cache_read`, so a
high creation share is the honest "we kept rebuilding the cache" signal.
Two figures: the token-weighted window churn (dominated by the largest
sessions) and a per-session mean (each session weighted equally, so one
huge session can't dominate). Both derive from the already-logged
`cache_read_tokens` / `cache_creation_tokens` — no migration. The line
is omitted when a window had no cache activity. Rendered only in
`render_period_block` surfaces (the `/metrics` panel, MCP
`rendered_panel`, and the PROJECT.md digest); the plain `memhub metrics
status` CLI text keeps its leaner per-line layout.

Reactivated surfaces: `memhub metrics status` (CLI) · `memhub.metrics` (MCP
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

## Code Index

Milestone 11 (design anchor:
[docs/reference/memhub-prd-addendum-m11-code-locator.md](docs/reference/memhub-prd-addendum-m11-code-locator.md))
adds a **code locator**: a cheap semantic file/symbol search over the
repo's own source, separate from project memory. `memhub locate <query>`
returns ranked `path:line-range` breadcrumbs plus a clipped snippet; the
agent then `Read`s that exact span. It never returns code into a recall
bundle and never edits.

**Isolated by construction (decision 107).** The index is a sibling DB at
`.memhub/code_index.sqlite` — gitignored, per-machine, and **never read by
`memhub recall`, never in `memhub export`, never in M10 sync**. That
physical separation is what preserves the recall eval-regression guarantee
(mirrors M9's registry-is-not-recall rule). It is also **derivable +
disposable**: no migration framework, just `CREATE TABLE IF NOT EXISTS` +
a `schema_version` in `index_meta`; a version mismatch drops and rebuilds,
so `memhub upgrade` is a no-op for it. The index set is `git ls-files`
filtered through the existing deny-list and scoped to grammar-known source
languages (task 69, below).

**Symbol-aware chunking (decisions 108, 115–120).** A tree-sitter AST
chunker emits one chunk per top-level item and one `Type::method` chunk per
method, with header-only chunks for container types. For Rust it also
emits one file-level `module-doc` chunk capturing the leading `//!` doc
comment, so a file whose purpose lives in its module prose stays
retrievable (a `ModuleDoc` grammar hook — task 85 added Rust inner-doc
capture; task 87 extended it to all six languages: Python docstring, Go
package doc, TS/JS file JSDoc, C#/Java file doc comment). Six languages get
real AST chunking — **Rust, Go, Python, TypeScript/JavaScript, Java, C#**
— via a hybrid `GrammarSpec` + typed hooks whose defaults reproduce Rust
byte-for-byte; a frozen snapshot test guards Rust output (the Rust freeze
is unchanged by task 87). Task 88 added a hermetic polyglot eval —
`tests/locate_polyglot.rs` writes a six-language fixture repo and runs
`eval::run_locate` over `tests/code_locate_golden_polyglot.json` — so
non-Rust module-doc capture is held to the same Recall@K contract as Rust
(100% Recall@3), not just the chunker unit tests.
Grammars are bundled unconditionally and ABI-pinned with a per-language
load canary; detection is extension-only. A grammar-known file that fails
to parse falls back to line-window chunking; files of any other type are
excluded from the index entirely (task 69, below) rather than line-windowed.

**Lazy git-aware freshness (decision 109).** Every `locate` first diffs
`(mtime, size)` per tracked file against the index, confirms changes with a
content hash, re-chunks only what moved, and drops deleted/renamed files —
so results always reflect the working tree. A warm index is near-free
(stat-only); the first-ever index is the one expensive pass (~171s cold vs
~1.4s warm on memhub's tree), so `memhub code index` is the explicit
warm-up. `locate` is therefore a read-then-write op, but writes nothing to
`project.sqlite`. `--no-refresh` (issue #67) opts out of this pass
entirely for tight repeat-call loops on an already-warm index — no `git
ls-files`, no per-file stat, no `git rev-parse HEAD` — trading the
freshness guarantee for the lowest possible latency; `files_total` /
`chunks_total` / `head` then report the last-*indexed* state, not a fresh
recount. Stale-by-choice, explicit opt-in; default behavior (refresh every
call) is unchanged without the flag.

**Retrieval default: fusion, reranker OFF (decisions 114, 122, 123).**
Recall is FTS BM25 + vector fusion with the cross-encoder reranker off and
no score floor. On `tests/code_locate_golden.json` fusion now scores
**100% Recall@3** (18/18; Recall@1 61.1%) and the reranker holds at 88.9%
Recall@3 / 77.8% Recall@1. Task 85 (decision 123) lifted fusion past the
reranker on the governing metric by closing the last two misses at their
source: a 0.90 test-path down-weight (`[code_index] test_path_penalty`) so
`tests/` / `benches/` / `examples/` chunks stop out-ranking implementation,
and capturing the Rust file-level `//!` module doc as a chunk so a file
described by its module prose is retrievable. This supersedes decision
122's fusion≈rerank tie — fusion now
wins Recall@3 outright and runs ~12× faster, so it stays decisively the
default. `--rerank` remains the opt-in and still wins single-best-guess
Recall@1 (77.8% vs 61.1%). No nonsense floor is free: a `--min-rerank-score`
of 0 rejects both gibberish probes but also kills a true match (lowest
true-match logit −5.44), so the 2 nonsense-probe leaks under fusion are an
accepted no-floor cost. `memhub eval locate [--rerank]` is the A/B harness;
it indexes memhub's own (Rust) tree, so the non-Rust grammars are A/B'd by
the polyglot fixture eval in `tests/locate_polyglot.rs` (task 88) instead.

Surfaces: `memhub locate` / `memhub code index|status|rm` (CLI) ·
`memhub.locate` (MCP, read-only — clipped snippets only, never full code) ·
`/locate` (skill).

**Scoring config is independent of recall (R11, issue #73).** Locate's
fusion weights and the test-path down-weight live under their own
`[code_index]` table — `fts_weight`, `vector_weight`, `test_path_penalty`
— rather than sharing `[retrieval.scoring]` with `memhub recall`. The two
tables used to be one struct, so tuning recall's blend silently retuned
locate's too (and vice versa); they are now separate, defaulting to the
same numeric values (0.5 / 0.5 / 0.90) so an untouched install's ranking
is unaffected. `[retrieval.scoring]`'s `stale_penalty` (and
`superseded_penalty`, `age_half_life_days`, `min_rerank_score`) have no
`[code_index]` counterpart at all — the code index has no staleness or
supersession concept, and `--rerank` here has no score floor.

**Source-scoped index (task 69).** The index is scoped to grammar-known
source files only — the deny-list still applies on top, and vendored/
minified `*.min.*` bundles are excluded (a real `.js` extension that is not
hand-written code). The grammar registry is the single source of truth for
"indexable source", so a new language row is auto-included; non-source
files (docs, `Cargo.lock`, JSON/YAML/TOML, `uplot.min.js`) are dropped and
auto-pruned from any pre-task-69 index on the next `memhub code index`.
This deliberately reverses the earlier "index every tracked path" behavior:
on `tests/code_locate_golden.json` it lifted fusion Recall@1 0%→44% and
Recall@3 72%→89% by removing non-source files (notably the golden JSON
itself) that were out-ranking real code. The reranker A/B numbers recorded
in decision 114 predated this scoping; re-measured on the clean index they
showed fusion and rerank tied at 88.9% Recall@3 (decision 122), and task 85
(decision 123) then closed the two residual source-vs-source misses to take
fusion to 100% Recall@3 (see Retrieval default, above).

## Machine-global memory

Milestone 9 (design anchor:
[docs/reference/memhub-prd-addendum-m9-machine-global-memory.md](docs/reference/memhub-prd-addendum-m9-machine-global-memory.md))
adds an optional second store at `~/.memhub/global.sqlite`,
structurally identical to a repo DB (same embedded migrations;
`project_id = 1` is per-database, so zero new SQL migrations). It is
the global-vs-repo `CLAUDE.md` idea made retrievable.

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
(exactly as repo `CLAUDE.md` overrides global `CLAUDE.md`).

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

Flow: `cargo install --path . --force --locked` → one-time, order-independent
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
`~/.codex/skills/` (dir-per-skill), `~/.config/opencode/skills/`
(dir-per-skill), and `~/.config/opencode/commands/` (flat `*.md`) — it
copies from the source repo's `templates/skills/{claude,codex,opencode}/`
and `templates/commands/opencode/`. It runs in the orchestrate phase
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
never creates `~/.claude/commands`, `~/.codex/skills`,
`~/.config/opencode/skills`, or `~/.config/opencode/commands`, and a
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
