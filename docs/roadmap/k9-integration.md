# K9 Claude Framework Integration

Status: phases 1 (`M5-001`) and 2 memhub-side (`M5-002`) shipped. The
v1 wrap-up contract lives at
[`docs/reference/k9-wrap-up-contract.md`](../reference/k9-wrap-up-contract.md).
The K9-repo consumer edit for phase 2 and phase 3
(`pending_writes` surfacing during wrap-up, `M5-003`) remain triaged.

## Goal

When `memhub` is installed alongside the K9 Claude Framework, K9's session
ritual (`/init-project`, `/wrap-up`, `/check-init`) should write durable
records into `memhub`'s SQLite database as part of the existing human-approval
gate, in addition to writing the K9 Markdown files. Each project can use
either system standalone, or both together.

## Operating modes

1. **K9 only** — `agent_docs/` exists, no `.memhub/`. K9 works as today.
2. **memhub only** — `.memhub/` exists, no `agent_docs/`. memhub works as today.
3. **K9 + memhub** — both exist. memhub is configured in K9 integration mode;
   K9 commands write to both Markdown and the database from the same approval.

memhub never requires K9. K9 never requires memhub.

## Source-of-truth model

- Git is the factual repository history.
- K9 Markdown (`agent_docs/`) is the human operating view.
- memhub SQLite is the indexed backend, written from the same approved drafts
  in the same step — not synced after the fact.
- memhub MCP `pending_writes` is the agent-originated proposal surface used
  mid-session; promotion happens at `/wrap-up` review time.

## Session flow

```
during session:
  agent calls memhub MCP propose_fact / propose_decision
    -> lands in pending_writes (already implemented)

/wrap-up:
  1. read four agent_docs files + git log since last session
  2. if .memhub/ exists, also read pending_writes
  3. draft per-file Markdown updates that fold in (2)
  4. show drafts for human approval (existing K9 behavior)
  5. on approval:
       a. DB writes first: shell out to `memhub decision add`,
          `memhub task add`, `memhub fact add`; mark related
          pending_writes as accepted
       b. Markdown writes second: apply approved drafts to agent_docs/
  6. if any DB write fails, abort before touching Markdown.
```

## Install / detect / configure

memhub install must detect K9 at install time and configure itself
accordingly:

- If K9 is not detected, install proceeds normally with the current default
  profile.
- If K9 is detected, install (or `memhub init`) writes a K9 integration
  section into `.memhub/config.toml`:

  ```toml
  [integrations.k9]
  enabled = true
  agent_docs_path = "agent_docs"
  ```

- Install is non-destructive: no K9 files are written or modified by memhub
  install.

## Non-goals

- No bidirectional sync. memhub does not re-render `agent_docs/*.md` from
  database state.
- No new `k9 import/export/sync` CLI surface. Existing `memhub decision add` /
  `task add` / `fact add` commands are the invocation surface.
- No managed-block writes inside `agent_docs/`. memhub continues to manage
  only the existing block in root `CLAUDE.md` / `AGENTS.md`.
- No mapping of `project_state.md` or `project_arch.md` into DB tables. Those
  stay K9-only.

## Schema fit

- `project_decisions.md` new entry -> `decisions` row (title, rationale)
- `project_backlog.md` new item -> `tasks` row (title, notes)
- Inline facts surfaced during wrap-up -> `facts` row (key, value)

`/wrap-up` collects discrete fields (title vs. rationale, etc.) at approval
time rather than parsing Markdown after the fact.

## Phasing

1. **K9 detection + config** — `memhub init` learns the K9 integration
   profile. `memhub integrations enable-k9 / disable-k9 / status`
   handle explicit toggling on already-initialized repos. Status
   surfaces drift between config and filesystem. **Shipped as
   `M5-001`.**
2. **`/wrap-up` post-approval hook** — K9 repo's `/wrap-up.md` gains an
   optional final step that shells out to memhub when `.memhub/` exists
   and `[integrations.k9].enabled = true`. **Memhub side shipped as
   `M5-002`**: v1 contract at `docs/reference/k9-wrap-up-contract.md`,
   `memhub integrations check-k9` exit-code gate, `--json` / `--actor`
   flags on every mutating command K9 needs. The K9 repo consumer edit
   that actually calls those commands lives outside this repo and
   remains a separate landing.
3. **Pending-write promotion** — `/wrap-up` reads `pending_writes` (via
   `memhub review list` or MCP `list_pending_writes`) and surfaces them
   for inclusion in wrap-up drafts. Triaged as `M5-003`. Lives entirely
   in the K9 repo; no memhub-side code change anticipated.

Each phase is independently useful and reversible.
