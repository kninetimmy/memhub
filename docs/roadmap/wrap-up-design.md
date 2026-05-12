# `wrap-up` design (memhub-primary)

Status: Open design doc. Slice 4 of the K9 deprecation plan
([`docs/roadmap/k9-deprecation-plan.md`](k9-deprecation-plan.md)).
Render slice steps 1 and 2 are shipped (`2757a0a`, `c3fbef0`); the
narrative storage and markdown emission paths the wrap-up brain will
call into both exist now. This doc commits the routing-brain
direction (Claude Code skill, not a `memhub wrap-up` CLI subcommand),
surfaces the secondary questions as two-option choices, recommends a
default for each, and sequences implementation. PRD §2 and the
`docs/roadmap/k9-integration.md` non-goals are not modified by this
document.

## Why a wrap-up brain exists at all

K9 today owns the human-approval gate between session work and
durable state. The agent does work, things accumulate (decisions
made, tasks discovered, facts learned, narrative drift in what the
project is currently doing), and at session end `/wrap-up`
orchestrates a single approval that lands all of it. Without a brain,
each writing primitive (`memhub decision add`, `memhub task add`,
etc.) is a separate decision point — wrap-up is what bundles them so
the user reviews one draft, not twenty.

Under K9 deprecation, that responsibility migrates. This doc decides
where it migrates *to*.

## What this doc decides

- The routing brain is a Claude Code skill, not a `memhub wrap-up`
  CLI subcommand. (Driven by the durable UX-mimic-K9 preference
  captured in slice 4 of the deprecation plan: typing `/wrap-up` at
  session end is part of the value the user wants preserved.)
- Where the skill file lives in the filesystem.
- What new memhub primitives the skill requires that don't exist yet.
- How the skill detects "what changed since last session."
- The scope of K9-vocabulary mirroring in v1 (`/wrap-up` only;
  `/init-project` and `/check-init` are deferred follow-ups under the
  same pattern).

## What this doc does not decide

- The exact prompt text inside the skill. Skill prompts iterate
  cheaply and shouldn't be locked by a roadmap doc.
- Whether a `memhub-skills` companion repo eventually carries
  third-party skills. Out of scope for this slice.
- The PRD §2 addendum wording. That follows once render + wrap-up
  are both in real use.
- Migration tooling for users who want to keep using K9's `/wrap-up`
  alongside memhub. Existing K9 + memhub coexistence stays supported
  via the v1 contract; the new memhub-native skill is opt-in.

## 1. Skill, not CLI subcommand (locked)

The deprecation plan offered two routes for the routing brain:

- **Into memhub** as a new CLI surface (`memhub wrap-up` walks the
  approval flow internally).
- **Into a Claude Code skill** that calls memhub primitives.

The skill route wins because:

1. **UX continuity.** The user's muscle memory is `/wrap-up`. Typing
   `memhub wrap-up` is a different gesture and feels like a
   regression even if it does the same work.
2. **PRD "memhub stays boring."** Putting the routing logic in a
   skill keeps the memhub binary as small primitives. The skill is
   the orchestrator; memhub stays a database and a CLI.
3. **Iteration cost.** Skill prompts iterate without a Rust
   recompile. Routing heuristics that turn out to be wrong are a
   one-line markdown edit, not a release.
4. **Approval gate naturally lives at the skill layer.** Claude Code
   skills already have the human-in-the-loop pattern; `memhub
   wrap-up` would have to re-implement it badly.

The trade-off accepted: users who don't run Claude Code don't get a
wrap-up brain at all. They have the CLI primitives. That's fine —
memhub's user base is the user, and the user runs Claude Code.

## 2. Where the skill file lives

- **A. In this `memhub` repo** under `.claude/agents/wrap-up.md` (or
  similar). The skill ships with the source tree; cloning memhub gets
  you the skill alongside the binary.
- **B. In a separate `memhub-skills` companion repo.** Keeps memhub
  itself purely a CLI/library; users opt in to the skill bundle
  separately.
- **C. In the user's `~/.claude/agents/`** as user-owned dotfiles.
  Memhub publishes a copy; users put it wherever they keep their
  other skills.

**Recommendation: A.** The skill is tightly coupled to memhub's CLI
surface — when `state set` syntax changes, the skill changes. Living
in the same repo means the skill never drifts from the schema, and a
contributor reviewing a memhub PR can see both halves of the change.
The "memhub stays boring" principle applies to the *binary*, not to
co-located docs and skills. Option B is a future move if a skill
bundle grows beyond memhub itself; option C is a rejected pattern
because it makes "did the skill update?" a user-managed problem.

## 3. New memhub primitives the skill needs

Most of what wrap-up needs already exists after the render slice
shipped. Missing:

- **`memhub note add <text> [--actor]`.** `commands::session_note`
  already implements `add()` and the `session_notes` table exists,
  but the only CLI surface today is `memhub note list`. The skill
  needs to write a session-summary note at the end of each wrap-up.
  This is a ~20-line CLI addition; ships with this slice.

What the skill does *not* need new primitives for:

- Reading state/arch latest — `memhub state show --json` /
  `memhub arch show --json` (shipped in step 1).
- Reading recent session notes — `memhub note list --json` (shipped).
- Reading staged proposals — `memhub review list --status pending --json`
  (shipped in K9 contract v1).
- Writing decisions/tasks/facts — `decision add` / `task add|done` /
  `fact add` (shipped).
- Promoting staged proposals — `review accept` / `review reject`
  (shipped).
- Setting state/arch — `state set` / `arch set` (shipped in step 1).
- Rendering markdown — `memhub render` (shipped in step 2).
- Reading git log — the skill shells out to `git log` directly. No
  memhub primitive needed; that's exactly what the PRD §3 "boring
  tech, shell out to git CLI" principle dictates.

## 4. "Since last session" boundary

The skill needs to scope "what changed since last session" when
drafting updates. Two options:

- **A. Implicit via `project_state` timestamps.** The most recent
  `project_state` row's `created_at` *is* the last session boundary,
  since `state set` is a normal wrap-up step. Anything newer in
  `decisions` / `tasks` / `facts` / `session_notes` /
  `pending_writes` / `commits` is in-window.
- **B. Explicit `sessions` table.** Add `sessions(id, started_at,
  ended_at, summary)` plus `memhub session start` / `memhub session
  end` CLI. The skill marks boundaries explicitly.

**Recommendation: A.** Option B is the kind of structure that sounds
load-bearing but is just bookkeeping until something queries it. The
implicit boundary is free, requires no schema change, and degrades
gracefully (if no `state` row exists, the skill falls back to "since
the last commit" or "since the last `writes_log` entry"). If real
use surfaces a need for explicit sessions, B is a future migration.

This option does mean the very first wrap-up in a fresh repo has no
boundary to reach back to. The skill handles that by treating the
window as "all current state" and producing a starter draft instead
of a delta draft.

## 5. K9-vocabulary scope in v1

The user's UX-mimic-K9 preference applies broadly, but not every K9
slash command needs to ship at once. Scope for this slice:

- **Ship: `/wrap-up`.** The session-end approval gate. Highest-value
  ergonomic continuity.
- **Defer: `/init-project`.** The K9 equivalent writes the four
  agent_docs files. The memhub equivalent would seed `state` and
  `arch` with starter narratives, run a first render, and possibly
  prompt the user for project name. Useful but not session-blocking.
- **Defer: `/check-init`.** Diagnostic that validates K9 structure.
  Memhub equivalent would check `.memhub/` exists, schema is at
  `LATEST_VERSION`, render output exists and matches DB. Useful for
  onboarding but redundant with `memhub status` for steady-state
  use.

Both deferred skills follow the same pattern (Claude Code skill in
`.claude/agents/`, calls memhub primitives, no new Rust code needed).
They're follow-ups, not blockers.

## 6. The `/wrap-up` flow

Concrete sequence the skill prompt orchestrates:

```
/wrap-up:
  1. Detection
     - Run `memhub status` to confirm .memhub/ exists. If not, abort
       with a clear "no memhub project here" message.
     - Capture the last `state set` timestamp (read latest state row).

  2. Read window
     - `memhub state show --json`  (current narrative)
     - `memhub arch show --json`   (current architecture)
     - `memhub note list --since-days 7 --json`
     - `memhub review list --status pending --json`
     - `git log --since="<state.created_at>" --oneline`
     - `git status --porcelain`

  3. Draft assembly
     - Synthesize a candidate new state body (currently building +
       next up + open questions).
     - Identify net-new decisions, tasks, facts to propose.
     - Identify staged pending_writes to accept or reject with
       reasons.
     - Synthesize a session note summarizing what happened this
       session.

  4. Approval gate
     - Show all drafts to the user as one reviewable block.
     - User edits / approves / rejects items individually or in bulk.

  5. On approval (DB writes first)
     a. `memhub state set <approved body>` (only if state changed)
     b. For each accepted pending write: `memhub review accept <id>`
     c. For each rejected pending write: `memhub review reject <id> --reason ...`
     d. For each new decision: `memhub decision add ...`
     e. For each new task: `memhub task add ...`
        For each task to close: `memhub task done <id>`
     f. For each new fact: `memhub fact add ...`
     g. `memhub note add <session summary>`

  6. Render and audit
     - `memhub render` — emits fresh PROJECT.md / PROJECT_LEDGER.md.
     - Print a one-line summary of what landed (counts per kind).
     - If any DB write fails: halt before render, surface the failure,
       leave the partial DB state durable for retry.
```

All `--actor` flags pass `claude:wrap-up` (or whatever the skill is
named) so `writes_log` distinguishes wrap-up writes from raw CLI
writes.

The skill does *not* git-commit the rendered files. That's a separate
gesture the user takes (or doesn't), preserving the explicit
local-vs-shared boundary the PRD calls out.

## 7. Sequencing within this slice

1. **Ship `memhub note add` CLI.** Trivial (~20 lines + tests). The
   skill needs it; nothing else does, so it's a clean precondition.
2. **Author `.claude/agents/wrap-up.md`.** The skill itself —
   prompt, expected commands, approval-gate framing. Iterates after
   first real use.
3. **Dogfood on this repo.** Run the new `/wrap-up` against memhub's
   own development sessions. Use the friction discovered to refine
   step 2.
4. **Defer `/init-project` and `/check-init`.** Same pattern, ship
   when first asked.
5. **PRD §2 addendum.** With render + wrap-up both in real use, the
   addendum can describe the shipped end state.

Steps 1 and 2 land in this repo. Step 3 is operational, not a code
change. Step 4 is a future slice. Step 5 is documentation hygiene.

## 8. Out of scope for this slice

- A `memhub wrap-up` CLI subcommand. The skill *is* the wrap-up
  surface; duplicating it as a CLI would defeat the UX-continuity
  argument and grow the binary's responsibility.
- Multi-agent skill coordination (memhub never decides which agent
  ran). The actor field captures who wrote each row; orchestration
  beyond that is outside memhub's scope.
- Auto-running wrap-up on Claude Code session-end hooks. Manual
  invocation is the contract; auto-fire is a future ergonomics
  question.
- A `sessions` table or explicit session boundary management.
  Deferred per §4 option B.
- Rolling back partial wrap-up writes. The "DB writes first, then
  render" sequence already gives durable retry semantics; rollback
  is more complex than it's worth.
- Re-implementing K9's `/wrap-up` as a memhub skill while K9 still
  exists. The new skill targets the memhub-primary world; the K9
  v1 contract stays for the transitional dual-tool flow.

## 9. Open questions

- Skill filename and naming convention: `wrap-up.md` matches K9 by
  ear, but Claude Code skills typically use longer descriptive
  names. Tentative: `wrap-up.md` for ergonomics, with a clear
  description in frontmatter.
- Does the skill need to know the *project name* to scope the
  approval prompt? Currently `memhub status` exposes it; the skill
  reads from there.
- Should the skill propose `arch` updates, or treat arch as
  user-edited only (rare-cadence)? Tentative: rare-cadence; the
  skill flags arch drift but doesn't auto-draft an update.
- How does the skill handle the very first wrap-up in a fresh repo
  (no prior `state` row)? Tentative: it produces a starter draft for
  state and arch from scratch and asks the user to fill in.
- Does the skill ever auto-accept low-risk pending_writes (e.g., a
  `propose_fact` for a build command that has a verified signal)?
  Tentative: no — every promotion goes through the gate. Auto-accept
  is the kind of feature that quietly becomes a problem.
- Where does `claude:wrap-up` (or equivalent) get registered as a
  known actor? `validate_actor` already accepts arbitrary
  ≤64-character strings; no registry exists or is needed.
