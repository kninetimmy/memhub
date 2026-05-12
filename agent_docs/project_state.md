# Project State

Last updated: 2026-05-12

## Currently building

Between tasks after `M5-003` (memhub side). The K9 wrap-up read path
is now locked into the v1 contract: `memhub review list` and `memhub
review show` accept `--json` and emit shapes that mirror the MCP
`PendingWriteToolRecord` field set. `review list --json` returns
`{"status": <filter|null>, "pending_writes": [...]}`; `review show
--json` returns a single record. Read surfaces are intentionally
side-effect-free — no `--actor`, no `writes_log` rows. The contract
doc gained a "Read surfaces" section before "Mutating commands" and
the sequencing step 3 now points directly at `review list --json`.
The amendment is additive within `v1` (no `v2` bump). No K9 repo
edits in this slice — the K9 `/wrap-up.md` consumer change remains
triaged separately and can now pick the CLI-only path end-to-end.

## Next up

1. Coordinate the K9 repo `/wrap-up.md` consumer change with whoever
   owns the K9 repo. With `M5-003` shipped on the memhub side, K9 can
   stay CLI-only end-to-end (gate + read + mutate); their slice is
   mechanical.
2. Decide whether MCP needs broader indexed retrieval over facts,
   tasks, or command history beyond the current narrow paths.
3. Revisit the open `MEMHUB_ACTOR` env-var question once a real K9
   `/wrap-up` consumer exists and we know how many CLI invocations
   per session it actually fans out to.

## Last session

2026-05-12 - Completed `M5-003` (memhub side). Added `--json` to
`memhub review list` and `memhub review show` in `src/cli/mod.rs`,
backed by a small `pending_write_record_to_json` helper that mirrors
the MCP `PendingWriteToolRecord` shape (`id, kind, status, actor,
actor_raw, rationale, payload_json, provenance_json, created_at,
reviewed_at`). `payload_json` and `provenance_json` stay as nested
JSON strings to preserve the durable representation byte-for-byte.
`review list --json` envelopes the rows as `{"status": <filter or
null>, "pending_writes": [...]}` — `null` only when `--status all`
is used. Read surfaces accept no `--actor` flag and write no
`writes_log` rows. Updated `docs/reference/k9-wrap-up-contract.md`
with a new "Read surfaces" section ahead of "Mutating commands",
amended step 3 of "Sequencing" to point directly at `review list
--json`, and added a `v1`-additive version-history entry (no `v2`
bump). Added 4 subprocess tests in `tests/k9_contract.rs`:
`review_list_json_emits_contract_shape`,
`review_list_json_filters_by_status` (verifies `--status all` produces
`status: null`), `review_show_json_emits_contract_shape`,
`review_show_json_missing_id_exits_nonzero`. README gained a
"Machine-readable read surfaces" bullet in the K9 contract subsection.
Verified with `cargo fmt`, `cargo build`, and `cargo test` (103 tests
across all suites, up from 99).

2026-05-12 - Completed `M5-002`. Shipped
`docs/reference/k9-wrap-up-contract.md` (v1 contract: sequencing,
gating with `check-k9`, JSON schemas per mutating command, actor
convention, exit codes, audit-trail query, explicit non-goals). Added
`memhub integrations check-k9` subcommand returning 0/1 with empty
stdout, gracefully handling missing `.memhub/` via silent exit 1.
Threaded a new `actor: &str` parameter through `fact::add`,
`decision::add`, `task::add`, `task::done`, `review::accept`,
`review::reject`, and `review::mark_status` (plus the internal
`fact::add` / `decision::add` calls from inside `review::accept` so
the actor propagates to durable writes). Added a `commands::DEFAULT_ACTOR`
constant (`cli:user`), `commands::MAX_ACTOR_LEN` (64), and
`commands::validate_actor` helper. CLI gained `--json` and `--actor`
flags on the six mutating commands; JSON output is rendered via
`serde_json::json!` and replaces the human-readable line when set.
Updated every existing internal and test caller (about 25 sites) to
pass `"cli:user"` explicitly. Added 11 integration tests in
`tests/k9_contract.rs` that exercise the actual CLI binary as a
subprocess — verifying JSON shape per command, `--actor` flowing to
`writes_log`, actor validation (empty / >64 chars), and `check-k9`
exit codes across all four states (enabled, missing section,
disabled, not initialized). README gained a "K9 `/wrap-up` shell-out
contract" subsection. Verified with `cargo fmt`, `cargo build`, and
`cargo test` (99 tests across all suites).

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
- Should `memhub migrate` remain implicit-on-open or become explicit
  once external users adopt the tool?
- Should `decisions` carry a derived confidence too, and what would the
  re-verification trigger be without a `sessions`-based reference
  signal in place?
