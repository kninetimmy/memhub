# Project State

Last updated: 2026-05-12

## Currently building

Dogfooding the new memhub-native `/wrap-up` skill in this repo. Render
slice fully shipped (steps 1 + 2): durable `project_state` /
`project_arch` tables, `memhub state|arch set|show|history` CLI, and
`memhub render` emitting `agent_docs/PROJECT.md` +
`agent_docs/PROJECT_LEDGER.md` per `docs/roadmap/memhub-render-design.md`.
Wrap-up slice steps 1 + 2 shipped: `memhub note add` CLI plus
project-level `.claude/commands/wrap-up.md` per
`docs/roadmap/wrap-up-design.md`. First-fire discovery during this
session: the project-level skill did **not** override the user-level
K9 `/wrap-up` — that's why this session is being wrapped up via the
K9 flow. The override gap needs investigation before the memhub-native
skill can take precedence in this repo.

## Next up

1. Investigate the project-level slash command override gap (new
   `M7-001`). Likely a Claude Code skill resolution rule, file
   placement convention, or naming collision — empirical question
   that gates real wrap-up dogfooding.
2. Migrate memhub's own `agent_docs/*.md` from K9 markdown to the
   memhub-native rendered files (new `M7-002`). Once shipped, this
   repo runs the full memhub-primary loop and `M6-004` (the K9-
   canonical dogfood migration) closes as obviated.
3. PRD §2 + `docs/roadmap/k9-integration.md` non-goal addendum,
   after the override gap is fixed and a real `/wrap-up` against
   memhub-primary runs end-to-end.

## Last session

2026-05-12 — Shipped two design docs and four implementation
commits across the K9 deprecation track. Render-design committed as
`c1becc5` (memhub-native two-file shape locked, four secondary
questions resolved with recommendations). Render slice step 1
(`2757a0a`): migration `0007_project_narrative.sql`,
`commands::narrative`, `memhub state|arch set|show|history` CLI,
8 tests. Step 2 (`c3fbef0`): `src/render/`, `commands::render`,
`memhub render` CLI, `[render] output_dir` config, backup-and-
overwrite conflict semantics reusing lifted `sync_md` helpers,
6 tests. Wrap-up-design committed as `a2b6606` (Claude Code skill
locked over CLI subcommand). Wrap-up step 1 (`5037033`):
`memhub note add` with shared `resolve_text_input` helper, 6 tests.
Step 2 (`588168b`): authored `.claude/commands/wrap-up.md`. 164
tests green at session end. Override-gap discovery during dogfood
remains unresolved.

2026-05-12 — Cleaned Free-AI-SSD's mojibake at the source (465
`â€"` → `—` replacements). Shipped M6-005 as `675f614`; opened
`docs/roadmap/k9-deprecation-plan.md` (`29cdbef`) and closed
M6-006 as won't-fix-deprecating-K9.

## Open questions

- Why didn't `.claude/commands/wrap-up.md` override the user-level
  `~/.claude/commands/wrap-up.md` when invoked inside the memhub
  repo? (Tracked as `M7-001`.)
- Does M6-004 close as obviated (since memhub renders its own
  narrative now) or land as a final K9-canonical pass before
  M7-002? Tentative: close as obviated; ship M7-002 instead.
- Should `MEMHUB_ACTOR` env var be added as an alternative to the
  `--actor` flag for skills that fan out many CLI calls?
- Should `enable-k9 --agent-docs-path` accept any path and create the
  marker file as part of an explicit "set up K9 here" flow?
- Should `FACT_STALE_AFTER_DAYS` become a config knob?
- Should memhub ship a future `gc` slice for ingested denied paths?
- Which additional `clientInfo.name` values appear in real handshakes?
- Should memhub ship a `cargo install`-able crate (or homebrew tap)?
- Should `memhub migrate` become explicit once external users adopt
  the tool?
- Should a `v2` export format be introduced to include `session_notes`?
