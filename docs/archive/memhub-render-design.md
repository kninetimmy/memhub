# `memhub render` design

Status: Open design doc. The deprecation direction is committed
([`docs/archive/k9-deprecation-plan.md`](k9-deprecation-plan.md)) but
the render slice is not yet implemented. This document picks the
output shape, surfaces alternatives on the secondary questions
(trigger, conflict semantics, location, state/arch storage), and
recommends a default for each. PRD §2 and the
`docs/archive/k9-integration.md` non-goals are not modified by this
document; the addendum that revises them is a separate slice
sequenced after this one.

## Why render exists

Under K9-primary, markdown was the input — humans wrote it, agents
read it, the DB (when present) was a cache. Under memhub-primary the
DB is the source of truth and humans can no longer read project state
by opening a file. `memhub render` is the bridge: it materializes the
DB into committed markdown so PR review, cross-machine clones, and
casual `cat` still work without an installed CLI.

This is the load-bearing slice from the deprecation plan. PRD §2's
inversion ("markdown becomes an output, not an input") is downstream
of the choices made here.

## What this doc decides

- The rendered output shape: a memhub-native two-file layout, not a
  K9 four-file mirror.
- How `state` and `arch` narrative content gets persisted in the DB
  so render has something to read.
- The default trigger model, conflict semantics, and on-disk
  location, each surfaced as a two-option choice with a
  recommendation.
- That `bootstrap-k9` stays as the priming path for existing K9
  repos until the transition ends. It is not the steady state; it is
  the ramp.

## What this doc does not decide

- The PRD §2 addendum wording. That slice runs after render lands so
  the addendum can describe what shipped, not what was planned.
- The wrap-up routing brain (slice 4 of the deprecation plan). Where
  it lives and what it calls is its own design.
- Whether `bootstrap-k9` ever deprecates. It stays useful as long as
  K9 repos exist that haven't migrated.
- Render's interaction with `memhub serve` MCP tools. Render is a
  CLI surface in v1; an MCP tool over it can come later if a real
  agent workflow asks for one.

## 1. Output shape — memhub-native two-file

Render emits two files into the project's rendered-docs directory:

```
agent_docs/
├── PROJECT.md          # narrative: state + architecture
└── PROJECT_LEDGER.md   # structured append-only: decisions + backlog + facts + recent activity
```

The split is along the natural seam in the DB itself:

- **PROJECT.md** is prose. It reads top-to-bottom and answers "what
  is this project, what is it doing right now, what is open." Sources:
  the durable `state` blob, the durable `arch` blob, and the most
  recent `session_notes` entries (capped). Updates are full
  rewrites — narrative does not append cleanly.
- **PROJECT_LEDGER.md** is tabular and append-only. Sources:
  `decisions`, `tasks`, `facts`, `commands`, and a windowed slice of
  `writes_log` for "recent activity." Each row carries provenance
  (id, actor, source, confidence where applicable) so a reader can
  reason about trust without re-querying the DB.

### Why two files, not one or four

A single `PROJECT.md` would be the smallest diff target but conflates
narrative-cadence content (rewritten on every state change) with
ledger-cadence content (appended on every decision/task/fact). That
mixing produces noisy diffs and pushes file size up faster than is
useful.

A K9-style four-file shape (state / arch / decisions / backlog) is
the comfortable default because it matches what already exists, but
it inherits K9's split decisions (state vs arch as separate concerns;
decisions and backlog as siblings) without earning them in the
memhub-primary world. The two-file shape is the cleanest break from
K9 that still keeps narrative and ledger from grinding against each
other in PR diffs. It is also visibly different in filenames, which
matters during transition: a repo can carry both K9 files and memhub
rendered files in `agent_docs/` without any name collision.

### Header convention

Every rendered file leads with a fixed header that is machine-readable:

```markdown
<!-- memhub:rendered -->
<!-- DO NOT EDIT. Generated from .memhub/project.sqlite. -->
<!-- To change content, use memhub CLI; then re-run `memhub render`. -->
<!-- Generated at: 2026-05-12T19:44:35Z by memhub <version> -->
```

The marker comment is the contract render uses to recognize its own
output on subsequent runs (see §4 conflict semantics).

## 2. State and architecture in the DB

Render needs `state` and `arch` content somewhere readable. Two
options:

- **A. Single durable-text blobs.** Add two tables —
  `project_state(id, body, updated_at, updated_by)` and
  `project_arch(id, body, updated_at, updated_by)` — each with at
  most one current row plus optional history rows for audit. The
  body is markdown prose. CLI surface:
  `memhub state set` / `memhub state show` /
  `memhub arch set` / `memhub arch show`.
- **B. Decompose into structured columns.** `project_state` becomes
  `(currently_building, next_up, open_questions, last_session)`;
  `project_arch` becomes `(purpose, stack, layout, subsystems,
  invariants, gaps)`. Render reassembles the markdown from the
  pieces.

**Recommendation: A.** Narrative resists clean decomposition without
losing the prose flow that makes a `project_state.md` useful in the
first place. The columns in option B are guesses at structure that
might not survive contact with how state actually evolves.
Decomposition can ship later as a migration if querying patterns
demand it; going the other direction (decomposed → blob) is harder.

A is also smaller surface area now: two tables, two pairs of
`set` / `show` commands, and render reads a single column.

## 3. Render trigger

When does render actually fire?

- **A. On-demand only.** `memhub render` is an explicit CLI command.
  The user (or a wrap-up flow) runs it when they want fresh output.
  No auto-fire on individual writes.
- **B. Auto-fire after every mutating write.** Mirrors the existing
  `auto_sync_md` config flag — every `decision add`, `task add`,
  `fact add`, etc. triggers a render of the affected file.

**Recommendation: A, with a config flag for B as opt-in later.** The
managed-block `sync-md` is small (5–10 lines) and cheap to rewrite
on every write. Render is bigger output and produces meaningful PR
diff churn — auto-firing on every fact add would clutter `git
status` mid-session. The natural cadence is "render at session end,"
which is a wrap-up step. Reserve `auto_render = true` in
`config.toml` as a future opt-in for users who want it.

## 4. Conflict semantics on human edits

What happens when a human edits a rendered file directly between
render runs?

- **A. Refuse on divergence.** Render reads the existing file's
  body, recomputes what render *would* have produced last time
  (deterministic from the render-time snapshot stored in the file's
  trailer comment), and aborts if they differ. User must either
  revert their edits or pass `--force` to overwrite.
- **B. DB-wins-with-backup.** Render unconditionally overwrites,
  but first copies the prior file under
  `.memhub/backups/rendered/<timestamp>/PROJECT.md` so accidental
  human edits are recoverable.

**Recommendation: B.** Rendered files are generated artifacts. A
human editing one is a category error — the change won't survive
the next render and isn't reflected in the DB. Refusing the render
(option A) punishes the user for a mistake the file's own header
already warned them about; backup-and-overwrite (option B) preserves
the edit content if they need it without blocking the workflow. The
backup directory mirrors the existing `sync-md` markdown backup
convention under `.memhub/backups/markdown/`.

Edge case: if the user's intent was actually to update DB content
through the rendered file, they should `memhub decision add` /
`task add` / `state set` instead. Render does not parse markdown
back into the DB — that path stays a non-goal.

## 5. Output location and gitignore behavior

**Update, 2026-05-14:** The original recommendation below has been
superseded by the machine-local rule. The default render output is now
`.memhub/rendered/`, which is covered by the default `.memhub/`
gitignore entry. `memhub init` also ignores the legacy
`agent_docs/PROJECT.md` and `agent_docs/PROJECT_LEDGER.md` paths so
old configs do not keep generating Git churn. Repos that want a
committed render view can still opt in with `[render].output_dir` and
matching `.gitignore` changes.

Where do the rendered files live, and are they committed?

- **A. In-repo under `agent_docs/`, committed.** Same directory K9
  used. Files travel with the repo so collaborators and PR reviewers
  see project state without installing memhub. `.memhub/` stays
  gitignored as today; only the rendered output is in git.
- **B. Under `.memhub/rendered/`, gitignored.** Stays alongside the
  DB. User opts in to commit by editing `.gitignore` themselves.
  Default keeps `.memhub/` self-contained.

**Original recommendation: A. Superseded by the 2026-05-14 update.**
In practice, committing rendered output from multiple machines created
avoidable Git conflicts because the files are generated from
machine-local DB state. The current default chooses B: render output
stays local unless a repo explicitly opts in to tracked markdown.

The render directory is configurable in `.memhub/config.toml` for
users who prefer a different layout:

```toml
[render]
output_dir = ".memhub/rendered"   # default
```

## 6. Bootstrap-k9 in the new world

`bootstrap-k9` stays. Its role is unchanged: a one-shot, refuses-on-
non-empty-target priming command that parses K9 markdown into DB
rows. It is the *ramp* into memhub-primary, not the steady state.

After bootstrap, render takes over. The flow for an existing K9 repo:

1. `memhub init` (creates `.memhub/`)
2. `memhub integrations bootstrap-k9` (one-shot import of existing
   `agent_docs/project_*.md`)
3. `memhub render` (emits `PROJECT.md` and `PROJECT_LEDGER.md`)
4. Decide whether to delete the old `project_*.md` files (manual
   cleanup; out of scope for memhub itself).
5. Steady state: edit through CLI, render at session end.

For greenfield repos with no K9 history, steps 2 and 4 don't apply.
`memhub init` followed by `memhub state set` / `memhub arch set` /
`memhub decision add` etc., then `memhub render`.

The `bootstrap-k9` parser already handles K9-canonical conventions
after M6-005. It does not need to know about the new render shape;
the bootstrap path imports into the same `decisions` / `tasks` /
`facts` tables render reads.

## 7. Sequencing within this slice

The render slice is itself a sequence. Order:

1. **Schema for state and arch.** Two new tables per §2 option A,
   migrations, `memhub state set|show` and `memhub arch set|show`
   CLI surface. No render yet — durable storage first.
2. **Render core.** `memhub render` walks the DB, produces
   `PROJECT.md` and `PROJECT_LEDGER.md` per §1. Backup-and-overwrite
   conflict handling per §4. Configurable output dir per §5.
3. **Render at wrap-up.** Wire render into the wrap-up routing brain
   (slice 4 of the deprecation plan) so it fires automatically at
   session end. Until that brain exists, render is invoked manually
   or by a thin shell wrapper.
4. **PRD §2 addendum.** With render shipped and behaving, revise the
   PRD to describe the actual end state. Revisit the
   `k9-integration.md` non-goals in the same pass.

Each step is independently useful. After step 1, state and arch can
be queried even without render. After step 2, a user can render and
commit even without a wrap-up brain. After step 3, the loop closes.
Step 4 is documentation hygiene.

## 8. Out of scope for this slice

- Rendering into formats other than markdown (HTML, JSON, etc.).
  Markdown is the only target.
- Reverse-direction parsing (markdown edits flowing back into DB).
  Stays a non-goal.
- Diff-ing render output against the DB without writing the file
  (`memhub render --check`). Useful for CI but not in the v1 cut;
  add later if a real workflow asks.
- Rendering arbitrary user-defined templates. The two-file shape is
  the contract; users who want a different shape file an issue and
  argue for it.
- Render as an MCP tool. CLI only in v1.
- Splitting `PROJECT_LEDGER.md` if it grows large enough to be
  unwieldy. Defer until the file actually hurts in real use.

## 9. Open questions

- Does `PROJECT_LEDGER.md` include the full decisions / tasks /
  facts content, or summary rows with a pointer to a paginated
  detail file? Probably full content until a real repo's ledger
  hits a size where that breaks down.
- Does render include `commands` and `writes_log`? They're durable
  but feel like CLI-query content rather than narrative. Tentative:
  include a windowed "recent activity" section sourced from
  `writes_log`; skip `commands` until a workflow asks.
- Should `memhub render` also rewrite the existing `sync-md`
  managed block in `CLAUDE.md` / `AGENTS.md`, or stay separate?
  Tentative: stay separate; the managed block predates render and
  serves a different purpose (agent-facing summary, not human-
  facing narrative).
- Is the `<!-- memhub:rendered -->` marker strict enough for the
  conflict-detection trailer, or does it need a content hash to
  catch in-place edits that preserved the marker comment?
- Does `bootstrap-k9` learn to also write a starter `state` and
  `arch` blob from the K9 narrative files, or does the user run
  `state set` / `arch set` manually after bootstrap? Tentative:
  manual, because bootstrap already does enough and narrative
  parsing is fragile.
