# K9 `/wrap-up` ↔ memhub CLI contract (v1)

Status: shipped as part of `M5-002`.

This document is the source-of-truth contract between the K9 Claude
Framework's `/wrap-up` skill and the `memhub` CLI. K9's `/wrap-up`
implementation should treat the commands and output shapes described
here as stable. Changes to this contract require a new version
(`v2`); existing `v1` consumers must continue to work until they are
explicitly migrated.

## Sequencing

```
/wrap-up:
  1. Read agent_docs/* and any session signals as usual.
  2. Run `memhub integrations check-k9`.
       - Exit 0 → memhub is initialized and the K9 integration is
                 enabled for this repo. Continue with the shell-out
                 path described below.
       - Exit non-zero → skip the memhub path entirely. Continue
                 with K9's standalone Markdown-only flow.
  3. If `.memhub/` is present, optionally fetch staged proposals via
     `memhub review list --status pending` (or MCP
     `list_pending_writes`) and fold them into the draft. This is
     the `M5-003` surface; it is optional for `M5-002` consumers.
  4. Show drafts for human approval (existing K9 behavior).
  5. On approval:
       a. DB writes first. For each approved item, invoke the matching
          memhub CLI command (see below) with `--json` and
          `--actor k9:wrap-up`. Parse the JSON to recover the new
          row's identifier.
       b. Markdown writes second. Apply the approved drafts to
          `agent_docs/*.md`.
  6. If any DB write fails (non-zero exit), abort the entire wrap-up
     before touching Markdown. The DB writes that did succeed are
     durable; the user can re-run `/wrap-up` to retry the rest.
```

## Gating: `memhub integrations check-k9`

```
memhub integrations check-k9
```

- **Exit 0**: `.memhub/project.sqlite` exists AND the project's
  `config.toml` has `[integrations.k9].enabled = true`.
- **Exit 1**: anything else — no `.memhub/`, missing database,
  section absent, `enabled = false`, or any internal error.
- **stdout**: empty in both cases. K9 should not parse stdout.
- **stderr**: empty on the enabled path. May contain a one-line
  error message on the disabled path; K9 should ignore stderr for
  this command.

K9 should run this once near the top of `/wrap-up` and gate the
entire shell-out path on the exit code. There is no need to re-check.

## Mutating commands

Every mutating command described here accepts both `--json` and
`--actor <name>` flags. When `--json` is set the command writes a
single JSON object to stdout (no trailing whitespace beyond a
newline) and suppresses the human-readable line. When `--actor` is
omitted, the actor defaults to `cli:user`; K9 should always pass
`--actor k9:wrap-up`.

Actor validation: non-empty, ≤64 characters. Invalid actor values
produce exit 1 with a clear stderr message; the durable tables and
`writes_log` are not touched.

### `memhub fact add`

```
memhub fact add <key> <value> [--source <source>] --json --actor k9:wrap-up
```

Insert-or-upsert a fact by `key`. Default `source` is `user`.

**JSON response:**
```json
{
  "id": 12,
  "key": "build-command",
  "value": "cargo build",
  "source": "user",
  "created": true
}
```

- `id`: durable `facts.id` of the row that now holds this key.
- `created`: `true` if a new row was inserted, `false` if an existing
  row was updated.

Side effects: `verified_at` is refreshed to the current timestamp on
every call (insert and upsert), so accepting a previously-stale fact
during wrap-up clears its stale flag automatically.

### `memhub decision add`

```
memhub decision add <title> --rationale <rationale> --json --actor k9:wrap-up
```

Append a new active decision. Decisions are append-only; updates ship
as new rows that supersede older ones.

**JSON response:**
```json
{
  "id": 7,
  "title": "Adopt the kraken pattern"
}
```

Side effects: the decision is indexed into the FTS `chunks` table so
subsequent `memhub search` calls hit it.

### `memhub task add`

```
memhub task add <title> [--notes <notes>] --json --actor k9:wrap-up
```

Create a new open task.

**JSON response:**
```json
{
  "id": 3,
  "title": "Ship K9 contract"
}
```

### `memhub task done`

```
memhub task done <id> --json --actor k9:wrap-up
```

Mark an existing task done.

**JSON response:**
```json
{
  "id": 3,
  "status": "done"
}
```

Failure: exit 1 if the task does not exist.

### `memhub review accept`

```
memhub review accept <id> --json --actor k9:wrap-up
```

Promote a staged `pending_writes` row into its durable table. For
`fact` rows this delegates to `fact add`; for `decision` rows it
delegates to `decision add`. The `pending_writes` row is marked
`accepted` with `reviewed_at` set.

**JSON response:**
```json
{
  "pending_id": 4,
  "kind": "fact",
  "durable_table": "facts",
  "durable_id": 12
}
```

- `pending_id`: the `pending_writes.id` that was accepted.
- `kind`: `"fact"` or `"decision"`.
- `durable_table`: `"facts"` or `"decisions"`.
- `durable_id`: the durable row id that now holds the accepted data.

Failure: exit 1 if the pending write does not exist or is not in
`pending` status (it may have been already accepted, rejected, or
expired).

### `memhub review reject`

```
memhub review reject <id> [--reason <reason>] --json --actor k9:wrap-up
```

Mark a staged proposal `rejected`. The optional `--reason` is
captured in `writes_log` for the audit trail.

**JSON response:**
```json
{
  "pending_id": 4
}
```

## Exit codes

- `0` — success.
- `1` — any failure (validation, missing row, database error, etc).
  K9 should treat any non-zero exit as a hard abort signal for the
  entire wrap-up DB-write phase.

## Audit trail

Every write produced by these commands generates a `writes_log` row
with `actor = "k9:wrap-up"` (when K9 passes the recommended actor).
K9 wrap-up writes can be reconstructed via:

```sql
SELECT * FROM writes_log
WHERE actor = 'k9:wrap-up'
ORDER BY at DESC;
```

## Explicit non-goals

- No `--dry-run` flag. K9 should validate proposed payloads
  client-side before invoking memhub.
- No reverse-direction sync. memhub never reads or writes
  `agent_docs/*.md` on K9's behalf.
- No `memhub k9-wrap-up` aggregate command. K9 should call the
  individual primitives so the audit trail remains granular.
- No JSON `schema_version` field. This document is the version
  artifact; bumps require an explicit `v2` of the contract.

## Version history

- `v1` (2026-05-12) — initial contract shipped with `M5-002`:
  `check-k9` gate, `--json`/`--actor` on `fact add`, `decision add`,
  `task add`, `task done`, `review accept`, `review reject`.
