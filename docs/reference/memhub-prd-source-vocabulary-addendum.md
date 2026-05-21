# PRD addendum: source vocabulary and multi-agent attribution

**Author:** Elswick
**Status:** Addendum to [`memhub-prd.md`](memhub-prd.md) (Draft v2). Authoritative for the items it modifies.
**Last updated:** 2026-05-13

This document supplements `memhub-prd.md` rather than replacing it.
The PRD stays verbatim per the project guardrail in `CLAUDE.md`.
Where this addendum and the PRD disagree on the items called out
below, this addendum is authoritative; everything not addressed here
continues to read from the PRD as-written.

---

## What this addendum modifies

| PRD section | Status after addendum | Reason |
|---|---|---|
| §8 "Data model" — `decisions` table lacks a `source` column | **Extended.** Migration `0008_decisions_source` adds `source TEXT NOT NULL DEFAULT 'user'` to decisions, matching facts. | Symmetry with facts; multi-agent attribution applies to decisions too. |
| §9 indexing principle — `source` value enumeration | **Refined.** The source enumeration becomes the compound vocabulary below. `agent:claude-code` / `agent:codex` / `agent:opencode` / `user` / `git` / `observed` remain valid; `user+agent:<id>` is added for agent-mediated user-approved writes. | Multi-agent bridge work; distinguishes "user typed it directly" from "agent surfaced it, user approved". |
| §11 write-back policy — accept path source assignment | **Specified.** When a pending write is accepted via `memhub review accept`, the durable row's `source` is derived from `pending_writes.actor` as `user+agent:<actor>` (or plain `user` if the actor is `user` / `unknown`). | The previous accept implementation hardcoded `source="user"`, dropping agent attribution. |

The PRD's design principles (§3), non-goals (§4), router design (§10),
MCP surface (§12), CLI surface (§13), migrations and export (§14),
security (§15), success metric (§17), and risks (§18) are otherwise
unchanged.

---

## 1. The compound source vocabulary

`source` is a single `TEXT` column on `facts` and (post-migration 0008)
on `decisions`. It identifies **where the claim came from**, distinct
from `writes_log.actor` / `pending_writes.actor`, which identify **who
performed the write**.

Valid `source` values:

| Value | Meaning |
|---|---|
| `user` | Direct human action. The operator ran `memhub fact add` (or equivalent) themselves with no agent intermediary. |
| `agent:<id>` | Agent-only assertion. A specific agent generated this claim; no user has endorsed it yet. Used for in-pipeline records and reserved for future direct-agent writes. Today, agent claims live in `pending_writes` until accepted, so this slot is rare on `facts` / `decisions`. |
| `user+agent:<id>` | Agent-mediated user-approved. An agent (Claude Code, Codex, OpenCode) surfaced the claim and a human approved it — typically via the `/wrap-up` flow. Both the user signal and the mediating agent are preserved. |
| `git` | Ingested from git history (commit messages, file moves, etc.). Reserved for the future git-ingestion path. |
| `observed` | Derived from observed signals (command exit codes, test results). Reserved for future writers. |

Format rules:
- `<id>` is the normalized client identity (e.g., `claude-code`, `codex`, `opencode`) as produced by the MCP `clientInfo.name` normalization map.
- The compound `user+agent:<id>` parses by splitting once on `+`; the left side is always `user`, the right side always begins with `agent:`.
- The column is unconstrained TEXT at the schema level; the vocabulary above is a convention enforced by writers, not by a CHECK constraint. This keeps the door open for additional facets without a migration.

## 2. Acceptance-path source derivation

When `memhub review accept <id>` promotes a `pending_writes` row to a
durable `facts` or `decisions` row, the durable row's `source` is
derived from the pending write's `actor` column:

```
pending_writes.actor         →   durable.source
─────────────────────────────────────────────────
"user"                       →   "user"
"unknown"                    →   "user"
"<agent>"  (e.g., "codex")   →   "user+agent:<agent>"
```

Rationale: the pending write was proposed by `<agent>` (captured at
MCP `initialize` time) and is being accepted now by an operator, so
both signals belong in the durable row.

## 3. Skill-driven writes

Agent wrap-up flows MUST pass `--source` explicitly on CLI writes:

- Claude Code `/wrap-up` (~/.claude/commands/wrap-up.md): `--source user+agent:claude-code`
- Codex `/wrap-up` (~/.codex/skills/wrap-up/SKILL.md): `--source user+agent:codex`
- OpenCode `/wrap-up` (~/.config/opencode/skills/wrap-up/SKILL.md): `--source user+agent:opencode`

This applies to `memhub fact add` and (post-0008) `memhub decision add`
when the write originates from an agent-mediated approval step in the
wrap-up flow.

Direct CLI use by a human (no agent in the loop) omits `--source` and
takes the default `user`.

## 4. Decisions symmetry (migration 0008)

`decisions` gains a `source` column with the same default and
semantics as `facts.source`. Touched code:

- `migrations/0008_decisions_source.sql` — `ALTER TABLE decisions ADD COLUMN source TEXT NOT NULL DEFAULT 'user';`
- `models::Decision` — adds `source: String`
- `commands::decision::add` — accepts `source: &str` (default `"user"` from CLI)
- `cli::DecisionCommand::Add` — adds `--source` flag (default `"user"`)
- `commands::review::accept` — passes the derived compound source through to `decision::add`
- `export::v1::Decision` — adds `source`
- `commands::import::run` — round-trips `source` (backward-compatible: missing `source` in older exports defaults to `"user"`)
- `render::load_decisions` — selects `source`; renders alongside title/rationale
- `mcp::DecisionDto` — exposes `source`

No backfill of historical rows: the `DEFAULT 'user'` covers existing
data, which is the correct semantic for rows written before this
addendum landed (pre-MCP-attribution era).

## 5. What does not change

- Schema stays additive. No `CHECK` constraints on `source` — values
  are convention, not enforcement.
- `writes_log` and `pending_writes` keep their `actor` / `actor_raw`
  columns as the write-performer record. `source` is not duplicated
  into those tables; the actor is sufficient for "who did this".
- Render output continues to surface `source` verbatim in the facts
  table. Decisions render gains the same column in the structured
  view.
- The PRD's "client alias normalization map" (§13 build notes line
  489) is the single source of truth for `<id>` values. Adding a new
  agent (e.g., `cursor`) means updating that map, not this vocabulary.
