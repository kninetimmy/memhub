# PRD addendum: M9 machine-global memory store

**Author:** Elswick
**Status:** Addendum to [`memhub-prd.md`](memhub-prd.md) (Draft v2). Authoritative for the items it modifies.
**Last updated:** 2026-05-18

This document supplements `memhub-prd.md` rather than replacing it.
The PRD stays verbatim per the project guardrail in `CLAUDE.md`.
Where this addendum and the PRD disagree on the items called out
below, this addendum is authoritative; everything not addressed here
continues to read from the PRD as-written.

This addendum is the design anchor for **Milestone 9: machine-global
memory**. Task 45 in the DB is the research+plan origin; this document
is the rationale and the contract. It deliberately rejects the
"Option 2" scope (scoped namespaces, routing rules, global↔machine
sync) that task 45's notes flag as a different product.

---

## What this addendum modifies

| PRD section | Status after addendum | Reason |
|---|---|---|
| §3.6 "One DB file = one repo" | **Relaxed (load-bearing).** A second, optional, machine-global SQLite at `~/.memhub/global.sqlite` is added alongside the per-repo `.memhub/project.sqlite`. Still local-only, still per-machine, still one repo DB per repo. | M9 §1. |
| §4 "Non-goals" — "general knowledge base" / multi-scope | **Clarified, not overturned.** Machine-global memory is *not* a knowledge base or multi-user sync: it is per-machine, offline, user-gated, and structurally identical to a repo DB. The other non-goals (multi-user sync, replacing git/agents, cloud, auto-compaction) remain in force. | M9 §1, §11. |
| §8 "Data model" | **Unchanged — explicitly.** The global store reuses the existing schema verbatim (same embedded migrations). `project_id = 1` / `CHECK(id = 1)` are per-database, so no schema change and **zero new SQL migrations** are required. Provenance is an in-memory recall tag, not a column. | M9 §3. |
| §12 "MCP tool surface" | **Extended.** `propose_fact` / `propose_decision` gain an optional `global` flag. The agent can *propose* to global; it can never *write* global. No MCP tool writes global without a human `review accept`. | M9 §6. |
| §13 "CLI surface" | **Extended.** Adds `memhub global enable\|disable\|status`, `--global` on `fact\|decision\|doc add`, and `fact\|decision promote <id> --global`. | M9 §5. |
| §16 "Milestones" — Milestone 5+ list | **Extended.** "Milestone 9: machine-global memory" added. | M9. |

The PRD's design principles (§3 except §3.6), other non-goals (§4),
router overview (§10), write-back policy (§11), migrations and export
(§14), security (§15), and risks (§18) are otherwise unchanged. The
M8 retrieval addendum is unmodified; M9 layers on top of its recall
pipeline.

---

## 1. The §3.6 relaxation (load-bearing)

PRD §3.6 commits to "one DB file = one repo," reaffirmed by the M8
addendum (§13: "Embeddings live in the same `.memhub/project.sqlite`.
No new files outside `.memhub/`.").

This addendum **narrowly relaxes** that, by exact analogy to global
vs. repo `CLAUDE.md`:

- One **optional** machine-global DB at `~/.memhub/global.sqlite`,
  alongside — never replacing — each repo's `.memhub/project.sqlite`.
- The relaxation is **opt-in and off by default** (`[global] enabled
  = false`). A memhub install with global disabled or the file absent
  behaves byte-identically to pre-M9.
- Still local-only, still offline, still per-machine. The global DB is
  **not** synced, exported, or shared. Each machine maintains its own,
  exactly as it maintains its own repo DBs and embeddings.
- All other §3 principles hold: local-first (§3.2), boring tech
  (§3.5), agents-are-untrusted-writers (§3.3).

**Why it's defensible (memhub's own thesis at machine scope).** Global
`CLAUDE.md` and Claude-Code auto-memory are both always-loaded flat
files (auto-memory `MEMORY.md` truncates ~200 lines) → they don't
scale. memhub's value is *retrieve-a-bundle, don't-load-the-ledger*; a
global memhub is precisely that, machine-wide. Auto-memory is also
Claude-only (`~/.claude/`); Codex cannot see it. memhub is the
agent-neutral shared store — the same argument that justified
memhub-per-repo over auto-memory, lifted one level to machine scope.

## 2. Scope guardrail: what is promotable (the anti-noise rule)

Task 45 flags the core risk: wrong routing → the global store fills
with repo noise (the entropy memhub fights) or stays empty. The
mitigation is **user-gated routing plus a documented taxonomy**, not a
classifier. Routing is never agent-automatic (§6).

Promotable to global — facts, decisions, and docs only:

- **Global facts** — machine/toolchain truths, install/env commands,
  cross-repo personal conventions, agent-collaboration preferences.
- **Global decisions** — standing engineering policy applied
  everywhere ("always integration-test against a real DB," "prefer X
  lib for dates across all projects"), *not* per-repo architecture.
- **Global docs** — broadly-applicable reference: universal coding
  style guides, language idioms. A *repo-specific* guide (e.g. a UI
  guide only one repo follows) stays a plain repo `doc add` —
  unchanged behavior. The M8 cross-encoder relevance floor
  (`doc_min_rerank_score`) already keeps an off-topic global doc
  silent on an unrelated query, so a global UI guide does not pollute
  a backend query.

**Never global:** tasks (always repo work items), `project_state` /
`project_arch` / `session_notes` (the repo's rendered view), and
anything naming a repo-specific path, symbol, or architecture.

## 3. Data model: deliberately unchanged

The global store is **structurally just another memhub SQLite.**
`project_id = 1` and the `projects` `CHECK (id = 1)` constraint are
*per-database*. The 45+ `project_id = 1` query sites are therefore
**not** a blocker: the global DB independently carries its own
singleton `projects` row. `migrations::apply_all` is stateless and
embedded; applying it to `~/.memhub/global.sqlite` yields an identical
schema (facts, decisions, tasks, doc_chunks, embeddings, FTS,
writes_log).

Consequences:

- **Zero new SQL migrations** for the store itself.
- The global DB has its own `writes_log` — its own audit trail.
- Provenance (`scope: "repo" | "global"`) is an **in-memory recall
  field**, not a schema column. It never persists; it is computed at
  merge time.
- The global `projects.root_path` is a sentinel (`"<global>"`), not a
  real repo path.

## 4. Recall merge and the provenance contract

M9 extends the M8 recall pipeline. The contract:

- Every `RecallHit` carries a new required field `scope`, value
  `"repo"` or `"global"`, sibling to `source_type`. It is surfaced in
  the CLI JSON output and the MCP `recall` response.
- **Precedence is provenance-tag-only.** Recall never silently drops a
  hit for being global, and performs no automatic global-vs-repo
  conflict resolution. The agent applies repo-overrides-global (the
  same way repo `CLAUDE.md` overrides global `CLAUDE.md`) using the
  `scope` tag. This matches memhub's agents-decide posture and task
  45's explicit "recall must carry provenance" note.
- The merge gathers and blends candidates from each connection
  independently, concatenates, then runs **one** cross-encoder rerank
  pass over the unified pool (rerank is the dominant cost; gathering
  twice is cheap). The query is embedded once and the embedding reused
  for the global gather.
- The global corpus is consulted **only when `[global] enabled` AND
  `~/.memhub/global.sqlite` exists.** Disabled or absent → recall
  output is byte-identical to pre-M9. This is the eval regression
  guarantee: `tests/retrieval_golden.json` baselines do not move.
- The global gather inherits the *active repo's* `[retrieval]` config
  (mode, weights, reranker, floors). The global store has **no
  separate retrieval config.** An FTS-mode repo does an FTS-only
  global gather.
- `available_docs` becomes the count of repo **plus** global ingested
  doc chunks not surfaced this call (global docs are in scope).
  `warnings` aggregates stale-embedding signals from both DBs.

## 5. New CLI surface (§13 extension)

```
memhub global enable                 # create ~/.memhub/global.sqlite, set [global] enabled
memhub global disable                # stop merging global into recall; refuse global writes
memhub global status [--json]        # path, schema version, fact/decision/doc-chunk counts

memhub fact add <k> <v> --global         # born-global fact
memhub decision add <title> ... --global # born-global decision
memhub doc add <path> --global           # born-global doc (universal style guide)

memhub fact promote <id> --global        # copy an existing repo fact into global
memhub decision promote <id> --global    # copy an existing repo decision into global
```

`memhub global enable|disable|status` mirrors the existing
`memhub metrics enable|disable` ergonomics — a known, boring pattern
in this codebase. All mutating commands keep `--actor NAME` for audit
attribution.

**Promotion is copy, not move.** The repo row stays; the repo copy
still wins locally (consistent with repo-overrides-global). Fact keys
are UNIQUE per DB, so re-promoting a key updates the global fact.
Decisions have no natural key, so re-promoting warns if a global
decision with the same title exists, then inserts (documented
limitation; revisit only if it bites).

**Enable-gate.** When `[global] enabled` is false, every global write
(`--global` add, `promote --global`, accepted global proposals)
refuses with a hint to run `memhub global enable`. The first global
write after enable prints a one-time disclosure naming the store path
and its machine-wide visibility.

## 6. MCP posture: propose, never write (§12 extension)

`propose_fact` / `propose_decision` gain an optional `global: bool`
(default false). A global-flagged proposal stages into the **repo's**
`pending_writes` with `target: "global"` recorded in the provenance
payload. It becomes durable only when a human runs
`memhub review accept`, which routes the durable write through the
global DB instead of the repo DB. The pending queue itself always
lives in the repo DB.

**MCP `doc_add` gets no `global` parameter.** A born-global write must
be human-typed (CLI / `/global` skill). This preserves the
untrusted-writer guardrail at machine scope: one bad global write
poisons every repo on the machine, so the global write path is never
agent-automatic. The agent's only route to global is a *staged
proposal a human accepts.*

## 7. Enablement and onboarding

- New `[global]` config section; `enabled = false` default. It lives
  in the tracked `.memhub/config.example.toml` baseline with a
  commit-back-here comment, matching the `[metrics]` precedent.
- Install/onboarding (README install blocks + the agent orientation in
  `CLAUDE.md` / `AGENTS.md`) documents two **explicit** toggles:
  - **Hybrid retrieval** (`[retrieval] mode = fts|hybrid`) — explicit
    choice (today defaults to `fts`).
  - **Global store** (`memhub global enable`) — explicit choice.
- Two **auto-followers**, documented as such with a manual override:
  - **Reranker** (`[retrieval] use_reranker`) — auto-on with hybrid
    (FTS-only bypasses it anyway, so the existing `true` default
    already behaves this way; document only, no behavior change).
  - **Docs-in-default** (`[retrieval] include_docs_in_default`) —
    already flips true on first `doc add` (M8 decision 90). Documented
    as auto with a manual-off override. Mirrored by
    `[global] include_docs_in_default`, flipping on first
    `doc add --global`.

## 8. What does not change

- **Local-first, offline, boring** (PRD §3.2, §3.5). No network. The
  global DB is the same rusqlite/SQLite engine at a second path. No
  new dependency beyond a home-dir resolver if one is not already
  vendored.
- **Agents are untrusted writers** (PRD §3.3). Strengthened at machine
  scope: the global write path is never agent-automatic (§6).
- **Write-back policy §11.** Wrap-up and accept still control what
  becomes durable. Global writes are an explicit user action or a
  human-accepted proposal.
- **Export/import §14.** The v1 export format is unchanged and stays
  **repo-scoped.** The global DB is not exported (per-machine posture,
  matching the docs-are-export-excluded precedent). Cross-machine
  carry of global memory is out of scope (deferred; task 45's lean
  scopes global sync out).
- **Security / privacy §15.** No new exfiltration surface. The global
  DB is a local file under `~/.memhub/`. Nothing leaves the machine.
- **M8 recall contract.** The hybrid scoring formula, floors, and
  eval discipline are unchanged; M9 only adds a second corpus behind
  an off-by-default flag and a `scope` field.

## 9. Out of scope (rejecting "Option 2")

- No scoped namespaces, per-scope routing rules, or global↔machine
  sync.
- No agent auto-routing to global; no `global` on MCP `doc_add`.
- No `memhub global export/import` / cross-machine carry in v1.
- Promotion is copy-not-move; no repo-side back-annotation.
- Tasks and rendered narratives are not promotable.
- No automatic global-vs-repo conflict resolution — provenance tag
  only; the agent applies precedence.

## 10. Reference design docs

Authoritative for the items they cover:

- This document (`memhub-prd-addendum-m9-machine-global-memory.md`) —
  M9 machine-global memory design.
- [`memhub-prd-addendum-m8-retrieval.md`](memhub-prd-addendum-m8-retrieval.md) — M8 SQL+RAG recall layer that M9 extends.
- [`memhub-prd-source-vocabulary-addendum.md`](memhub-prd-source-vocabulary-addendum.md) — `source` column semantics; global rows surface `source` verbatim per the existing convention.
- [`memhub-prd.md`](memhub-prd.md) — base PRD. Everything not modified by an addendum reads from here.

Task 45 in the DB is the research+plan origin of this addendum. The
addendum is the rationale; the DB is the source of truth for what is
locked.
