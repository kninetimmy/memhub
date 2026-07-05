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
| 0 | Fix-now defects | 4 / 17 | Q29 (F1), Q39 (F11) |
| 1 | Loud states (doctor/status/integrity) | 0 / 5 | Q35 |
| 2 | Session-start token diet | 0 / 7 | Q21–Q25, **Q41 (spike gates trim)** |
| 3 | Staleness / lifecycle | 0 / 7 | Q1–Q6 |
| 4 | Retrieval performance | 0 / 12 | Q17–Q19, Q24, Q40 |
| 5 | Upgrade / GC hardening | 0 / 8 | Q12–Q16 |
| 6 | Wrap-up policy / verbosity | 0 / 6 | Q7–Q11 |
| 7 | Cross-machine sync / metrics | 0 / 6 | Q30–Q33 |
| 8 | CI / infra / licensing | 0 / 2 | Q37–Q38 |
| 9 | Housekeeping | 0 / 7 | Q26–Q27, Q36 |

Decisions resolved: 1 / 56 (Q32).

---

## Wave 0 — Fix-now (broken today; all small)

Two PRs: **PR-A text/docs** (no decisions) and **PR-B safe code** (no decisions), plus
two gated items (F1, F11).

### PR-A — text/docs (zero decisions)
- [x] **F13/P3** — fixed invalid YAML frontmatter: folded `description:` to block
  scalar (`>`) in recall/locate/doc/metrics templates ×3 agents + added dependency-free
  regression guard `skill_frontmatter_descriptions_are_valid_yaml_scalars` in
  `tests/skill_parity.rs`. — 2026-07-05, PR-A (wave0/pr-a-text).
  *Installed-copy resync via `memhub upgrade` is a post-merge machine step (still pending).*
- [ ] **F5** — remove skill/MCP description falsehoods (docs-in-default flipped by
  decision 90; `sync-md` "managed block" that doesn't exist; "unproven until M11 PR5"
  remnant). Files: `templates/skills/**`, `src/mcp/mod.rs`.
- [ ] **F4** — README: `[retrieval]` duplicate-table instruction + add the Claude MCP
  registration step; fix self-contradictory `propose_fact` global bullet.
- [x] **F8** — added root `LICENSE` (MIT, © 2026 kninetimmy) + `THIRD-PARTY-NOTICES.md`
  covering both models (BGE-small MIT, ms-marco reranker Apache-2.0 — verified on the
  upstream cross-encoder HF card), the 7 tree-sitter grammar crates (MIT), uPlot (MIT),
  and the vendored tiktoken vocab (MIT); full MIT + Apache-2.0 texts reproduced.
  — 2026-07-05, PR-A (wave0/pr-a-text).
- [x] **F14/P9** — removed the non-existent `--json` from `integrations enable-k9` in
  both init-project variants (bootstrap-k9 `--json` verified valid, left intact).
  — 2026-07-05, PR-A (wave0/pr-a-text).
- [ ] **F16/N13** text half — `doc rm`/`doc show` messaging (exit-code fix is code, see PR-B).
- [ ] text-pass riders: **P8** (`/check-init-k9`, `/init-project-k9`, wrap-up
  "project-scoped" falsehood), **P10** (stale locate-reranker rationale in skills +
  `--help`), **P11** (AGENTS.md Codex-accounting note, every clause false), **P12**
  (root files cite a non-existent "re-render" MCP rule), **P17** (upgrade-resync omits
  OpenCode), **P18** (OpenCode recall stub `available_docs`), **N11** (sync courier prose).

### PR-B — safe code (zero decisions)
- [x] **F2a** — machine workaround: set `claude_transcripts_dir`, run `metrics rescan`;
  accounting restored (120→129 sessions). — 2026-07-05, local config (not committed; per-machine)
- [ ] **F2b** — code fix: `detect_claude_transcripts_dir` uses `db::home_dir()`, strips
  `\\?\`, encodes `:`/`\`→`-`, no leading dash; same HOME fix for Codex detector +
  Windows-shape test.
- [ ] **F3** — reconciler guard against attributing to open sessions older than N hours;
  post-adopt hygiene closing foreign `session_metrics` rows.
- [ ] **F6** — upgrade exit code 3 = "handed off, pending"; `unwrap_or(0)`→`unwrap_or(1)`;
  add `memhub upgrade --verify-last`.
- [ ] **F7** — `apply_all` refuses/warns on schema versions newer than the compiled list;
  stop the downgrade in `upsert_project`.
- [ ] **F9** — pin `PRAGMA recursive_triggers = OFF` in both `open_connection`s; fix the
  inverted migration 0014 comment.
- [ ] **F10** — reorder `embed_missing`: query `missing` first, early-return (skips a
  full-table vector decode per warm `locate`).
- [ ] **F12/P16** — `snapshot()` runs `check` first, refuses on `drive-ahead`/`diverged`
  without `--force`; touch all three wrap-up variants + one skill step.
- [ ] **F15/P7** — map `codex-mcp-client` in `normalize_client_name`; warn on unmapped
  client names.
- [ ] **F16/N13** code half — `doc rm`/`doc show` exit nonzero on ident miss (keep
  `{"found": false}` for `--json`).

### Gated (later in Wave 0)
- [ ] **F1** — add `--json` to `status`, `init` (+ `fact/decision/command list`).
  **Gated on Q29** (JSON shape convention).
- [ ] **F11** — MCP `doc_add` path confinement (canonicalize, require repo-root/allowlist,
  apply deny-list). **Gated on Q39.**

### Immediate / no-PR
- [~] **F17/N20** — commit `docs/reviews/` so the plan is reachable from the Mac.
  — in progress (this PR, branch `docs/improvement-review-tracking`)

---

## Wave 1 — Loud states  (gating: Q35)
- [ ] S2 `memhub doctor` (absorbs `/check-init`, config-key validation, D1 integrity, X4 health line, plus probes P1/P4, N21/N23)
- [ ] S3 `status` refresh (subsystem states; K9 lines only when detected)
- [ ] D1 integrity surface (FTS rowcounts, FTS5 integrity-check, orphan embeddings, `integrity_check`+`foreign_key_check`)
- [ ] X4 session-accounting health line
- [ ] D9 map SQLITE_BUSY to friendly error

## Wave 2 — Token diet  (gating: Q21–Q25; **Q41 spike gates any trim**)
- [ ] C1 rewrite repo CLAUDE.md to ~2,500-token target + doc-ingest addenda/operations.md
- [ ] C2 generate AGENTS.md from CLAUDE.md; upgrade `skill_parity` to content equality
- [ ] C3 trim user-global CLAUDE.md (−40%)
- [ ] C4 root managed block (implement versioned pointer, namespace vs the foreign P21 markers)
- [ ] C5 `memhub audit md [--json] [--strict]`
- [ ] C6 `/audit-md` skill (judgment layer)
- [ ] C7 `memhub upgrade` nag line
- [ ] rider: N1 MCP description diet (same PR as Q40/R2); N4 keystone-phrase parity test

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
**CLAUDE.md:** [ ] Q21 [ ] Q22 [ ] Q23 [ ] Q24 [ ] Q25
**Surfaces:** [ ] Q26 [ ] Q27 [ ] Q28 [ ] Q29
**Cross-machine:** [ ] Q30 [ ] Q31 [x] Q32 *(resolved — decision 134; Mac lineage not adopted, ported as Windows 128–133)* [ ] Q33
**DB:** [ ] Q34 [ ] Q35 [ ] Q36
**Infra:** [ ] Q37 [ ] Q38 [ ] Q39
**Parity/free-form:** [ ] Q40 [ ] Q41 [ ] Q42 [ ] Q43 [ ] Q44 [ ] Q45 [ ] Q46 [ ] Q47 [ ] Q48 [ ] Q49 [ ] Q50 [ ] Q51 [ ] Q52 [ ] Q53 [ ] Q54 [ ] Q55 [ ] Q56
