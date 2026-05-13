<!-- memhub:rendered -->
<!-- DO NOT EDIT. Generated from .memhub/project.sqlite. -->
<!-- To change content, use memhub CLI; then re-run `memhub render`. -->
<!-- Generated at: 2026-05-13T18:40:36Z by memhub 0.1.0 -->

# memhub — Ledger

## Decisions

_18 decision(s). Most recent first._

### D18 — State and arch narratives are wrap-up-only; no MCP tools for state_set or arch_set

**Status:** active • **Decided:** 2026-05-13 18:40:26 • **Source:** user+agent:claude-code

Letting agents rewrite the state narrative mid-session invites drift. The wrap-up approval gate exists for exactly this kind of write, where the agent's draft gets per-item human review before landing. Bigger trust surface than facts/decisions (one row replaces the whole narrative) and used rarely (once per session), so not worth a structured tool that bypasses the gate.

---

### D17 — MCP tool trust split: direct writes for intent, staged writes for claims

**Status:** active • **Decided:** 2026-05-13 18:40:19 • **Source:** user+agent:claude-code

Tasks are intent that the user prunes; session notes are scratch; render regenerates from the DB — all low-trust and worth direct MCP tools. Facts and decisions are claims about reality and need the user-approval staging gate; bypassing it via direct MCP fact_add / decision_add would erode the 'agents are untrusted writers' principle that makes memhub trustworthy as a multi-agent store. Codified by which MCP tools exist in e67167e (task_add, task_done, list_facts, render direct; propose_fact, propose_decision staged).

---

### D16 — Memhub-native skills ship as installable Claude and Codex templates

**Status:** active • **Decided:** 2026-05-13 18:23:38 • **Source:** user+agent:codex

templates/skills/claude/* and templates/skills/codex/* are now the distribution shape for /wrap-up, /init-project, and /check-init, while user-level installed copies are runtime artifacts. This keeps the repo as the source for skill prompts without making installed dotfiles part of the project worktree.

---

### D15 — Durable source provenance is separate from audit actor

**Status:** active • **Decided:** 2026-05-13 18:23:30 • **Source:** user+agent:codex

Migration 0008_decisions_source and the source vocabulary addendum establish that durable facts/decisions record the claim source (user, agent:<id>, user+agent:<id>, git, observed) while writes_log.actor records who performed the write. Codex and Claude wrap-up flows should pass --source user+agent:<id> for user-approved fact/decision writes and --actor <agent>:wrap-up for audit attribution.

---

### D14 — /wrap-up promoted to user-level skill placement (supersedes D13)

**Status:** active • **Decided:** 2026-05-13 17:32:55 • **Source:** user

D13 kept /wrap-up at project-level inside ~/memhub/.claude/commands/wrap-up.md on the reasoning that it only fires in memhub-initialized repos — but D13 itself acknowledged /init-project and /check-init are at user-level despite the same .memhub/ precondition. That asymmetry was a bug and caused a real UX gap this session: /wrap-up was not available in Free-AI-SSD, forcing a mid-task lift. Resolution: copied the skill to ~/.claude/commands/wrap-up.md and deleted the project-level copy. All three memhub-native skills now live at user-level (single source of truth, no project-vs-user drift); each gates on .memhub/ existing. The project-level .claude/commands/ directory in this repo is now empty.

---

### D13 — Memhub-native skill placement: /wrap-up is project-level; /init-project and /check-init are user-level

**Status:** active • **Decided:** 2026-05-13 03:28:07 • **Source:** user

/wrap-up only makes sense in memhub-initialized repos (requires .memhub/), so it ships at .claude/commands/wrap-up.md inside the memhub source repo and only fires there. /init-project creates memhub in any repo and /check-init diagnoses any memhub-using repo, so they must apply globally and live at user-level (~/.claude/commands/). The collision-rename pattern from M7-001 stays the same in both placements: rename the K9 file to -k9.md, drop in the memhub-native version at the original filename. Single rule: the memhub-native skill lives where its trigger makes sense; the K9 collision gets renamed regardless.

---

### D12 — PRD evolves via addendum docs; the PRD itself stays verbatim

**Status:** active • **Decided:** 2026-05-13 02:22:14 • **Source:** user

CLAUDE.md guardrail says 'Keep docs/reference/memhub-prd.md verbatim.' When PRD-level wording needs to change (e.g., the K9 deprecation track inverting §2's 'markdown is entry point' framing), the change ships as a separate addendum doc at docs/reference/memhub-prd-<topic>-addendum.md that is authoritative for the items it modifies. Future addenda follow the same naming pattern. The PRD's structure, design principles, non-goals, and section ordering all stay as they are. Captured in 7c162b2 as the first instance: docs/reference/memhub-prd-deprecation-addendum.md. This pattern keeps the original PRD as a stable canonical artifact while letting the project evolve under explicit, dated revisions readable by anyone tracing the PRD's history.

---

### D11 — Slash command collision resolution: rename the user-level skill

**Status:** active • **Decided:** 2026-05-13 01:50:34 • **Source:** user

Claude Code resolves /command names by filename and applies enterprise > personal > project precedence — personal shadows project. Documented at code.claude.com/docs/en/skills.md, not a bug. There is no /project:<name> namespace escape hatch for non-plugin commands. When a project-level memhub-native skill needs to claim a /command name already used by a user-level skill (typically K9), the resolution is to rename the user-level skill (e.g., wrap-up.md -> wrap-up-k9.md) plus update its frontmatter description. The forward-looking ergonomic (memhub) wins; the deprecating ergonomic (K9) takes the longer name. This pattern applies to any future memhub-native /init-project and /check-init equivalents and is reflected in cross-session memory at feedback_ux_mimic_k9.md. Captured operationally in the M7-001 backlog closure (103eea0).

---

### D10 — Wrap-up session boundary is implicit; no sessions table

**Status:** active • **Decided:** 2026-05-13 00:07:40 • **Source:** user

The most recent project_state row created_at is the previous wrap-up timestamp. Anything newer in decisions, tasks, facts, session_notes, pending_writes, or commits is in-window. Rejected an explicit sessions(id, started_at, ended_at, summary) table plus memhub session start/end CLI because that structure is bookkeeping until something queries it. If real use surfaces a need for explicit sessions, the table ships as a future migration. Until then, since-last-state-set is a free boundary that requires no schema change.

---

### D9 — Wrap-up routing brain is a Claude Code skill, not a CLI subcommand

**Status:** active • **Decided:** 2026-05-13 00:07:39 • **Source:** user

The session-end approval gate that orchestrates state set, decision add, task close, fact add, note add, review accept/reject, and render lives as .claude/commands/wrap-up.md in this repo, not as memhub wrap-up. Rejected a memhub wrap-up CLI subcommand because UX continuity matters: typing /wrap-up is the user muscle memory; memhub wrap-up is a different gesture and feels like a regression. The skill route also keeps the memhub binary as small primitives per the PRD boring tech principle, and skill prompts iterate without a Rust recompile. Trade-off accepted: users who do not run Claude Code do not get a wrap-up brain, only the CLI primitives. Captured in docs/roadmap/wrap-up-design.md section 1.

---

### D8 — Render conflict semantics: DB wins, prior file backed up

**Status:** active • **Decided:** 2026-05-13 00:07:39 • **Source:** user

memhub render unconditionally overwrites existing rendered files but first copies them under .memhub/backups/rendered/<stamp>/, mirroring the existing sync_md markdown-backup convention. Rejected refuse-on-divergence (require --force if file content diverges from prior render) because refusing punishes the user for a mistake the file own header already warns about; backup-and-overwrite preserves the edit content if they need it. Rendered files are generated artifacts. A human editing one is a category error: the change will not survive the next render and is not reflected in the DB.

---

### D7 — Render trigger is on-demand; auto_render is opt-in for later

**Status:** active • **Decided:** 2026-05-13 00:07:39 • **Source:** user

memhub render is an explicit CLI command. There is no auto_render config flag in v1. Rejected auto-firing after every mutating write (mirroring auto_sync_md) because render output is bigger than the small managed block sync-md produces and would clutter git status mid-session. The natural cadence is render-at-session-end, which is a wrap-up step. auto_render = true is reserved in [render] config as a future opt-in.

---

### D6 — State and arch durable storage uses single-blob tables, not decomposed columns

**Status:** active • **Decided:** 2026-05-13 00:07:39 • **Source:** user

Migration 0007_project_narrative.sql adds project_state and project_arch tables with (id, project_id, body, actor, actor_raw, created_at) shape. set always inserts a new row (append-only history); show returns the most recent; history lists prior. Rejected decomposing narrative into structured columns (currently_building, next_up, open_questions) because decomposition is a guess at structure that may not survive contact with how state actually evolves. Going decomposed to blob is harder than blob to decomposed if querying patterns later demand it, so blob ships first.

---

### D5 — Render output shape is memhub-native two-file (PROJECT.md + PROJECT_LEDGER.md)

**Status:** active • **Decided:** 2026-05-13 00:07:39 • **Source:** user

memhub render emits two files into the configured output dir (default agent_docs/): PROJECT.md for narrative (state, arch, recent session notes) and PROJECT_LEDGER.md for structured append-only content (decisions, backlog, facts, recent activity). Rejected K9-style four-file mirror because mirror would inherit K9's split decisions without earning them and would name-collide with K9 files during transition. Two files split along the natural seam in the DB: narrative-cadence (rewritten on state changes) versus ledger-cadence (appended on decisions/tasks/facts). Each rendered file leads with a memhub:rendered marker comment plus ISO timestamp and memhub version. Captured in docs/roadmap/memhub-render-design.md section 1.

---

### D4 — K9 framework deprecation: memhub becomes primary durable store

**Status:** active • **Decided:** 2026-05-12 22:40:23 • **Source:** user

End-state has memhub as the single durable source; K9 retires its slash commands, markdown templates, and routing brain. Direction committed in intent; implementation slices land individually with their own design docs. PRD §2 and k9-integration non-goals stay in force until each slice argues an explicit change. See docs/roadmap/k9-deprecation-plan.md for the four load-bearing slices and docs/roadmap/memhub-primary-evaluation.md as the origin record.

---

### D3 — K9 canonical conventions (H3 backlog items, em-dash decisions) are the parser target

**Status:** active • **Decided:** 2026-05-12 21:03:01 • **Source:** user

K9-Claude-Framework/docs/file-structure.md:156-208 is the authoritative K9 framework spec. project_backlog.md items use ### Title H3 delimiters with bulleted bolded-field bodies. The Status field uses vocabulary: triaged, planning, in-progress, blocked, or done. project_decisions.md entries use ## YYYY-MM-DD plus em-dash plus Title with a Unicode em-dash separator (U+2014). M6-001 and M6-002 target only this canonical format. Free-AI-SSD follows it; memhub agent_docs does not. M6-004 migrates memhub agent_docs files to canonical structural delimiters (em-dash separator on decisions, H3 headings on backlog) while preserving prose Notes paragraphs rather than decomposing into K9 bulleted slots. The lenient support-both-formats option was rejected because doubling the parser surface would bake memhub divergence into the supported contract; the narrower trailing Status line is retained only as a tolerated input recognition path, not a supported authoring convention.

---

### D2 — Evaluating memhub-primary is staged behind parser-fix evidence

**Status:** active • **Decided:** 2026-05-12 21:02:58 • **Source:** user

A working-session analysis report (local, not committed) identifies six bootstrap parser bugs in src/commands/bootstrap_k9.rs and a 12-gap roadmap for full memhub-primary replacement of the K9 framework. Direction is not committed. Evaluation plan lives in docs/roadmap/memhub-primary-evaluation.md: Phase 1 lands parser fixes (M6-001 and M6-002), Phase 2 re-runs bootstrap on Free-AI-SSD and captures findings (M6-003), Phase 3 routes to commissioning a separate memhub render design doc, extending the parser further, or closing the evaluation with memhub staying complementary. PRD section 2 (markdown files stay as the entry point), PRD section 4 non-goals, and docs/roadmap/k9-integration.md non-goals (no bidirectional sync, no DB mapping of project_state.md or project_arch.md) all remain in force. Reasoning: replacement is genuinely better only if parser fixes land cleanly AND a memhub render materializes acceptably; committing the roadmap before parser-fix evidence would be premature.

---

### D1 — One-shot K9 bootstrap is the narrow exception to the no-import non-goal

**Status:** active • **Decided:** 2026-05-12 19:38:52 • **Source:** user

Steady-state non-goal of no bulk K9 import stays. First-install bootstrap-k9 is the narrow carve-out for new-machine clones with populated K9 history. Refuses on non-empty target; writes through decision::add and task::add_with_status with actor=k9:bootstrap; skips project_state.md/project_arch.md and facts by design.

---

## Backlog

_10 task(s), 1 open. Open first, then by recency._

### T10 — Dogfood Codex memhub skills in fresh and existing repos

**Status:** open • **Updated:** 2026-05-13 18:23:52

Exercise /wrap-up, /init-project, and /check-init from the Codex skill templates after install; verify attribution, render output, and fresh-repo behavior.

---

### T9 — Migrate memhub's own agent_docs from K9 markdown to memhub-rendered files (M7-002)

**Status:** done • **Updated:** 2026-05-13 01:50:34

Blocked on M7-001. Once the memhub-native /wrap-up runs in this repo: (a) populate project_state and project_arch tables from current K9 narrative via memhub state set --from-file and memhub arch set --from-file; (b) run memhub render to produce PROJECT.md and PROJECT_LEDGER.md; (c) update CLAUDE.md Session Continuity to point at the new files instead of the K9 four-file set; (d) memhub integrations disable-k9; (e) decide whether to remove or archive the four K9 project_*.md files. Closes the M6-004 dogfood gap by replacing it. Validates the full memhub-primary loop end-to-end.

---

### T5 — M6-004 - Migrate memhub agent_docs to K9 canonical structural delimiters

**Status:** done • **Updated:** 2026-05-13 01:50:34

_No notes._

---

### T8 — Investigate .claude/commands/wrap-up.md override gap (M7-001)

**Status:** done • **Updated:** 2026-05-13 01:30:02

Project-level slash command at .claude/commands/wrap-up.md was expected to take precedence over user-level ~/.claude/commands/wrap-up.md when invoked inside the project. Empirically the user-level K9 /wrap-up fired instead during step 3 dogfood. Until this is understood and fixed, the memhub-native wrap-up cannot actually be dogfooded inside the memhub repo. Investigation: check Claude Code docs for skill resolution rules, verify file placement convention (commands/ vs agents/ vs skills/), test with a unique-named project skill to confirm whether project-level loading works at all, then either fix placement or rename to avoid collision while preserving slash-command ergonomics. Gates M7-002.

---

### T7 — M6-006 - Accept UTF-8 mojibake separator (â€" triple-codepoint sequence) in extract_date_and_title

**Status:** done • **Updated:** 2026-05-12 22:40:23

_No notes._

---

### T6 — M6-005 - Extend done-marker detection to recognize 'merged DATE' intervention and 'Shipped' vocabulary

**Status:** done • **Updated:** 2026-05-12 22:40:23

_No notes._

---

### T4 — M6-003 - Re-run bootstrap on Free-AI-SSD post-fix, capture findings, write Phase 2 results into the evaluation doc

**Status:** done • **Updated:** 2026-05-12 21:39:53

_No notes._

---

### T3 — M6-002 - Accept em-dash in strip_date_prefix and extract decision date into decided_at

**Status:** done • **Updated:** 2026-05-12 21:15:10

_No notes._

---

### T2 — M6-001 - Rewrite parse_backlog around K9 canonical H3 delimiters with structured done-detection

**Status:** done • **Updated:** 2026-05-12 21:03:14

_No notes._

---

### T1 — M5-006 - Add memhub integrations bootstrap-k9 first-install priming

**Status:** done • **Updated:** 2026-05-12 19:38:58

First-install-only memhub integrations bootstrap-k9 [--dry-run] [--json]. Refuses unless K9 enabled AND decisions+tasks empty; no --force. Parses project_decisions.md (heading-block, body becomes rationale) and project_backlog.md (bullets, Status: completed skipped, Status: blocked maps to blocked). Writes through decision::add and new task::add_with_status helper with actor=k9:bootstrap. project_state.md/project_arch.md not parsed; no fact extraction. 5 subprocess + 4 parser unit tests. Roadmap non-goal softened with carve-out. Discoverability: README install-prompt step 6 and checked-in audit prompt; K9 framework v1.2.1 shims outside this repo.

---

## Facts

_No facts recorded._

## Recent activity (last 30 days)

| When | Actor | Table | Action | Reason |
|------|-------|-------|--------|--------|
| 2026-05-13 18:40:30 | claude:wrap-up | session_notes | insert | mcp log_session_note |
| 2026-05-13 18:40:26 | claude:wrap-up | decisions | insert | decision add |
| 2026-05-13 18:40:19 | claude:wrap-up | decisions | insert | decision add |
| 2026-05-13 18:40:13 | claude:wrap-up | project_state | insert | state set |
| 2026-05-13 18:24:09 | cli:user | render | render | memhub render |
| 2026-05-13 18:23:59 | codex:wrap-up | project_arch | insert | arch set |
| 2026-05-13 18:23:57 | codex:wrap-up | session_notes | insert | mcp log_session_note |
| 2026-05-13 18:23:52 | codex:wrap-up | tasks | insert | task add |
| 2026-05-13 18:23:38 | codex:wrap-up | decisions | insert | decision add |
| 2026-05-13 18:23:30 | codex:wrap-up | decisions | insert | decision add |
| 2026-05-13 18:23:25 | codex:wrap-up | project_state | insert | state set |
| 2026-05-13 17:33:00 | cli:user | render | render | memhub render |
| 2026-05-13 17:32:55 | claude:wrap-up | project_arch | insert | arch set |
| 2026-05-13 17:32:55 | claude:wrap-up | session_notes | insert | mcp log_session_note |
| 2026-05-13 17:32:55 | claude:wrap-up | decisions | insert | decision add |
| 2026-05-13 17:32:55 | claude:wrap-up | project_state | insert | state set |
| 2026-05-13 03:28:15 | cli:user | render | render | memhub render |
| 2026-05-13 03:28:11 | claude:wrap-up | session_notes | insert | mcp log_session_note |
| 2026-05-13 03:28:07 | claude:wrap-up | decisions | insert | decision add |
| 2026-05-13 03:28:01 | claude:wrap-up | project_state | insert | state set |
| 2026-05-13 02:22:14 | cli:user | markdown_sync | update | sync-md |
| 2026-05-13 02:22:14 | cli:user | render | render | memhub render |
| 2026-05-13 02:22:14 | claude:wrap-up | session_notes | insert | mcp log_session_note |
| 2026-05-13 02:22:14 | claude:wrap-up | decisions | insert | decision add |
| 2026-05-13 02:22:14 | claude:wrap-up | project_state | insert | state set |
| 2026-05-13 01:53:11 | cli:user | markdown_sync | update | sync-md |
| 2026-05-13 01:50:58 | cli:user | render | render | memhub render |
| 2026-05-13 01:50:34 | cli:user | config | update | integrations disable k9 |
| 2026-05-13 01:50:34 | claude:wrap-up | session_notes | insert | mcp log_session_note |
| 2026-05-13 01:50:34 | claude:wrap-up | tasks | update | task done |
| 2026-05-13 01:50:34 | claude:wrap-up | tasks | update | task done |
| 2026-05-13 01:50:34 | claude:wrap-up | decisions | insert | decision add |
| 2026-05-13 01:50:34 | claude:wrap-up | project_state | insert | state set |
| 2026-05-13 01:50:34 | claude:wrap-up | project_arch | insert | arch set |
| 2026-05-13 01:30:09 | cli:user | render | render | memhub render |
| 2026-05-13 01:30:02 | claude:investigation | tasks | update | task done |
| 2026-05-13 00:14:23 | cli:user | render | render | memhub render |
| 2026-05-13 00:12:12 | cli:user | markdown_sync | update | sync-md |
| 2026-05-13 00:07:40 | k9:wrap-up | tasks | insert | task add |
| 2026-05-13 00:07:40 | k9:wrap-up | tasks | insert | task add |
| 2026-05-13 00:07:40 | k9:wrap-up | decisions | insert | decision add |
| 2026-05-13 00:07:40 | k9:wrap-up | decisions | insert | decision add |
| 2026-05-13 00:07:39 | k9:wrap-up | decisions | insert | decision add |
| 2026-05-13 00:07:39 | k9:wrap-up | decisions | insert | decision add |
| 2026-05-13 00:07:39 | k9:wrap-up | decisions | insert | decision add |
| 2026-05-13 00:07:39 | k9:wrap-up | decisions | insert | decision add |
| 2026-05-12 22:41:15 | cli:user | markdown_sync | update | sync-md |
| 2026-05-12 22:40:23 | k9:wrap-up | decisions | insert | decision add |
| 2026-05-12 22:40:23 | k9:wrap-up | tasks | update | task done |
| 2026-05-12 22:40:23 | k9:wrap-up | tasks | update | task done |
