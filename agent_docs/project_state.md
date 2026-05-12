# Project State

Last updated: 2026-05-12

## Currently building

Between tasks after `M5-001` phase 1. K9 detection and integration
config landed in this repo. `memhub init` now detects the K9 four-file
framework by probing `<agent_docs_path>/project_state.md` (defaults to
`agent_docs`) and, when the config is freshly created, writes a
`[integrations.k9]` section with `enabled = true` into
`.memhub/config.toml`. Existing configs are never modified. A new
`memhub integrations status | enable-k9 | disable-k9` subcommand
provides explicit toggling. Drift between config and filesystem is
surfaced in `memhub status` (and the MCP `status` tool) as a `note:`
line but never auto-corrected. No K9 repo edits in this slice — the
`/wrap-up` shell-out is `M5-002`, and surfacing `pending_writes` during
wrap-up review is `M5-003`.

## Next up

1. Plan `M5-002`: K9 repo `/wrap-up.md` post-approval shell-out into
   `memhub decision add` / `memhub task add` / `memhub fact add`.
   Needs a stable contract doc (`docs/reference/k9-wrap-up-contract.md`)
   describing the CLI invocations and exit-code semantics.
2. Plan `M5-003`: surfacing `pending_writes` during K9 `/wrap-up`
   review drafts via the existing `memhub review list` / MCP
   `list_pending_writes` paths.
3. Decide whether MCP needs broader indexed retrieval over facts,
   tasks, or command history beyond the current narrow paths.

## Last session

2026-05-12 - Completed `M5-001` phase 1 (K9 detection + config +
status surfacing). Added `src/config/integrations.rs` with
`IntegrationsConfig`, `K9Config`, and `detect_k9`; wired
`integrations: IntegrationsConfig` into `ProjectConfig` behind
`#[serde(default)]` so existing configs upgrade cleanly. New
`src/commands/integrations.rs` exposes `enable_k9` (refuses without
detection unless `--force`), `disable_k9` (flips `enabled = false`,
preserves section), `status`, and a `k9_state` helper used by
`commands::status`. `commands::init::run` (and `run_with_backup`)
call `apply_k9_detection_on_init` after fresh config creation; existing
configs are left alone. `StatusSummary` and the MCP `StatusToolResponse`
gained `k9_detected`, `k9_enabled`, `k9_agent_docs_path`, `k9_drift`.
CLI gained a `memhub integrations` subcommand with `status`,
`enable-k9 [--agent-docs-path <PATH>] [--force]`, and `disable-k9`
variants. Added 12 integration tests in `tests/integrations.rs` plus 5
unit tests in `src/config/integrations.rs` and 1 new MCP-side test for
the new status fields. Updated README with a "K9 Claude Framework
integration" section and bumped roadmap to introduce Milestone 5 and
shift the speculative bucket to Milestone 6+. Verified with `cargo
fmt`, `cargo build`, and `cargo test` (88 tests across all suites).

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

## Open questions

- Should `M5-002` ship as a memhub-side contract doc first
  (`docs/reference/k9-wrap-up-contract.md`) and let the K9 repo land
  the consumer changes asynchronously, or should both land together?
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
- Should `memhub migrate` remain implicit-on-open or become explicit
  once external users adopt the tool?
- Should `decisions` carry a derived confidence too, and what would the
  re-verification trigger be without a `sessions`-based reference
  signal in place?
