---
name: recall
description: >
  Look up relevant facts, decisions, and tasks for the current conversation via memhub recall (SQL+RAG hybrid). Prefer this over reading PROJECT_LEDGER.md mid-session. Trigger on: "what did we decide about X", "is there a fact/decision/task about Y", "recall X", "what do we know about Z", "look this up in memhub".
framework: memhub
framework_version: 1.0.0
last_updated: 2026-05-13
---

Ask memhub for context. Returns a ranked evidence bundle of facts,
decisions, and tasks pulled from this repo's `.memhub/project.sqlite`.
Read-only — no writes, no review staging.

Use this instead of grepping `PROJECT_LEDGER.md` whenever you need
project context mid-session. `PROJECT.md` is already in your
session-start context for the big-picture summary; reach for the
ledger only if recall comes up empty for something you suspect is
recorded.

## Preconditions

- `.memhub/` exists in the working repo (run `/check-init` if unsure).
- `memhub` binary on PATH.

If either is missing, surface that and stop; do not run recall.

## Invocation

Default: prefer the `memhub.recall` MCP tool when it's available — it
returns structured JSON directly and avoids shell quoting.

```
memhub.recall(query="<one-line natural-language question>")
```

CLI fallback (no MCP, or you want to pipe to other shell tools):

```bash
memhub recall "<query>" --json
```

The CLI also accepts a human-readable form (drop `--json`) when the
user explicitly asks to see it.

## Filters

Pick a filter only when the question narrows naturally; otherwise let
the default behavior surface across all three source types.

- `source_types=["fact"]` / `--source-type fact` (repeatable):
  restrict to one or more of `fact`, `decision`, `task`, `doc`. Plain
  recall (no filter) already surfaces doc chunks once the repo has
  ingested at least one doc (see "Reaching for ingested docs" below);
  scope to `doc` explicitly when you want docs only.
- `max_results=N` / `--max-results N`: cap the bundle. Default comes
  from `.memhub/config.toml` (`[retrieval] default_max_results`,
  usually 6).
- `mode="fts"` or `"hybrid"` / `--mode fts|hybrid`: override the
  project default. Only override if the user explicitly asks for one
  mode; otherwise honor the config.
- `accepted_only=true` / `--accepted-only`: only rows whose `source`
  is `user` or `user+agent:<id>`. Use when the user wants
  "approved-only" context and the repo records `agent:<id>`,
  `git`, or `observed` rows that would otherwise leak in.
- `include_stale=true` / `--include-stale`: include facts past the
  staleness window (90 days unverified). Off by default. Pass when
  the user is explicitly asking about historical state.

## Interpreting the response

The `memhub.recall` MCP bundle has the shape:

```json
{
  "query": "...",
  "mode": "fts" | "hybrid",
  "results": [
    {
      "source_type": "decision",
      "source_id": 17,
      "title": "...",
      "body": "...",
      "stale": false,
      "source": "user+agent:claude-code",
      "rerank_score": 2.31,
      ...
    }
  ],
  "candidate_count": 41,
  "returned_count": 6,
  "available_docs": 0,
  "warnings": [],
  "provenance": { "matcher": "recall:hybrid", "elapsed_ms": 12 }
}
```

The CLI's `memhub recall --json` keeps the fuller diagnostic shape
(`rank`, `score`, `fts_score`, `vector_score`, plus `rerank_score`) for
debugging and calibration; the MCP bundle above is trimmed to what you
need day to day (issue #72) — no `rank`/`score`/`fts_score`/
`vector_score`, and no `confidence` field either way.

- Use `title` and `body` directly — they are pulled from the durable
  source tables, not paraphrases.
- `rerank_score` is the cross-encoder logit that decided this hit's
  place in `results` — array order is the final rank, there is no
  separate `rank` field on this path. `null` when the re-ranker didn't
  run for this call (fts mode, or hybrid with the re-ranker off);
  positive means relevant, and nonsense candidates are dropped before
  they ever reach you (the `min_rerank_score` floor).
- `stale = true` means a fact past the verification window or a
  decision marked superseded/draft or a task marked done. Surface
  staleness when it matters; don't quote a stale fact as current.
- `source` is the row's provenance string (`user`, `user+agent:X`,
  `agent:X`, `git`, `observed`). Cite it when the user asks where a
  claim came from.
- `available_docs` (integer) counts ingested reference-doc chunks that
  did NOT surface this call — see "Reaching for ingested docs".
- Empty `results` is a real answer, not a failure — see below.

## Reaching for ingested docs

After the first `memhub doc add` in a repo, `[retrieval]
include_docs_in_default` auto-enables (decision 90): a plain recall
call already surfaces a relevant doc chunk whenever it clears the
`[retrieval.scoring] doc_min_rerank_score` relevance floor, so an
off-topic doc stays silent while an on-topic one surfaces alongside
facts/decisions/tasks — no scoping needed.

The `available_docs` count is chunks that did **not** surface this
call — the long tail. When it is **> 0** and the user's question is
design/spec/architecture/style-flavored, a doc-scoped follow-up can
still be worth running:

```
memhub.recall(query="<same or refined question>", source_types=["doc"])
```

Use judgment, not reflex: this is a per-question decision, not an
every-turn one. A question clearly answerable from facts/decisions, or
one already well-covered by the first bundle, does not need the
doc pass. When `available_docs` is 0 there are either no ingested docs
or none clear the floor for this query — do nothing extra.

Doc hits come back with `source_type: "doc_chunk"` and a `title` of
`<document title> — <section breadcrumb>`; cite the document and
section when you use one.

## Empty results

When `results` is empty:

1. State that recall returned nothing for the query and quote the
   exact query you ran.
2. Offer one of: rephrase the query, broaden filters
   (drop `--accepted-only` or `--source-type`), or — if the question
   really needs the full ledger — open the configured rendered
   `PROJECT_LEDGER.md` directly.

Never invent a result to fill an empty bundle.

## Warnings

When `warnings` is non-empty, the most common entry is:

```json
{ "kind": "stale_embeddings", "stale_count": 12, "total_count": 47,
  "reason": "missing_embeddings" | "content_drift" | "model_upgrade",
  "fix": "Run /reindex ..." }
```

Surface the warning to the user and **ask before invoking `/reindex`**.
Reindex is a one-time, multi-second operation; the user decides
whether to run it. Recall results are still usable in the meantime —
the warning just means hybrid scoring may be undercounting some rows.

## When not to use recall

- For exact file history (who changed `src/foo.rs`), use
  `memhub search "file:src/foo.rs"` — the file-history matcher.
- For decision text search with no fact/task crossover, the
  legacy `memhub search "decision <terms>"` still works but
  `recall` covers it.
- For session notes — they are write-only scratch and intentionally
  not indexed in recall.
- For commands (build/test/run/lint) — use `memhub get_command` or
  `memhub list_facts`; recall does not surface the `commands` table.

## Notes

- Read-only. Recall never writes to durable tables, never stages a
  pending write, never logs to `writes_log`.
- Default mode comes from `[retrieval] mode` in `.memhub/config.toml`.
  Repos in `fts` mode (the install default) get FTS-only scoring;
  switching to `hybrid` requires `memhub index rebuild` to backfill
  embeddings for pre-existing rows.
- Recall is the **mid-session** read. Session-start context lives in
  the configured rendered `PROJECT.md` when present. The full ledger
  is the fallback.
