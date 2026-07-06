# memhub Improvement Review — Execution Tracker

**Source of truth:** [`2026-07-improvement-review.md`](2026-07-improvement-review.md)
(1,190 lines; findings F1–F17, P1–P22, N1–N28, decisions Q1–Q56, waves 0–9).
This file records **what has been done** so no session re-investigates a closed item.
Update it in the same commit that closes an item — status + commit/PR reference.

**Legend:** `[ ]` todo · `[~]` in progress · `[x]` done · `[-]` won't do / superseded
Each done item carries `— <date> <commit-or-PR>`.

**Sequencing rule (from §12):** resolve `§11` decisions *per wave, not all up front*.
Every wave that touches recall scoring / embedding / bundle shape must re-run the
golden evals (`memhub eval retrieval`, `eval locate`, polyglot fixture) and record the
numbers. Default-off config additions must keep an untouched install byte-identical.

---

## Status at a glance

| Wave | Theme | Done / Total | Gating decisions |
|---|---|---|---|
| 0 | Fix-now defects | 17 / 17 | — (complete) |
| 1 | Loud states (doctor/status/integrity) | 5 / 5 | Q35 ✓ (complete) |
| 2 | Session-start token diet | 6 / 7 | Q21–Q25 ✓ · Q41 ✓ (only C3 left — user-gated) |
| 3 | Staleness / lifecycle | 0 / 7 | Q1–Q6 |
| 4 | Retrieval performance | 0 / 12 | Q17–Q19, Q24, Q40 |
| 5 | Upgrade / GC hardening | 0 / 8 | Q12–Q16 |
| 6 | Wrap-up policy / verbosity | 0 / 6 | Q7–Q11 |
| 7 | Cross-machine sync / metrics | 0 / 6 | Q30–Q33 |
| 8 | CI / infra / licensing | 0 / 2 | Q37–Q38 |
| 9 | Housekeeping | 0 / 7 | Q26–Q27, Q36 |

Decisions resolved: 10 / 56 (Q21, Q22, Q23, Q24, Q25, Q29, Q32, Q35, Q39, Q41).

---

## Post-review inbox — new asks beyond the July review (added 2026-07-06)

Captured from a working session; **not** part of the July review's F/P/N/Q taxonomy and
**not** wired into the active Wave 2 chain. Each needs its own scoping pass → plan gate →
issues before execution. IDs are `A#` to keep them distinct from the review's findings.

- [ ] **A1 — Onboarding / install-prompt feature-coverage audit (all 3 CLIs).** Ensure a
  new user running the install/onboarding path — CLI `memhub init` (+ its epilogue) and the
  `/init-project` skill in Claude / Codex / OpenCode — is offered **every current feature and
  toggle**, not just the pre-M9 set the onboarding was designed around. Since-PRD additions to
  surface: `[retrieval] mode` + `use_reranker`; machine-global store (`global enable`); metrics
  (`metrics enable` + `recall_proxy` / `session_accounting` sub-switches); doc-ingest (+
  `include_docs_in_default`); code-index warm-up (`code index`); Drive sync (`sync enable` +
  `drive_subpath`); tokenizer `metrics calibrate`. **Related/subsumes:** review N10/Q50 (init
  epilogue + `init --profile` / `doctor --setup` interview). **Open decision:** epilogue-only
  vs an interactive setup interview; which toggles default-on. Effort: med.

- [ ] **A2 — README refresh + visual assets.** Beyond Wave 0's F4 factual fixes: add diagrams /
  screenshots. Candidates: the M10 Drive-sync model (VACUUM-INTO snapshot + manifest into an
  OS-synced folder, fully offline), the recall pipeline (FTS BM25 + BGE-small vector → cross-
  encoder rerank), the `/viz` dashboard, the render/category flow. **Considerations:** commit
  assets as SVG / optimized PNG under `docs/assets/` (never in the binary — offline-first
  unaffected); diagrams can be generated via the dataviz/artifact tooling. **Open decision:**
  which diagrams; SVG vs PNG; asset location. Effort: med, low risk.

- [ ] **A3 — Storage-category review + user-defined categories.** Review the fixed durable set —
  today `SourceType::{Fact, Decision, Task, DocChunk}` (recall-scored) plus write-only session
  **notes** and **commands** (`record_command`); "arch" is narrative in rendered `PROJECT.md`,
  not an entity — and decide add/trim, **plus a mechanism for user-defined categories** (example:
  an "API memory" bucket). **Architecture-adjacent — cascades** across schema (each type is its
  own table + FTS + embeddings + triggers today), retrieval scoring & `source_type` filters,
  render sections, MCP `propose_*` / `*_add` surfaces, and the "agents are untrusted writers"
  guardrail. **Spectrum:** (a) *light* — optional `kind` / tag on facts (= review **W4**, Wave 6)
  + filter/render by kind; (b) *medium* — a first-class user-category registry with per-category
  retrieval/render; (c) *heavy* — a generic typed-memory subsystem. Tension with the
  "intentionally boring / narrow milestones / no speculative subsystems" guardrail. **Leaning:**
  start at (a)/W4 to satisfy "API memory" cheaply, promote to (b) only if first-class treatment is
  genuinely needed. **Open decision (yours — load-bearing, gates the schema design):** how far up
  the spectrum. Effort: (a) small · (b) large · (c) very large.

---

## Wave 0 — Fix-now (broken today; all small)

Four PRs: **PR-A text/docs** and **PR-B safe code** (no decisions), **PR #17**
(F11, decision Q39), and **PR #18** (F1, decision Q29) — **Wave 0 complete.**

### PR-A — text/docs (zero decisions)
- [x] **F13/P3** — fixed invalid YAML frontmatter: folded `description:` to block
  scalar (`>`) in recall/locate/doc/metrics templates ×3 agents + added dependency-free
  regression guard `skill_frontmatter_descriptions_are_valid_yaml_scalars` in
  `tests/skill_parity.rs`. — 2026-07-05, PR-A (wave0/pr-a-text).
  *Installed-copy resync via `memhub upgrade` is a post-merge machine step (still pending).*
- [x] **F5** — removed skill/MCP description falsehoods (docs-in-default flipped by
  decision 90; `sync-md` "managed block" that doesn't exist; "unproven until M11 PR5"
  remnant). Files: `templates/skills/**`, `src/mcp/mod.rs`, `src/cli/args.rs`.
  — 2026-07-05, PR #13 (wave0/pr-a-falsehoods).
- [x] **F4** — README: fixed the `[retrieval]` duplicate-table instruction (×4 install
  paths), added the Claude MCP registration step, fixed the self-contradictory
  `propose_fact` global bullet. — 2026-07-05, PR #13 (wave0/pr-a-falsehoods).
- [x] **F8** — added root `LICENSE` (MIT, © 2026 kninetimmy) + `THIRD-PARTY-NOTICES.md`
  covering both models (BGE-small MIT, ms-marco reranker Apache-2.0 — verified on the
  upstream cross-encoder HF card), the 7 tree-sitter grammar crates (MIT), uPlot (MIT),
  and the vendored tiktoken vocab (MIT); full MIT + Apache-2.0 texts reproduced.
  — 2026-07-05, PR-A (wave0/pr-a-text).
- [x] **F14/P9** — removed the non-existent `--json` from `integrations enable-k9` in
  both init-project variants (bootstrap-k9 `--json` verified valid, left intact).
  — 2026-07-05, PR-A (wave0/pr-a-text).
- [x] **F16/N13** — done in **PR-B** (message wording + exit-code fix share the same
  `cli/mod.rs` lines). — 2026-07-05, PR-B (wave0/pr-b-safe-code).
- [x] text-pass riders: **P8** (`/check-init-k9`, `/init-project-k9`, wrap-up
  "project-scoped" falsehood), **P10** (stale locate-reranker rationale in skills +
  `--help`), **P11** (AGENTS.md Codex-accounting note, every clause false), **P12**
  (root files cite a non-existent "re-render" MCP rule), **P17** (upgrade-resync omits
  OpenCode), **P18** (OpenCode recall stub `available_docs`), **N11** (sync courier prose —
  also corrected the same wording in `src/config/mod.rs` and `src/commands/sync.rs`).
  — 2026-07-05, PR #13 (wave0/pr-a-falsehoods).

### PR-B — safe code (zero decisions)
- [x] **F2a** — machine workaround: set `claude_transcripts_dir`, run `metrics rescan`;
  accounting restored (120→129 sessions). — 2026-07-05, local config (not committed; per-machine)
- [x] **F2b** — code fix: `detect_claude_transcripts_dir`/`detect_codex_sessions_dir` use
  `db::home_dir()`; new `encode_claude_project_dir` strips `\\?\` and maps `/`,`\`,`:`→`-`
  (no leading dash on Windows), preserving the Unix leading-dash shape byte-for-byte.
  Pure-fn Windows+Unix shape test. — 2026-07-05, PR-B (wave0/pr-b-safe-code).
- [x] **F3** — reconciler caps an open session's window at `started_at +
  OPEN_SESSION_MAX_HOURS` (12h) instead of `now`, so a sync-adopted zombie can't swallow
  local recalls; `sync adopt` also closes foreign open `session_metrics` rows post-swap.
  Lib + integration tests both directions. — 2026-07-05, PR-B (wave0/pr-b-safe-code).
- [x] **F6** — Windows staged handoff exits `3` (handed off, pending) not `0`; finish-child
  `unwrap_or(0)`→`unwrap_or(1)`; `last_upgrade.json` gains a `state` field; new `memhub
  upgrade --verify-last` (exits 0/1/3). — 2026-07-05, PR-B (wave0/pr-b-safe-code).
- [x] **F7** — `apply_all` refuses a DB whose `schema_migrations` holds a version newer
  than the compiled list (points at `memhub upgrade`); `upsert_project` ratchets
  `schema_version` with `MAX(...)` so an older binary can't downgrade it. Tests.
  — 2026-07-05, PR-B (wave0/pr-b-safe-code).
- [x] **F9** — pinned `PRAGMA recursive_triggers = OFF` in both `open_connection` and
  `open_code_index`; corrected the inverted migration 0014 comment (trigger fires on
  direct chunk deletes only, not the FK cascade). — 2026-07-05, PR-B (wave0/pr-b-safe-code).
- [x] **F10** — reordered `embed_missing`: query `missing` first and early-return before
  the full-table embedding-cache decode (skips it per warm `locate`).
  — 2026-07-05, PR-B (wave0/pr-b-safe-code).
- [x] **F12/P16** — `snapshot()` runs `check` first and refuses on `drive-ahead`/`diverged`
  without `--force`/`force=true` (CLI `--force`, MCP `sync_snapshot force`); all three
  wrap-up variants gain a `sync check` step. Test both branches.
  — 2026-07-05, PR-B (wave0/pr-b-safe-code).
- [x] **F15/P7** — mapped `codex-mcp-client`→`codex` in `normalize_client_name`; warn on
  unmapped client names. Test. — 2026-07-05, PR-B (wave0/pr-b-safe-code).
- [x] **F16/N13** code half — `doc rm`/`doc show` `process::exit(1)` on an ident miss
  (JSON `{"removed"/"found": false}` body still printed).
  — 2026-07-05, PR-B (wave0/pr-b-safe-code).

### Gated (later in Wave 0)
- [x] **F1** — added `--json` to `status`, `init`, `fact list`, `decision list`, `command
  list`; adopted the noun-keyed wrapped-object JSON convention (`{"status":{...}}`,
  `{"facts":[...]}`, etc.) per **Q29**, and migrated `doc ls --json` from a bare array to
  `{"docs":[...]}` to match. `tests/cli_args.rs` adds the clap-surface smoke test (N24).
  — 2026-07-05, PR #18 (feat/15-json-cli-flags).
- [x] **F11** — MCP `doc_add` path confinement: canonicalize the agent-supplied path,
  require repo-root **or** a new `[doc] allowed_dirs` entry, apply the existing
  `PathMatcher` deny-list on top (a deny-listed path inside repo-root still refuses).
  Gate lives on the MCP path only (`doc_add_impl`); CLI `doc add` / `prepare_doc` is
  unchanged and stays unrestricted. **Gated on Q39** (resolved: repo-root + allowlist,
  not repo-root-only). — 2026-07-05, PR #17.

### Immediate / no-PR
- [x] **F17/N20** — committed `docs/reviews/` (review + tracker), reachable from the Mac
  once PR #11 merges to main. — 2026-07-05, PR #11.
  *(N20's wrap-up untracked-path guard is a separate later enhancement, not this fix-now.)*

---

## Wave 1 — Loud states  (gating: Q35) — COMPLETE
- [x] S2 `memhub doctor` (absorbs `/check-init`, config-key validation, D1 integrity, X4
  health line, plus probes P1/P4, N21/N23). — 2026-07-06, PR #25 (issue #21).
- [x] S3 `status` refresh (subsystem states; K9 lines only when detected) — reuses doctor's
  side-effect-free checks. — 2026-07-06, PR #27 (issue #22).
- [x] D1 integrity surface (FTS rowcounts, FTS5 integrity-check, orphan embeddings,
  `integrity_check`+`foreign_key_check`) — absorbed into doctor (`check_integrity`).
  — 2026-07-06, PR #25 (issue #21).
- [x] X4 session-accounting health line — absorbed into doctor (`check_metrics_health`).
  — 2026-07-06, PR #25 (issue #21).
- [x] D9 map SQLITE_BUSY / SQLITE_LOCKED to a friendly error — shipped with D5/Q35
  (`synchronous = NORMAL` at both `open_connection` and `open_code_index`) in the same
  PR. — 2026-07-05, PR #23 (issue #20).
- [x] Wave 1·A follow-up — doctor flags `integrations.k9.enabled` drift and warns on sync
  `project_id_mismatch` / `schema_blocks_adopt`; module-doc accuracy fix.
  — 2026-07-06, PR #28 (issue #26).

## Wave 2 — Token diet  (gating: Q21–Q25 ✓ · Q41 ✓ — gate cleared 2026-07-06)
<!-- Rulings: Q21 generate AGENTS.md from CLAUDE.md (derived, content-equal parity).
     Q22 ~2,500-tok target; inline: Guardrails, Session Continuity, Delegation,
     stale-embeddings gate, sync_adopt gate. Q23 implement versioned managed block.
     Q24 fix — register `memhub serve` (confirmed 0 CLIs; registration lands Wave 4/Q40).
     Q25 opt-in `[audit] user_md_path`. Q41 fail-safe — keep a ~10-line compact routing
     block in AGENTS.md for Codex/OpenCode, trim CLAUDE.md's verbose routing; spike
     deferred to Wave 4 to decide whether even that block can go. -->
- [x] C1 rewrite repo CLAUDE.md to ~2,500-token target — cut 8,272→1,166 cl100k tok;
  subsystem prose relocated content-preserving into new `docs/reference/operations.md`.
  — 2026-07-06, PR #34 (issue #30).
- [x] C2 generate AGENTS.md from CLAUDE.md (pure `generate_agents_md`); upgraded
  `skill_parity` from `##`-header parity to content-equality. — 2026-07-06, PR #34 (issue #30).
- [ ] C3 trim user-global CLAUDE.md (−40%) — **no issue yet**; edits the machine-local
  `~/.claude/CLAUDE.md` outside this repo → user-gated, scope separately.
- [x] C4 root managed block — versioned `memhub:managed-block` in `src/managed_block.rs`,
  propagates CLAUDE.md→AGENTS.md unchanged via the generator. — 2026-07-06, PR #35 (issue #31).
- [x] C5 `memhub audit md [--json] [--strict]` — read-only linter over root memory files:
  size (2,500 target / 2,600 ceiling), AGENTS.md drift (reuses #30 `generate_agents_md`),
  managed-block presence/version (reuses #31 `parse_managed_block`), keystone phrases
  (single-source const now shared with `skill_parity`), malformed/missing CLAUDE.md,
  opt-in `[audit] user_md_path`. `--json` → `{"audit_md":{...}}` (Q29); `--strict` exits 1
  iff ≥1 finding. No new deps, no DB writes. — 2026-07-06, PR #37 (issue #32).
- [x] C6 `/audit-md` skill (judgment layer) — read-only skill across all 3 agents
  (Claude/Codex/OpenCode + OpenCode command wrapper) wrapping `audit md --json`, with
  per-finding-id fix recommendations and a forward-compatible fallback for unknown ids;
  `skill_parity` stays green. — 2026-07-06, PR #38 (issue #33).
- [x] C7 `memhub upgrade` nag line — best-effort `check_audit_md` runs `audit md` under
  the fresh binary in `finish_phase` + `dry_run_report`, emits a single nag line only when
  findings exist (silent on clean), additive `audit_md` JSON field; degrades to a `Warn`
  row on audit error, structurally never fails the upgrade (returns `AuditNag`, not
  `Result`). New `tests/upgrade_audit_nag.rs`. — 2026-07-06, PR #38 (issue #33).
- [x] rider N4 keystone-phrase parity test — landed with C2. — 2026-07-06, PR #34 (issue #30).
- [ ] rider N1 MCP description diet — deferred to Wave 4 (same PR as Q40/R2)

## Wave 3 — Lifecycle  (gating: Q1–Q6)
- [ ] L1 `memhub fact verify` (no add-upsert side effects) + wrap-up per-item step
- [ ] L2 un-silence staleness: `[retrieval] fact_stale_after_days` + demote/flag
- [ ] L3 wire supersession (migration 0018 `facts.superseded_by`; verbs; hydrate/score/render)
- [ ] L4 `memhub review stale` audit queue
- [ ] L5 accept-time contradiction probe
- [ ] L6 optional age decay (default-off) — eval-gated, last
- [-] L7 hard archival — **recommend against / defer** (per review)
- [ ] rider: N28 hermetic retrieval golden fixture

## Wave 4 — Performance  (gating: Q17–Q19, Q24, Q40)
- [ ] R1 pre-warm models in `mcp::serve`
- [ ] R2 register MCP server (per Q40 all-three-CLIs)
- [ ] R3 batch doc-chunk embedding
- [ ] R5 `locate --no-refresh`
- [ ] R6 MCP recall bundle trim (+ add `rerank_score`)
- [ ] R8 debounce metrics maintenance
- [ ] R9 consolidate retrieval helpers; delete dead `min_vector_score`
- [ ] R10 evals: doc-chunk/global fixture sections + warm-latency p50
- [ ] R11 knob hygiene (split locate vs recall weights; `TEST_PATH_PENALTY` config)
- [ ] R12 record surface (CLI vs MCP) column in `recall_metrics`
- [ ] R7 int8-quantized ONNX — **separate eval-gated experiment** (Q18)
- [ ] R4 = F10 (tracked in Wave 0)

## Wave 5 — Upgrade / GC  (gating: Q12–Q16)
- [ ] U2 extend gc to `build/memhub-*` OUT_DIRs + `examples/` hashes
- [ ] U3 call `sweep_stale_staging` from `gc::run`
- [ ] U4 consolidate `tests/*.rs` into 1–3 harness binaries
- [ ] U5 revisit two shipped gc exclusions (superseded incremental; >100MB multi-hash third-party)
- [ ] U6 skill-resync honesty (orphan report, install manifest, symlink-fallback message)
- [ ] U7 degrade-don't-die on corrupt registry / PATH-shadow IO error
- [ ] U8 backups retention cap (N newest; legacy k9-bootstrap-backup prompt)

## Wave 6 — Wrap-up  (gating: Q7–Q11)
- [ ] W1 verbosity knob + `memhub wrapup-policy --json`
- [ ] W2 level semantics (minimal/standard/full/transcript)
- [ ] W3 transcript mode (zst archive + pointer row; excluded from recall/export)
- [ ] W4 optional `kind` tag on facts
- [ ] W5 `SourceType::Note` retrievable on explicit request
- [ ] W6 skill content fixes ×3 agents (record_command, --summary, OpenCode contract)

## Wave 7 — Cross-machine  (gating: Q30–Q33)
- [ ] X1 `sync check --diff`
- [ ] X2 import-time printed checklist (+ fix inverted docs hint)
- [ ] X3 metrics consolidation (after F2/F3 baselines accrue)
- [ ] X5 marker temp+rename; degrade unparseable marker
- [ ] X6 adopt pre-swap `BEGIN IMMEDIATE` probe

## Wave 8 — Infra  (gating: Q37–Q38)
- [ ] G1 CI (windows+macos build/test, SHA-keyed model cache, branch protection, cargo audit)
- [ ] G2 write-time secret-pattern warning

## Wave 9 — Housekeeping  (gating: Q26–Q27, Q36)
- [ ] S5 docs prune (NEXT_STEPS.md, current-architecture.md, milestones.md, roadmap archive, Source PRD/)
- [ ] G3 legacy surface disposition (`ingest-git`/`search`/`stats`/`bootstrap-k9`)
- [ ] D10 retire `chunks`/`chunk_fts` (after G3)
- [ ] D4 `memhub db maintain`
- [ ] D7 rendered-ledger cap
- [ ] D8 migration sha256 checksums + drift warning
- [ ] §10.6 minor gaps as judged (MEMHUB_LOG docs, fuzz tests, recovery runbook, file perms, SECURITY.md)

---

## Decisions (§11 + §13/§14) — resolution tracker

Resolve per wave. Recommendations are in the review; mark here when the user rules.

**Lifecycle:** [ ] Q1 [ ] Q2 [ ] Q3 [ ] Q4 [ ] Q5 [ ] Q6
**Wrap-up:** [ ] Q7 [ ] Q8 [ ] Q9 [ ] Q10 [ ] Q11
**Upgrade/GC:** [ ] Q12 [ ] Q13 [ ] Q14 [ ] Q15 [ ] Q16
**Retrieval:** [ ] Q17 [ ] Q18 [ ] Q19 [ ] Q20
**CLAUDE.md:** [x] Q21 [x] Q22 [x] Q23 [x] Q24 [x] Q25 *(all resolved 2026-07-06 — Wave 2 gate: Q21 generate AGENTS.md from CLAUDE.md (derived, content-equal parity); Q22 accept ~2,500-tok target + inline {Guardrails, Session Continuity, Delegation, stale-embeddings gate, sync_adopt gate}; Q23 implement versioned managed block; Q24 fix — register `memhub serve` (confirmed registered in 0 CLIs; work lands Wave 4/Q40); Q25 opt-in `[audit] user_md_path`)*
**Surfaces:** [ ] Q26 [ ] Q27 [ ] Q28 [x] Q29 *(resolved 2026-07-05 — wrapped noun-keyed objects; `doc ls` migrated; PR #18)*
**Cross-machine:** [ ] Q30 [ ] Q31 [x] Q32 *(resolved — decision 134; Mac lineage not adopted, ported as Windows 128–133)* [ ] Q33
**DB:** [ ] Q34 [x] Q35 *(resolved 2026-07-05 — `synchronous = NORMAL` alongside WAL across all DB surfaces; memhub decision 140 / review D5)* [ ] Q36
**Infra:** [ ] Q37 [ ] Q38 [x] Q39 *(resolved 2026-07-05 — repo-root + `[doc] allowed_dirs` allowlist for MCP `doc_add`; PR #17)*
**Parity/free-form:** [ ] Q40 [x] Q41 *(resolved 2026-07-06 — adopt fail-safe: keep a ~10-line compact routing block in AGENTS.md for Codex/OpenCode, trim CLAUDE.md's verbose routing; the per-CLI instructions-delivery spike is deferred to Wave 4)* [ ] Q42 [ ] Q43 [ ] Q44 [ ] Q45 [ ] Q46 [ ] Q47 [ ] Q48 [ ] Q49 [ ] Q50 [ ] Q51 [ ] Q52 [ ] Q53 [ ] Q54 [ ] Q55 [ ] Q56
