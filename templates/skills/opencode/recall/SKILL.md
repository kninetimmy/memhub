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
- A hit flagged `low_confidence: true` (CLI text: `[low-confidence]`) did not clear the relevance floor. It is returned only when the floor dropped every candidate (with a `rerank_floor_dropped_all` warning) and is a weak lead: verify it against the source before relying on it, and say it is low-confidence when you cite it.
- Use source-type filters only when the user asks for docs/facts/decisions/tasks specifically.
- If recall returns stale-embedding warnings, surface the warning and ask before running `/reindex`.
- If recall is empty (nothing matched, or only doc chunks that missed their own floor; docs are never returned as low-confidence leads), say that clearly and try rephrasing as a question; only fall back to rendered files when there is a concrete reason.
- The response includes `available_docs` — ingested doc chunks that did not surface this call. If it's non-zero and the question is design/spec/architecture-flavored, consider a doc-scoped follow-up: `memhub recall "<query>" --source-type doc`.
