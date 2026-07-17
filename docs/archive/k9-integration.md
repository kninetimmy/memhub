# K9 Claude Framework Integration

Status: phases 1 (`M5-001`), 2 memhub-side (`M5-002`), and 3 memhub-side
(`M5-003`) shipped. The v1 wrap-up contract lives at
[`docs/archive/k9-wrap-up-contract.md`](k9-wrap-up-contract.md).
The K9-repo consumer edit that calls into the contract end-to-end (gate
+ read + mutate) remains triaged separately, outside this repo. The
operator-facing handoff for that work is the audit prompt at
[`docs/archive/k9-consumer-audit-prompt.md`](k9-consumer-audit-prompt.md).

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

## Non-goals (revised after K9 deprecation track shipped)

The non-goals below were authored when memhub was complementary to
K9. The deprecation addendum (`docs/reference/memhub-prd-deprecation-addendum.md`)
revisits each one explicitly. Status of each is called out inline.

- **No reverse-direction sync of human edits to rendered markdown.**
  *Still in force.* `memhub render` is a one-way projection from the
  DB. There is no parser that reads edits to `agent_docs/PROJECT.md`
  or `PROJECT_LEDGER.md` back into the DB. Humans who want to change
  durable content use the CLI (`memhub state set`, `decision add`,
  etc.). The original wording covered the K9 four-file shape; the
  same principle now covers the memhub-rendered shape.
- **No general `k9 import/export/sync` CLI surface.** *Still in
  force.* `memhub integrations bootstrap-k9` remains the one narrow
  exception — first-install-only, refuses on any non-empty target,
  writes through `decision::add` and `task::add_with_status` with
  `actor = "k9:bootstrap"`. After bootstrap, `memhub render` takes
  over as steady state. No reverse direction; no general re-sync.
- **No managed-block writes inside `agent_docs/`.** *Clarified.*
  The `<!-- memhub:managed:start -->` block (the small at-a-glance
  status section) is still only emitted into root `CLAUDE.md` /
  `AGENTS.md` by `memhub sync-md`. Render is a *separate* mechanism
  with its own marker (`<!-- memhub:rendered -->`) that emits whole
  files (`PROJECT.md`, `PROJECT_LEDGER.md`) into the configured
  output directory (default `.memhub/rendered/` as of 2026-05-14).
  The two surfaces don't
  overlap; managed blocks and rendered files coexist.
- **No mapping of `project_state.md` or `project_arch.md` into DB
  tables.** *Overturned.* Migration `0007_project_narrative` adds
  `project_state` and `project_arch` tables (single durable-text
  blobs, append-only history). The K9-canonical narrative files are
  no longer the source of truth in memhub-primary repos; the DB
  blobs are, and `memhub render` projects them into `PROJECT.md`.
  See the addendum §2 for full rationale.

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
   `M5-002`**: v1 contract at `docs/archive/k9-wrap-up-contract.md`,
   `memhub integrations check-k9` exit-code gate, `--json` / `--actor`
   flags on every mutating command K9 needs. The K9 repo consumer edit
   that actually calls those commands lives outside this repo and
   remains a separate landing.
3. **Pending-write promotion** — `/wrap-up` reads `pending_writes` (via
   `memhub review list --json` or MCP `list_pending_writes`) and surfaces
   them for inclusion in wrap-up drafts. **Memhub side shipped as
   `M5-003`**: `--json` read surfaces on `review list` and `review show`
   mirroring the MCP `PendingWriteToolRecord` shape, locked into the v1
   contract as an additive amendment (no `v2` bump). The K9-repo
   consumer edit that actually folds these into draft assembly lives
   outside this repo.

Each phase is independently useful and reversible.
