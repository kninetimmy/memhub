---
name: eval-recall
description: Run the memhub Recall@K eval harness against tests/retrieval_golden.json and report the baseline. Read-only; never mutates the DB or writes_log.
framework: memhub
framework_version: 1.0.0
last_updated: 2026-07-06
---

Run the M8 retrieval acceptance gate. Drives `memhub eval retrieval`
under the hood. Returns a Recall@K number plus per-query pass/fail
detail, surfaces safety failures (empty-probe queries that leaked
results), and never writes to durable tables or `writes_log`.

This is the Codex counterpart to the Claude Code `/eval-recall` skill.
Both call into the same `memhub eval retrieval` CLI; they differ only
in the agent identifier on whatever read-side telemetry the host
captures.

Use this when:

- The user wants to know "is recall still good after change X?"
- A scoring knob, the embedding model, or the recall engine itself
  changed and you need a regression check.
- You're closing M8 (or a future retrieval PR) and want the baseline
  number for the wrap-up.

## Preconditions

- `.memhub/` exists in the working repo (run `/check-init` if unsure).
- `memhub` binary on PATH.
- `tests/retrieval_golden.json` exists at the repo root (the default).
  If the user maintains a different golden set, pass `--golden <path>`.

If preconditions fail, surface that and stop; do not invent a golden
set.

## Invocation

```bash
memhub eval retrieval --json
```

Flags:

- `--golden <path>`: override the default
  `tests/retrieval_golden.json` location.
- `--k <N>`: change Recall@K (default 3, per addendum §9).
- `--mode fts|hybrid`: override the project's `[retrieval] mode`
  config. Use only when explicitly comparing modes.
- `--json`: structured output (default markdown).

## Interpreting the response

JSON shape:

```json
{
  "golden_path": "tests/retrieval_golden.json",
  "mode": "fts" | "hybrid",
  "k": 3,
  "totals": {
    "queries": 12,
    "match_queries": 11,
    "empty_queries": 1,
    "match_passes": 10,
    "empty_passes": 1,
    "safety_failures": 0
  },
  "recall_at_k": 0.909,
  "elapsed_ms": 47,
  "outcomes": [
    {
      "id": "decision-recall-readonly",
      "query": "recall read-only writes_log",
      "kind": "match" | "empty",
      "passed": true,
      "matched_rank": 1,
      "matched_score": 0.5,
      "returned_count": 1,
      "failure_reason": null
    }
  ]
}
```

Headline numbers to report:

- **Recall@K**. `recall_at_k` × 100, rounded to one decimal place.
  Per the addendum, the M8 acceptance gate is ≥ 75% on the starter
  set.
- **Safety**. `safety_failures` MUST be zero. A non-zero count means
  a `kind: empty` probe returned hits — recall is surfacing
  false-positives that the golden set treats as forbidden.
- **Failing queries**. List by `id` with `failure_reason`. Don't
  paraphrase; quote the reason string so the user can map it to
  the matchers in the golden file.

## When the harness regresses

The harness regresses when:

1. `recall_at_k` drops below the recorded baseline.
2. `safety_failures > 0` (any leakage).
3. A previously-passing query now fails with `"no top-K hit
   matched"`.

In all three cases:

- Quote the failing query IDs and reasons.
- Do **not** "fix" by loosening the matchers in
  `tests/retrieval_golden.json`. The golden set is the spec; the
  retrieval surface adapts to it, not the other way around.
- The fix is usually in `src/retrieval/recall.rs` (scoring,
  tokenization), in `src/retrieval/persist.rs` (embed text format),
  or in the embedding model itself. Surface a hypothesis, get
  confirmation, then change the engine.

## When the harness is silent

If `match_queries == 0`, the golden file has no positive cases — the
harness can run but Recall@K is undefined (returns 0.0). Surface that
and ask whether the user expected the file to be all-negative.

If the eval reports `Recall@K = 100%` and the user just doubled the
fact/decision/task corpus, mention that the baseline may need a fresh
read — the test is most useful when retrieval has to discriminate
between many candidates, not when each golden query has only one
plausible target.

## Notes

- Read-only. Eval never writes to durable tables, never stages a
  pending write, never logs to `writes_log`. Safe to run mid-session.
- Default mode comes from `[retrieval] mode` in `.memhub/config.toml`.
  Repos in `fts` mode get FTS-only scoring; `--mode hybrid` requires
  `memhub index rebuild` to have backfilled embeddings first
  (otherwise expect `stale_embeddings` warnings in recall, but eval
  itself still runs).
- **In the memhub source repo specifically**, this invocation scores
  against *this machine's* live `.memhub/project.sqlite` (the golden
  set is self-referential — its queries target memhub's own real
  decisions/facts/tasks), so treat the number it reports as a
  self-hosted calibration/dogfood signal, not the enforced baseline.
  The enforced, deterministic reference is
  `cargo test --test retrieval_golden_hermetic` (issue #44, N28): it
  seeds a disposable fixture DB from scratch and reproduces the same
  18-query golden set independent of this machine's DB state. Recorded
  baseline there: Recall@3 100% (17/17), 0 safety failures
  (2026-07-06, see `docs/reference/operations.md`'s Retrieval section).
  If this live invocation and that test disagree, trust the test.
- For the equivalent gesture from Claude Code, see the
  `/eval-recall` skill under `templates/skills/claude/`.
