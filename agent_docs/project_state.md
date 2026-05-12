# Project State

Last updated: 2026-05-12

## Currently building

Between tasks. The K9 framework is now on a deprecation track — see
`docs/roadmap/k9-deprecation-plan.md` for the four load-bearing
slices that gate the transition (memhub render, PRD §2 + non-goal
revisits, `project_state.md` / `project_arch.md` as DB content, and
the wrap-up routing brain). M6-005 (done-marker recall fixes)
shipped as `675f614`; M6-006 closed as won't-fix because building
permanent parser tolerance for mojibake doesn't pay back when
`bootstrap-k9` itself is a transition tool.
`docs/roadmap/memhub-primary-evaluation.md` is marked superseded —
Phase 3's three open routes are now obsolete since the
render-design route is committed.

## Next up

1. `docs/roadmap/memhub-render-design.md` — the gating artifact per
   the deprecation plan. Markdown becomes an *output* of memhub
   instead of an *input*; this doc surfaces which files to emit, who
   triggers regeneration, conflict semantics on human edits, and
   where emitted files live. Loaded with implications for PRD §2, so
   likely sequences before the PRD addendum.
2. PRD §2 + `docs/roadmap/k9-integration.md` non-goal revisits.
   Downstream of the render design — let it surface which non-goals
   actually need to move before editing the PRD.
3. M6-004 — migrate memhub's own `agent_docs/*.md` to K9 canonical.
   May be obviated entirely by the render design (once memhub emits
   its own narrative, dogfooding K9 canonical disappears as a goal).
   Hold until render design clarifies.

## Last session

2026-05-12 — Cleaned Free-AI-SSD's mojibake at the source (465
`â€"` → `—` replacements across
`agent_docs/{project_decisions,project_backlog,mac_project_backlog}.md`
via byte-mode perl; changes uncommitted in that repo). Shipped
M6-005 as `675f614`: `line_has_inline_done_pr_marker` now
optionally consumes `(merged|shipped|landed|released) YYYY-MM-DD?`
between `**done**` and the PR # tail; new `extract_status_clause`
matches bolded non-bulleted `**Status:** word` lines;
`map_legacy_status` gained `shipped|landed|released → completed`.
6 new unit tests + 144 total green. End-to-end re-bootstrap on
cleaned Free-AI-SSD source: 22 done-skips (was 8), 82/82 clean
decisions (was 63). Then committed `29cdbef`: opened
`docs/roadmap/k9-deprecation-plan.md` capturing the direction
(memhub becomes primary, K9 retires) and four load-bearing slices.
Closed M6-006 as won't-fix-deprecating-K9; marked
memhub-primary-evaluation.md superseded. No PRD edits or non-goal
changes — each slice argues its own case under explicit reasoning.

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

## Open questions

- Render-design sequencing: render-first vs PRD-addendum-first?
  (Captured in `k9-deprecation-plan.md` "Sequencing" section; this
  bullet is just a pointer.)
- Does M6-004 (memhub's own `agent_docs` K9-canonical migration)
  get obviated by `memhub render`, or does it ship as a final
  dogfood pass before render?
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
