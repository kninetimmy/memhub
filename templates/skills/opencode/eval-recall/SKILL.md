---
name: eval-recall
description: Run the memhub retrieval eval harness from OpenCode; use to check Recall@K quality.
compatibility: opencode
---

# Skill: eval-recall

Run the read-only retrieval evaluation harness.

Workflow:
- Run `memhub eval retrieval` from the memhub source repo.
- Optionally compare `memhub eval retrieval --no-rerank` if the user asks for reranker A/B data.
- To judge a change per query, save two `memhub eval retrieval --json` runs (for a code change: one from the old binary, one from the new) and run `memhub eval compare <A.json> <B.json>`. It pairs the runs by query id and reports, for rank-1 and for found@K, each run's pass count, the ids that passed only in A or only in B, and the exact two-sided McNemar p-value, plus each run's recorded settings and any `empty` probe whose result differs. It exits non-zero without a comparison if the query ids, a query's text, or K differ between the runs.
- A golden query may list several acceptable answers with `also_accept` (each entry uses `source_type` / `title_contains` / `body_contains`); it passes when any top-K hit matches any one of them.
- Report Recall@K metrics and whether rerank changed results.
- An `empty` probe passes when no returned hit clears the relevance floor; low-confidence hits (`low_confidence: true`, returned only when the floor dropped every candidate) do not fail it. A non-zero `safety_failures` means an `empty` probe returned a floor-clearing hit.
- Do not mutate the database or write project memory from this skill.
- Note: in the memhub source repo, this scores against that machine's live `.memhub/project.sqlite` (a calibration signal, not the enforced baseline). The deterministic reference is `cargo test retrieval_golden_hermetic` (issue #44) — see `docs/reference/operations.md`.
