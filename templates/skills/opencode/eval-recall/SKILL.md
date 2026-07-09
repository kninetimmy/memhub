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
- Report Recall@K metrics and whether rerank changed results.
- Do not mutate the database or write project memory from this skill.
- Note: in the memhub source repo, this scores against that machine's live `.memhub/project.sqlite` (a calibration signal, not the enforced baseline). The deterministic reference is `cargo test retrieval_golden_hermetic` (issue #44) — see `docs/reference/operations.md`.
