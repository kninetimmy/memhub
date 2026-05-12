# Project State

Last updated: 2026-05-12

## Currently building

Between tasks. M6-003 shipped as `9c8c103`; Phase 2 evaluation
evidence is captured in `docs/roadmap/memhub-primary-evaluation.md`
and routed to **marginal** (not "acceptable"). M6-004 (memhub's own
agent_docs migration to K9 canonical) remains open and independent.
M6-005 (done-marker recall fixes) and M6-006 (mojibake separator)
opened as the narrow follow-ups recommended by Phase 2.

## Next up

1. M6-005 — extend done-marker detection to recognize
   `**done** — merged DATE (PR #N)` syntax and "Shipped"
   vocabulary. Expected to lift Free-AI-SSD recall on body markers
   from 8/12 to 12/12.
2. M6-006 — accept the UTF-8 mojibake `â€"` triple-codepoint
   sequence (U+00E2 U+20AC U+201D) as a third separator branch in
   `extract_date_and_title`. Expected to lift 19/82 Free-AI-SSD
   decisions from dirty-title to clean.
3. M6-004 — migrate memhub's own `agent_docs/project_backlog.md`
   and `project_decisions.md` to K9 canonical structural delimiters
   (independent; can land in any order).

## Last session

2026-05-12 — Ran the M6-003 Phase 2 evaluation against
Free-AI-SSD. Built a fresh `cargo --release` binary, wiped
Free-AI-SSD's `.memhub/` (backup preserved at
`/Users/stephenelswick/Free-AI-SSD/project.sqlite.pre-m6-003-rerun.bak`),
re-ran `memhub integrations bootstrap-k9 --json`. Captured: 82/82
decisions imported (63 clean / 19 mojibake-stuck on `â€"`
separator), 49/49 backlog items found (41 open / 8 skipped done;
human's index-table truth is 16-20 done). Surfaced three concrete
parser limitations on real data: mojibake separator in 19
decisions, `**done** — merged DATE (PR #N)` syntax missing 4 done
items (F2, F2a, F3, X13), and "Shipped" / preamble index-table
conventions unrecognized. Wrote +195 lines into
`docs/roadmap/memhub-primary-evaluation.md` including counts vs
`grep` ground truth, 5-decision + 10-task spot-check, DB ergonomics
comparison, and Phase 3 routing on "marginal." Recommendation: do
NOT commission `memhub render` yet; close M6-005/M6-006 first then
revisit. Shipped as `9c8c103` alongside the M6-005/M6-006 backlog
entries.

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
