# memhub Export Format

This document specifies the on-disk format produced by `memhub export` and
consumed by `memhub import`. The format is the durable recovery contract;
the SQLite schema may evolve, but a `memhub` build must always be able to
read every format version it has shipped support for.

## Versioning

Exports carry a top-level `memhub_export_version` (integer). A new format
version is introduced when a schema change makes the existing layout
insufficient — never as a silent shape change.

**Before:** export always wrote version `1` and import accepted only `1`.
Version `1` had no retirement or task drop/keep metadata.
**After:** export always writes version `2`, even when there is no
lifecycle metadata. Import accepts versions `1` and `2`, translating
`1` to the current shape without lifecycle metadata. Older v1-only
builds refuse a v2 file with `unsupported memhub_export_version`, rather
than accepting it and restoring a retired fact as active.

`memhub_export_version` is independent of `source_schema_version`. The
former is the durable file contract; the latter records which migration
the source database was at when the export was taken.

## Top-level shape

This v2 example represents a development build after the v0.3.1 release,
with the package version still at `0.3.1`. The released v0.3.1 wrote v1.

```json
{
  "memhub_export_version": 2,
  "exported_at": "2026-11-21 04:00:00",
  "exported_by": "memhub 0.3.1",
  "source_schema_version": "0026_memory_lifecycle",
  "project": {
    "root_path_at_export": "/Users/.../my-repo",
    "created_at": "2026-04-21 09:00:00"
  },
  "facts":         [ /* see Fact */ ],
  "decisions":     [ /* see Decision */ ],
  "tasks":         [ /* see Task */ ],
  "commands":      [ /* see Command */ ],
  "pending_writes":[ /* see PendingWrite */ ],
  "writes_log":    [ /* see WriteLogEntry */ ]
}
```

`exported_at` is the value of SQLite `CURRENT_TIMESTAMP` at export time
(UTC, second precision). `exported_by` is the package name plus
`CARGO_PKG_VERSION` of the binary that produced the file.

## Record shapes

Each record carries its original primary key. Import preserves these IDs
so cross-table references (`decisions.superseded_by`, `writes_log.row_id`)
remain valid after restore.

### Fact

```json
{
  "id": 1,
  "key": "build-command",
  "value": "cargo build",
  "confidence": 1.0,
  "source": "user",
  "verified_at": null,
  "created_at": "2026-04-21 09:00:00"
}
```

In version `2`, retired facts also carry `retired_at` (UTC timestamp)
and `retired_reason`. Version `1` did not carry these fields and restores
facts as non-retired; absent fields in version `2` are treated as null.

### Decision

```json
{
  "id": 3,
  "title": "Use rusqlite bundled mode",
  "rationale": "Avoid system SQLite setup friction.",
  "status": "active",
  "decided_at": "2026-04-21 09:00:00",
  "superseded_by": null
}
```

`status` is one of `active`, `superseded`, `draft`. `superseded_by` may
reference a decision with a higher `id` than the current row, since newer
decisions supersede older ones.

### Task

```json
{
  "id": 5,
  "title": "Implement MCP server",
  "status": "open",
  "notes": "Milestone 3",
  "created_at": "2026-04-21 09:00:00",
  "updated_at": "2026-04-22 14:30:00"
}
```

In version `1`, `status` was one of `open`, `done`, `blocked`. Those
tasks still import without lifecycle metadata. Version `2` also accepts
`dropped`, which requires a nonblank `dropped_reason`, `pre_drop_status`
of `open` or `blocked`, and a non-null `pre_drop_updated_at`. Other
statuses must omit the drop metadata (or set it to null).

A saved keep requires a non-null `pre_keep_updated_at` and a nonblank `kept_at`
together, and is only valid for open, blocked, or dropped tasks. `kept_at`
must match `updated_at`, or `pre_drop_updated_at` for a dropped task.
The saved prior times accept exactly what `updated_at` accepts: any string,
including a blank legacy value. Undo restores it verbatim. This allows
`task unkeep` after restore (after `task undrop` if dropped).
Import rejects inconsistent undo metadata, naming the task, before
changing target rows or replacing the import safety backup.

### Command

```json
{
  "id": 7,
  "kind": "build",
  "cmdline": "cargo build",
  "last_exit_code": 0,
  "last_run_at": "2026-04-22 14:30:00",
  "success_count": 3,
  "fail_count": 1
}
```

### PendingWrite

```json
{
  "id": 11,
  "kind": "decision",
  "payload_json": "{...}",
  "rationale": "Proposed via MCP",
  "status": "pending",
  "actor": "claude-code",
  "actor_raw": "Claude Code",
  "created_at": "2026-04-22 15:00:00",
  "provenance_json": "{...}"
}
```

`kind` is one of `fact`, `decision`. `status` is one of `pending`,
`accepted`, `rejected`, `expired`. `payload_json` and `provenance_json`
are stored as opaque JSON strings, not parsed objects — this keeps the
format stable as their internal shape evolves.

### WriteLogEntry

```json
{
  "id": 15,
  "actor": "cli:user",
  "table_name": "decisions",
  "row_id": 3,
  "action": "insert",
  "reason": "decision add",
  "at": "2026-04-21 09:00:00"
}
```

## What is NOT in the export

By design, derived state is excluded. Recovery rebuilds it from the
restored durable rows or from git:

- `commits`, `files`, `commit_files` — re-run `memhub ingest-git` after
  import.
- `chunks` and `chunk_fts` — regenerated automatically from imported
  decisions during `memhub import`.
- `schema_migrations` — applied by the target database when opened.
- `.memhub/config.toml` — per-machine; copy manually if desired.
- `.memhub/backups/` — local-machine artifacts, not durable state.
- Ingested documents, including their archive state — re-ingest source
  files in the target repo if needed. Import leaves existing documents alone.

## Import contract

`memhub import <path>` restores an export into the current repo's
`.memhub/` (which must already exist via `memhub init`). The operation:

1. Validates `memhub_export_version` against the supported set
   (before: `{1}`; now: `{1, 2}`).
2. Refuses if the target database has any rows in `facts`, `decisions`,
   `tasks`, `commands`, or `pending_writes`, unless `--force` is passed.
3. Wraps the restore in a single transaction with
   `PRAGMA defer_foreign_keys = ON` so that the
   `decisions.superseded_by` self-reference does not block id-order
   inserts.
4. Wipes durable tables plus the `chunks` rows of `source_type =
   'decision'` so FTS state is rebuilt from a clean baseline.
5. Inserts all rows with their original IDs.
6. Regenerates decision chunks (and their FTS index) from the imported
   decisions.
7. Records an entry in `writes_log` for the import itself.
8. Runs `sync-md` so managed `AGENTS.md` / `CLAUDE.md` blocks reflect
   restored state.

`projects.root_path` is reconciled to the current target's repo root by
the standard `memhub` open path. The imported
`project.root_path_at_export` and `project.created_at` values are kept
in the export file for audit only; they are not written back.

## Forward compatibility

When `memhub_export_version` is bumped to a newer version:

- Older `memhub` builds continue to reject the newer version with a
  clear error message.
- Newer `memhub` builds keep accepting version `1` until that version
  is explicitly retired and documented.
- Per-version reader modules live alongside `v1` (e.g. `src/export/v2.rs`)
  rather than mutating the existing `v1` definitions.

The export format is the recovery contract. Treat it accordingly.
