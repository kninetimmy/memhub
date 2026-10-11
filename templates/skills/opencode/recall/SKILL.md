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
- Phrase the query as a natural-language question ("are task writes reviewed or direct writes?"), not a keyword list; the cross-encoder scores question phrasings several logits higher, so a keyword bag can miss the relevance floor even when the row exists.
- A decision, task or fact you name by number in the query ("decision 201", "task #99", "fact 7") is returned first, ahead of every other hit and never flagged `low_confidence`, when that row exists and the filters allow it (`accepted_only` drops a task); it takes one of the `max_results` slots. Shorthand such as `D17` and a number after any other word (`issue 290`, `PR #328`) is not a row id.
- A hit flagged `low_confidence: true` (CLI text: `[low-confidence]`) did not clear the relevance floor. It fills the result slots left after every floor-clearing hit, up to `max_results` (with a `rerank_floor_dropped_all` warning when nothing cleared the floor), and is a weak lead: verify it against the source before relying on it, and say it is low-confidence when you cite it.
- Use source-type filters only when the user asks for facts, decisions, tasks, architecture sections (`arch`), or docs specifically.
- If recall returns stale-embedding warnings, surface the warning and ask before running `/reindex`.
- If recall is empty (nothing matched, or only default-included doc chunks that missed their own floor; those are never returned as low-confidence leads, while doc-scoped recalls fill like other rows), say that clearly and try rephrasing as a question; only fall back to rendered files when there is a concrete reason.
- The response includes `available_docs` — ingested doc chunks that did not surface this call. If it's non-zero and the question is design/spec/architecture-flavored, consider a doc-scoped follow-up: `memhub recall "<query>" --source-type doc`.
