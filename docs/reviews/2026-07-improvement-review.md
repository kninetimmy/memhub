# memhub Improvement Review — July 2026

**Produced:** 2026-07-04, on the Windows machine, repo at commit `314f4ff`.
**Method:** eight parallel read-only research agents (one per review dimension) plus a
completeness critic, synthesized by the orchestrating session. All findings carry
`file:line` evidence gathered against the working tree; live-machine measurements
(disk sizes, DB contents, config state) were taken the same day.
**How to use this document:** it is written to be handed to an implementing session.
Section 11 collects every decision that needs the user's call — resolve those first;
everything else is implementable as specified. Section 12 proposes PR-sized waves.
Sections 13–14 (added 2026-07-05) extend the defect and decision lists — treat their
F13–F17 and Q40–Q56 as first-class peers of the originals.

> **PREREQUISITE SATISFIED (2026-07-05, memhub task 94 closed).** The cross-CLI parity
> review has run — results in **§13**. A same-session free-form pass (token economics,
> CLI ergonomics, workflow shape, wildcard) is recorded as **§14**. Method mirrored this
> document's: nine parallel agents (four parity surfaces, four lenses, one completeness
> critic doubling as a quality gate against manufactured findings), static analysis plus
> live smoke tests on this machine; the codex/opencode binaries were never invoked, only
> their on-disk configs read. Together the two sections add fix-now defects **F13–F17**,
> user decisions **Q40–Q56**, and in-place amendments to F1, F5, F12, Q24, and Q32
> (marked where they occur). The smells the original banner named (F1/F5 falsehoods, the
> OpenCode wrap-up stub, the AGENTS.md twin, `skill_parity.rs` depth) were all confirmed
> and are now evidenced findings rather than smells.

**Provenance / confidence legend** (every section verdict carries these):

- `[U]` — user-proposed avenue · `[E]` — evidence-driven finding from the sweep · `[C]` — reviewer addition, nobody asked
- Confidence: **High** (verified in code/on disk) / **Med** (strong inference) / **Low** (needs a spike)

**The cross-cutting theme.** The sweep's most damaging findings share one root cause:
*"best-effort, never fatal" has repeatedly decayed into "silently broken for weeks."*
Session accounting dead on Windows with no health signal; shipped skills invoking CLI
flags that don't exist; `gc` covering a shrinking fraction of the disk problem; upgrade
returning exit 0 on failure; facts silently vanishing from recall at 90 days. memhub's
politeness is its biggest weakness. Many of the highest-value proposals below are the
same idea wearing different clothes: **make silent states loud** (doctor command,
integrity checks, misconfig warnings, honest exit codes, health lines in `status`).

---

## 0. Executive summary — ranked order, impact per effort

If I owned this tool, I would do these in this order:

1. **Wave 0: fix-now batch (§1).** Twelve defects that are live today, each small.
   Two shipped skills are broken on their primary paths; session accounting has been
   dead on this machine for ~6 weeks; a README instruction can brick a fresh install.
2. **Session-start token diet (§6).** Repo CLAUDE.md ≈8,360 tokens + a hand-mirrored
   AGENTS.md twin + PROJECT.md ≈ **16.5k tokens before any work, every session, twice
   (Claude + Codex)**. Target −70% on the repo file, −40% on the user-global file,
   AGENTS.md generated not mirrored. Highest *recurring* ROI in this review, and the
   moved detail is already in the DB — recall serves it on demand.
3. **Staleness/lifecycle overhaul (§2).** The user's core question, and the evidence
   inverted it: the problem is not stale data polluting recall — it is **valid facts
   silently vanishing** at a hardcoded 90 days. Verify verb → configurable/un-silenced
   window → wire the (already-in-schema) supersession → audit queue.
4. **MCP-first performance (§5).** Register the memhub MCP server (it is not registered
   on this machine — the best routing instructions in the system currently reach zero
   sessions), pre-warm models in the long-lived server, batch doc embedding. Turns
   ~2–3.5 s per recall into ~300 ms without violating the no-daemon invariant.
5. **Upgrade/GC hardening (§4).** Honest exit codes; extend gc to the ~3 GiB of
   uncovered memhub-owned `build/` dirs; consolidate 31 test binaries × 218 MB of
   embedded models (~7.5 GiB structural); revisit two shipped exclusions whose
   rationale is now measurably false.
6. **Trust boundary (§10).** MCP `doc_add` path confinement, push-side sync gate,
   write-time secret-pattern warning. Small fixes to real holes.
7. **`memhub doctor` + integrity surface (§7, §9).** The umbrella that keeps all of the
   above loud: absorb `/check-init`, validate config keys, FTS/embedding integrity
   counts, session-accounting health, schema-version sanity.
8. **Wrap-up policy + verbosity knob + ontology polish (§3).** Binary-rendered policy
   (`memhub wrapup-policy`), four verbosity levels with transcript mode as disk archive
   + pointer row (never embedded), a `kind` tag on facts instead of new categories,
   session notes retrievable on explicit request.
9. **CI + licensing (§10).** Public repo, zero CI, no LICENSE file despite an MIT
   manifest claim and redistributed Apache-2.0 model weights. A Windows CI leg would
   have caught two of the bugs this sweep found.
10. **Metrics right-sizing + sync divergence UX (§8).** Fix component B, let the
    empirical baseline accrue, then retire the stale ledger-counterfactual framing;
    `sync check --diff` so the one lossy operation stops being confirmed blind.
11. **Housekeeping (§7, §9, §10).** Docs prune (root `NEXT_STEPS.md` still says
    "finish Milestone 3"), legacy-surface disposition (`ingest-git`/`search`/`stats`/
    `bootstrap-k9`), DB maintain op, migration checksums.

**Verdicts on the six user-proposed avenues:**

| Avenue | Verdict |
|---|---|
| Staleness handling | **Pursue — stronger than proposed.** Current behavior is arguably a bug (silent 90-day vanishing). Demote-and-flag, never delete. |
| Wrap-up storage criteria | **Pursue.** Criteria are fuzzy at exactly the points that shape the DB long-term; OpenCode variant has no criteria at all. |
| More/less granular categories | **Neither.** Keep the category count; add an optional `kind` tag on facts and make notes retrievable-on-request. New top-level categories would add routing burden for no retrieval gain. |
| Verbosity knob | **Pursue, with guardrails.** Four levels; transcript mode as disk archive + pointer row, excluded from recall and export. Secret handling needs a user decision (§11 Q8). |
| CLAUDE.md audit | **Pursue — evidence is overwhelming.** Layered: deterministic `memhub audit md` + judgment-layer `/audit-md` skill + one-line `upgrade` nag. |
| Install/upgrade robustness | **Pursue.** 28 GB decomposed to root causes; exit-0-on-failure confirmed as a defect class; concrete hardening list ready. |

---

## 1. Fix-now defects (broken today; all small)

Each item names its home section for full context. These can ship as one or two PRs.

| ID | Defect | Fix | Evidence |
|---|---|---|---|
| F1 | `/check-init` and `/init-project` (all 3 agents) invoke `memhub status --json` / `memhub init --json` — neither flag exists; both skills die with clap usage errors | Add `--json` to `status` and `init` (and `fact list`, `decision list`, `command list` — same task-89 class); settle the JSON shape convention first (§11 Q29) | `src/cli/args.rs:24-28`, `templates/skills/claude/check-init.md:35`, `init-project.md:129` |
| F2 | Session accounting dead on Windows: transcript-dir auto-detection builds a garbage path (Unix-only encoding, raw `HOME`), persists `""`, scraper early-returns forever, silently | ~20-line fix in `detect_claude_transcripts_dir` (use `db::home_dir()`, strip `\\?\`, encode `:` and `\`→`-`, no leading dash) + same HOME fix for the Codex detector + a Windows-shape test. **This machine today:** set `claude_transcripts_dir = 'C:\Users\Kninetimmy\.claude\projects\C--Users-Kninetimmy-memhub'` and run `memhub metrics rescan` | `src/commands/metrics.rs:1070-1083`, `src/metrics/session_scraper.rs:79-84` |
| F3 | Zombie session attribution: a Mac session row with `ended_at = NULL` arrived via sync adopt; the reconciler's `COALESCE(ended_at, now)` gives it an infinite window — all 20 Windows recalls were attributed to it | Reconciler guard (don't attribute to open sessions older than N hours); post-adopt hygiene closing/tagging foreign `session_metrics` rows | `src/metrics/maintenance.rs:20-25` |
| F4 | README tells all four install paths to *append* `[retrieval]` to a config that already has that table — duplicate-table TOML, every subsequent command fails to parse. Claude quickstart also never registers the MCP server (Codex/OpenCode both get snippets) | Reword to "set `mode = \"hybrid\"` under the existing `[retrieval]` table"; add the Claude MCP registration step | `README.md:113,193,285,364-367`; registration gap `README.md:87-156,784-788` |
| F5 | Shipped skills/descriptions state falsehoods: `recall.md`+`doc.md` (claude, codex) still say docs never enter the default bundle (decision 90 flipped it; opencode variant is already correct); `init-project.md` claims `sync-md` populates a root-file "managed block" that does not exist; MCP descriptions for `doc_add`/`recall`/`locate` carry the same doc-staleness plus a "unproven until M11 PR5" remnant | Text-only resync pass across `templates/skills/` and `src/mcp/mod.rs` descriptions | `templates/skills/claude/recall.md:50-52,113-132`, `doc.md:3,13`, `init-project.md:196-199`, `src/mcp/mod.rs:605,654,665` |
| F6 | Upgrade lies about failure: Windows staged handoff exits 0 unconditionally; a signal-killed `--finish` child maps to success via `unwrap_or(0)` | Exit code 3 = "handed off, result pending" + print the poll command; `unwrap_or(0)` → `unwrap_or(1)`; add `memhub upgrade --verify-last` reading `~/.memhub/last_upgrade.json` (exits 0/1/3) | `src/commands/upgrade.rs:258,463` |
| F7 | An older binary opening a newer DB silently **downgrades** `projects.schema_version` and writes into a schema it doesn't understand (only sync adopt guards newer-schema) | In `apply_all`: refuse (or warn + skip the stomp) when `schema_migrations` contains versions not in the compiled list; stop the downgrade in `upsert_project` | `src/db/mod.rs:219-233`, `src/db/migrations.rs:80-110` |
| F8 | Public repo, no LICENSE file (manifest says MIT; GitHub reports no license ⇒ "all rights reserved"); binary redistributes Apache-2.0 model weights with NOTICE obligations, MIT grammars, vendored uPlot | Add root `LICENSE` (MIT) + `THIRD-PARTY-NOTICES.md` (both models, six grammars, uPlot — build.rs header comments already carry half the provenance). Verify the Xenova MiniLM export's license on its HF card | `Cargo.toml` license field; `src/retrieval/embeddings.rs:19`, `rerank.rs:24` |
| F9 | The doc-chunk cleanup invariant relies on SQLite's *default* `recursive_triggers = OFF` (never pinned), and migration 0014's comment documents the **opposite** of the actual behavior — a trap for future writers | Pin `PRAGMA recursive_triggers = OFF` in both `open_connection`s; fix the 0014 comment | `src/db/mod.rs:205-217`, `src/code_index/mod.rs:47-58`, `migrations/0014_documents.sql:123-126` |
| F10 | Every warm `locate` decodes all 1,845 code-index embedding vectors into a HashMap *before* discovering there is nothing to embed | Reorder: query `missing` first, return early | `src/code_index/mod.rs:466-492` |
| F11 | MCP `doc_add` is an ungated arbitrary-file read into retrievable memory: agent-supplied path, no repo-root confinement, deny-list not consulted — an agent (or a prompt injection) can durably ingest `~/.aws/credentials` | MCP path only: canonicalize, require under repo root (or config allowlist — §11 Q40), apply the existing `PathMatcher` deny-list. CLI stays unrestricted (user-typed) | `src/mcp/mod.rs:266-277`, `src/commands/doc.rs:67-71` |
| F12 | Push-side sync clobber is ungated: `snapshot` deletes and rewrites the remote with no check of its state; the wrap-up habit is the trigger. "The lossy case is operator-gated" is only true on pull | `snapshot()` runs `check` first, refuses on `drive-ahead`/`diverged` without `--force`/`force=true`; wrap-up skill gains one step | `src/commands/sync.rs:229-267,632-662`, `templates/skills/claude/wrap-up.md:181-191` |

---

## 2. Memory staleness & lifecycle — `[U]`, confidence High

**Verdict: pursue, but the problem is the inverse of the one proposed.** The user
feared stale data polluting the DB. The shipped behavior is that **facts silently
vanish from recall 90 days after last verification** — hardcoded window
(`FACT_STALE_AFTER_DAYS = 90`, `src/models/mod.rs:13`), filtered before scoring when
`include_stale_by_default = false` (the default; `src/retrieval/recall.rs:384-393`),
with no warning of any kind. Staleness applies to facts only; decisions/tasks/docs
hardcode `is_stale: false`.

**Key findings:**

- **Supersession is schema theater.** `decisions.status` (`active/superseded/draft`)
  and `superseded_by` have existed since migration 0001 — no CLI/MCP verb ever sets
  them; recall hydration doesn't even SELECT them (`recall.rs:972-974`). The
  "D123 supersedes D122" convention lives only in rationale prose.
- **Refreshing a fact launders it.** The only way to touch `verified_at` is re-running
  `fact add`, which resets `confidence` to 1.0 and **overwrites `source`**
  (`src/commands/fact.rs:38-45`) — re-verifying an agent-attributed fact silently
  rewrites its provenance.
- **Done tasks recall as if live** — hydration selects no status (`recall.rs:995-1013`);
  the rendered ledger lists every task and decision ever (`src/render/mod.rs:188-218`),
  growing monotonically.
- **Contradictions accumulate.** Same-key fact add = silent last-writer-wins, prior
  value captured nowhere. Differently-phrased contradictions both surface with no link.
  `review accept` never dedupes (`src/commands/review.rs:156-171`).
- **Confidence is vestigial** — always written 1.0, never enters scoring
  (`recall.rs:1089-1090`). The PRD envisioned tiered initial confidence
  (verified 0.7 / observed 0.9 / user 1.0), decay, and confidence-gated auto-accept
  (`docs/reference/memhub-prd.md:218-219,322-327,344-351`); only the stale flag and
  the review flow shipped.
- Pending-write expiry (30 d) exists but is **manual-only**; PRD §11.3 implied automatic.

**Proposals, in recommended order** (all demote/annotate — nothing deletes):

| ID | Proposal | Cost | Risk |
|---|---|---|---|
| L1 | `memhub fact verify <id\|key>` — touch `verified_at` without the add-upsert's side effects; wrap-up gains a "re-verify the N oldest facts" per-item step inside the existing approval gate | No migration; small CLI + skill edit | Rubber-stamping if phrased "verify all" — keep per-item. Keep verify off MCP or stage it (agent self-verification is exactly what the guardrail forbids) |
| L2 | Un-silence staleness: promote 90 to `[retrieval] fact_stale_after_days`; change default handling from silent exclusion to **demote + `stale:true` flag** (or at minimum a `stale_facts_excluded` warning with a count) | Config key + score/warning change + eval sweep | A bad default hides valid memories — demote is the no-loss posture; §11 Q1 |
| L3 | Wire supersession: migration 0018 adds `facts.superseded_by`; new verbs `fact/decision supersede <old> --by <new>`; recall selects status/superseded_by and demotes (new `superseded_penalty`) with a `superseded_by: N` tag; render annotates rather than hides | 1 small migration + verb + hydrate/score/render edits + eval re-run | Low — reversible link, nothing deleted. User-gated on CLI; MCP at most a staged `propose_supersede` |
| L4 | `memhub review stale` audit queue: read-only union of facts near horizon, done tasks older than N, expired pending writes, and docs whose on-disk hash no longer matches `documents.content_hash`; each row suggests an action executed only through existing verbs; one-line count in `status` | One command, no migration | None (read-only); keep it pull-based to avoid nag fatigue |
| L5 | Accept-time contradiction probe: inside `review accept`, similarity-probe the incoming payload against existing rows (same-key or rerank logit above threshold); on hit, require `--supersede N` or `--force`. Same-key `fact add` starts logging the prior value into `writes_log.reason` | Read-only recall call inside accept + CLI UX | False-positive friction — advisory, one prompt, never auto-resolving |
| L6 | Optional continuous age decay: `[retrieval.scoring] age_half_life_days = 0` (off) keyed on `verified_at` (facts) / `updated_at` (done tasks); decisions excluded — standing policy retires by supersession, not age | Config + score change + eval sweep; default-off = byte-identical | Under the reranker, decay only shifts pool membership — document the limited effect |
| L7 | Hard archival — **recommend against / defer.** Highest cost, only proposal that can genuinely lose access to a valid memory; L2+L3 deliver the benefit softly | — | — |

Also: retire or repurpose the vestigial confidence field (§11 Q5), and make
pending-write expiry automatic at `open_project` (§11 Q6).

---

## 3. Wrap-up policy, category granularity, verbosity knob — `[U]`, confidence High

**Verdict: pursue.** The storage criteria exist only as prompt text, triple-maintained
across agent variants, fuzzy at exactly the boundaries that shape the DB long-term —
and one of the three copies is empty.

**Key findings:**

- The Claude/Codex wrap-up skills carry seven draft items (header still says "five")
  with judgment-only routing rules: "settled enough to record", "real change",
  "durable" undefined. **The OpenCode variant is an 18-line stub with none of the
  criteria** (`templates/skills/opencode/wrap-up/SKILL.md:11-18`) — one of three
  agents wraps up unconstrained by construction.
- The skill routes build/test/run commands to **facts** (`wrap-up.md:94`) even though
  the `commands` table with verified success/fail counts exists for exactly this
  (`src/commands/command.rs:63-80`).
- The skill never uses decision `--summary` — the feature proven to lift Recall@3
  76.5%→100% on jargon-titled decisions (decision 72) is absent from the wrap-up
  template (`wrap-up.md:134`).
- **The only session-history categories are the only non-retrievable ones**: session
  notes and state/arch narrative never enter recall (absent from `SourceType`); after
  10 sessions a note falls off PROJECT.md and is effectively dark.
- Newer ontology (docs, M9 `--global` promotion, `record_command`) never made it into
  the draft-assembly step.

**Proposals:**

| ID | Proposal | Cost | Risk |
|---|---|---|---|
| W1 | **Verbosity knob:** `[wrap_up] verbosity = "minimal" \| "standard" \| "full" \| "transcript"` + a read-only `memhub wrapup-policy --json` returning `{verbosity, instructions}` rendered from the binary. Skills shrink to: detect → run policy command → follow instructions → approval gate → write sequence. One source of truth for all three agents, per-repo variation for free | Config field ~10 lines; policy command ~100 lines; template rewrites ×4 | Trades "skill text iterates without recompile" for single-sourcing — §11 Q10; middle path: command returns only the level |
| W2 | **Level semantics:** minimal = `state set` (currently building / next up) + task closures — the floor that keeps turn-1 continuity working; standard = today's behavior; full = + mandatory summaries, arch-drift check, richer note, always-run triage; transcript = full + archive | Included in W1 | — |
| W3 | **Transcript mode:** copy the session JSONL to `.memhub/transcripts/<date>-<session-id>.jsonl.zst` + one pointer row in a new `session_transcripts` table (`CREATE TABLE IF NOT EXISTS`). Reuse `session_scraper`'s dir-resolution and session-id mapping. **Never embedded, never in default recall, excluded from export** (mirrors the M11 isolation rule). Retention via `[wrap_up] transcript_retention_days` | ~150 lines | Transcripts can contain secrets — deny-list filters paths, not content; §11 Q8. Disk growth: zstd + retention |
| W4 | **Ontology: tune, don't multiply.** Optional nullable `kind` tag on facts (`gotcha \| env \| preference \| command \| constraint`) surfaced in recall output and ledger — a checklist for the writing agent, not a new table. Untagged stays legal | One nullable column + surfaces | None — additive |
| W5 | Make session notes retrievable **on explicit request only**: add `SourceType::Note`, reachable via `source_types=["note"]`, never in the default bundle (exactly the pre-decision-90 docs pattern; test it the way `recall.rs:1677` tests docs) | Small migration + embed path | Near-zero if default-exclusion is tested |
| W6 | Skill content fixes: route verified commands to `record_command`; add `--summary` drafting to the decision template; give OpenCode the real contract; fold docs/global/commands into draft assembly | Text-only | None |

---

## 4. Install / upgrade / GC — `[U]`, confidence High

**Verdict: pursue.** The 28 GB mystery is fully decomposed; the upgrade pipeline is
thoughtfully built but structurally dishonest about failure on the Windows staged path.

**Where the 28 GB lives** (measured 2026-07-04; `gc --dry-run` would free only 2.0 GiB):

| Bucket | Size | Status |
|---|---|---|
| Current-set artifacts (libmemhub rlib+rmeta, memhub.exe, ~31 test exes × ~120–275 MB, ×2 profiles) | **~15–16 GiB** | Irreducible by gc — structural: 218 MB of `include_bytes!` ONNX multiplied into every binary |
| Stale `build/memhub-<hash>` OUT_DIRs (28 dirs, each staging ~216 MB of models) | **~3.0 GiB** | memhub-derived, gc doesn't cover — same append-only pathology gc was built for, one directory over |
| `debug/incremental` superseded `memhub-*` session dirs | **~3+ GiB** | Excluded by shipped decision whose rationale ("no comparable disk win") is now measured false (4.0 G vs 2.0 G) |
| Stale `ort_sys` rlibs (307 MB each, 3 stale copies) | **~0.9 GiB** | Excluded by "third-party never balloons" premise — false for ort_sys |
| gc-covered stale hashes | 2.0 GiB | Working as designed |
| Staged upgrade shim in %TEMP% (from May 31) | 272 MB | Swept only at the *next* upgrade — always lingers between runs |

**Upgrade failure-mode inventory (condensed):** staged handoff always exits 0 (F6);
signal-killed finish child reads as success (F6); Windows symlink-privilege fallback
*copies* but the success message still claims "symlink" — silently re-creating the
stale-shadow problem it was built to fix (`upgrade.rs:803-824`); skill resync
unconditionally overwrites same-named files (a user's own `~/.claude/commands/recall.md`
would be silently replaced) and never reports orphans; a corrupt registry aborts the
whole upgrade at `upgrade.rs:544` instead of degrading; PATH-shadow IO error aborts
post-install. Good news: per-repo migrate failures correctly don't abort the others;
`last_upgrade.json` is durable and truthful; staged-death leaves no half-swapped state.

**Proposals:**

| ID | Proposal | Cost | Risk |
|---|---|---|---|
| U1 | F6 (exit codes + `--verify-last`) — see §1 | Small | Wrappers treating nonzero as failure — still more honest |
| U2 | Extend gc to `build/memhub-*` OUT_DIRs + `examples/` hash-suffixed binaries (same keep-newest-per-stem rule) | Moderate | Low — worst case one re-staged model set |
| U3 | Call `sweep_stale_staging` from `gc::run` (kills the 272 MB inter-upgrade shim leak) | Tiny | None |
| U4 | **Consolidate `tests/*.rs` into 1–3 harness binaries** (`tests/main.rs` + `mod` pattern): ~7.5 GiB of current-set disk + much faster `cargo test` links. Attacks the bucket no gc can touch | Medium | Loses per-file `cargo test --test X` granularity — §11 Q13 |
| U5 | Revisit two shipped exclusions (user decision, §11 Q12): (a) prune *superseded* `incremental/memhub-*` session dirs; (b) narrow opt-in for >100 MB multi-hash third-party artifacts (ort_sys) | Small each | Touches documented decisions — do not act unilaterally |
| U6 | Skill-resync honesty: report orphans (never delete); keep a manifest/hash of memhub-installed files so future runs distinguish "ours" from "user's" (§11 Q15); fix the symlink-fallback message and return the mechanism used | Small–medium | Low |
| U7 | Degrade, don't die, on corrupt registry (fall back to source-repo-only + warning); PATH-shadow IO error degrades to warn | Small | None |
| U8 | Backups retention: cap `.memhub/backups/rendered\|markdown` (~107 files) at N newest; one-time prompt to delete the legacy `project.sqlite.k9-bootstrap-backup`; report-only mode first. (Note: `backups/sync/` is already single-slot — not unbounded) | Small | §11 Q16 for N |
| U9 | Alternative/adjunct to U4: a test-only seam loading models from disk instead of `include_bytes!` | Larger design change | Flag first — touches the bundling contract |

---

## 5. Retrieval efficiency & cost — `[C]`, confidence High

**Verdict: the model lifecycle is the whole cost story; the corpus is tiny**
(227 embeddings in project.sqlite, 1,845 in code_index). One CLI hybrid+rerank recall
≈ **2–3.5 s, of which ~275 ms is useful rerank work (~10–15% efficiency)** — both ONNX
models cold-load per process (`OnceLock` caches die with the CLI process), and
`include_bytes!` + `.to_vec()` additionally double-buffers 218 MB in heap
(`src/retrieval/embeddings.rs:47`, `rerank.rs:58`). The MCP server is the sanctioned
long-lived process (`src/mcp/mod.rs:30-47`) — but nothing pre-warms it, so the first
recall of every session still pays the full tax. Measured bundle sizes are healthy
(avg 375 cl100k tokens, median 187); **37% of recalls returned empty** and still paid
full model init.

**Proposals (ordered by value-per-effort; none require a daemon):**

| ID | Proposal | Win | Cost / risk |
|---|---|---|---|
| R1 | Pre-warm models in `mcp::serve` (background thread: one `embed_one` + tiny rerank at startup) | First recall of every session: ~2–3 s → ~300 ms | ~10 lines / none |
| R2 | Register the MCP server on this machine (see §6 — currently not registered at all) | Makes R1 matter; unlocks the warm path agents actually use | Config-only / none |
| R3 | Batch doc-chunk embedding (`doc.rs:278-293` embeds per-chunk in a loop; `index.rs:113-160` already shows the `embed_batch` pattern) | Large-doc ingest seconds → sub-second | Small / low |
| R4 | F10 (`embed_missing` reorder) — see §1 | Removes a full-table vector decode per warm locate | Trivial / none |
| R5 | `locate --no-refresh` flag for tight loops (skip `git ls-files` + stat pass) | Sub-100 ms repeat locates when warm | Small / stale-by-choice, explicit opt-in |
| R6 | Trim the MCP recall hit shape: drop `rank`, `score`, `fts_score`, `vector_score`, always-1.0 `confidence`; **add `rerank_score`** (the one score reflecting final order is currently the one agents can't see). CLI `--json` keeps the diagnostic shape | ~25–35% smaller bundles, zero capability loss | Small / grep skills for dropped-field references first; §11 Q19 |
| R7 | Int8-quantized ONNX variants (both HF repos publish them; new pinned SHAs in build.rs) | Binary 272 MB → ~90 MB; faster init/inference; shrinks the §4 disk multiplier at its source | Medium / **mandatory** golden-set re-runs + floor recalibration (`min_rerank_score`, `doc_min_rerank_score`) — quantization shifts logit distributions; revert if Recall@3 regresses; §11 Q18 |
| R8 | Debounce metrics maintenance (reconcile/prune at most hourly; keep the offset-gated scrape per-call) | Removes unbounded-window DELETEs from every command | Small / low |
| R9 | Consolidate duplicated retrieval helpers (fts match builder, normalize, cosine, byte codecs, four copies of `sha256_hex`) into `retrieval/`; delete the dead `min_vector_score` plumbing (`recall.rs:357,714`) | Divergence-proofing | Mechanical / none |
| R10 | Two cheap evals: (a) extend `retrieval_golden.json` with fixture-seeded doc-chunk and global-store sections — the two zero-coverage ranking paths; (b) per-query warm-latency p50 in `eval --json` (report, not gate) | Catches doc-floor/global-blend regressions + 10× latency regressions | Small / none |
| R11 | Knob hygiene: split `fts_weight`/`vector_weight` for locate vs recall (currently shared — tuning one silently retunes the other, `locate.rs:128,143`); promote `TEST_PATH_PENALTY` from hardcoded const to `[code_index]` config; document that `stale_penalty` is facts-only | Small | None |
| R12 | Record surface (CLI vs MCP) in `recall_metrics` — one column; answers "is CLI latency even felt?" with data (§11 Q17) | Informs R7 priority | Tiny |

---

## 6. CLAUDE.md / AGENTS.md + the audit capability — `[U]`, confidence High

**Verdict: pursue — the evidence is overwhelming.** A memhub session starts
**~16.5k tokens deep** (repo CLAUDE.md ≈8,360 + PROJECT.md ≈5,600 + user-global ≈2,480),
and AGENTS.md hand-mirrors another ≈8,470 for Codex sessions.

**Key findings:**

- **Triple bookkeeping, proven by the ledger itself:** the Retrieval / Code Index /
  Token Accounting / Sync / Upgrade sections re-narrate decision bodies that exist in
  the DB *with more detail*, and again in the PRD addenda. Decision 122's body
  literally records having to hand-patch three markdown files when one decision
  superseded another.
- **AGENTS.md is a near-verbatim twin** enforced only at section-name level by
  `tests/skill_parity.rs`; different line-wrapping makes drift undiffable.
- **Right division of labor:** CLAUDE.md keeps only what an agent needs *before its
  first tool call* — invariants/gates, session-start routing, one-liners per feature.
  Everything answerable *after* a recall (eval history, tuning rationale, ops
  mechanics) moves to the DB/addenda and gets doc-ingested. Per-section targets in the
  audit report sum to **~2,300–2,500 tokens (−70%)**.
- **User-global file:** ~2,480 tokens; 64% is three sections (Model Fit Check ≈665
  tokens is the weakest value-per-token — mostly negative-space rules; Dispatch Policy
  compressible ~50%; Ultracode conventions belong in an on-demand skill). Target −40%.
- **The MCP server is not registered on this machine** (`.claude.json` has only
  context7) — the routing-rules block in `src/mcp/mod.rs:745-767`, the best-written
  instructions in the system, currently reaches zero sessions. Skills and CLI
  allowlists have been carrying the load. (Verified independently by the orchestrating
  session.)
- **No root-file managed block exists** despite `init-project` claiming one;
  `sync_md` writes whole generated files under `.memhub/rendered/`, and the generated
  `.memhub/rendered/CLAUDE.md` is a strict subset of PROJECT.md sitting next to it —
  marginal value.

**Proposals:**

| ID | Proposal | Cost | Risk |
|---|---|---|---|
| C1 | Rewrite repo CLAUDE.md to the ~2,500-token target per the section table; doc-ingest the three PRD addenda + a new `docs/reference/operations.md` so moved detail stays retrievable | One writing session + `doc add` | Must-stay-inline list needs user sign-off (§11 Q22) |
| C2 | **Generate AGENTS.md from CLAUDE.md** (header swap + allowlisted Codex-only section injection); upgrade `skill_parity` to content equality | Small build/test change | §11 Q21 |
| C3 | Trim the user-global file (−40%): compress Model Fit Check to ~5 lines, halve the Dispatch Policy, move Ultracode conventions to a skill | User's file — offer a diff | None |
| C4 | Managed block: either implement a real ~10-line versioned root-file pointer block (recommended — it makes wiring auditable in every consumer repo) or retract the claim (F5 covers the text fix either way) | Small–medium | §11 Q23 |
| C5 | **`memhub audit md [--json] [--strict]`** — deterministic checks: token counts vs `[audit]` thresholds (reuses the calibration-aware tokenizer), memhub-wiring presence, decision-title/8-gram duplication vs the DB, stale `memhub` invocations validated via clap `try_parse` (never executed), twin drift, rendered-file hygiene. Output in the upgrade-style table; exit 0 unless `--strict` | Medium | None — read-only |
| C6 | `/audit-md` skill: runs C5, then does the judgment layer (value-per-token per section, move-to-recall rewrites, prose-claims-vs-behavior). Approval-gated edits | Small | None |
| C7 | `memhub upgrade` nag: one best-effort line per repo when counts exceed thresholds (`--no-audit` to skip) | Tiny | None |

---

## 7. Surfaces, UX, config, docs hygiene — `[C]`, confidence High

**Verdict: the parity matrix is fundamentally healthy; the accidental gaps cluster
around `--json` and stale text.** F1/F4/F5 cover the broken-today items.

**Key findings beyond §1:**

- JSON shapes use three conventions (`{"tasks":[...]}` vs `{"kind":…,"entries":[...]}`
  vs bare array for `doc ls`). Settle one (wrapped object keyed by noun — §11 Q29)
  *before* adding the missing flags.
- **Dead config keys:** `log_level` (read by nothing — `MEMHUB_LOG` env var wins,
  `src/logging/mod.rs:2`); `[metrics] tokenizer` (changes a label, not behavior).
  Stale doc comment on `drive_subpath` claims memhub doesn't resolve it (false since
  `resolve_remote_dir`).
- **`memhub status` shows a new user M1-era counters + two lines about deprecated K9**
  and nothing about retrieval mode, reranker, sync/global/metrics state, or doc count.
- **No `memhub doctor`:** `/check-init` is prose spread over ~6 CLI calls and has
  already drifted from the binary (F1 + a hedge about a flag that doesn't exist).
  Checks encoded in prose rot; checks encoded in the binary can't.
- Error-message quality is genuinely good (5 of 6 spot-checked paths say what's wrong
  *and* what to do next).
- MCP has `doc_add` but no `doc_ls`/`doc_show` — an agent can ingest but not enumerate
  (§11 Q28).
- `src/cli/mod.rs` is a 1,895-line dispatcher with formatting inline per match arm
  while `output.rs` holds formatters for only some commands — consolidate when adding
  the new `--json` flags.

**Docs prune list (all tracked → they mislead future sessions and implementing agents):**

| File | State | Action |
|---|---|---|
| `NEXT_STEPS.md` (root) | "Finish Milestone 3" — repo is at M11 | Delete |
| `docs/architecture/current-architecture.md` | Named "current"; predates retrieval/locate/metrics/sync entirely | Rewrite or delete (worst offender) |
| `docs/roadmap/milestones.md` | Skips M6/M7/M10/M11; lists shipped features as speculative | Update through M11 or archive |
| `docs/roadmap/memhub-primary-evaluation.md`, `k9-deprecation-plan.md`, `k9-integration.md` | Self-declared closed/historical | Move to `docs/archive/` |
| `docs/roadmap/wrap-up-design.md`, `memhub-render-design.md` | Headers claim unshipped; both shipped long ago | Stamp "Shipped — historical" + archive |
| `Source PRD/Local Agent Memory PRD.md` (root dir) | Apparent duplicate of the declared PRD authority | Confirm + delete (§11 Q26) |

**Proposals:** S1 `--json` completion + shape convention (F1 superset). S2 `memhub
doctor` (absorbs `/check-init` checks, config-key validation flagging dead/unknown
keys, plus §9's integrity checks and §8's session-accounting health line; skill becomes
a thin wrapper). S3 status refresh (new-subsystem states; K9 lines only when detected).
S4 dead-key cleanup + comment fixes. S5 docs prune per table. S6 README fixes (F4 +
the self-contradictory `propose_fact` global-flag bullet at README.md:838 + missing
command-table rows).

---

## 8. Cross-machine: sync, export/import, metrics — `[C/E]`, confidence High

**Verdict: sync's core is well-defended (no corruption paths found — torn two-file
states all degrade safely because adopt re-hashes actual bytes); the gaps are the
blind diverged gate, the ungated push (F12), and a metrics subsystem that is the
largest in the codebase (~4,540 lines) while half-dead and measuring against a stale
counterfactual.**

**Key findings beyond §1 (F2/F3/F12):**

- **The diverged gate is confirmed blind.** `CheckReport` carries verdict + counters
  only; `/catch-up`'s "summarize" step shows verdicts, and its own example ("pulled 3
  newer decisions and a session note") is aspirational — the agent has no data source
  to say that. On the one lossy case the design gates on, the user approves an
  overwrite without any view of what they'd lose.
- **The task/decision ID divergence is real and visible:** CLAUDE.md cites
  "decision 109" for two unrelated things — the signature of two machines minting the
  same ID independently. Check the Mac's decision 109 before the next adopt overwrites
  the evidence (§11 Q32).
- Metrics: component A earns its keep; component B was right-but-silently-dead (F2);
  `calibrate` (144 lines + the product's only network path) has never been run
  (`calibration_factor = 1.0`); the 1,034-line viz dashboard's usage is unknown
  (§11 Q30). The headline "context offset vs full-ledger baseline" measures against a
  ledger the MCP instructions steer agents away from — the denominator grows as a file
  nobody loads gets bigger. Task 64's empirical baseline was the right correction and
  depends on B.
- Import prints the embeddings hint but the retained-docs hint is **inverted** (gated
  on `retained_doc_chunks > 0`, so the fresh-machine case that needs it most prints
  nothing); nothing mentions global re-adds, metrics/sync enable, or `code index`.
- Marker writes are plain `fs::write` (not temp+rename); an unparseable marker
  propagates a parse error instead of degrading to "no baseline".
- Adopt vs a live process: on macOS, sidecar deletes + rename can succeed under a live
  MCP-server connection, orphaning that writer against an unlinked inode. A pre-swap
  `BEGIN IMMEDIATE` probe closes it.

**Proposals:**

| ID | Proposal | Cost | Risk |
|---|---|---|---|
| X1 | **`sync check --diff`**: per-table added/updated counts + fact/decision/task titles since the common baseline, both sides (local DB + downloaded snapshot are both local files; `writes_log WHERE id > baseline` each side). De-risks the diverged gate directly | ~150 read-only lines, no schema change | Low — highest UX value per line in this review |
| X2 | Import-time printed checklist (also `init --from-backup`): "not carried: docs → `doc add`, global store, metrics enable, sync enable, code index"; fix the inverted docs hint | Trivial | None |
| X3 | Metrics consolidation: after F2/F3 land and empirical baselines accrue, demote/retire the assumed-ledger line and the ledger-size measurement; drop or shelve `calibrate`; remove the surprising prune side-effect from `metrics status` | Medium (mostly deletion) | Gated on §11 Q30/Q33 |
| X4 | Session-accounting health line: when `session_accounting = true` and the dir is empty/missing, say so in `metrics status`, the panel, and `doctor` | ~10 lines | None |
| X5 | Marker robustness: temp+rename writes; degrade unparseable marker to "no baseline" + warning | ~15 lines | Low |
| X6 | Adopt pre-swap `BEGIN IMMEDIATE` probe against a live second process | Small | Low |

---

## 9. DB & code health — `[C]`, confidence High

**Verdict: a clean codebase (13 defensible unwraps, zero `panic!`, zero TODOs, per-dep
justification comments in Cargo.toml) with a handful of default-by-omission pragmas,
two comment-enforced invariants, and no way to notice when they're violated.**

**Key findings beyond §1 (F7/F9):**

- Pragmas are consistent across all five surfaces (WAL, `foreign_keys=ON`,
  `busy_timeout=5000`) — but **`synchronous` is never set** (bundled default FULL =
  an fsync per commit; and every `open_project` writes, even for pure reads, via the
  unconditional `upsert_project`). WAL+NORMAL is the standard pairing (§11 Q35).
- **Eager embedding runs inside the write transaction** (`persist.rs:85-142`) — the
  first embed in a process pays ONNX model load (seconds) while holding the write
  lock; the one realistic way to trip the 5 s busy budget.
- **No integrity surface anywhere:** nothing runs `PRAGMA integrity_check` /
  `foreign_key_check`, compares FTS rowcounts to source tables, runs FTS5's own
  `integrity-check`, or counts orphaned embeddings. The two riskiest invariants are
  comment-enforced with no detection.
- Main DB never VACUUMed; five FTS tables never optimized; no FTS repair surface short
  of hand SQL.
- Migrations are atomic (all pending in one transaction — good) but have **no checksum
  guard**: editing a shipped `.sql` silently diverges machines.
- **Legacy double-indexing:** migration 0009 promised removal of `chunks`/`chunk_fts`;
  decisions still write to both, and `memhub search` still reads `chunk_fts`
  (`search.rs:27,144-150`) — a live path, so retirement requires the §10 disposition
  first.
- Scale cliffs (at 10–100×): unbounded `render` (all decisions + all tasks into the
  ledger) hits first; the metrics reconciler's unindexable datetime-wrapped subquery
  second. `writes_log`/`session_notes` grow forever but are LIMIT-read — years from
  mattering.
- SQLITE_BUSY surfaces as a raw rusqlite error string through MCP; no friendly mapping.

**Proposals:** D1 `memhub status --verify` / fold into `doctor` (FTS rowcounts, FTS5
integrity-check, orphan embeddings, `integrity_check` + `foreign_key_check`) — small,
none; converts comment-enforced invariants into detectable states. D2 = F7. D3 = F9.
D4 `memhub db maintain` (FTS optimize ×5 + optional VACUUM + size report), user-gated
or an upgrade step — small, low. D5 `synchronous = NORMAL` next to WAL — 1 line,
flagged as a posture change (§11 Q35). D6 move `embed_one` outside the write tx —
small refactor, low. D7 cap the rendered ledger (collapse done tasks + superseded
decisions past N; coordinate with the metrics ledger-tokens contract) — medium.
D8 migration sha256 checksums + drift warning — small, none. D9 map SQLITE_BUSY to a
friendly error — tiny. D10 retire `chunks`/`chunk_fts` after the `search` disposition
(§10/§11 Q27) — medium.

---

## 10. Completeness-critic findings — `[C]`, confidence High

Things no dimension covered; the critic verified each against the repo and GitHub.

### 10.1 Zero CI/CD, release engineering, or branch protection — on a public repo
No `.github/`, no runs, no tags, no releases, no branch protection — while
`gh repo view` reports **PUBLIC** and 10 PRs merged with no checks ever run. This
sweep found two Windows-specific bugs a `windows-latest` CI leg would have caught
pre-merge. No `cargo audit`/`deny` ever scans the ort/tokenizers tree. The Mac update
path is rebuild-from-source per pull.
**Proposal G1 (medium):** one workflow — build + test on `windows-latest` +
`macos-latest`, OUT_DIR model cache keyed on the SHA-256s build.rs already pins (so CI
doesn't pull 218 MB per run); branch protection requiring it; scheduled `cargo audit`.
Optionally tag-triggered release binaries — gated on F8 (licensing) landing first.

### 10.2 Licensing — F8 in §1. Gates release binaries.

### 10.3 MCP `doc_add` arbitrary-file read — F11 in §1. The critic's sharpest finding:
it violates the repo's own core guardrail (agents as untrusted writers) at the one
surface designed around it, converting a single prompt-injected tool call into durable,
silently-recalled contamination. Consider also **G2 (small):** a write-time
secret-pattern warning (AKIA…, `-----BEGIN`, `sk-…`) on all durable writes —
warn-not-block.

### 10.4 Push-side sync clobber — F12 in §1.

### 10.5 Legacy surface disposition — two parallel code-history systems
`ingest-git` + `search` (M2) populate/read `commits`/`commit_files`/`files` in
project.sqlite while M11's locate tracks the same tree in code_index.sqlite — different
freshness models, overlapping intent, and every `search` call keeps the legacy `chunks`
table alive (`search.rs:27`). `stats` predates and overlaps `metrics`; `bootstrap-k9`
remains wired while K9 is declared disabled and archived.
**Proposal G3 (decision-first, then mechanical):** a one-page disposition — likely
deprecate `ingest-git` + `search file:` in favor of locate (or explicitly document
them as commit-history vs working-tree tools), fold `stats` into `metrics`, remove
`bootstrap-k9`, keep `note`. §11 Q27 gates it. D10 (chunks retirement) follows.

### 10.6 Minor gaps (one-liners)
`MEMHUB_LOG` documented nowhere user-facing · no property/fuzz tests on the three
externally-owned formats memhub hand-parses (transcript JSONL, markdown chunker,
`parse_git_log`) · no recovery runbook (backups exist; restore procedure undocumented;
live `.memhub/` inside a Drive-synced folder is an undocumented corruption hazard
distinct from M10) · no file-permission hardening (DBs/config/exports world-readable
on multi-user macOS) · dashboard token rides the URL query string, non-CSPRNG,
non-constant-time compare — acceptable at the default 127.0.0.1 bind, worth a comment;
`--host 0.0.0.0` inherits them · no SECURITY.md/Dependabot/support-expectation note ·
second-repo onboarding cost unmeasured (no `memhub init --defaults`).

### 10.7 Verified fine (no action)
build.rs model downloads SHA-256-pinned and verified on fetch *and* cache reuse ·
dashboard auth real, default bind localhost · MCP layer has genuine test coverage ·
`ANTHROPIC_API_KEY` env-only, never persisted · torn Drive snapshots fail safe via
manifest checksum.

---

## 11. Decisions needed from the user

Grouped; each carries my recommendation. Everything else in this document is
implementable without further input.

**Lifecycle (§2)**
1. Stale facts at the window: silent exclusion (today) vs **demote + `stale:true` flag** (recommended) — the single biggest currency decision.
2. Should decisions ever age? **Recommend no** — supersession is how a decision retires.
3. Superseded rows: exclude vs **demote-with-link** (recommended — no-loss posture).
4. Done tasks: **recommend** recall hits carry status, and render collapses old done tasks (D7).
5. Confidence: revive (tiered initial values entering the score) or retire? **Recommend retiring the surface field now** (stop emitting always-1.0), defer any scoring use until a real need appears.
6. Pending-write expiry: **recommend automatic** at `open_project` (matches PRD intent), keep the manual command.

**Wrap-up (§3)**
7. `[wrap_up] verbosity` a commit-back baseline (repo policy) or per-machine? **Recommend:** baseline in `config.example.toml`; transcript mode additionally per-machine.
8. Transcript archiving: is content-level secret redaction a hard requirement, or is warn + per-wrap-up approval acceptable? **Recommend warn+approve for v1**, redaction as follow-up.
9. Session notes retrievable via explicit source type? **Recommend yes** (W5).
10. Binary-rendered policy text (recompile to iterate) vs level-only? **Recommend full policy text from the binary** — single-sourcing beats prompt-iteration freedom here.
11. Commands vs facts: **recommend `record_command`** for verified invocations; stop writing command facts.

**Upgrade/GC (§4)**
12. Amend the two shipped gc exclusions (superseded `incremental/memhub-*`; opt-in >100 MB multi-hash third-party)? **Recommend yes to both**, behind flags. ~4 GiB today.
13. Test-binary consolidation (lose per-file `--test X`, gain ~7.5 GiB + faster links)? **Recommend yes** — 2–3 grouped harnesses keep coarse granularity.
14. Nonzero "pending" exit code for the staged handoff? **Recommend yes** (code 3).
15. Skill-install manifest so memhub can refuse to overwrite files it didn't write? **Recommend yes.**
16. Backups retention N? **Recommend keep 20 rendered/markdown** (sync is already single-slot).

**Retrieval (§5)**
17. Is CLI recall latency actually felt, or does everything flow through (warm-able) MCP? R12's surface column answers this with data — **recommend adding it first**.
18. Quantization: accept a possible 1–2 pp golden-set movement (pending re-eval, revert if regression)? **Recommend running the experiment** — the disk/binary win compounds across §4.
19. Bundle trim scope: **recommend MCP-only trim**; CLI `--json` keeps diagnostics.
20. 37% empty recalls: probing (fine) or query-phrasing problem? **Recommend deferring** until R12 data exists.

**CLAUDE.md (§6)**
21. Generate AGENTS.md from CLAUDE.md? **Recommend yes** — makes the −70% rewrite one edit, kills the undiffable twin.
22. Accept ~2,500 tokens as the repo CLAUDE.md target? Must-stay-inline list: Guardrails, Session Continuity, Delegation, stale-embeddings gate, sync_adopt gate. **Recommend accepting.**
23. Root managed block: implement the small versioned pointer block or retract the claim? **Recommend implement** — it makes memhub wiring auditable in every consumer repo (C5's A2 check then verifies it precisely).
24. MCP server not registered on this machine: intended or fix? **Recommend fix** — it unlocks R1/R2 and the routing instructions.
25. Should `memhub audit md` read `~/.claude/CLAUDE.md` by default? **Recommend opt-in** via `[audit] user_md_path` (shipped set in the example).

**Surfaces (§7)**
26. Is root `Source PRD/` an intentional pre-import original or a leftover duplicate? (Likely delete.)
27. Do you actually use `memhub search` / `ingest-git`? Gates G3 and D10. **Recommend deprecation** unless commit-history search is load-bearing for you.
28. Add read-only MCP `doc_ls`? **Recommend yes** — or write down "agents discover docs only through recall" as a deliberate rule.
29. JSON shape convention: **recommend wrapped objects** (`{"facts":[...]}`) everywhere; migrate `doc ls`'s bare array.

**Cross-machine (§8)**
30. Have you ever opened `/viz` or run `metrics calibrate`? If no: **recommend feature-gating viz out of default builds and shelving calibrate** (X3).
31. Should metrics rows travel with adopt? **Recommend post-adopt scrub/close** (F3 hygiene) — whole-file snapshots make exclusion impossible; scrubbing is honest.
32. Action item, time-sensitive: check the Mac's decision 109 against Windows' before the next adopt overwrites the evidence of the ID divergence. **RESOLVED 2026-07-05 (see §14 N27):** already settled by decision 134, recorded 2026-07-04 the same evening this review was produced — the Mac lineage was not adopted; Mac decisions 107–112 were manually ported as Windows 128–133 (Mac 109 = Windows 130) and the Mac snapshot is preserved byte-for-byte at `.memhub/backups/sync/mac-lineage-2026-07-03.sqlite`. Do not chase this and do not run an adopt to "verify" it.
33. Once the empirical baseline accrues: retire the assumed-ledger line entirely, or keep as comparison? **Recommend keep one release, then retire.**

**DB (§9)**
34. `synchronous = FULL` a durability stance or accidental? **Recommend NORMAL** (WAL pairing; loses at most the last commit on power loss, never consistency).
35. `--verify` failures: warn in plain `status` or only behind the flag? **Recommend cheap checks auto-warn in `doctor`; heavy checks behind the flag.**
36. `writes_log`: "never prune the audit log" as contract, or operator-gated compaction someday? **Recommend never-prune by default**; revisit at 100k rows.

**Infra (§10)**
37. LICENSE/NOTICES now? **Recommend now** (F8) — gates any release binaries and closes the public-repo inconsistency.
38. CI: model-dependent full test suite (needs the SHA-keyed cache) or build-only v1? **Recommend full suite with cache** — the tests are where the Windows bugs live.
39. `doc_add` confinement: repo-root-only, or deny-list + config allowlist for out-of-repo docs? **Recommend repo-root + allowlist** — you do point it at external specs, so pure repo-root may be too tight.

**Parity & free-form additions (2026-07-05; sources in §13–§14)**
40. MCP registration scope (P1): register in all three CLIs, preferring repo-scoped files that travel with the repo — a committed `.mcp.json` for Claude Code and the `mcp.memhub` block in the tracked `opencode.json` (Codex has no project scope; it needs `[mcp_servers.memhub]` + a trust entry in `~/.codex/config.toml`)? **Recommend yes, repo-scoped where supported** — the per-machine global-config edits demonstrably never happened.
41. Routing-rules carrier (P2): before wave 2 deletes the last redundant copies, run the 15-minute per-CLI spike — register `memhub serve`, start a session in each CLI, ask the model to recite the routing rules. If Codex or OpenCode drops the `instructions` field, restore a ~10-line compact routing block to AGENTS.md (or skill preambles) for those CLIs. **Recommend spike first; wave 2's trim is gated on the result.**
42. OpenCode posture on this machine (P5): (a) sanctioned bootstrap (`memhub upgrade --init-agent opencode` creating the dirs once with consent), (b) loud-skip only (upgrade hints when the binary is on PATH but dirs are absent), or (c) declare "OpenCode is AGENTS.md + in-repo `opencode.json` only" as deliberate. **Recommend (b) now** plus fixing the `opencode.json` 11-of-13 command drift; revisit (a) alongside U6/Q15.
43. Parity gate depth (P13, N24): adopt the content-level checks — YAML-validity of every template frontmatter, `Cli::try_parse_from` over every `memhub ...` line extracted from templates (converts C5's audit into a `cargo test` gate), a per-skill required-guardrail-token table, plus `tests/cli_surface.rs` with `debug_assert` — and decide the stub posture: minimum-content floor vs explicit per-variant allowlist. **Recommend yes to all checks; explicit allowlist for stubs** (the OpenCode compression may be intentional — make it a declared decision).
44. Per-CLI permission/trust parity (P6, N18): trim the blanket `Bash(memhub *)` allowlist to read-only verbs + the specific wrap-up write forms, moving `sync adopt`/`import --force`/`review accept`/`doc rm`/`upgrade --yes` to ask; add the current repo path to Codex's trust list; audit OpenCode permissions; and add a CLI-side confirm on `sync adopt` without `--yes` when stdin is not a TTY (a fail-closed guard no allowlist can bypass). **Recommend all four.**
45. Actor grammar (P15): document `<normalized-id>:<flow>` as the canonical actor form in the vocabulary addendum, rename the Claude wrap-up actor to `claude-code:wrap-up` (forward-only; historical rows untouched), and have `validate_actor` warn on `user+agent:`/`agent:` prefixes leaking into `--actor`. **Recommend yes.**
46. Subagent/headless write policy (§13.4c): should subagents be denied direct durable memhub writes (routed through `propose_*` or the main thread), given skill guardrails do not travel into them and the live `writes_log` already shows non-gated agent actors writing directly? **Recommend deny + document** — matches the "agents are untrusted writers" guardrail.
47. PROJECT.md note tail (N2): render the most recent 2–3 session notes in full and first-lines for the rest (saves ~1.2–1.5k tokens per session ×2 agents; notes stay in the DB, W5 makes them retrievable)? **Recommend 3 full + one-liners.**
48. `available_docs` shape (N3): report doc FILES not chunks, split repo vs global counts, and suppress the CLI note / zero the MCP field when only global docs exist? **Recommend split + suppress** — today the nudge fires topic-blind on every recall in this repo (329 cross-repo style-guide chunks, zero repo docs).
49. User-global skill roster (N6): move source-repo-only skills (`/eval-recall`, `/upgrade`) to memhub's project-level commands dir; merge `/viz` into `/metrics`? **Recommend the move; defer the merge.**
50. Onboarding (N10): ship the cheap `init` epilogue (next-steps block: MCP snippet, optional enables, code-index warm-up) now, and the profile-based setup (`init --profile` or `doctor --setup` interview) with the S2 doctor work? **Recommend both, epilogue first.**
51. Sync verbs (N11): add `memhub sync push` / `sync pull` composites (push gains F12's gate), fix the superseded courier prose in the sync abouts (F5-class, new surface), and optionally a small `/push` skill? **Recommend composites + text fix; add the skill only if mid-session pushes actually happen.**
52. Task verbs (N12): add `task block <id>` / `task reopen <id>` (today `blocked` is filterable/rendered but unreachable, and a mistyped `task done` is permanent short of hand SQL)? **Recommend yes; no `task rm`** — the audit trail outranks deletion.
53. Which write contract is real (N17): the staged pipeline has carried zero writes ever while the MCP instructions mandate it and the wrap-up skill writes direct. (a) Register MCP and steer skill writes through `propose_*` — accepting the review-fatigue risk of end-of-session triage, or preferring inline accept-at-propose prompts; or (b) bless CLI-direct-under-approval-gate as the model and rewrite `mod.rs:762` + PRD framing. **Recommend (b) for wrap-up + (a) for mid-session MCP writes with inline accept** — but this is the trust-boundary call and it is yours. Either way `status`/doctor gains "pending writes: N (oldest X days)".
54. `[global] include_docs_in_default` (N19): honor the mirror via a per-corpus source-type split, or delete the key and let the repo-level flag govern both corpora? **Recommend delete the mirror** + a status/doctor warning when the global store has doc chunks excluded from default recall.
55. DB backups (N25): rotating local backup via the existing `VACUUM INTO` machinery — always before any pending migration runs, plus a debounced cadence; retention folded into Q16/U8; map the `SQLITE_NOTADB` open error to a friendly recovery-sources message? **Recommend yes; pre-migration unconditionally, cadence weekly.**
56. PRD §9 plan-check contract (N26): the promised EXPLAIN-QUERY-PLAN suite was never built past two M2 queries. Extend the existing `explain_query_plan` helper over the modern recall/metrics/render/MCP hot paths in wave 1, or record an explicit decision that golden-set evals supersede the contract? **Recommend extending over the hot paths** — the §9 scale cliffs are exactly the class it was specified to catch; silent drift from the PRD is what the guardrail forbids.

---

## 12. Implementation sequencing

Waves are PR-sized and dependency-ordered; independent waves can interleave. Resolve
§11 questions per-wave, not all up front — each wave names its gating questions.

| Wave | Contents | Gating decisions |
|---|---|---|
| **0. Fix-now** | F1–F12 (≈2 PRs: one code, one text/docs). F2's config nudge + `metrics rescan` on this machine immediately, no PR needed | Q29 (JSON shape) for F1; Q39 for F11 |
| **1. Loud states** | S2 `memhub doctor` (absorbs `/check-init`, config validation, D1 integrity checks, X4 health line), S3 status refresh, D9 busy mapping | Q35 |
| **2. Token diet** | C1 CLAUDE.md rewrite + doc-ingest, C2 AGENTS.md generation, C4 managed block, C5–C7 audit capability, C3 global-file trim | Q21–Q25 |
| **3. Lifecycle** | L1 → L2 → L3 → L4 → L5 (each its own PR; L6 last, eval-gated; L7 never) | Q1–Q6 |
| **4. Performance** | R1 pre-warm, R2 MCP registration, R3 batch embed, R5 `--no-refresh`, R6 bundle trim, R8 debounce, R9 helpers, R11 knobs, R12 surface column; R7 quantization as a separate eval-gated experiment; R10 evals | Q17–Q19, Q24 |
| **5. Upgrade/GC** | U2 build-dir gc, U3 shim sweep, U5 exclusions, U4 test consolidation, U6 resync honesty, U7 degrade paths, U8 backups retention | Q12–Q16 |
| **6. Wrap-up** | W1–W3 knob + policy command + transcript mode, W4 kind tag, W5 notes source type, W6 skill content ×3 agents | Q7–Q11 |
| **7. Cross-machine** | X1 `sync check --diff`, X2 import checklist, X5 marker, X6 adopt probe; X3 metrics consolidation (after wave-0's F2/F3 have had time to accrue baselines) | Q30–Q33 |
| **8. Infra** | G1 CI + branch protection + `cargo audit` (+ release workflow once F8 is in), G2 secret-pattern warn | Q37–Q38 |
| **9. Housekeeping** | S5 docs prune, G3 legacy disposition → D10 chunks retirement, D4 db maintain, D7 ledger cap, D8 migration checksums, minor gaps from §10.6 as judged | Q26–Q27, Q36 |

**Verification contract for every wave:** the golden-set evals (`memhub eval
retrieval`, `eval locate`, the polyglot fixture) must hold their documented numbers;
any change touching recall scoring, embedding, or bundle shape re-runs them and
records the result. Default-off config additions must keep an untouched install
byte-identical — the project's established precedent. *(§14 N28 caveat: the base
retrieval golden runs against this machine's live DB, so the contract is currently a
property of this DB's row population, not only of the code — seed a hermetic fixture
when R10 lands.)*

**2026-07-05 additions to the waves.** Wave 0 gains **F13–F17** (§13.1; F17 is an
immediate `git add`, no PR). The F5 text pass gains P8/P10/P11/P12/P17/P18/P22 and
N11's courier prose. Wave 2 (token diet) is now **gated on Q41** — do not delete the
last redundant routing copies until the instructions-delivery spike answers whether
Codex/OpenCode ever receive the MCP `instructions` field. Wave 4's Q24/R2 work follows
Q40's all-three-CLIs scope and should take N1's description-diet pass in the same PR.
Wave 1's doctor absorbs the new probes (P1 registration state, P4 drive-ahead, N21
render freshness, N23 baseline drift, Q54's warning). New decisions Q40–Q56 gate as
marked per finding.

---

## 13. Cross-CLI parity review — results (memhub task 94, run 2026-07-05) — confidence High

**Status: complete.** Run 2026-07-05 on this Windows machine at HEAD `314f4ff`, by four
parallel parity agents (skill/command wrappers · MCP · wrap-up contract ·
routing/instructions) plus a completeness critic that spot-re-verified the highest-risk
claims against the working tree and killed one overreaching cluster. Method: static
comparison of templates, installed wrappers, and per-CLI configs, plus live smoke tests
of memhub's own surfaces (including a stdio initialize/tools-list handshake against
`memhub serve`); the `codex`/`opencode` binaries were never invoked. Findings P1–P22,
severity-ordered; healthy baseline in §13.3; what this review did NOT close in §13.4.

**Verdict.** The wrapper layer is healthier than the smells suggested — installed
Claude/Codex copies are byte-current and orphan-free, every wrap-up invocation is real,
the sync push exists in all three wrap-up variants, and the OpenCode stubs kept the
safety guardrails. The breakage is in the connective tissue: the MCP server is
registered in **zero** CLIs (not one, as §6 recorded) while the mid-session routing
rules live **only** in its instructions field; HEAD's own task-77 commit shipped invalid
YAML that silently kills the frontmatter of the four highest-traffic skills on all three
agents; and "three first-class CLIs" is in practice *Claude first-class, Codex
near-parity but untrusted, OpenCode in-repo-only and drifting*.

### 13.1 New fix-now defects (join wave 0)

- **F13** — task-77 trigger descriptions are invalid YAML; recall/locate/doc/metrics
  frontmatter silently discarded on Claude Code (P3). 12 template files × 3 agents +
  installed copies; add the frontmatter-parses test so the class can't recur.
- **F14** — `integrations enable-k9 --json`: the flag does not exist; both init-project
  variants instruct it (P9). F1-class; fold into F1's PR.
- **F15** — `codex-mcp-client` escapes `normalize_client_name`; Codex MCP writes go
  off-vocabulary (P7). One-line map fix + a warning on unmapped client names.
- **F16** — `doc rm`/`doc show` exit 0 on ident miss (§14 N13). F6 exit-code-honesty
  class.
- **F17** — `docs/reviews/` is untracked: this document is unreachable from the Mac
  (§14 N20). Immediate `git add`, no PR machinery needed.

### 13.2 Findings

**P1 · MCP server registered in zero of the three CLIs — upgrades §6/Q24/R2** — `[E]`
High, severity **high**, → **Q40**. `~/.claude.json`: global `mcpServers` = context7
only, the memhub project entry's `mcpServers` = `[]`, no repo `.mcp.json`;
`~/.codex/config.toml`: no `mcp_servers` table at all; `~/.config/opencode/opencode.jsonc`:
a bare `$schema` key. Meanwhile AGENTS.md:82 asserts Codex registration as fact and
README:794/802 claim it for both non-Claude CLIs. Consequences verified live: every
skill variant's "prefer the `memhub.*` MCP tool" first instruction can never fire (all
sessions pay §5's cold-CLI path), the confirm-gated `sync_adopt` flow is unreachable
everywhere, and wrap-up's pending-triage step is structurally empty (`pending_writes`:
0 rows all-time — see §14 N17). Four of the eight reviewers found this independently;
it is recorded once, here. Fix per Q40 (repo-scoped registration) + a per-CLI
registration probe in the S2 doctor. Note: the machine's OpenCode config is
`opencode.jsonc` while README:258 writes `opencode.json` — precedence unverified
(§13.4b).

**P2 · Mid-session routing rules are single-sourced into the MCP `instructions` field,
which only Claude Code verifiably delivers** — `[E]` Med, severity **high**, → **Q41**,
**gates wave 2**. Commit `4f55340` trimmed the routing rules from CLAUDE.md/AGENTS.md
(both now say they "live in the memhub MCP server's own instructions and are not
duplicated here"); `src/mcp/mod.rs:744-767` carries them and the live handshake
confirmed the field is emitted (1,500 chars). But Claude Code is the only client
verified to inject server instructions into model context; Codex likely drops the field
(Med) and OpenCode is unverified — and AGENTS.md, the file those CLIs *do* read,
deliberately declines to carry the rules. Even after P1's registration fix, 2 of 3 CLIs
may never see recall-over-ledger or turn-1-only. The single-sourcing decision rests on
an unexamined client assumption; run Q41's spike before wave 2 trims anything.

**P3 (=F13) · Task 77 broke the YAML frontmatter of the four highest-traffic skills —
observed live in this session** — `[E]` High, severity **high**. Task 77 appended
`Trigger on: "..."` to the descriptions of recall/locate/doc/metrics as *plain unquoted
scalars*; a `: ` inside a plain scalar is invalid YAML, so the parser drops the whole
description and Claude Code falls back to body line 1 (this session's own skill list
shows recall rendering as a truncated mid-sentence body line while the nine untouched
skills render correctly; PyYAML fails on exactly those four files). Installed copies are
byte-identical to templates, so the defect shipped everywhere; the user's own skills
with trigger phrases (`pr.md`, `five.md`) use `description: >` and parse fine. Fix: fold
to block scalars in all 12 files ×3 agents, `memhub upgrade` to resync, and add
frontmatter YAML validation to `tests/skill_parity.rs` (Q43). Ironic proof of the gate
gap: `skill_parity.rs` passed this regression without complaint.

**P4 · "Catch-up before writing on another machine" reaches zero sessions through any
ambient channel — and the reminder lives in the medium that needs catching up** — `[E]`
High, severity **high**, complements F12/X1. Coverage-matrix result: no root file's
Session Continuity, not the MCP instructions block, not PROJECT.md instructs a
session-start freshness check on a sync-enabled repo; wrap-up templates mention
catch-up only from the push side. The one mitigation is a per-machine, Claude-only
auto-memory note — and PROJECT.md:45's "Mac: run /catch-up BEFORE writing" is written
into the Windows DB the Mac only receives *after* catching up. The 2026-07-03 clobber
already demonstrated the failure. Mechanical guard (cheap, verified): `sync::check`
reads only `manifest.json`, so `open_project` (debounced, registry-UPSERT posture) or
MCP serve start can compare the remote digest to `sync_marker.json` and emit
"remote is ahead — run /catch-up before writing" in `status`/recall `warnings[]`;
byte-identical when sync is disabled. A Claude `SessionStart` hook running `memhub sync
check` is a second carrier — note hooks are Claude-only (no Codex equivalent), which is
itself a parity datum (§13.4f). F12's push gate then becomes the backstop rather than
the only defense.

**P5 · OpenCode: in-repo surface works on paper, user-global surface never installed,
and the tracked `opencode.json` has drifted** — `[E]` High, severity **medium**,
→ **Q42**. Re-scoped by the critic from four reviewers' "structurally skill-less"
overreach: the git-tracked repo-root `opencode.json` wires `skills.paths` →
`templates/skills/opencode` plus inline command templates, so an in-repo OpenCode
session gets 13 skills and the commands — never stale, straight from templates. The
real residuals: (a) `opencode.json` defines **11 of 13** commands — `catch-up` and
`locate` are missing, i.e. exactly the M10 pull orchestrator and the M11 locator, and
`skill_parity.rs` never parses `opencode.json` so the drift is invisible (fix + gate per
Q43); (b) no user-global install exists (`~/.config/opencode/{skills,commands}` absent
since OpenCode was set up 2026-05-21) and `upgrade`'s only-act-on-what-exists rule skips
it silently forever — when the agent binary is detectably on PATH, the skip should say
so (Q42); (c) no OpenCode session has ever been observed in the DB (zero opencode
actors in `writes_log`) — the in-repo surface is untested (§13.4e).

**P6 · Codex trusts only the pre-rename repo path — every Codex session here runs
untrusted; per-CLI permission parity was never part of anyone's model** — `[E]` High,
severity **medium**, → **Q44**, pairs with §14 N18. `~/.codex/config.toml:21-22`: the
only memhub-related trust entry is `[projects.'c:\users\kninetimmy\local agent memory
hub']` — the repo's pre-rename path; `C:\Users\Kninetimmy\memhub` appears nowhere. So
the 13 installed Codex skills run under untrusted-project approval/sandbox posture in
this repo. The mirror image of N18's finding that Claude's blanket
`Bash(memhub *)` allowlist is over-permissive: the trust boundary is demolished on one
CLI and over-tight on another, and OpenCode's permission model was never examined. The
parity matrix needs a permissions/trust row per CLI.

**P7 (=F15) · Codex's real MCP client name `codex-mcp-client` falls through
`normalize_client_name`** — `[E]` High, severity **medium**. Proven by live rows:
`writes_log` ids 406/407 carry `actor='codex-mcp-client'`; the map
(`src/mcp/mod.rs:1726-1733`) only knows `codex|codex-cli|openai-codex`, and the real
string has never appeared in the repo. Downstream, the vocabulary addendum derives
accept-time source as `user+agent:<actor>`, so a Codex-staged proposal would mint an
off-vocabulary source. OpenCode's three mapped aliases are guesses (zero live rows) —
capture its real `clientInfo.name` before trusting them. Fix + warn-on-unmapped per
F15.

**P8 · Claude skill variants carry falsehoods their Codex twins already fixed** — `[E]`
High, severity **medium**, extends F5, G3-adjacent. (a) `claude/check-init.md:12-13,21-22`
and `claude/init-project.md:14-15` advertise `/check-init-k9` and `/init-project-k9` —
neither exists (the K9 originals were overwritten in place by resync; only
non-loadable `.pre-k9-backup-*` files remain). (b) `claude/wrap-up.md:16-18,224-226`
claims the skill is "project-scoped to the memhub repo" with a user-level K9 wrap-up
still firing elsewhere — structurally false since decision 97 installs it user-global;
the Codex twin states the truth (`codex/wrap-up/SKILL.md:230`). (c) Both check-init
variants suggest unworkable recovery for a broken K9 path (`codex/check-init/SKILL.md:84`
points at `/init-project`, which never creates K9 files) — the correct fix is
`memhub integrations disable-k9` per the G3 archive posture. All text-only; ride the F5
pass.

**P9 (=F14) · New F1-class broken invocation, and the only one** — `[E]` High, severity
**medium**. `claude/init-project.md:148` + `codex/init-project/SKILL.md:150` invoke
`memhub integrations enable-k9 --agent-docs-path agent_docs --json`; the subcommand has
no `--json` (clap usage error). Balancing verification worth recording: **every other
distinct `memhub` invocation across all four template dirs was checked against the live
binary and exists** — the F1 class is otherwise closed at HEAD.

**P10 · Stale locate-reranker rationale in both skill variants AND `--help`,
contradicting decisions 122/123** — `[E]` High, severity **medium**, extends F5 (which
caught only the MCP-description remnant). `claude/locate.md:61`,
`codex/locate/SKILL.md:65`, and `src/cli/args.rs:221` all still say the reranker's fit
on code is "unproven until M11 PR5 calibrates it" / "still being calibrated" — the
calibration is done and decided: fusion default at 100% Recall@3, `--rerank` the
legitimate Recall@1 opt-in. Same F5 pass.

**P11 · AGENTS.md's "Codex note" tells Codex its own accounting doesn't exist — every
clause false** — `[E]` High, severity **medium**, extends F5, F2-adjacent.
AGENTS.md:171-175 says component B "scrapes Claude Code transcripts only" and
`codex_transcripts_dir` "is a stub". Shipped code contradicts it:
`session_scraper.rs:86-90,126-184` walks `~/.codex/sessions/YYYY/MM/DD` with real
token mapping, and `metrics enable` auto-detects the dir. CLAUDE.md has no such note —
pure one-sided drift.

**P12 · Both root files point at a "re-render after changes" rule that does not exist
in the MCP instructions** — `[E]` High, severity **medium**, F5-class. CLAUDE.md:15-18
and AGENTS.md:11 name three rules living in the MCP block; the block
(`src/mcp/mod.rs:745-767`) carries recall-over-ledger and turn-1-only but **no**
re-render rule — that exists only in wrap-up skill text, an end-of-session channel. A
mid-session MCP write therefore leaves PROJECT.md stale with no channel instructing a
re-render. Either add the line to the instructions block or correct both root files.

**P13 · `tests/skill_parity.rs` is a name-set gate; a content gate is cheap and would
have caught four of this review's findings** — `[C]` High, severity **medium**,
→ **Q43**, unifies with §14 N24. The test asserts skill-name sets, opencode command
names, README slash-token enumerations, and CLAUDE.md/AGENTS.md header sets — file
existence and naming only. It structurally cannot catch broken flags (F1/F14),
falsehoods (F5/P8/P10), semantic divergence (P8c), guardrail loss, stub degradation, or
invalid frontmatter (F13) — a 1-line SKILL.md with the right filename passes every
assertion. Q43 specifies the three additive checks + `cli_surface.rs`.

**P14 · The sync-md rendered twins are a dead channel: month-stale, self-described as
always-fresh, and sitting in the turn-1 read path** — `[E]` High, severity **medium**,
C4 disposition input. `src/sync_md/mod.rs:25-48` writes byte-identical
`.memhub/rendered/{CLAUDE,AGENTS}.md`; `auto_sync_md=false` in live config AND the
tracked baseline; `sync_if_enabled` hooks every write command but **not** `memhub
render` — so the twins are frozen at Jun 1 ("Active tasks: 0 open" vs 5 live) while
their own header claims regeneration "on every sync". Hazard (Med): Claude Code
auto-loads nested CLAUDE.md files, so the mandated turn-1 PROJECT.md read can inject
the stale sibling that contradicts the fresh file beside it. Whatever C4 decides:
have `render` refresh-or-delete the outputs, rename them so a generated state file
doesn't collide with the agent-config filename, and fix the self-description.
Incidental: the twin records that the stale-embeddings guardrail intentionally stays
out of the MCP block — do not re-flag that as a gap.

**P15 · Actor vocabulary has fragmented — 10+ Claude spellings in live `writes_log`,
and Claude's wrap-up prefix mismatches its own normalized id** — `[E]` High, severity
**low**, → **Q45**. Wrap-up actors are `claude:wrap-up`/`codex:wrap-up`/... but the
normalized MCP ids are `claude-code`/`codex`/`opencode` — Claude alone spans two agent
keys by construction. Live GROUP BY: `claude:wrap-up`(363), `claude-code`(20),
`claude:backlog-triage`(9), `claude:planning`(8), `opus-review`(7),
`user+agent:claude-code`(4 — a SOURCE value in the actor column), `agent:claude-code`(3),
more. `validate_actor` checks only non-empty+length; `stats` groups by exact string, so
per-agent aggregation is already unanswerable.

**P16 · F12's fix as evidenced names only the Claude wrap-up file — the identical
ungated push lives in all three variants** — `[E]` High, severity **low**, amends
**F12**. Same sequence in `codex/wrap-up/SKILL.md:179-201` and compressed into
`opencode/wrap-up/SKILL.md:17` (plus byte-identical installed copies). When F12 lands,
touch all three templates — and note the binary-side guard is what actually protects
the OpenCode one-liner, which has no room for skill-text gating. Positive parity: the
push step is *present* in all three; no wrapper silently lacks it.

**P17 · Upgrade-resync documentation omits OpenCode though the code syncs four dirs** —
`[E]` High, severity **low**, U6-adjacent. `--no-skills` help (`args.rs:135-137`) and
both upgrade skill templates enumerate only the claude+codex dirs and show stale
"synced 11" counts (13 exist); only CLAUDE.md is correct. One text pass.

**P18 · OpenCode recall stub lacks the `available_docs` follow-up contract; the other
stubs kept their guardrails** — `[E]` High, severity **low**, W6/F5. The 15-line stub
never mentions `available_docs`, so an OpenCode session has no cue for the doc-scoped
follow-up. Balancing verification: reindex ask-before, catch-up confirm-gated adopt +
refusal vocabulary, stale-embeddings gate, and global user-gated routing are all
present in the stubs — the compression lost contract depth, not safety. When F5
rewrites the recall/doc text (and §14 N3 reshapes `available_docs`), give the stub a
2-line version.

**P19 · "PRD changes land as addendum files" exists only in AGENTS.md — Claude sessions
have no sanctioned PRD-change path** — `[E]` High, severity **low**, §6/C2. AGENTS.md:91
carries the sentence; CLAUDE.md:163 stops at "keep verbatim". The addendum convention is
real, load-bearing practice. Copy the sentence (or let C2's generator make it moot —
with CLAUDE.md as the source keeping it).

**P20 · AGENTS.md condensations silently drop normative caveats — outside the declared
intentional-divergence scope** — `[E]` High, severity **low**, C2/Q21 test cases. Two
verified: the `min_rerank_score` paragraph loses "parity calibration, not an
improvement" + the eval override flag; the decision-summary paragraph loses the
empirical numbers and clear-to-NULL affordance. Record both as fidelity test cases for
the C2 generator; no hand-fix.

**P21 · Both root files carry MANAGED-block markers no code maintains — §6's "no root
managed block exists" is literally false and C4 will collide with them** — `[E]` High,
severity **low**, C4 spec note. `CLAUDE.md:165/182` + `AGENTS.md:93/112`:
`BEGIN/END MANAGED: delegation-policy` with "delete this whole block to revert" —
repo-wide grep shows zero maintaining logic (`sync_md` writes rendered files only).
The critic notes HTML comments are stripped from Claude's rendering of CLAUDE.md, which
is how §6 missed them. C4 must namespace its own markers (`MANAGED: memhub-wiring`) and
treat the existing ones as foreign; or strip the misleading markers.

**P22 · Minor MCP-surface asymmetries** — `[E]` High, severity **low**, F5 pass. MCP
`recall` hardcodes `use_reranker: None`/`min_rerank_score: None` while MCP `locate`
exposes `rerank` — an agent can A/B the reranker via locate but not recall, and nothing
documents the omission as deliberate. CLI about-string (`args.rs:15`) omits OpenCode;
`memhub serve --help` has no description identifying it as the MCP server (also §14
N9).

### 13.3 Verified-healthy baseline (no action; do not re-audit)

All 13 installed Claude wrappers and all 13 Codex `SKILL.md` are byte-identical to HEAD
templates; no memhub orphans in either dir; task-77 trigger phrases deployed everywhere
(their YAML is broken — F13 — but the deployment mechanism worked). Every CLI
invocation in the Claude/Codex wrap-up and catch-up templates exists as written
(`--help`-verified); `sync status --json` emits exactly the keys the skills read.
Source values match the vocabulary addendum §3 across all three agents. Codex wrap-up
is proven in production (28 `codex:wrap-up` rows, 6 session notes). Catch-up exists for
all three CLIs with gating semantically identical across variants (confirm-gated adopt,
project-id/schema refusal vocabulary). The sync push step is present in all three
wrap-up variants. OpenCode stubs retain the safety guardrails (P18's gap is contract
depth, not safety).

### 13.4 What this review did not close (gaps the critic surfaced)

- **(a) Instructions-field delivery per CLI** — asserted from client knowledge, not
  tested (P2 is Med for Codex, Low for OpenCode). Q41's 15-minute registration spike
  settles it; gate wave 2 on the answer.
- **(b) First-install path per CLI never walked** — everyone verified *resync*; nobody
  dry-ran the README quickstarts from scratch, so whether the OpenCode hole is a doc
  defect or a skipped step is undetermined. Concrete smell: README writes
  `opencode.json`, the machine has `opencode.jsonc`; merge/shadow precedence unknown.
  Walk the three quickstarts on a scratch HOME; consider Q42's `--init-agent` as the
  sanctioned path.
- **(c) Subagent and headless sessions are outside every reviewed contract** — this
  repo's own orchestrator mode routes work through subagents that inherit no slash
  commands (so no skill guardrails), while live `writes_log` shows non-gated agent
  actors writing directly. → **Q46**.
- **(d) The Mac half of every claim is unexamined** — its installed skills, MCP state,
  binary/calibration lag are invisible from here, and every fix shipped from this
  review reaches the Mac only via `memhub upgrade` there. **Mac checklist for the next
  Mac session, before any writes:** `/catch-up` (adopt); `memhub upgrade` (resyncs
  skills incl. the F13 fix); `memhub upgrade --dry-run` first if cautious; check
  `~/.claude.json` mcpServers per Q40. Treat all skill-text fixes as unresolved on the
  Mac until then.
- **(e) No actual in-repo OpenCode session was ever run** — P5's "works on paper"
  deserves one smoke session before OpenCode parity is declared.
- **(f) Hooks/plugins as a parity row** — Claude hooks are live on this machine and are
  the natural mechanical-guard channel (P4); Codex has no equivalent; OpenCode's
  installed `@opencode-ai/plugin` was never examined. Any hook-based guard is
  Claude-only and must be documented as such.

---

## 14. Free-form pass — token economics, ergonomics, workflow shape (2026-07-05) — confidence High

Same session and conventions as §13; four lenses (token/context economics · CLI
ergonomics · workflow shape · wildcard), each instructed to re-read §§1–12 first and
report only what the July sweep missed, with the same critic quality-gating the output.
Findings N1–N28, grouped by lens, severity-ordered within group.

### 14.1 Token / context economics

**N1 · Registering the MCP server adds an unmeasured ~1.8k+ tokens to every session —
diet the descriptions in the same PR as Q24/Q40** — `[E]` High, severity **medium**.
Measured live: 23 tool descriptions = 5,867 B + the 1,500 B instructions block ≈ 1,841
tokens before schema serialization; the full `tools/list` payload is 32.9 KB of which
21.4 KB (65%) is rmcp auto-generated `outputSchema` (recall 2,388 / metrics 2,273 /
locate 2,183 chars) — whether clients forward output schemas to context is
client-dependent and unmeasured. §5's R2 line costs registration as "Config-only /
none". Concrete trims: the "Cross-machine Drive sync (M10):" preamble is repeated ×5
and the canonical-path sentence ×4; `propose_fact`/`propose_decision`/`task_add` carry
~1.2 KB of overlapping steering prose; legacy `search` (Q27/G3) rides every session.
~30–40% of description bytes (~600–800 tokens/session, forever) recoverable; check
rmcp for suppressing `outputSchema` on read-only tools before the surface goes live ×3
CLIs.

**N2 · PROJECT.md's turn-1 read is 36% session-note tail** — `[E]` High, severity
**medium**, → **Q47**, extends §6/D7. Measured: 21,779 B total; "Recent session notes"
= 7,801 B (35.8%, 10 full notes) vs the actual state block at ~2,460 B (11%). §3 already
established notes are non-retrievable after falling off the render; yet they dominate
the one file both agents read every session. Render 2–3 in full + first-lines for the
rest: ~1.2–1.5k tokens saved per session ×2 agents, zero durable loss.

**N3 · `available_docs` counts cross-repo global CHUNKS, so the doc-follow-up nudge
fires permanently and topic-blind in this repo** — `[E]` High, severity **medium**,
→ **Q48**. Live: a sync question returned `available_docs: 329` + the unconditional CLI
note — all 329 are five *global* style guides (Swift/HTML/Rust/C#/design); the repo has
zero docs; the obedient doc-scoped follow-up returned 0 results (measured). The cue
should be informative: files not chunks, repo/global split, suppressed when the repo
itself is doc-less. Also manufactures Q20's empty recalls.

**N4 · Standing routing rules are hand-maintained in up to 8 sources each — the
measured duplication behind the F5 drift class** — `[E]` High, severity **low**,
extends W1/C1/C2. Counted: the stale-embeddings rule in 8 maintained sources (CLAUDE.md
×2 within one file, AGENTS.md, {recall,reindex,eval-recall} × 2 agents) + 6 installed
copies; recall-over-ledger in 7. CLAUDE.md claims the routing rules are "not duplicated
here" while duplicating the stale-embeddings rule twice itself. When C1/C2 land, extend
W1's binary-rendered-policy pattern beyond wrap-up, or at minimum add a keystone-phrase
parity test so one copy can't move without the others.

**N5 · Skill-body invocation tax: each /recall injects ~1.8k tokens of static
instructions to fetch a median-187-token bundle** — `[C]` High, severity **low**.
Installed bodies: recall.md 7,220 B, locate.md 4,630 B, metrics.md 2,629 B — the full
body enters context per invocation, ~10× the median payload (§5: median 187, avg 375).
An unstated *token* argument for MCP registration: after Q40, shrink the
high-frequency read skills to thin triggers and keep long-form bodies only for
orchestration skills (wrap-up, catch-up, init-project, upgrade).

**N6 · 13 user-global memhub skills ride every Claude session in every repo on the
machine** — `[C]` High, severity **low**, → **Q49**. ~2.7 KB of description frontmatter
(~650–700 tokens) in every session's skill list machine-wide, duplicated into Codex;
`/eval-recall` and `/upgrade` are memhub-source-repo-only by their own text and are the
obvious relocations.

**N7 · The sanctioned empty-recall fallback is now a ~52k-token file** — `[C]` High,
severity **low**, extends D7/§8, pairs Q20. `PROJECT_LEDGER.md` = 209,276 B ≈ 52k
tokens; the MCP instructions bless it as the fallback while §5 measured 37% of recalls
empty — the designed miss-path costs ~140× the average bundle. Feed to D7's cap
prioritization; have the fallback instruction name a bounded alternative first.

### 14.2 CLI ergonomics

**N8 · `memhub init` anchors to cwd with no git-root discovery — nested split-brain
`.memhub` footgun** — `[E]` High, severity **medium**. `src/cli/mod.rs:176-179` passes
cwd straight through; no walk-up, no `.git` check (`init.rs:10-21`,
`db/mod.rs:46-56`) — while *read* commands DO walk up, and the not-found error
("run `memhub init`") invites running init in the wrong directory. `init` in `repo/src/`
silently creates `repo/src/.memhub/` + a stray `.gitignore`. Resolve the git toplevel,
warn/confirm on mismatch, refuse when an ancestor `.memhub` exists.

**N9 · `--help` is blank for 24 of 31 top-level commands — including `recall`, `serve`,
`status`, `init`** — `[E]` High, severity **medium**. Only the M9–M11 verbs carry about
text; `serve` (the doc's own Q24 priority) is undiscoverable from the binary; sub-helps
repeat the pattern; the inverse problem too (upgrade/sync/gc paragraphs render
full-length in the command list — about vs `long_about` misuse); tagline omits
OpenCode. One-line `about` for every command, paragraphs to `long_about`, `serve` text
pointing at the registration snippet.

**N10 · Onboarding is 32 config keys / 4 independent enable switches across four
surfaces, and `init` prints zero next-step guidance** — `[C]` High, severity
**medium**, → **Q50**, extends S2/§10.6. The guided path exists only as
`/init-project` skill prose; bare-CLI and README users (F4: the README path is broken)
get silent defaults. Cheap: an init epilogue block. Structural: profile-based setup or
`doctor --setup`.

**N11 · The sync CLI still teaches the superseded courier mental model, with a
git-inverted `commit` verb and no executable push/pull pairing** — `[E]` High, severity
**medium**, → **Q51**, F5-class text + F12/X1 interaction. `args.rs:161-164,377-379,428-431`
all describe an "agent courier" uploading snapshots — decision 104 superseded that
model entirely. The documented composites (push = snapshot+commit, pull = check+adopt)
exist nowhere as commands, and the pull side has a skill (/catch-up) while push has
none — mid-session push requires knowing the two-step pairing or running a full
wrap-up.

**N12 · Task status `blocked` is filterable and rendered but unreachable; `done` is
irreversible; tasks are undeletable** — `[E]` High, severity **medium**, → **Q52**,
§2-adjacent. `TaskCommand` = Add/List/Done only; only the K9 importer can mint
`blocked`; yet `sync_md` renders "N blocked" and render sort-ranks it. A mistyped
`task done <id>` (94 tasks in this repo) is permanent short of hand SQL.

**N13 (=F16) · `doc rm`/`doc show` ident misses exit 0** — `[E]` High, severity
**low**. `cli/mod.rs:755-758,774-781` print the miss and return Ok — a wrap-up script
doing `memhub doc rm old.md && ...` proceeds believing it removed. Contrast `task done`
(errors). Exit nonzero; keep `{"found": false}` for `--json`.

**N14 · Three names for index operations across surfaces, and CLAUDE.md instructs the
invalid bare `memhub index`** — `[E]` High, severity **low**. `index rebuild`
(embeddings) vs `code index` (locator) vs skill `/reindex`; repo CLAUDE.md's
cross-machine section says "Run `memhub index`" — a clap usage error (the import hint
correctly says `index rebuild`). Fix wording in C1; add abouts distinguishing the two
indexes.

**N15 · Verb split: `doc ls|rm` vs `list` everywhere else, no aliases** — `[E]` High,
severity **low**. One `visible_alias` per verb fixes muscle-memory failures both
directions.

**N16 · Windows `\\?\` extended-length prefix stored and displayed verbatim in doc
paths; path-based `rm` fails exactly when the source file is gone** — `[E]` High,
severity **low**, same class as F2's fix. `doc.rs:71-72` stores raw `fs::canonicalize`
output; lookup re-canonicalizes the user's ident so matching works only while the file
exists — after deletion (the natural time to `doc rm` by path) canonicalize fails and
the fallback string never matches the stored `\\?\` form; only rm-by-id works, and the
miss exits 0 (F16). Strip the prefix dunce-style; add a normalized-suffix fallback in
`resolve_doc_id`.

### 14.3 Workflow shape

**N17 · The staged-write pipeline has never carried a single write, and the two shipped
write contracts contradict each other** — `[E]` High, severity **high**, → **Q53**,
extends §6/Q24. Read-only DB queries: `pending_writes` = 0 rows of any status
*all-time*; zero of 779 `writes_log` rows touch it; all 137 decisions + 17 facts were
direct writes. The MCP instructions mandate "NEVER write facts/decisions directly —
stage via propose_*" (`mod.rs:762`) while the shipped wrap-up skill's write sequence is
direct `decision add`/`fact add`. With MCP unregistered (P1), review UX, migration
0016's idempotency, and expiry all guard a path with zero production traffic. Decide
which contract is real (Q53); either way `status`/doctor gains a pending-writes line so
a re-enabled staged path can't silently accumulate.

**N18 · Blanket `Bash(memhub *)` allowlist removes the permission backstop from every
destructive memhub op — including the one op M10 designed to be operator-gated** —
`[E]` High, severity **high**, → **Q44**, pairs P6. `.claude/settings.local.json`
allowlists `Bash(memhub *)` plus PowerShell wildcards covering `sync adopt --yes`,
`import --force`, `review accept`, `doc rm`, `upgrade --yes` — the MCP `sync_adopt`
confirm gate is moot in this repo because the CLI equivalent is prompt-free. Partial
mitigation: adopt's single-slot backup. Fix per Q44 (allowlist rebalance + non-TTY
confirm on adopt).

**N19 · `[global] include_docs_in_default` is write-only — 329 global doc chunks are
invisible to default recall and the documented auto-flip governs nothing** — `[E]`
High, severity **medium**, → **Q54**, extends §7's dead-key list. Only consumers of the
key: the auto-flip itself (`doc.rs:237,240`); recall resolves DocChunk inclusion solely
from the repo-level `[retrieval]` flag and passes the same source_types to both corpora
(`recall.rs:154,167,267`). Live: global store enabled, 5 docs/329 chunks ingested, both
flags false — the style guides can never surface here, silently. Structurally the flip
fires in whichever repo ran `doc add --global` and can't propagate anyway.

**N20 (=F17) · The cross-machine loop moves the DB but not the artifacts it points at —
this document is unreachable from the Mac** — `[E]` High, severity **medium**.
`git status`: `?? docs/reviews/`; PROJECT.md and task 94 direct the next session to a
file that exists only on this machine (the 2026-07-04 session note even says so). After
a Mac catch-up, its PROJECT.md points at nothing. Immediate: commit `docs/reviews/`.
Mechanical guard, zero plumbing: wrap-up already runs `git status --porcelain` — add
one drafting rule flagging any repo-relative path referenced in state/tasks/notes that
is untracked ("won't exist on your other machines").

**N21 · A skipped or crashed wrap-up leaves no trace — PROJECT.md carries no staleness
signal for readers forbidden to re-read it** — `[E]` Med, severity **medium**, extends
S3/doctor. The MCP instructions forbid re-reads after turn 1, so a session following an
unwrapped session works from silently-old state. Scale: 120 scraped sessions (through
May 23 only — F2) vs 94 all-time wrap-up notes. The data exists: "N durable writes
since last render" is one `writes_log` query (today: 4). Add the freshness line to
`status`/doctor and stamp it into PROJECT.md's generated header.

**N22 · The wrap-up read-window is anchored to a *conditional* write, so a
no-state-change wrap-up stalls the window** — `[E]` High, severity **low**, §3
W1-adjacent. The window opens at the last `project_state` row, but draft rule 1 writes
state "only if there's a real change" — a skipping wrap-up makes the next one re-propose
already-recorded items. Already happened once (94 notes vs 93 state rows). Anchor to
`MAX(session_notes.created_at)` — the one unconditional write — in all three variants
(or via W1's policy command).

**N23 · Config baseline changes never propagate to existing machines and nothing
detects the drift** — `[E]` High, severity **low**, extends S2. `config.example.toml`
is copied only when `config.toml` is absent; no code path compares them afterward,
despite the example self-documenting "commit-back-here" fields that must not drift. A
doctor check: warn when a commit-back field differs (read-only; per-machine fields
legitimately diverge).

### 14.4 Wildcard — blind spots in this review's own taxonomy

**N24 · The clap CLI surface — the exact layer where the F1 class lives — has zero test
coverage in 31 test files** — `[E]` High, severity **medium**, root-causes F1,
→ **Q43** (with P13). No `try_parse`/`parse_from`/`Cli::` anywhere under `tests/`; only
5 of 31 integration files execute the real binary, three of them legacy surfaces. Any
flag rename ships silently. `tests/cli_surface.rs`: (a) `Cli::command().debug_assert()`;
(b) a `try_parse_from` matrix mechanically including every `memhub ...` line extracted
from templates — converting C5's audit into a build-failing gate, at milliseconds cost
(no models load for arg parsing).

**N25 · The source-of-truth DB has no backup mechanism outside sync-adopt — a default
(sync-disabled) repo has zero DB backup artifacts** — `[E]` High, severity **medium**,
→ **Q55**, sharpens §10.6's "backups exist" (for the DB, they don't). Complete
inventory of backup writes: render/sync_md (markdown only) and adopt's single slot.
Nothing in the open/migrate path ever copies the DB; corruption = total loss minus
manual exports; `open_connection` surfaces raw rusqlite errors with no recovery
pointer. The `VACUUM INTO` machinery already exists in `sync.rs` — reuse it
pre-migration + debounced (Q55) and map `SQLITE_NOTADB` to a friendly
recovery-sources message.

**N26 · PRD §9's self-declared "non-negotiable" EXPLAIN-QUERY-PLAN enforcement was
never built past two M2-era queries** — `[E]` High, severity **low**, → **Q56**.
PRD:248-252 promises plan assertions on every MCP-layer query plus a `memhub explain`
command; reality is `tests/milestone2.rs:123-162` covering two queries (one on the
legacy path slated for retirement). The §9 scale cliffs this review found are precisely
the class the promised suite was specified to catch. Extend or formally supersede —
doing neither is the silent PRD drift the guardrail forbids.

**N27 · Q32 was already resolved the evening the review was produced — the doc and the
assistant's memory index were stale on it** — `[E]` High, severity **low**. Decision
134 (2026-07-04 23:00) records the full resolution; §11 Q32 is now marked resolved in
place and the memory note corrected. Recorded here so an implementing session doesn't
chase the check or run an adopt to "verify" it. C1's citation reconciliation has its
mapping table in decisions 128–133's rationales.

**N28 · The retrieval golden eval is non-hermetic — §12's verification contract gates
on this machine's DB content, not just code** — `[E]` High, severity **low**, extends
R10. `eval.rs:103-133` recalls against the live repo DB; the 18 golden queries assert
rows exist in *this* `project.sqlite` (decision 134's near-miss adopt would have moved
Recall@K with zero code change; wave-3 lifecycle changes will too). The hermetic
pattern already exists (`tests/locate_polyglot.rs`) — seed a fixture DB for the base
set when R10 lands; keep the live-DB run as calibration, not the gate.
