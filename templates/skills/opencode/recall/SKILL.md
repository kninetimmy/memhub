---
name: recall
description: >
  Recall memhub project memory in OpenCode; use when project facts, decisions, tasks, or docs are needed mid-session. Trigger on: "what did we decide about X", "is there a fact/decision/task about Y", "recall X", "what do we know about Z", "look this up in memhub".
compatibility: opencode
---

# Skill: recall

Ask memhub for focused context instead of reading the full ledger.

Workflow:
- Prefer the MCP tool `memhub.recall` when available; otherwise run `memhub recall "$ARGUMENTS"` from the repo.
- Phrase the query as a natural-language question ("are task writes reviewed or direct writes?"), not a keyword list; the cross-encoder scores question phrasings several logits higher, so a keyword bag can push the row you want down the list, or under the nonsense cutoff, even when the row exists.
- A decision, task or fact you name by number in the query ("decision 201", "task #99", "fact 7") is returned first, ahead of every other hit and exempt from the cutoff (it is an explicit lookup, not ranking), when that row exists and the filters allow it (`accepted_only` drops a task); it takes one of the `max_results` slots. Shorthand such as `D17` and a number after any other word (`issue 290`, `PR #328`) is not a row id.
- Hits are compact: each carries `source_type`, `source_id`, `scope`, `title`, `rerank_score`, `created_at`, `status` (tasks only), `stale`, `superseded_by` and `text`, at most 300 characters of the row's text ending in `…` when cut. Before you quote or rely on a cut hit, fetch its full text with the MCP tool `memhub.get_row` (its `source_type`, `source_id` and `scope`) or `memhub get-row <source_type> <id> [--scope global]`; `memhub recall --json --full` also adds each hit's whole `body`.
- You judge relevance. Hits come in strict re-ranker score order, highest first, after any row named by number; recall does not mark any hit as an answer, and the best answer is not always first. Only rows under a low nonsense cutoff (`[retrieval.scoring] rerank_cutoff`, default -7.0) are dropped, so a low or negative `rerank_score` is a weaker match, not a rejected one. Without the re-ranker (fts mode, or the re-ranker off) `rerank_score` is null, hits follow the blended FTS/vector score and no cutoff applies.
- Use source-type filters only when the user asks for facts, decisions, tasks, architecture sections (`arch`), or docs specifically.
- If recall returns a `stale_embeddings` or `missing_embeddings` warning, surface the warning and ask before running `/reindex`.
- If recall is empty (nothing matched, or every candidate scored below the cutoff, which a `rerank_cutoff_dropped_all` warning reports) or unhelpfully thin, say what it returned and quote the query. Make one targeted retry with alternate phrasings of the same question and `explain=true`, preserving user-requested filters. Keep the main query a natural-language question; variants may be keyword or answer-shaped phrasings. At most four nonblank variants are accepted. They widen candidate discovery; reranking and the cutoff still score against the main question (or its best question-part), never a variant.
- Inspect `explain`: `accepted_only_excluded` counts filter exclusions; `dropped_by_cutoff`, `rerank_pool_cut`, and `result_limit_cut` count relevance, candidate-pool, and result-limit cuts; `reranker_ran` indicates whether reranking and its cutoff applied. `docs_not_searched` and `docs_dropped_no_rerank` report unsearched or excluded doc chunks; `global_store_not_searched` flags an existing global store skipped because this repo has not opted in. Diagnostics do not search additional stores or restore dropped rows. Keep user filters and retrieval configuration unchanged.
- If the retry is still insufficient and there is concrete reason to expect recorded context, open the configured rendered `PROJECT_LEDGER.md`. A user explicitly requesting the ledger may go straight to it. Never invent a result to fill an empty bundle.
- The response includes `available_docs` — ingested doc chunks that did not surface this call. If it's non-zero and the question is design/spec/architecture-flavored, consider a doc-scoped follow-up: `memhub recall "<query>" --source-type doc`.

Retry examples (carry over the same filters):

```
memhub.recall(query="<same question>", variants=["<keyword phrasing>", "<answer-shaped phrasing>"], explain=true)
```

```bash
memhub recall "<same question>" --variant "<keyword phrasing>" --variant "<answer-shaped phrasing>" --explain --json
```
