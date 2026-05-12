# Evaluating memhub-primary as a K9 replacement

Status: Open evaluation. No PRD or roadmap non-goals are modified by
this document; it captures the hypothesis being tested, the evidence
required to advance, and the explicit decision points that follow.

## Hypothesis

memhub today is complementary to K9 (PRD §2). The hypothesis under
evaluation is that memhub could plausibly become the primary durable
store, with K9 markdown files becoming a generated artifact — without
violating the PRD's design principles and without producing worse
outcomes than the current dual-tool workflow.

The hypothesis is *not* committed. This document plans the test, not
the migration.

## What triggered the evaluation

A working session ran `memhub integrations bootstrap-k9` against
Free-AI-SSD's K9 history (`project_decisions.md` 3525 lines,
`project_backlog.md` 1843 lines) and produced two findings:

1. Six concrete bugs in `src/commands/bootstrap_k9.rs` that caused the
   parser to produce 519 tasks from a 49-task backlog, leave the date
   prefix bolted onto every decision title, and report
   `tasks_skipped_completed: 0` despite many entries being marked done
   in source. Captured as M6-001 / M6-002 in `project_backlog.md`.
2. A 12-gap analysis of what a full K9→memhub replacement would cost.
   Gap 5 (`memhub render` for PR review and cross-machine
   collaboration) and Gap 3 (the wrap-up routing brain) are the
   load-bearing ones. The remaining 10 are either small CLI surface
   gaps, accept-and-move-on, or downstream of Gap 5.

The analysis report's own honest-take is that replacement is genuinely
better *only* if parser fixes land cleanly *and* `memhub render`
materializes acceptably. Until both exist, the current dual-tool
workflow wins on the math.

## What this evaluation will and won't decide

Decides:

- Whether the bootstrap parser, after M6-001/M6-002, produces DB
  content trustworthy enough that memhub could plausibly become the
  source of truth for an existing K9 repo.
- Whether to commission a separate `memhub render` design doc as the
  next artifact.

Does not decide:

- Whether `memhub render` will be built. That decision belongs to its
  own design doc, after it exists.
- Whether to absorb `project_arch.md` or `project_state.md` into DB
  tables. Both remain explicit PRD / roadmap non-goals.
- Whether to grow a wrap-up routing brain inside memhub or leave that
  responsibility in K9.
- The K9 framework's own canonical conventions. This evaluation is
  about memhub's interop quality, not K9's internal design.

## Evaluation plan

### Phase 1 — Land parser fixes (M6-001, M6-002)

Fix the bootstrap parser per the analysis report's Part 1, with
regression fixtures derived from real K9 entries. Done criteria:
`parse_backlog` produces ~50 tasks not ~500 on Free-AI-SSD's
`project_backlog.md`, decision titles no longer carry their date
prefix, and `decisions.decided_at` reflects the heading date when
present.

### Phase 2 — Re-run bootstrap and capture findings (M6-003)

Wipe the bootstrap DB on Free-AI-SSD, re-run `memhub integrations
bootstrap-k9`, and write findings back into this document under a new
"Phase 2 results" section:

- Final counts: decisions imported, tasks imported, tasks skipped as
  done. Compare to source-of-truth `grep` counts.
- Spot-check sample: pick 5 decisions and 10 tasks at random, compare
  the resulting DB rows against the source markdown. Do titles match?
  Are done-state classifications correct? Are notes coherent?
- DB ergonomics: can `memhub search`, `task list`, and `decision list`
  surface the same things `grep agent_docs/` would have found?
  Comparable speed? Worse signal-to-noise?

### Phase 3 — Decision point

After Phase 2, route to one of three outcomes:

- **Bootstrap quality acceptable.** Bridge fixes are sufficient.
  Commission `memhub render` design doc as a separate slice (new file:
  `docs/roadmap/memhub-render-design.md`). Do not modify PRD or
  roadmap non-goals here — the render design doc itself argues for or
  against that change.
- **Bootstrap quality marginal.** Identify the specific patterns that
  produce bad rows. Decide whether to extend the parser further or
  accept the limitation. memhub stays complementary; K9 stays primary.
- **Bootstrap quality poor.** Stay K9-primary. The analysis report's
  Gap 5 / Gap 3 work stays unstarted. memhub remains the indexed
  cache, not the source of truth.

## Sub-questions surfaced by the analysis

1. **K9's canonical backlog convention. RESOLVED.**
   `K9-Claude-Framework/docs/file-structure.md:156-167` is the
   authoritative spec: `### Title` H3 delimiter, bulleted bolded-field
   body, status vocabulary `triaged | planning | in-progress |
   blocked | done`. Decisions use `## YYYY-MM-DD — Title` with em-dash
   (`file-structure.md:202`). memhub's own `agent_docs/` diverges from
   both. M6-001 targets only K9 canonical; M6-004 migrates memhub's
   own files to match.
2. **Decision date extraction.** Currently `decision::add` ignores any
   date in the heading and defaults `decided_at` to import time.
   M6-002 should extract the heading date when present and fall back
   to import time only when absent.
3. **Done-detection patterns.** The analysis report identifies four
   K9 patterns for marking a backlog item done (heading suffix
   `— **done**`, strikethrough table rows, body `**done** PR #N`, and
   the rarely-used trailing `Status: done` line). M6-001 covers the
   first three; the existing `Status: done` path continues to work.

## Explicit non-decisions

The following remain open questions handled separately, not by this
evaluation:

- The 8 other gaps in the analysis report beyond render and wrap-up
  brain. They are noise relative to the load-bearing two until the
  load-bearing two have a credible path.
- Bug 5 (decisions H2 discriminator) and Bug 6 (UTF-8 mojibake) from
  the analysis report's Part 1. Both flagged "not biting today" by
  the report itself and deferred until a real workflow demands them.

## Outcome routing

If Phase 3 lands on "commission `memhub render` design," the next
artifact is `docs/roadmap/memhub-render-design.md`. PRD §2 and the
non-goals in `docs/roadmap/k9-integration.md` stay in force until that
design doc proposes a specific change with explicit reasoning that
the user accepts.
