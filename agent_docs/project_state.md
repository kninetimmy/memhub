# Project State

Last updated: 2026-05-12

## Currently building

Between tasks. M6-001 + M6-002 shipped together as `cd25a25`.
M6-003 (Free-AI-SSD bootstrap re-run for Phase 2 evidence) is now
unblocked. M6-004 (memhub's own agent_docs migration to K9
canonical) remains open and independent.

## Next up

1. M6-003 — wipe Free-AI-SSD's bootstrap DB, re-run `memhub
   integrations bootstrap-k9`, and write Phase 2 results into
   `docs/roadmap/memhub-primary-evaluation.md`. Outcome routes the
   Phase 3 decision (commission `memhub render` design, extend the
   parser further, or close the evaluation).
2. M6-004 — migrate memhub's own `agent_docs/project_backlog.md`
   and `project_decisions.md` to K9 canonical structural delimiters
   (independent; can land in any order).

## Last session

2026-05-12 — Shipped M6-001 + M6-002 in `cd25a25`. M6-002 replaced
`strip_date_prefix` with `extract_date_and_title`, which accepts
ASCII hyphen or em-dash (U+2014, K9 canonical) as the separator and
extracts the heading date into `decisions.decided_at` as
`YYYY-MM-DD 00:00:00`. New `decision::add_with_decided_at` honors an
explicit timestamp; existing `decision::add` delegates with `None`
and preserves the schema default. Wrap-up choices: en-dash
intentionally rejected (typo tolerance would mask drift from K9
canonical); date format `YYYY-MM-DD 00:00:00` chosen to match
SQLite's `CURRENT_TIMESTAMP` shape. 4 new tests (unit covering all
separator forms, unit for em-dash headings, updated test for
ASCII-hyphen + date assertion, subprocess asserting persisted
`decided_at`). Commit bundle also carries M6-001's H3-driven parser
rewrite that had been landed locally last session.

2026-05-12 — Tested bootstrap-k9 on Free-AI-SSD's K9 history; the
test surfaced 6 parser bugs (519 tasks from a 49-task backlog, dates
bolted on every decision title, 0 done-skips) and prompted a 12-gap
analysis of full memhub-primary replacement. Settled on a
bridge-first evaluation strategy in
`docs/roadmap/memhub-primary-evaluation.md`. Added M6-001 through
M6-004 to the backlog and two decisions to `project_decisions.md`
(evaluation staging; K9 canonical conventions H3 + em-dash per
`K9-Claude-Framework/docs/file-structure.md:156-208` as the parser
target). M6-001 landed locally pending commit at session end.

## Open questions

- Should `MEMHUB_ACTOR` env var be added as an alternative to the
  `--actor` flag for K9 invocations that fan out to many CLI calls?
- Should `enable-k9 --agent-docs-path` accept any path and create the
  marker file as part of an explicit "set up K9 here" flow, or stay
  read-only as it is today?
- Should `FACT_STALE_AFTER_DAYS` become a config knob, or stay
  hardcoded at 90 days until a real workflow needs otherwise?
- Should `memhub` ship a future `gc` slice that purges already-ingested
  denied paths after a pattern change, or is filter-on-read sufficient
  indefinitely?
- Which additional `clientInfo.name` values do Codex and Claude Code
  send in real handshakes beyond the initial alias map?
- Should `memhub` ship a `cargo install`-able crate (or homebrew tap)
  so the README's "put the binary on PATH" step becomes a single
  command, or stay source-only until external adoption pulls?
- Should `memhub migrate` remain implicit-on-open or become explicit
  once external users adopt the tool?
- Should a `v2` export format be introduced to include `session_notes`,
  or do notes stay scratch-only and lost on export/import indefinitely?
