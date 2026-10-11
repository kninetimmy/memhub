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
floor-clearing hits), and never writes to durable tables or `writes_log`.

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
- `--no-rerank`: skip the cross-encoder re-ranker (A/B against the
  rerank-on baseline).
- `--min-rerank-score <F>`: override the cross-encoder score floor.
- `--json`: structured output (default markdown).

## Interpreting the response

JSON shape (the values below are illustrative, not a recorded baseline):

```json
{
  "golden_path": "tests/retrieval_golden.json",
  "mode": "fts" | "hybrid",
  "k": 3,
  "totals": {
    "queries": 36,
    "match_queries": 31,
    "empty_queries": 5,
    "match_passes": 31,
    "match_passes_at_1": 29,
    "empty_passes": 5,
    "safety_failures": 0
  },
  "settings": {
    "mode": "hybrid",
    "reranker": true,
    "min_rerank_score": 2.0,
    "rerank_candidate_pool": 20
  },
  "recall_at_k": 1.0,
  "recall_at_1": 0.935,
  "elapsed_ms": 47,
  "warm_latency_p50_ms": 12.5,
  "outcomes": [
    {
      "id": "decision-recall-readonly",
      "query": "recall read-only writes_log",
      "kind": "match" | "empty",
      "passed": true,
      "matched_rank": 1,
      "matched_score": 0.5,
      "matched_low_confidence": false,
      "returned_count": 1,
      "failure_reason": null
    }
  ]
}
```

Headline numbers to report:

- **Recall@K**. `recall_at_k` × 100, rounded to one decimal place.
  Per the addendum, the M8 acceptance gate is ≥ 75% on the starter
  set. `recall_at_1` (with `totals.match_passes_at_1`) is the same
  measure at rank 1, and `settings` records what the run actually
  used (mode, whether the re-ranker was enabled for the run, the rerank
  score floor and candidate pool size).
- **Safety**. `safety_failures` MUST be zero. A non-zero count means
  a `kind: empty` probe returned a hit that cleared the relevance
  floor — recall is surfacing false-positives that the golden set
  treats as forbidden. An `empty` probe passes when no returned hit
  clears the floor; low-confidence hits (`low_confidence: true`,
  returned only when the floor dropped every candidate) do not fail it.
- **Failing queries**. List by `id` with `failure_reason`. Don't
  paraphrase; quote the reason string so the user can map it to
  the matchers in the golden file.

## Comparing two runs

To judge a retrieval change per query instead of eyeballing two
totals, save each run's JSON and compare them:

```bash
memhub eval retrieval --json > a.json   # baseline (e.g. old binary)
memhub eval retrieval --json > b.json   # candidate (e.g. new binary)
memhub eval compare a.json b.json       # add --json for structured output
```

`memhub eval compare <A> <B>` pairs the runs by query id and reports,
separately for rank-1 and for found@K: each run's pass count, the ids
that passed only in A and only in B, and the exact two-sided McNemar
p-value. It also shows each run's recorded settings and lists every
`empty` probe whose pass/fail differs, saying which run returned a
hit. It compares saved output, not config flags, so it also works for
code changes. It exits non-zero without printing a comparison when the
runs' query id sets differ, a shared id has different query text, or
the runs used different K. A small p-value (conventionally under 0.05)
means the difference is unlikely to be chance; with few discordant
queries the test cannot reach significance, which is not evidence of
no difference. Read-only: it only reads the two files.

## Live golden set (A/B on this repo's own database)

`tests/retrieval_golden_live.json` is a second, larger golden set that
is scored against the repository's own memhub database instead of a
seeded fixture, so it has real distractors among roughly 1,000
candidate rows. It is not run in CI. Use it to measure any retrieval change (tasks 160, 175, 98, an
embedder swap) before it lands. The hermetic `tests/retrieval_golden.json`
cannot tell close variants apart, and numbers from the two sets are not
comparable.

To A/B a code change, run both binaries against the same database,
back to back, with nothing writing memhub state in between (no `add`,
`accept`, `doc add`, `index rebuild`, `upgrade` or wrap-up, and no other
agent session writing to the repo):

```bash
<old-memhub> eval retrieval --golden tests/retrieval_golden_live.json --k 5 --json > a.json
<new-memhub> eval retrieval --golden tests/retrieval_golden_live.json --k 5 --json > b.json
memhub eval compare a.json b.json       # add --json for structured output
```

Run the older binary first. Every `eval retrieval` run applies memhub's
maintenance-on-open pass (the Maintenance-on-open contract section of
`docs/reference/operations.md`), so a binary with a newer schema migrates
the database during its run, and an older binary then refuses to open it.

Save the files with a redirect that writes UTF-8 (Git Bash, zsh, bash,
or PowerShell 7 and later). Windows PowerShell 5.1's `>` writes UTF-16,
which `eval compare` rejects. For a settings-only change, one binary
with a flag such as `--no-rerank` or `--min-rerank-score` works for
run B. Report both commits, the settings and the counts, and say the
numbers come from this machine's database. The first baseline and the
full step-by-step procedure are in `docs/reference/operations.md`
(Retrieval section).

## Multi-answer golden queries

A golden query normally names one acceptable answer
(`source_type` / `title_contains` / `body_contains`). When several
rows legitimately answer one question (say a fact, a decision and a
doc chunk), the query may add `"also_accept": [{...}, ...]`, each
entry using the same three matcher fields. The query passes when any
hit in the top K satisfies any one of the answers. Queries without
`also_accept` score exactly as before.

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
  `cargo test retrieval_golden_hermetic` (issue #44, N28): it
  seeds a disposable fixture DB from scratch and reproduces the same
  golden set (36 queries: 31 match, 5 empty) independent of this
  machine's DB state. Recorded baseline there: Recall@3 31/31, 0 safety
  failures (see `docs/reference/operations.md`'s Retrieval section).
  If this live invocation and that test disagree, trust the test.
- For the equivalent gesture from Claude Code, see the
  `/eval-recall` skill under `templates/skills/claude/`.
