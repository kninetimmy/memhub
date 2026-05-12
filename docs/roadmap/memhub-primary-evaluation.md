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

## Phase 2 results (2026-05-12)

Captured after wiping Free-AI-SSD's `.memhub/` and re-running
`memhub integrations bootstrap-k9` against the unchanged
`project_decisions.md` (3525 lines, 82 H2 headings) and
`project_backlog.md` (1843 lines, 49 H3 headings).

### Final counts vs grep ground truth

| Metric | grep ground truth | bootstrap output | Match |
|---|---:|---:|---|
| Decisions (H2 with date) | 82 | `decisions_imported: 82` | ✓ exact |
| Backlog items (H3) | 49 | `tasks_imported + tasks_skipped_completed = 41 + 8 = 49` | ✓ exact |
| Tasks skipped as done | 16 in preamble index table (8 with `###` body) | 8 | partial |

Pre-fix the parser produced 519 tasks from this same 49-item backlog
and 0 done-skips. M6-001's H3 rewrite resolves the structural
overcounting; M6-002's date extraction populates `decided_at` from
the heading.

### Decision quality breakdown

`extract_date_and_title` accepts ASCII hyphen (U+002D) or em-dash
(U+2014) only. Free-AI-SSD's `project_decisions.md` separator
histogram by codepoint at position 11:

| Separator | Count | Parsed cleanly? |
|---|---:|---|
| U+2014 (em-dash `—`) | 58 (71%) | ✓ |
| U+002D (ASCII hyphen `-`) | 5 (6%) | ✓ |
| U+00E2 + U+20AC + U+201D (`â€"` mojibake) | 19 (23%) | ✗ |

Outcome: 63/82 (77%) decisions get a correct `decided_at` and a clean
title. 19/82 (23%) — all the oldest entries dated 2026-04-17 through
2026-04-18 — keep their `YYYY-MM-DD â€" ` prefix in the title and
default `decided_at` to import time. This is Bug 6 (UTF-8 mojibake)
from the original analysis, explicitly deferred when M6-001/M6-002
were scoped. The data is recoverable by fixing the source file (the
mojibake is a one-time `â€"` → `—` find-and-replace) or by extending
the parser with a third mojibake-aware separator branch.

### Task done-detection breakdown

The parser scans body content between `###` headings for done
markers. Free-AI-SSD's actual done-state truth lives in **two
locations**:

1. **Index table at the top of `project_backlog.md` (lines 60-260).**
   28 rows containing `**done** PR #N` markers, covering 16 distinct
   item IDs (including 8 from `mac_project_backlog.md` which the
   bootstrap parser doesn't read at all). The parser cannot see any
   of these — they sit in the preamble before the first `###` at
   line 269.
2. **`**Status:**` lines inside `###` bodies.** 12 `###` items have a
   body `**done**` marker.

Parser results against the 12 body markers:

- **8 detected (skipped as done):** C1, C2, C3, C4, C5, C6, C24, C27.
  Pattern: `**Status:** **done** — PR #N`.
- **4 missed (imported as open despite body marker):** F2, F2a, F3,
  X13. Pattern: `**Status:** **done** — merged YYYY-MM-DD (PR #N)`.
  The parser's `line_has_inline_done_pr_marker` accepts only
  whitespace/em-dash/hyphen/colon/paren/bracket between `**done**`
  and the `pr` token, so "merged DATE" between them defeats the
  match.

True precision/recall against the body-marker subset: 8/8 detected
correctly (100% precision on what it caught), 8/12 (67% recall) on
body markers. Against the human's full done-state truth including
the preamble table, recall is closer to 8 / (16 + 4) ≈ 40% — the
parser cannot reach 100% on this repo without either reading the
preamble table or having the human migrate completion tracking into
the `###` bodies.

### Spot-check sample (5 decisions, 10 tasks)

**Decisions:**

- `[1] 2026-05-12 21:24:57 | "2026-04-17 â€" Initialized project_docs framework"` — mojibake group; title carries date prefix, `decided_at` is import time. Source heading: `## 2026-04-17 â€" Initialized project_docs framework`. **Faithful but un-cleaned.**
- `[20] 2026-04-21 00:00:00 | "F4 Stage 1: PrepApp owns first-run profile selection..."` — ASCII-hyphen group; correct date, clean title. Source: `## 2026-04-21 - F4 Stage 1: ...`. **Clean.**
- `[21] 2026-05-05 00:00:00 | "MAC2 platform boundary: shared stays mixed only..."` — em-dash group; correct date, clean title. **Clean.**
- `[22] 2026-05-05 00:00:00 | "MAC1 supported Mac baseline: Apple Silicon..."` — em-dash group; correct. **Clean.**
- `[24] 2026-05-05 00:00:00 | "Cross-platform PrepApp parity (amends MAC1)"` — em-dash group; correct. **Clean.** (The `(amends MAC1)` annotation is part of the source title and is preserved verbatim.)

**Tasks:**

- `[1] C25 — Visual differentiation...` — open. Source body has no `**done**` marker; the table row IS strikethrough (`~~C25~~`) and marked done. Parser missed because table is in preamble. **False open.**
- `[3] R1 â€" Runner CLI REPL ...` — open, mojibake heading. Source ### body confirms it is genuinely not done. **Correct open.**
- `[6] F2 â€" Live model list fetch ...` — open, mojibake heading. Source body has `**Status:** **done** — merged 2026-05-07 (PR #202, ...)`. **False open** — parser regex doesn't accept "merged DATE" between `**done**` and `PR`.
- `[7] F2a â€" Model picker UX gaps ...` — open, same `**done** — merged DATE (PR #N)` pattern. **False open.**
- `[8] F3 â€" PrepApp 2-tab restructure ...` — same. **False open.**
- `[10] F4 â€" Profile FTUE moves entirely to PrepApp ...` — open. Source body has no done marker; index table also doesn't list it as done. **Correct open.**
- `[12] X1 â€" Voice pipeline hang ...` — open. Source body has `**Status:** Closed-out — superseded by X1-Redux`. The status text is "Closed-out" not "done", which neither the index table nor the parser treats as completed. **Ambiguous; reasonably open.**
- `[22] H1 â€" Repo spring cleaning` — open. Source body active. **Correct open.**
- `[27] X13 â€" Chat/STT surface real failures` — open. Source body has `**Status:** **done** — merged 2026-05-08 (PR #220, ...)`. **False open** (same parser-regex miss).
- `[41] X25 â€" Extend File.Replace retry ...` — open. Source body has `**Status:** Shipped â€" PR #155 ...`. The word is "Shipped" not "done"; this is yet another vocabulary variant the parser doesn't recognize. **False open.**

Net spot-check: 10 tasks sampled, 4 false-opens (F2, F2a, F3, X13)
from the "merged DATE" regex gap, 1 false-open (X25) from "Shipped"
vocabulary, 1 false-open (C25) from index-table-only state, 4 correct
(R1, F4, X1, H1). Decisions sampled were all faithful to source; the
only quality split is mojibake-prefix vs. clean.

### DB ergonomics vs `grep agent_docs/`

**`memhub search 'cold-load'`** returns one ranked decision match
(C1) with a 12-line rationale snippet inline. Speed: ~9 ms.
`grep -c 'cold-load' agent_docs/{project_backlog,project_decisions}.md`
returns 6 + 7 = 13 raw line matches across both files in similar wall
time. The ranked single-result form is strictly better when the
question is "what decision covers cold-load?"; grep is strictly better
when the question is "show me everywhere cold-load appears."

**`memhub task list --status open`** returns all 41 tasks with notes
inlined (verbose). `--status` filter is the meaningful ergonomic
gain — grep cannot express "open tasks only" without first parsing
status out of mixed locations. Lack of `--limit`/`--offset` flags
means the listing is all-or-nothing.

**`memhub decision list`** dumps all 82 decisions with rationale
inline. No filter or limit flag. Comparable to `awk '/^## /'
agent_docs/project_decisions.md` for the heading-only view; richer
than grep for the heading+rationale view.

**Gap:** `memhub search` indexes only the `decision_chunks` FTS5
table. Task notes, `project_state.md`, `project_arch.md`, and
`mac_project_backlog.md` are all outside the search index. For
`grep`-replacement at the "find any mention of X anywhere in
agent_docs/" granularity, memhub is not currently a substitute.

### Findings summary

1. **The structural parser fixes work.** 49 H3 items → 49 tasks
   exactly; the dry-run reports the same number every time; no
   spurious phantom items. M6-001 closed the 519-vs-49 gap cleanly.
2. **Mojibake remains a 23% blocker for decisions.** Bug 6 was
   deferred and Free-AI-SSD's data demonstrates concretely what
   "deferred" costs: 19 decisions with dirty titles, no extracted
   dates, all bunched in the early-April history.
3. **Done-detection misses ~60% of the human's done-state truth.**
   Two separate causes: (a) Free-AI-SSD tracks completion in a
   preamble index table that's structurally outside the `###` items,
   and (b) the parser's `**done** PR #N` regex doesn't accept
   "merged DATE" between `**done**` and `PR`, missing 4 of 12
   body-markered items.
4. **Vocabulary variance is real.** Free-AI-SSD uses "**done**",
   "Shipped", and "Closed-out" as completion vocabulary in
   `**Status:**` lines. Only "**done**" is recognized.
5. **DB ergonomics are strong for decisions, weak for tasks.**
   FTS5-ranked decision search is genuinely better than grep; task
   notes are not indexed at all.

### Phase 3 routing

Per the evaluation plan's three outcomes, the result lands on
**marginal**:

- Decisions: 77% clean, with the residual 23% tractable via either a
  one-time `â€"` → `—` source fix (zero parser change needed) or a
  third separator branch in `extract_date_and_title`.
- Tasks: parsable structurally (49/49 found) but done-state
  classification is fragile against real-world status conventions.
  Closing the gap requires either (a) parser extensions to recognize
  more done-marker syntaxes and vocabulary, (b) Free-AI-SSD's K9
  files migrating to canonical body-level status bullets, or (c)
  accepting that bootstrap is a one-time priming step and the human
  hand-corrects the residual classification.

Concrete recommendation: do **not** commission `memhub render` yet.
The bootstrap quality is good enough to seed an empty repo with
reasonable structural truth, but not good enough to make memhub the
durable source of truth without manual correction. Two targeted
follow-ups would close most of the remaining gap and are each
narrowly scoped:

- `M6-005` (suggested) — extend `line_has_inline_done_pr_marker` to
  skip an intervening "merged DATE" / "shipped DATE" token before
  `PR`; extend the bulleted-status recognizer (or the inline marker
  detector) to recognize "Shipped" as an additional done synonym.
  Expected lift: 4-5 false-opens recovered on this data set.
- `M6-006` (suggested) — accept mojibake as a third separator branch
  in `extract_date_and_title`, or document a one-line `iconv` /
  `sed` fix in `docs/reference/k9-consumer-audit-prompt.md` so the
  human can clean the source file before bootstrap. Expected lift:
  19 decisions promoted from dirty-title to clean.

After either of those lands, a third Free-AI-SSD bootstrap run would
re-evaluate whether the residual error rate is low enough to revisit
the render-design commission. Today, with ~40% done-detection recall
and ~23% mojibake decisions, the dual-tool workflow still wins.

These follow-ups stay non-PRD-modifying. The render design doc and
the K9-non-goal carve-outs are not opened by this Phase 2 result.

## Outcome routing

If Phase 3 lands on "commission `memhub render` design," the next
artifact is `docs/roadmap/memhub-render-design.md`. PRD §2 and the
non-goals in `docs/roadmap/k9-integration.md` stay in force until that
design doc proposes a specific change with explicit reasoning that
the user accepts.
