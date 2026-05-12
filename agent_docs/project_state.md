# Project State

Last updated: 2026-05-12

## Currently building

Between tasks. By PRD §16 milestones memhub is at v1: Milestones 1–4
are complete and the memhub side of Milestone 5 (K9 framework interop)
is shipped, including the PRD-§12 and PRD-§17 surfaces (`memhub stats`
and the `log_session_note` MCP tool + `memhub note list` CLI).
Continuous confidence decay (PRD §11.4) was explicitly dropped this
session. The only named remaining slice lives outside this repo: the
K9 `/wrap-up.md` consumer edit that calls into the v1 contract
end-to-end (gate + read + mutate).

## Next up

1. Coordinate the K9 repo `/wrap-up.md` consumer change with whoever
   owns the K9 repo. With `M5-003` shipped on the memhub side, K9 can
   stay CLI-only end-to-end; their slice is mechanical and lives
   outside this repo.
2. Decide whether MCP needs broader indexed retrieval over facts,
   tasks, command history, or session notes beyond the current narrow
   paths.
3. If session notes start carrying durable value, bump the export
   format to `v2` and include them. Until then they're intentionally
   lost on export/import.

## Last session

2026-05-12 - Shipped four commits closing out PRD-named surfaces.
`b103792` (`M5-003`) added `--json` to `memhub review list` and
`review show`, mirroring the MCP `PendingWriteToolRecord` shape and
amending the K9 v1 contract additively (no v2 bump). `b6cc920`
refreshed the README and roadmap docs to reflect actual shipped state
after they'd drifted into reading like mid-M4. `eedc973` (`M5-004`)
added `memhub stats [--window 7d|30d|90d|all] [--json]` covering
totals, windowed write activity from `writes_log`, pending-write
review rate, top commands by run count, and recent verified facts;
PRD §17's "simple read counter" was explicitly deferred and the
deviation is surfaced in both human and JSON outputs. `072c087`
(`M5-005`) added a new `session_notes` table (migration `0006`),
the `log_session_note` MCP tool, and `memhub note list [--limit]
[--actor] [--since-days] [--json]` — write-only scratch with no
promotion path and intentional omission from the v1 `memhub export`
format. Test count moved 99 → 119 across this session. Continuous
confidence decay (PRD §11.4) was dropped entirely after planning.

2026-05-12 - Completed `M5-002`. Shipped
`docs/reference/k9-wrap-up-contract.md` (v1 contract: sequencing,
gating with `check-k9`, JSON schemas per mutating command, actor
convention, exit codes, audit-trail query, explicit non-goals). Added
`memhub integrations check-k9` subcommand returning 0/1 with empty
stdout, gracefully handling missing `.memhub/` via silent exit 1.
Threaded a new `actor: &str` parameter through `fact::add`,
`decision::add`, `task::add`, `task::done`, `review::accept`,
`review::reject`, and `review::mark_status`. CLI gained `--json` and
`--actor` flags on the six mutating commands. Added 11 integration
tests in `tests/k9_contract.rs`.

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
- Should a `v2` export format be introduced to include `session_notes`,
  or do notes stay scratch-only and lost on export/import indefinitely?
