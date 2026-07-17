# PRD addendum: K9 deprecation and the markdown inversion

**Author:** Elswick
**Status:** Addendum to [`memhub-prd.md`](memhub-prd.md) (Draft v2). Authoritative for the items it modifies.
**Last updated:** 2026-05-14

This document supplements `memhub-prd.md` rather than replacing it.
The PRD stays verbatim per the project guardrail in `CLAUDE.md`.
Where this addendum and the PRD disagree on the items called out
below, this addendum is authoritative; everything not addressed here
continues to read from the PRD as-written.

---

## What this addendum modifies

| PRD section | Status after addendum | Reason |
|---|---|---|
| §2 "Why this exists" — final paragraph (markdown as entry point) | **Inverted.** Markdown is now an *output* of the DB. | K9 deprecation track. |
| §6.2 "Layout per repo" — local render output not previously listed | **Extended.** `memhub render` emits `PROJECT.md` and `PROJECT_LEDGER.md` into `.memhub/rendered/` by default. Render output is local generated state, not committed by default. | Render slice (`c3fbef0`), machine-local default update (2026-05-14). |
| §8 "Data model" — `project_state` and `project_arch` tables not present | **Extended.** Migration `0007_project_narrative` added both as durable-text-blob tables. | Render slice step 1 (`2757a0a`). |
| §13 "CLI surface" — no `state`, `arch`, `render`, `note add` commands | **Extended.** All four ship; full list in §3 below. | Render slice + wrap-up step 1 (`5037033`). |
| §16 "Milestones" — Milestone 5+ list | **Extended.** "Milestone 6: K9 deprecation" added with shipped slices listed. | This addendum. |

The PRD's design principles (§3), non-goals (§4), indexing principle
(§9), router design (§10), write-back policy (§11), MCP surface (§12),
migrations and export (§14), security (§15), success metric (§17),
and risks (§18) are all unchanged.

---

## 1. The §2 inversion (load-bearing)

PRD §2's final paragraph reads:

> The fix is a single local database per repo that both agents read from through MCP. Both tools see the same facts, same decisions, same file history, same learned commands. **The markdown files stay as the entry point — but their "durable knowledge" section is generated from the DB instead of hand-maintained in two places.**

**Replace the bolded sentence with:**

> The DB is the source of truth. Markdown is generated from the DB by `memhub render` and lives at `.memhub/rendered/PROJECT.md` (narrative view) and `.memhub/rendered/PROJECT_LEDGER.md` (structured view) by default. These files are local generated state, ignored with `.memhub/`, and should not be committed unless a repo explicitly opts into a tracked render path. The agent `CLAUDE.md` / `AGENTS.md` files remain at the repo root as project instructions, with their managed `<!-- memhub:managed:start -->` block still rendered by `memhub sync-md` for at-a-glance status. Session-start convention is to read the local rendered `PROJECT.md` if present, then use `memhub recall` for deeper context.

The original "two places to hand-maintain" framing was a property of
the K9-coexistence era. Under deprecation, there is one place: the DB.
Render is one-way. There is no parser that pulls human edits to
`PROJECT.md` back into the DB; humans who want to change durable
content use `memhub state set` / `memhub arch set` /
`memhub decision add` / `memhub task add|done` / `memhub fact add` /
`memhub note add`.

## 2. State and architecture as DB content

`docs/archive/k9-integration.md` previously listed "no mapping of
`project_state.md` or `project_arch.md` into DB tables" as a non-goal.
That non-goal is **overturned**. Migration `0007_project_narrative`
adds two tables:

```sql
project_state(id, project_id, body, actor, actor_raw, created_at)
project_arch (id, project_id, body, actor, actor_raw, created_at)
```

Both are append-only history; `set` always inserts a new row,
`show` returns the most recent, `history` lists prior. Bodies are
markdown prose, validated non-empty after trim and capped at 64K
characters.

Decision rationale (single-blob over decomposed columns) is captured
in `agent_docs/project_decisions.md` (decision dated 2026-05-12) and
in `docs/archive/memhub-render-design.md` §2. Future schema work can
decompose if querying patterns demand it; blob ships first because
narrative resists clean decomposition without losing prose flow.

## 3. CLI surface added since PRD v2

In addition to the §13 list, the following commands now exist:

```
memhub state set [BODY] [--from-file PATH] [--actor NAME] [--json]
memhub state show [--json]
memhub state history [--limit N] [--json]

memhub arch set [BODY] [--from-file PATH] [--actor NAME] [--json]
memhub arch show [--json]
memhub arch history [--limit N] [--json]

memhub render

memhub note add [TEXT] [--from-file PATH] [--actor NAME] [--json]
```

`note add` shipped as the only new primitive the wrap-up routing
brain (§5 below) requires. `state` and `arch` are the durable
storage for what was previously `project_state.md` and
`project_arch.md`. `render` emits `PROJECT.md` and `PROJECT_LEDGER.md`
into the configured output dir (default `.memhub/rendered/`, configurable
via `[render].output_dir` in `.memhub/config.toml`).

## 4. Render is one-way; conflict semantics are DB-wins-with-backup

`memhub render` is on-demand only — there is no `auto_render` config
flag in v1, and one is reserved as a future opt-in only. Existing
rendered files are unconditionally backed up under
`.memhub/backups/rendered/<stamp>/` before being overwritten.

Human edits to rendered files do not survive the next `memhub render`.
The header marker on every rendered file (`<!-- memhub:rendered -->`)
documents this contract. To change content, use the CLI; the file is
a view, not a source.

Full design rationale lives in [`docs/archive/memhub-render-design.md`](../archive/memhub-render-design.md).

## 5. Wrap-up routing brain is a Claude Code skill, not a CLI subcommand

PRD §13's CLI surface is the primitive set. The orchestrator that
walks the session-end approval gate (read window → draft → per-item
approval → DB writes → render) lives as a Claude Code project-level
slash command at `.claude/commands/wrap-up.md`, not as a
`memhub wrap-up` CLI subcommand.

Memhub itself stays small primitives. Skill prompts iterate without a
Rust recompile, and the slash-command UX preserves the muscle memory
the deprecation track was specifically committed to keep.

Full design rationale, including why-not-CLI and the slash-command
collision-resolution pattern (rename the user-level skill, not the
project-level one), lives in [`docs/archive/wrap-up-design.md`](../archive/wrap-up-design.md).

Future memhub-aware skills (`/init-project`, `/check-init`
equivalents) follow the same pattern. None ship in v1.

## 6. K9 framework deprecation status

The K9 Claude Framework is on a deprecation track. The end state has
memhub as the single durable store; K9's slash commands, markdown
templates, and routing brain retire.

Shipped:

- Render slice (steps 1+2): `2757a0a`, `c3fbef0`
- Wrap-up slice (steps 1+2 + dogfood): `5037033`, `588168b`,
  `103eea0` (override-gap fix), `591832f` (memhub-primary migration
  of this repo)
- This addendum (slice 2 of the deprecation plan)

Carried forward:

- Existing K9 repos (e.g., Free-AI-SSD) continue to use K9 with
  memhub as the optional cache via the v1 K9 `/wrap-up` shell-out
  contract (`docs/archive/k9-wrap-up-contract.md`). No forced
  migration.
- `memhub integrations bootstrap-k9` stays as the priming ramp for
  K9 repos that opt into memhub-primary. After bootstrap, render
  takes over as steady state. Removal is deferred until the K9
  framework itself stops shipping releases.

Full plan and the four load-bearing slices are documented in
[`docs/archive/k9-deprecation-plan.md`](../archive/k9-deprecation-plan.md).

## 7. What did not change

These PRD-level commitments hold without modification:

- **Local-first, local only by default** (PRD §3.2). No new network
  behavior. Render writes locally only.
- **Agents are untrusted writers** (PRD §3.3). Wrap-up still gates
  every write through human approval. Render itself only reads.
- **Boring tech** (PRD §3.5). No new dependencies introduced by the
  render or wrap-up slices beyond what was already in the tree.
- **One DB file = one repo** (PRD §3.6). The new tables live in the
  same `.memhub/project.sqlite`. Render output, backups, config,
  embeddings, and the database are machine-local by default under
  `.memhub/`; Git is not the synchronization layer for those files.
- **Non-goals §4.** All six PRD non-goals (multi-user sync, replacing
  git/GitHub/agents, becoming a general knowledge base, embeddings,
  auto-compaction, cloud) remain in force.
- **Indexing principle §9.** No new full-table-scan paths introduced.
  `memhub render` walks indexed tables only; the windowed
  `writes_log` slice for "recent activity" uses the existing
  `created_at`/`at` index.
- **Write-back policy §11.** Wrap-up writes are user-authored
  (`source = "user"`, `confidence = 1.0`) because they pass through
  the human approval gate. The verifiable-vs-self-reported split is
  unchanged.
- **MCP tool surface §12.** No new MCP tools shipped in this slice.
  `memhub render` is CLI-only; an MCP wrapper can come later if a
  real workflow asks.
- **Export format §14.** `state` and `arch` rows are not yet included
  in the v1 export. A `v2` export would add them; until then they
  are recreated by the user re-running `memhub state set` /
  `memhub arch set` after import.

## 8. Migration path for existing repos

For a fresh repo (no K9 history): `memhub init` then start using
`memhub state set` / `memhub arch set` / `memhub decision add` /
`memhub task add`. Run `memhub render` at session end (or from
`/wrap-up`). `.memhub/rendered/PROJECT.md` and
`.memhub/rendered/PROJECT_LEDGER.md` are local generated output; do
not commit them unless the repo explicitly opts into a tracked render
path.

For an existing K9 repo that wants to migrate to memhub-primary:

1. `memhub init` (creates `.memhub/`).
2. `memhub integrations bootstrap-k9` — one-shot import of existing
   `agent_docs/project_decisions.md` and `project_backlog.md` into
   `decisions` and `tasks`.
3. `memhub state set --from-file agent_docs/project_state.md`
4. `memhub arch set --from-file agent_docs/project_arch.md`
5. `memhub render` (emits local `PROJECT.md` and `PROJECT_LEDGER.md`).
6. Update `CLAUDE.md` Session Continuity to point at local rendered
   files or at `memhub recall` instead of the four `project_*.md` files.
7. `memhub integrations disable-k9`.
8. Decide whether to remove the four legacy `project_*.md` files or
   keep them as historical archive (gitignored or in-tree).

This repo executed steps 1–8 across `c1becc5..591832f`; that
sequence is the reference implementation.

## 9. Reference design docs

Authoritative for the items they cover:

- [`docs/archive/memhub-render-design.md`](../archive/memhub-render-design.md) — output shape, conflict semantics, trigger model, output location, state/arch storage.
- [`docs/archive/wrap-up-design.md`](../archive/wrap-up-design.md) — wrap-up routing brain location, session-boundary model, K9-vocabulary scope.
- [`docs/archive/k9-deprecation-plan.md`](../archive/k9-deprecation-plan.md) — full deprecation direction and slice enumeration.
- [`docs/archive/k9-wrap-up-contract.md`](../archive/k9-wrap-up-contract.md) — v1 K9 `/wrap-up` shell-out contract (still authoritative for K9-using repos).
- [`docs/reference/export-format.md`](export-format.md) — export format v1; addendum will follow if `state`/`arch`/`session_notes` get added in v2.
