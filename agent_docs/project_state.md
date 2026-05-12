# Project State

Last updated: 2026-05-12

## Currently building

Between tasks. By PRD §16 milestones memhub is at v1: Milestones 1–4
are complete and the memhub side of Milestone 5 (K9 framework interop)
is shipped, including the PRD-§12 and PRD-§17 surfaces (`memhub stats`
and the `log_session_note` MCP tool + `memhub note list` CLI).
Continuous confidence decay (PRD §11.4) was explicitly dropped from
v1 (see `project_decisions.md`). The README's onboarding surface is
also now fully fleshed out — manual install, agent-driven install,
and end-to-end typical-workflow narratives for both K9 and
standalone tracks. The only named remaining slice lives outside this
repo: the K9 `/wrap-up.md` consumer edit that calls into the v1
contract end-to-end (gate + read + mutate).

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

2026-05-12 - Extended the README onboarding surface with two
follow-on doc-only additions. `f382a50` added an "Install with
Claude Code" subsection with a copy-pasteable prompt that walks an
agent through clone → `cargo build --release` → PATH symlink →
`memhub init` / `status`, with explicit guardrails against modifying
repo files beyond `.memhub/`. `a7060aa` added a "Typical Workflow"
section sitting between Quick Start and Backup and Restore:
narrates an end-to-end working session on each track (orient →
record-as-you-go → close out → maintain for standalone; mid-session
MCP plus `/wrap-up` shell-out for K9), framing memhub as a
"deliberate notebook" without K9 and as the structured half of a
Markdown + database transaction with K9. No code, schema, or
contract changes.

2026-05-12 - Restructured the README install/usage sections into
parallel "without K9" and "with K9" tracks (`c27ed69`). Added a
Prerequisites section (Rust 1.85+, git CLI, optional K9 prereq),
split Install into a shared `cargo build --release` step plus Step 2a
"init without K9" / Step 2b "init with K9 already installed",
documented the auto-detection + `enable-k9` / `disable-k9` toggle
path, and split Quick Start into "Usage without K9" (manual CLI
flows) and "Usage with K9" (the `/wrap-up` shell-out using
`--json --actor k9:wrap-up`, linked to
`docs/reference/k9-wrap-up-contract.md` as v1 truth). Switched every
example from `cargo run --` to a `memhub` binary on PATH and added a
"put it on PATH" install step (copy or symlink to `~/.local/bin/`)
so the K9 shell-out actually resolves the binary. Fixed a stray
`enable k9` (space) → `enable-k9` typo in the existing K9 deep-dive
section. Doc-only; no code or schema changes.

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
