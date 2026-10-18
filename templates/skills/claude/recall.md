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
context if the session-start hook delivered it or you read it at
session start, but only as a frame: the current state, an architecture
section index, and session note stubs (the architecture section index
and the session note stubs may be dropped to keep it within its size
budget). Recall fetches the full architecture sections (`--source-type
arch`) and notes (`--source-type note`); reach for the ledger only if
recall comes up empty for something you suspect is recorded.

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

Phrase the query as a natural-language question ("are task writes
reviewed or direct writes?"), not a keyword list ("task writes ungated
review require_review"). The cross-encoder scores question phrasings
several logits higher, so a keyword bag can push the row you want down
the list, or under the nonsense cutoff, even when the row exists.

A decision, task or fact you name by number in the query ("what does
decision 201 say about X", "task #99", "fact 7") is returned first,
ahead of every other hit and exempt from the cutoff (it is an explicit
lookup, not ranking), when that row exists and the filters allow it
(`accepted_only` drops a task); it takes one of the `max_results`
slots, and the other hits fill the rest.
Shorthand such as `D17` and a number after any other word (`issue 290`,
`PR #328`) is not treated as a row id.

CLI fallback (no MCP, or you want to pipe to other shell tools):

```bash
memhub recall "<query>" --json
```

The CLI also accepts a human-readable form (drop `--json`) when the
user explicitly asks to see it.

## Filters

Pick a filter only when the question narrows naturally; otherwise let
the default behavior surface facts, decisions, tasks, and architecture
sections.

- `source_types=["fact"]` / `--source-type fact` (repeatable):
  restrict to one or more of `fact`, `decision`, `task`, `arch`,
  `doc`, `note`. `note` (session notes) is returned only when requested
  explicitly; plain
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
  `git`, or `observed` rows that would otherwise leak in. It also
  drops tasks, architecture sections, and session notes, which carry
  no such source.
- `include_stale=true` / `--include-stale`: accepted and ignored. Stale
  facts (past the 90-day verification window) are always returned,
  ranked like fresh ones and flagged `stale = true`.

## Interpreting the response

The `memhub.recall` MCP bundle has the shape:

```json
{
  "query": "...",
  "mode": "fts" | "hybrid",
  "results": [
    {
      "source_type": "decision",
      "scope": "repo",
      "source_id": 17,
      "title": "...",
      "text": "at most 300 characters of the row's text…",
      "created_at": "2026-05-13T10:00:00Z",
      "stale": false,
      "superseded_by": null,
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

A task hit also carries `status` (`open`, `done` or `blocked`). The
CLI's `memhub recall --json` carries the same compact hits plus the
diagnostic `rank`, `score`, `fts_score` and `vector_score`; add
`--full` to get each hit's whole stored text as `body`. The MCP bundle
above is trimmed to what you need day to day (issue #72).

- Hits are compact: `title` and `text` come from the durable source
  tables, not paraphrases, but `text` is at most 300 characters of the
  row's text and ends in `…` when it was cut. Before you quote or rely
  on a cut hit, fetch its full text with
  `memhub.get_row(source_type="decision", source_id=17, scope="repo")`
  (CLI: `memhub get-row decision 17`, add `--scope global` for a
  global hit). `get_row` is read-only.
- You judge relevance. Hits come in strict re-ranker score order,
  highest first, after any row named by number; recall does not mark
  any hit as an answer or a non-answer, and the best answer is not
  always first. Read the titles and text and pick the hits that answer
  the question; say so when none do.
- `rerank_score` is the cross-encoder logit that decided this hit's
  place in `results` — array order is the final rank, there is no
  separate `rank` field on this path. Higher is a closer match. Only
  rows under a low nonsense cutoff (`[retrieval.scoring] rerank_cutoff`,
  default -7.0) are dropped, so a low or negative score is a weaker
  match, not a rejected one. `null` when the re-ranker didn't run for
  this call (fts mode, or hybrid with the re-ranker off): hits then
  follow the blended FTS/vector score and no cutoff applies.
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
call already ranks doc chunks with facts, decisions and tasks (at most
`[retrieval] rerank_doc_cap` chunks are re-ranked per call), so an
on-topic chunk surfaces alongside them while an off-topic one scores
under the cutoff or below the other hits — no scoping needed.

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
or every chunk already made this bundle — do nothing extra.

Doc hits come back with `source_type: "doc_chunk"` and a `title` of
`<document title> — <section breadcrumb>`; cite the document and
section when you use one.

## Empty results

When `results` is empty (nothing matched at all, or every candidate
scored below the nonsense cutoff, which a `rerank_cutoff_dropped_all`
warning reports):

1. State that recall returned nothing for the query and quote the
   exact query you ran.
2. Offer one of: rephrase the query as a natural-language question,
   broaden filters (drop `--accepted-only` or `--source-type`), or — if the question
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

In a hybrid repo you may also see `missing_embeddings` (issue #382):
`stale_count` is how many rows have no embedding for the active model,
out of `total_count` rows, and `fix` names `/reindex`
(`memhub index rebuild`). The counts cover the whole repo store and,
when the repo is opted into machine-global memory, the global store
too. `stale_embeddings` counts only this recall's candidates;
`missing_embeddings` counts every row, so both can appear together.

For either kind, surface the warning to the user and **ask before
invoking `/reindex`**. Reindex is a one-time, multi-second operation;
the user decides whether to run it. Recall results are still usable in
the meantime — the warning just means hybrid scoring may be
undercounting some rows.

## When not to use recall

- For exact file history (who changed `src/foo.rs`), use
  `memhub search "file:src/foo.rs"` — the file-history matcher.
- For decision text search with no fact/task crossover, the
  legacy `memhub search "decision <terms>"` still works but
  `recall` covers it.
- For session notes — default recall leaves them out; scope to them
  with `--source-type note` (MCP `source_types=["note"]`), or list them
  with `memhub note list`. `PROJECT.md` shows only session note stubs, and may drop them to keep it within its size budget.
- For commands (build/test/run/lint) — use `memhub get_command` or
  `memhub list_facts`; recall does not surface the `commands` table.

## Notes

- Read-only. Recall never writes to durable tables, never stages a
  pending write, never logs to `writes_log`.
- Default mode comes from `[retrieval] mode` in `.memhub/config.toml`.
  Repos in `fts` mode (the install default before issue #382; now
  the opt-out, with `hybrid` the default) get FTS-only scoring;
  switching to `hybrid` requires `memhub index rebuild` to backfill
  embeddings for pre-existing rows.
- Recall is the **mid-session** read. Session-start context lives in
  the configured rendered `PROJECT.md` when present. The full ledger
  is the fallback.
