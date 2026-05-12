# Project State

Last updated: 2026-05-12

## Currently building

Between tasks after `M4-005`. Confidence scoring and staleness handling
is shipped, which closes out Milestone 4. Facts now carry a derived
`is_stale` flag computed in SQL via `julianday('now') - julianday(verified_at) > 90`
(with a null `verified_at` also stale). The threshold lives as
`models::FACT_STALE_AFTER_DAYS = 90` — hardcoded for this slice, not
configurable. `CommandRecord::confidence()` returns
`success_count / (success_count + fail_count)` as `Option<f64>`, with
`None` when there have been no runs at all. `memhub fact list` renders
a `[stale]` marker, `memhub command list` renders a confidence column,
`memhub status` reports `Facts: N (M stale)`, and the MCP `status`
response gained `stale_facts` while `CommandToolRecord` gained
`confidence`. No schema migration, no new write triggers — the existing
`fact::add` and `command verify` paths already touch the right state.

## Next up

1. Plan `M5-001` K9 Claude Framework integration per
   `docs/roadmap/k9-integration.md`. All four M4 preconditions
   (`M4-001` export/import, `M4-002` recovery, `M4-003` review flow,
   `M4-004` deny list) are now shipped alongside `M4-005`, and the
   `list_pending_writes` MCP tool plus the `memhub review` CLI are the
   surface K9 `/wrap-up` was designed for.
2. Decide whether MCP needs broader indexed retrieval over facts,
   tasks, or command history beyond the current narrow paths.
3. Decide whether to promote `FACT_STALE_AFTER_DAYS` to a config knob
   once a real workflow shows the 90-day default needs tuning.

## Last session

2026-05-12 - Completed `M4-005` (confidence and staleness). Added
`models::FACT_STALE_AFTER_DAYS`, `Fact::is_stale`,
`CommandRecord::confidence()`, `StatusSummary::stale_facts`, and a
`fact::count_stale` helper. `fact::list` computes staleness in SQL via
`julianday`. CLI surfaces `[stale]` on facts, a confidence column on
commands, and a stale-facts count in `status`. MCP `StatusToolResponse`
gained `stale_facts`; `CommandToolRecord` gained `confidence`. Added
11 integration tests in `tests/staleness.rs` covering the 90-day
boundary (89d/91d), null `verified_at`, `fact add` upsert refreshing
staleness, `review accept` producing fresh facts, `count_stale`
correctness, and command confidence math edge cases. Updated README
with a "Confidence and staleness" section and marked Milestone 4
complete in the roadmap. Verified with `cargo fmt`, `cargo build`, and
`cargo test` (70 tests across all suites).

2026-05-12 - Completed `M4-004` by adding the `globset` crate, a new
`src/config/deny.rs` module with `DenyList`/`PathMatcher`/`default_patterns`,
ingest-git skip logic with `denied_files_skipped` reporting,
search-side post-filtering for both prefixed and inferred file lookups,
and a `deny_patterns` count in `memhub status` and the MCP `status`
tool. Added 5 integration tests in `tests/deny_list.rs` plus 4 unit
tests in `src/config/deny.rs`. Updated README with a "Deny list"
section. Verified with `cargo fmt`, `cargo build`, and `cargo test`
(59 tests across all suites).

2026-05-12 - Completed `M4-003` by adding migration
`0005_pending_write_reviewed_at`, the `commands::review` module with
`list/show/accept/reject/expire`, the `memhub review` CLI subcommand,
and the read-only `list_pending_writes` MCP tool. Promotion delegates
to `fact::add`/`decision::add` (which regenerate FTS chunks and run
sync-md when enabled). Added 9 integration tests in `tests/review.rs`
plus 1 MCP test. Updated README with a "Reviewing staged MCP proposals"
section. Verified with `cargo fmt`, `cargo build`, and `cargo test`
(50 tests across all suites).

## Open questions

- Should `FACT_STALE_AFTER_DAYS` become a config knob, or stay
  hardcoded at 90 days until a real workflow needs otherwise?
- Should `memhub` ship a future `gc` slice that purges already-ingested
  denied paths after a pattern change, or is filter-on-read sufficient
  indefinitely?
- Which additional `clientInfo.name` values do Codex and Claude Code
  send in real handshakes beyond the initial alias map?
- Should `memhub migrate` remain implicit-on-open or become explicit
  once external users adopt the tool?
- Should `decisions` carry a derived confidence too, and what would the
  re-verification trigger be without a `sessions`-based reference
  signal in place?
