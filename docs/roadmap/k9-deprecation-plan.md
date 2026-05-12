# K9 framework deprecation plan

Status: Open planning doc. No PRD or roadmap non-goals are modified
by this document. It captures the direction under consideration, the
artifacts the transition will require, and the explicit decision
points that have to land before deprecation can ship.

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
  non-goals in `docs/roadmap/k9-integration.md` will be revisited
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

Phase 2 of the memhub-primary evaluation named this the long pole.
With K9 retired, memhub still needs to produce markdown humans, PRs,
and cross-machine collaborators can read without opening the DB. The
inversion is the key: markdown becomes an *output* of memhub, not an
*input*.

Open questions: which files to emit (mirror K9's four-file shape, or
re-shape entirely?); who triggers regeneration (CLI command, hook
after each write, on-demand `memhub render`?); conflict semantics if
a human edits the emitted file (refuse to overwrite, three-way merge,
DB-wins-with-backup?); where the emitted files live (in-repo,
`.memhub/`, or both?).

Artifact: `docs/roadmap/memhub-render-design.md` (does not exist
yet). This is the next design doc on the path.

### 2. PRD §2 and the K9-integration non-goals

PRD §2 currently reads markdown as the entry point. Under the
deprecation direction it inverts. The PRD addendum (or a numbered
revision) has to spell this out — silently reinterpreting the prior
wording is the wrong path.

Similarly the non-goals in `docs/roadmap/k9-integration.md` ("no
bidirectional sync," "no DB mapping of `project_state.md` /
`project_arch.md`") were boundary conditions for the K9-stays-primary
era. Each needs explicit re-decision under the deprecation framing.

Artifact: PRD addendum or revision; non-goal carve-outs in the
roadmap doc. Likely downstream of slice 1 since the render design
will surface which non-goals actually need to move.

### 3. `project_state.md` and `project_arch.md` as DB content

Currently both files live outside any DB representation by design.
Under K9-primary that was correct — the markdown narrative is more
information-dense than a tabular schema can be without losing prose.
Under memhub-primary, narrative still lives somewhere; the question
is whether it lives in DB columns (decomposed into discrete fields)
or as a single durable-text blob (essentially the same markdown,
persisted in the DB instead of the filesystem).

Open questions: does `state` decompose into "currently building" +
"next up" + "open questions" columns, or stay as a single free-form
column? Same for `arch`. How does slice 1's render step reconstruct
the original markdown shape from whichever representation we pick?

Artifact: probably folded into the `memhub render` design doc unless
the schema impact is large enough to earn its own slice.

### 4. Wrap-up routing brain

K9's `/wrap-up.md` is the only place today that orchestrates the
human-approval gate between session work and durable state. Under
deprecation, that responsibility migrates. Two routes:

- **Into memhub** as a new CLI surface (`memhub wrap-up` walks the
  same approval flow K9 does today, calling the existing mutating
  commands internally).
- **Into a thin shell wrapper** that lives in the user's dotfiles or
  a small companion tool, with memhub staying dumb durable storage.

Open questions: which preserves the "memhub stays boring" PRD
principle? Where do slash-command definitions live without K9 (a
small `agents/` directory in the user's home? Inside memhub as
embedded prompts? Outside memhub entirely as user-owned dotfiles?)?

Artifact: design slice TBD, downstream of slices 1-3.

## Sequencing

Sequencing is not committed. Two plausible orderings:

- **Render-first.** Slice 1 ships before any PRD edits; the render
  doc itself argues for the PRD addendum based on what it needs.
  Lower per-slice risk because each is small and each can be
  evaluated on its own merits before the next starts.
- **PRD-first.** PRD addendum lands first to remove the contradiction
  between PRD §2's current wording and the deprecation direction.
  Each downstream slice then proceeds without the "we're violating
  the PRD" footnote. Higher up-front cost but cleaner narrative for
  anyone reading the roadmap mid-transition.

The two-option framework applies when the first design slice is
actually started.

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
- `docs/roadmap/memhub-primary-evaluation.md` Phase 3 ("three
  outcomes — render, marginal, poor") no longer routes between
  open alternatives. The deprecation direction commits to the
  render-design route. That document becomes a historical artifact
  rather than an open evaluation; future readers should treat it as
  the *origin* of the deprecation direction, not as a live decision
  surface.
