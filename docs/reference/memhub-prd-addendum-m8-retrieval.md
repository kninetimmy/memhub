# PRD addendum: M8 SQL+RAG hybrid recall

**Author:** Elswick
**Status:** Addendum to [`memhub-prd.md`](memhub-prd.md) (Draft v2). Authoritative for the items it modifies.
**Last updated:** 2026-05-13

This document supplements `memhub-prd.md` rather than replacing it.
The PRD stays verbatim per the project guardrail in `CLAUDE.md`.
Where this addendum and the PRD disagree on the items called out
below, this addendum is authoritative; everything not addressed here
continues to read from the PRD as-written.

This addendum is the design anchor for **Milestone 8: SQL+RAG hybrid
recall**. The 19 decisions and 6 PR-shaped tasks routed into the DB
on 2026-05-13 (decisions 19–37, tasks 11–16) are the operational
breakdown; this document is the rationale and the contract.

---

## What this addendum modifies

| PRD section | Status after addendum | Reason |
|---|---|---|
| §4 "Non-goals" — bullet "Embedding-based semantic search in v1 (deferred to v2, may never ship)" | **Relaxed.** Embedding-based semantic search becomes a v2 opt-in via an install-time mode toggle. FTS-only remains a first-class supported mode. | M8 retrieval layer. |
| §8 "Data model" — `chunks(id, project_id, source_type, source_id, text, created_at)` + `chunk_fts` | **Restructured.** FTS5 moves to contentless virtual tables attached directly to source tables (`facts`, `decisions`, `tasks`); the legacy `chunks` table is deprecated for new sources. A new `embeddings` table is added. | Decisions 25 + 26. |
| §9 "Indexing principle" — query-shape table | **Extended.** Adds a new query-shape row: vector similarity search via brute-force cosine over BLOB-stored embeddings, gated by hybrid mode. The "no full scans on unbounded tables" rule still applies; embedding scans are bounded by the corpus size and explicitly accepted at memhub scale. | Decision 24. |
| §12 "MCP tool surface" | **Extended.** Adds `memhub.recall` as the primary retrieval surface for agents. | Decision 30. |
| §13 "CLI surface" | **Extended.** Adds `memhub recall`, `memhub index status`, `memhub index rebuild`, and `memhub eval retrieval`. | Decisions 31, 32, 36. |
| §16 "Milestones" — Milestone 5+ list | **Extended.** "Milestone 8: SQL+RAG hybrid recall" added with the six PRs enumerated below. | Decision 19. |
| §17 "Success metric" | **Extended.** Recall@3 over `tests/retrieval_golden.json` is added as the M8 acceptance metric. | Decision 36. |

The PRD's design principles (§3), other non-goals (§4), router
overview (§10), write-back policy (§11), MCP read/write split (§12
unchanged otherwise), migrations and export (§14), security (§15),
and risks (§18) are otherwise unchanged.

---

## 1. The §4 non-goal relaxation (load-bearing)

PRD §4 lists as a non-goal:

> Embedding-based semantic search in v1 (deferred to v2, may never ship)

This addendum **partially overturns** that non-goal. The relaxation is
narrow and explicit:

- Semantic search becomes a **v2 opt-in**, never a default. The
  install-time `[retrieval] mode` flag defaults to `fts`. Users who
  want hybrid retrieval set `mode = "hybrid"` explicitly.
- The FTS-only path remains a fully supported, first-class mode. Every
  `recall` query that works under hybrid mode must also work under
  FTS-only mode, returning sensibly-ranked results from keyword match
  alone.
- The vector index is derived state. Deleting all embeddings must
  leave memhub fully functional in FTS-only mode. Export/import does
  not include embeddings; they are rebuildable.
- All other §4 non-goals (multi-user sync, replacing git/agents,
  general knowledge base, auto-compaction, cloud) remain in force.

The "may never ship" hedge in the original non-goal is now answered:
it ships in M8 as an opt-in. The conservative framing carries forward
as the default: FTS-only is the safe path; hybrid is the
performance-curious path.

## 2. Install-time retrieval mode toggle

Added to `.memhub/config.toml`:

```toml
[retrieval]
mode = "fts"                     # "fts" | "hybrid"
default_max_results = 6
default_budget_tokens = 1200
accepted_only_by_default = true
include_stale_by_default = false

[retrieval.scoring]
fts_weight = 0.5
vector_weight = 0.5
stale_penalty = 0.3

[retrieval.embeddings]
model = "bge-small-en-v1.5"      # bundled in binary; informational
```

The `mode` flag is the only required key. The rest have defaults
suitable for hobby-project memhub use; tuning is a PR4 concern, not a
user-facing concern by default.

Switching modes is non-destructive. `fts → hybrid` runs
`memhub index rebuild` to generate embeddings for existing rows;
`hybrid → fts` simply stops consulting the embeddings table. The
embeddings table is preserved either way.

## 3. Data model changes (§8 restructure)

### 3.1 FTS5 surface

Decision 26 ("FTS5 virtual tables attached to source tables") changes
the FTS approach from the PRD's §8:

**Before (PRD §8):**

```sql
chunks(id, project_id, source_type, source_id, text, created_at)
chunk_fts  -- FTS5 virtual table over chunks.text
```

The current implementation backs decision search with a row per
decision in `chunks`, FTS-indexed via `chunk_fts`. Facts and tasks
are not currently FTS-indexed.

**After (M8):**

Contentless FTS5 virtual tables that point directly at source-table
columns, eliminating data duplication:

```sql
CREATE VIRTUAL TABLE facts_fts USING fts5(
    value,
    content='facts', content_rowid='id'
);
CREATE VIRTUAL TABLE decisions_fts USING fts5(
    title, rationale,
    content='decisions', content_rowid='id'
);
CREATE VIRTUAL TABLE tasks_fts USING fts5(
    title, notes,
    content='tasks', content_rowid='id'
);
```

Triggers on `INSERT`, `UPDATE`, and `DELETE` of each source table
keep the FTS index synced.

**Legacy `chunks` table.** The existing `chunks` table is deprecated
for new use. Migration `0009` (see PR2 in §7 below) introduces the
contentless FTS tables and backfills them from existing source rows.
`chunks` rows for decisions stay populated by the existing decision
write path until a follow-up cleanup migration removes the table; the
new decision FTS query path reads `decisions_fts` exclusively. No
behavior depends on the legacy `chunks` table after PR2 lands.

### 3.2 New `embeddings` table

```sql
CREATE TABLE embeddings (
    id              INTEGER PRIMARY KEY,
    project_id      INTEGER NOT NULL,
    source_type     TEXT    NOT NULL,    -- 'fact' | 'decision' | 'task'
    source_id       INTEGER NOT NULL,
    model_name      TEXT    NOT NULL,    -- e.g. 'bge-small-en-v1.5'
    dimension       INTEGER NOT NULL,    -- 384 for BGE-small
    vector          BLOB    NOT NULL,    -- packed little-endian f32 array
    content_hash    TEXT    NOT NULL,    -- BLAKE3 (or similar) of source body
    created_at      TEXT    NOT NULL,
    UNIQUE(source_type, source_id, model_name)
);

CREATE INDEX embeddings_lookup
    ON embeddings(source_type, source_id, model_name);
CREATE INDEX embeddings_model
    ON embeddings(model_name);
```

Notes:

- One row per `(source_type, source_id, model_name)`. Multiple models
  can coexist; queries select by active model. Practical use is one
  active model at a time.
- `vector` stores the embedding as a packed little-endian `f32` BLOB.
  At 384 dimensions, that's 1,536 bytes per row.
- `content_hash` lets the eager-embed path skip no-op writes when the
  source body hasn't changed (decisions 27 + 28). Mismatch on read
  marks the embedding stale (§5 below).
- No `FOREIGN KEY` on `(source_type, source_id)` — the column is
  polymorphic and SQLite can't enforce a multi-table FK. The
  application layer is responsible for orphan cleanup, which is
  triggered by the same writes-table `DELETE` that already removes
  decision chunks today.

### 3.3 Decision 25 in practice: no `memory_chunks` table

The proposal that seeded M8 (`memhub_sql_plus_rag_proposal.md`)
suggested a `memory_chunks` table that would normalize all sources
into a single chunk shape. Decision 25 rejects that table:

- Facts, decisions, and tasks are short enough that one row is one
  chunk. No splitting required.
- Avoiding the normalization table eliminates the chunk-source-drift
  problem (content_hash on chunks rows + on source rows).
- The `recall` query path UNIONs across source tables instead. The
  query is straightforward at memhub scale.

If memhub ever grows source types with bodies that exceed practical
chunk sizes (e.g., long session notes, ingested architecture docs),
the table can be added in a future addendum without breaking the M8
contract.

## 4. Indexing principle: vector similarity (§9 extension)

PRD §9 enumerates valid query shapes and their required indexes.
M8 adds:

| Query shape                              | Index type required         | Example |
|------------------------------------------|-----------------------------|---------|
| Vector similarity (top-K nearest)        | Brute-force cosine over BLOB column, bounded by corpus size | `recall` hybrid mode |

The brute-force cosine path is **explicitly accepted at memhub
scale.** §9's "no full scans on unbounded tables" rule still binds
all other paths. The bound for `embeddings` is the corpus size: at
1,000 rows × 384-dim × 4-byte f32, a full scan is 1.5 MB and well
under §9's 500 ms FTS budget. If a memhub project ever crosses
~10,000 rows of embeddable content, the `vector_storage` decision
(Decision 24) is revisited — likely by introducing `sqlite-vec` as an
optional extension. That revisit is a future addendum, not in M8.

### Hybrid scoring formula

The default hybrid scoring formula:

```
final_score(candidate) =
      fts_weight     * normalized_fts_score(candidate)
    + vector_weight  * cosine_similarity(query_vec, candidate_vec)
    - stale_penalty  * (1 if candidate.stale else 0)
```

With config defaults `fts_weight = 0.5`, `vector_weight = 0.5`,
`stale_penalty = 0.3`. Tuning is a PR4 concern guided by the
Recall@3 metric (§9 below); the addendum does not lock the weights.

`normalized_fts_score` is BM25 rank rescaled to `[0, 1]` per query
(top hit is 1.0). `cosine_similarity` is in `[-1, 1]` but in
practice for embedding-of-text inputs is in `[0, 1]`.

Filters apply before scoring, not after:

- `accepted_only` (decision review status)
- `include_stale` (fact staleness)
- `source_type` allowlist
- deny-list path filters

## 5. Eager-embed write path (decisions 27 + 28)

Fact, decision, and task `add` handlers re-embed the affected row
inside the same DB transaction as the source write. The flow:

1. Source row write (INSERT or UPDATE) inside transaction `T`.
2. Compute `content_hash` of the new body.
3. Look up `embeddings.content_hash` for the same `(source_type, source_id, active_model)`.
4. If match, no-op the embedding write (saves ~50 ms).
5. If miss, run the embedding model on the body, UPSERT the
   embeddings row inside `T`.
6. Commit `T`.

Target latency: ~50 ms additional per write, dominated by inference.
Acceptable per Decision 27 because the consistency guarantee is
worth it: a successful write guarantees a current embedding exists,
ruling out a class of "embedding missing for new row" bugs.

`DELETE` of a source row cascades to the embedding via an explicit
delete in the same transaction. There is no SQL-level FK to fall
back on.

## 6. Stale-embedding detection and UX (decision 29)

Embeddings go stale in three cases:

1. **Source body changed but embedding wasn't refreshed.** Detected
   by `content_hash` mismatch on read. Should be rare given eager
   embed, but possible if a migration writes source rows directly.
2. **Model upgrade.** A new memhub version ships with a different
   embedding model (e.g., BGE-small → BGE-base). All existing
   embeddings reference the old model and are stale by definition.
3. **Source row exists but no embedding row.** New schema or
   not-yet-indexed source row.

`memhub.recall` (hybrid mode) checks the active embeddings on each
call. If any candidate source row has a stale or missing embedding,
the call:

- Still returns results, using FTS-only scoring for the affected
  rows.
- Includes a `warnings` array in the response:

```json
{
  "results": [...],
  "warnings": [
    {
      "kind": "stale_embeddings",
      "stale_count": 12,
      "total_count": 47,
      "reason": "model_upgrade",
      "fix": "Run /reindex to refresh."
    }
  ]
}
```

The CLAUDE.md / AGENTS.md rule (§10 below) tells the agent to surface
the warning to the user and **ask before running `/reindex`**, not to
auto-run it. Reindex is a one-time, multi-second operation; the user
should be the one to trigger it.

## 7. New CLI surface (§13 extension)

```
memhub recall <query>
    [--source-type fact|decision|task]      # repeatable allowlist
    [--max-results N]                        # default from config
    [--include-stale]                        # opt in to stale facts
    [--accepted-only]                        # opt in to status='accepted' decisions
    [--mode fts|hybrid]                      # override config default
    [--json]                                 # JSON output (default markdown)

memhub index status [--json]                 # counts, last rebuild, stale ratio
memhub index rebuild [--model NAME] [--json] # wipe and rebuild embeddings

memhub eval retrieval
    [--golden tests/retrieval_golden.json]   # override default path
    [--json]                                 # JSON output for skill consumption
```

`recall` is the primary surface. `index` and `eval` are admin /
maintenance surfaces. All three commands accept `--actor NAME` for
audit attribution consistency with the rest of the CLI.

## 8. New MCP tool: `memhub.recall` (§12 extension)

Added to the MCP tool surface as a read tool:

```
memhub.recall
  Input:
    query           string  (required)
    mode            "fts" | "hybrid"  (optional, defaults to config)
    max_results     integer (optional, defaults to config)
    source_types    array<string>  (optional, allowlist)
    accepted_only   boolean (optional, defaults to config)
    include_stale   boolean (optional, defaults to config)
```

Output shape (cited evidence bundle):

```json
{
  "query": "why staged writes?",
  "mode": "hybrid",
  "results": [
    {
      "rank": 1,
      "source_type": "decision",
      "source_id": 17,
      "title": "Stage agent-originated writes before promotion",
      "body": "Agents may propose facts and decisions, but durable...",
      "score": 0.91,
      "fts_score": 0.84,
      "vector_score": 0.92,
      "confidence": 1.0,
      "stale": false,
      "source": "user+agent:claude-code"
    }
  ],
  "candidate_count": 41,
  "returned_count": 6,
  "warnings": [],
  "provenance": {
    "matcher": "recall:hybrid",
    "elapsed_ms": 12
  }
}
```

`provenance` is required per PRD §10.4. Empty `results` returns an
empty array, never a hallucinated guess (decision 33).

MCP write tools are unchanged in M8. Agent-originated facts and
decisions continue to stage through `pending_writes` and require
review (PRD §11.3); `recall` is read-only.

## 9. Eval discipline: golden queries and Recall@3 (§17 extension)

PRD §17's success metric is "agent answers project-context questions
correctly without re-reading CLAUDE.md or grepping the repo." M8 adds
a measurable, automated metric specifically for the retrieval layer:

**Recall@3** over `tests/retrieval_golden.json`: across N golden
queries, what fraction had the expected row in the top 3 `recall`
results? Single number, easy to interpret, easy to track across
scoring or model changes.

The starter golden set has 12 queries seeded during M8 planning
covering decisions, facts, tasks, negative cases (nonsense queries),
and at least one self-referential probe that only passes after M8
decisions are themselves indexed. Queries use `title_contains` and
`body_contains` matchers rather than hard IDs so the test survives
row-ID shifts.

Acceptance gate for M8:

- The harness exists and reports a baseline Recall@3 number.
- The baseline is the metric reference; later scoring/model tweaks
  must not regress it without an explicit override.
- Safety assertions: nonsense queries return empty bundles;
  rejected/denied content stays out of results.

Per Decision 37, `/eval-recall` is the agent-driven invocation of
the harness. CI integration is left as an open question (see §11
below).

## 10. Behavior change: agents prefer `recall` over the ledger

The token-savings motivation for M8 only materializes if agents
actually use `recall`. Decision 34 and 35 encode the rule:

> At session start, read `agent_docs/PROJECT.md` only — it's the
> thin summary. When you need decisions, facts, or tasks beyond
> what's in the summary, call `memhub.recall` rather than reading
> `agent_docs/PROJECT_LEDGER.md`. Read the ledger only as a
> fallback when `recall` doesn't return enough.

Implementation:

- Project-level `CLAUDE.md` (in this repo) gets this rule added to
  the Session Continuity section.
- Templates under `templates/skills/claude/` and
  `templates/skills/codex/` are updated to teach the same pattern
  to other repos that initialize memhub.
- The existing `/wrap-up` skill is updated only minimally: the
  read-window step continues to read prior state and arch directly,
  not via `recall`, because the wrap-up authoritative read needs
  exactness, not similarity.

`PROJECT.md` and `PROJECT_LEDGER.md` continue to render as today
(K9-deprecation addendum §1, §4). The render output shape is
unchanged. What shifts is the **agent consumption pattern**, not
the render contract.

## 11. Open implementation questions

These remain open and will be resolved during the PR sequence:

- **Per-result token estimates in `recall` response.** Whether the
  output bundle should include a `token_estimate` per result so the
  agent can apply its own budget mid-stream. Decision deferred to PR4.
- **Eval harness on CI vs. on-demand.** Whether `memhub eval
  retrieval` runs in CI on every PR touching the retrieval code, or
  only on-demand via `/eval-recall`. Tradeoff is signal vs. CI
  flakiness from model nondeterminism.
- **Embedding model swap policy.** Whether memhub versions can ship
  a different bundled model. The eager-embed path supports model
  coexistence in the schema (UNIQUE on
  `(source_type, source_id, model_name)`); the open question is the
  migration UX when the active model changes. Likely a `/reindex`
  prompt via the §6 warning mechanism.

## 12. PR sequence (decisions → tasks 11–16)

| Task ID | PR | Scope |
|---|---|---|
| 11 | PR1 | `fastembed-rs` integration + bundled BGE-small ONNX via `include_bytes!`. Inference wrapper. Smoke test. Binary grows ~10 MB → ~140 MB. |
| 12 | PR2 | Migration `0009`: contentless FTS5 over `facts` / `decisions` / `tasks`; new `embeddings` table; backfill on first run. Legacy `chunks` deprecated (§3.1). |
| 13 | PR3 | Eager-embed in fact/decision/task `add` paths inside the source-write transaction. `content_hash` short-circuit. |
| 14 | PR4 | `memhub recall` CLI + `memhub.recall` MCP tool. Hybrid scoring. Filters. Empty-bundle behavior. Provenance. |
| 15 | PR5 | `/recall` and `/reindex` Claude / Codex skill templates. `CLAUDE.md` Session Continuity rule update (§10). |
| 16 | PR6 | `tests/retrieval_golden.json` (12 starter queries). `memhub eval retrieval` command. `/eval-recall` skill. |

Acceptance for M8 lands when PR6 reports a baseline Recall@3 above
75% on the starter golden set, with zero safety-test failures
(nonsense queries empty, rejected/denied content excluded).

## 13. What does not change

These PRD-level commitments hold without modification:

- **Local-first, local only by default** (PRD §3.2). The embedding
  model is bundled in the binary (decision 23). No network calls at
  runtime. No model download on first run.
- **Boring tech** (PRD §3.5). `fastembed-rs` is a pure-Rust crate
  over ONNX runtime; no Python, no C++ vector DB, no extension
  loading. The decision to skip `sqlite-vec` (decision 24) is
  explicitly a boring-tech choice.
- **One DB file = one repo** (PRD §3.6). Embeddings live in the same
  `.memhub/project.sqlite`. No new files outside `.memhub/`.
- **Agents are untrusted writers** (PRD §3.3). `recall` is read-only.
  The pending-writes review flow is unchanged.
- **Write-back policy §11.** Wrap-up and accept paths still control
  what becomes durable; `recall` reads from the durable tables it
  finds.
- **Export/import §14.** The v1 export format is unchanged. The
  `embeddings` table is derived state and explicitly excluded from
  export, matching the source-vocabulary addendum's treatment of
  derived FTS data. A `v2` export could include embeddings as an
  optional payload; default behavior stays "export canonical, rebuild
  derived."
- **Security / privacy §15.** No new exfiltration surface. Embedding
  inference is local. Vectors never leave the machine.
- **Indexing principle §9** *(otherwise unchanged).* The new vector
  query shape is explicitly bounded (§4 above). Every other query
  path in the codebase continues to obey the full-scan prohibition.
- **MCP tool surface §12** *(write tools unchanged).* `memhub.recall`
  is purely read; nothing about the existing staged-write flow
  changes.

## 14. Reference design docs

Authoritative for the items they cover:

- This document (`memhub-prd-addendum-m8-retrieval.md`) — M8 retrieval
  layer design.
- [`memhub-prd-deprecation-addendum.md`](memhub-prd-deprecation-addendum.md) — render output shape, the PROJECT.md / PROJECT_LEDGER.md contract that §10 above changes consumption of (not shape of).
- [`memhub-prd-source-vocabulary-addendum.md`](memhub-prd-source-vocabulary-addendum.md) — `source` column semantics. `recall` results surface `source` verbatim per the existing convention.
- [`memhub-prd.md`](memhub-prd.md) — base PRD. Everything not modified by an addendum reads from here.

Decision rows 19–37 and tasks 11–16 in the DB are the operational
breakdown of this addendum. The addendum is the rationale; the DB
is the source of truth for what's locked.
