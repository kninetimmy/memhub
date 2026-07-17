# K9 framework deprecation plan

Status: All four load-bearing slices have shipped. This document
remains as the planning anchor and historical record of the
direction; ongoing PRD-level changes live in
[`docs/reference/memhub-prd-deprecation-addendum.md`](../reference/memhub-prd-deprecation-addendum.md).
PRD §2 and the `docs/archive/k9-integration.md` non-goals have been
formally revisited (see the addendum and the revised non-goals
section in `k9-integration.md`).

## Direction

The K9 Claude Framework is on a deprecation track. The end state has
memhub as the single durable source of project memory; K9's slash
commands, markdown templates, and routing brain are retired.

The direction is committed in intent. The implementation slices below
are not — each ships as its own design doc with explicit reasoning,
and the existing PRD non-goals around markdown stay in force until
those design docs propose specific changes the user accepts.

## What this document does and does not do

Decides:

- That the deprecation direction is real and load-bearing — future
  design slices should be framed under "memhub becomes primary," not
  under "memhub stays complementary."
- That PRD §2 ("markdown files stay as the entry point") and the
  non-goals in `docs/archive/k9-integration.md` will be revisited
  under explicit reasoning, not silently reinterpreted.

Does not decide:

- A date or sequence for deprecation. Slices land when they're
  individually designed.
- What happens to existing K9 repos that don't migrate.
- Whether the K9 repository stops shipping releases (that's a K9-repo
  decision, not memhub's).
- The fate of any specific PRD non-goal — each is revisited in its
  own slice with its own argument.

## Load-bearing slices

### 1. `memhub render` — markdown emission from durable state

**Status: shipped (`c1becc5`, `2757a0a`, `c3fbef0`).**

The design doc at [`docs/archive/memhub-render-design.md`](memhub-render-design.md)
locked the output shape (memhub-native two-file:
`PROJECT.md` for narrative + `PROJECT_LEDGER.md` for the ledger) and
resolved the four secondary questions: trigger is on-demand only,
conflict semantics are DB-wins-with-backup, output lives at the
configurable `[render].output_dir` (default changed to
`.memhub/rendered/` on 2026-05-14), and state/arch persist as single
durable-text blobs in new DB tables. Migration
`0007_project_narrative` shipped both tables together. This slice
subsumed slice 3 (state/arch as DB content) since the render shape
required the DB representation to exist first.

### 2. PRD §2 and the K9-integration non-goals

**Status: shipped (this commit).** Addendum lives at
[`docs/reference/memhub-prd-deprecation-addendum.md`](../reference/memhub-prd-deprecation-addendum.md);
revised non-goals live inline at the top of
[`k9-integration.md`](k9-integration.md). The PRD itself stays
verbatim per the project guardrail. The four prior non-goals were
explicitly re-decided: reverse-direction sync, general
`k9 import/export/sync`, and managed-block writes inside `agent_docs/`
all stay in force (with clarification on the third); the DB mapping
of `project_state.md` / `project_arch.md` was overturned, with
migration `0007_project_narrative` providing the new tables.

### 3. `project_state.md` and `project_arch.md` as DB content

**Status: shipped — folded into slice 1.** As predicted in the
original plan, this question was resolved by the render design.
Both files map to single durable-text blob tables (`project_state`
and `project_arch`), append-only history, with `memhub state set` /
`memhub arch set` / `show` / `history` as the CLI surface.
Decomposed-columns option was rejected; rationale captured as a
durable decision dated 2026-05-12 in `agent_docs/project_decisions.md`
and in `docs/archive/memhub-render-design.md` §2.

### 4. Wrap-up routing brain

**Status: shipped (`a2b6606`, `5037033`, `588168b`, `103eea0`,
`591832f`).** Design doc at
[`docs/archive/wrap-up-design.md`](wrap-up-design.md) locked the
routing brain as a Claude Code project-level slash command at
`.claude/commands/wrap-up.md`, not a `memhub wrap-up` CLI
subcommand. Only new CLI primitive needed was `memhub note add`
(everything else already shipped via the render slice and prior
work). M7-001 (slash-command override gap) closed by renaming the
user-level K9 `wrap-up.md` to `wrap-up-k9.md` after empirical
discovery that personal-overrides-project is documented Claude Code
behavior; rename-the-collision is the durable pattern for future
memhub-aware skills (`/init-project`, `/check-init` if shipped).
M7-002 (memhub-primary migration of this repo) closed inline during
the dogfood wrap-up.

## Sequencing — what actually happened

The render-first ordering was followed. Slice 1 (render) shipped
first, then slice 4 (wrap-up) including its dogfood (M7-001 + M7-002),
then slice 2 (the PRD addendum) once render and wrap-up were in real
use. Slice 3 was folded into slice 1 as predicted. Each slice landed
as its own design doc + implementation commits + tests, evaluated
on its own merits before the next started. The render-first
sequencing kept per-slice risk low and let each slice argue its own
case under explicit reasoning rather than retroactive justification.

## Out of scope for this document

- Whether the K9 repository stops shipping releases or stays as a
  legacy artifact. K9 is owned separately.
- Migration tooling for users who want to leave K9 but not adopt
  memhub. They get the same `agent_docs/*.md` files they had before
  and that's it.
- `bootstrap-k9` itself. Once the transition ends it can deprecate
  too; until then it stays as the supported priming path.

## Status of related items

- `M6-006` (parser-side mojibake branch) closed as
  "won't-fix-deprecating-K9." The source-cleanup approach already
  shipped on Free-AI-SSD. Future K9-to-memhub bootstrap runs apply
  the same cleanup; no new parser surface area.
- `M6-004` (memhub's own `agent_docs` migration to K9 canonical)
  stays open but may be obviated by slice 1 — once memhub emits its
  own narrative via `memhub render`, the "dogfood K9 canonical in
  memhub's `agent_docs`" goal disappears entirely.
- `docs/archive/memhub-primary-evaluation.md` Phase 3 ("three
  outcomes — render, marginal, poor") no longer routes between
  open alternatives. The deprecation direction commits to the
  render-design route. That document becomes a historical artifact
  rather than an open evaluation; future readers should treat it as
  the *origin* of the deprecation direction, not as a live decision
  surface.
