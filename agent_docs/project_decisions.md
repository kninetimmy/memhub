# Project Decisions

Append-only. Superseding decisions should be added as new dated entries rather than rewriting old ones.

---

## 2026-04-21 - Initialized project_docs framework

- Adopted `AGENTS.md`, `CLAUDE.md`, and `agent_docs/` as durable continuity files for future agent runs.

## 2026-04-21 - Milestone 1 stays intentionally narrow

- The initial scaffold implements CLI, SQLite, migrations, config, logging, and basic CRUD for facts, decisions, tasks, and status.
- MCP, git ingestion, markdown sync, router logic, confidence decay, and advanced write-back remain deferred.

## 2026-04-21 - One-database-per-repo schema still keeps `project_id`

- Even though each `.memhub/project.sqlite` only serves one repo, the schema keeps a `projects` row plus `project_id` foreign keys to stay close to the PRD and reduce future migration churn.

## 2026-04-21 - Migrations auto-apply when the CLI opens a project

- An explicit `memhub migrate` command is deferred.
- This keeps the current scaffold usable while still preserving numbered SQL migrations in the repository.

## 2026-04-21 - Milestone 1 command verification uses explicit exit-code recording

- The CLI records command history through `memhub command verify` instead of adding automated capture or a review queue early.
- Exit code is the Milestone 1 verification signal for command history in the existing schema; richer verification metadata remains deferred with the broader write-policy work.

## 2026-04-21 - Initial Milestone 2 search indexes decision text plus exact file history

- `memhub search` uses exact indexed file-path lookups for history queries and SQLite FTS5 over decision text for free-text retrieval.
- Additional chunk sources can be added later, but the first implementation stays narrow so the router remains explainable and the query-plan tests stay simple.

## 2026-04-22 - README should be refreshed at each milestone completion

- When a milestone or a major milestone slice is completed, update `README.md` so the public project description, current capabilities, and roadmap status reflect the new state of the codebase.
- Treat README maintenance as part of milestone completion rather than a later cleanup task.

## 2026-04-22 - Milestone 3 MCP work uses the official `rmcp` Rust SDK

- `memhub` uses `rmcp` for the first MCP server slice because it is the official Rust SDK and supports the stdio server transport needed for the local-first CLI workflow.
- The initial MCP surface stays narrow: thin adapters over existing services plus explicit verified command recording, while broader agent-originated write policy remains deferred.

## 2026-04-22 - Milestone 4 recovery work ships in two slices

- The supported recovery path should start with portable `memhub export` / `memhub import`, not a raw database-file copy workflow.
- `memhub init` should stay non-interactive at first; any convenience recovery UX should layer on after export/import exists and is stable.
- If `.memhub/` exists but `.memhub/project.sqlite` does not, `memhub` should treat that as an explicit recovery/safety case instead of silently creating a fresh database.
- When recovery features ship, `README.md` should gain a clear backup/restore section with readable step-by-step instructions.

## 2026-04-22 - M3-003 stages agent-originated MCP writes instead of promoting them directly

- Agent-originated MCP fact and decision writes land in `pending_writes`, not `facts` or `decisions`.
- `memhub status` and the MCP `status` tool expose pending-write count so staged proposals are visible before a review UX exists.
- MCP client identity is derived from `clientInfo.name`, normalized for known Codex and Claude Code aliases, and stored alongside the raw observed value.

## 2026-04-22 - Pending-write provenance stores only MCP metadata that exists today

- `pending_writes` now stores MCP provenance as a JSON blob so the schema can remain forward-compatible without pretending prompt or session text is available yet.
- The stored provenance currently covers MCP request ID, request meta, protocol version, client name/version, and initialize meta where `rmcp` exposes them.
- Prompt/session review context remains deferred until the transport surface or Milestone 4 review design gives a concrete source for it.

## 2026-05-12 - Export format version is independent of database schema version

- `memhub_export_version` is the durable on-disk contract; `source_schema_version` records which SQLite migration the source was at.
- A new format version is introduced only when a schema change makes the existing layout insufficient, and lands as a sibling reader module (`src/export/v2`) rather than mutating `v1`.
- Older `memhub` builds must reject newer format versions with a clear error; newer builds keep accepting older versions until the older version is explicitly retired and documented.

## 2026-05-12 - Export covers durable user data only; derived state is regenerated

- The export contains `facts`, `decisions`, `tasks`, `commands`, `pending_writes`, and `writes_log`.
- Git-derived data (`commits`, `files`, `commit_files`), FTS state (`chunks`, `chunk_fts`), `schema_migrations`, and per-machine config are excluded by design.
- Import regenerates decision chunks from the restored decisions; the user re-runs `memhub ingest-git` after import to repopulate git history from the local repository.

## 2026-05-12 - Import is wipe-and-restore with ID preservation, not merge

- `memhub import` refuses on a non-empty target unless `--force` is passed, and a forced run wipes durable tables before inserting.
- Row IDs are preserved so cross-table references (`decisions.superseded_by`, `writes_log.row_id`) stay valid.
- The restore runs in a single transaction with `PRAGMA defer_foreign_keys = ON` so the self-referencing `decisions.superseded_by` FK does not block id-order inserts.
- Merge semantics are explicitly deferred; if needed they ship as a separate command rather than an `--merge` mode on `import`.

## 2026-05-12 - Import target must be initialized first

- `memhub import` requires the target to already have a `.memhub/` created via `memhub init` and will not auto-initialize.
- This keeps the recovery flow explicit and leaves the missing-`.memhub/` and missing-`project.sqlite` scenarios to be handled by `M4-002`.

## 2026-05-12 - Missing `project.sqlite` is an explicit recovery error, not a silent re-init

- `db::open_project` and `db::init_project` both return `MemhubError::MissingDatabase` when `.memhub/` exists without `project.sqlite`.
- The error names the missing path and points the user at `memhub init --from-backup <path>` for recovery or at removing `.memhub/` to start over.
- `db::init_project_for_recovery` is the explicit recovery-mode entry point used by the `init --from-backup` flow; plain `memhub init` stays strict and non-interactive.

## 2026-05-12 - `memhub init --from-backup <path>` is the single recovery entry point

- The single-step recovery UX lives on `init`, not on `import`, so `memhub import` keeps its prior "target must be initialized first" contract unchanged.
- `init --from-backup` refuses to run when `.memhub/project.sqlite` already exists; the documented overwrite path remains `memhub import --force <path>` on a live database.
- The flag works in both the clean-clone case (no `.memhub/`) and the missing-database case (existing `.memhub/` without `project.sqlite`).

## 2026-05-12 - Promoted facts use `source = "user"`, not encoded original-actor strings

- When `memhub review accept` promotes a staged fact, the resulting `facts` row uses `source = "user"` and `confidence = 1.0`, matching the PRD §8 user-authored category and existing `memhub fact add` behavior.
- The original-actor chain (which agent proposed it, when, with what provenance JSON) is preserved on the `pending_writes` row (which stays around with `status = 'accepted'` and `reviewed_at` set) and in `writes_log`.
- This keeps the `facts.source` vocabulary small and consistent rather than expanding it with `user-confirmed-from:<actor>` variants that would be harder to query.

## 2026-05-12 - Review and promotion is CLI-only; MCP gains a read-only proposal list

- Promotion of staged proposals into durable `facts` / `decisions` happens through `memhub review accept` only. There is no MCP tool that accepts on the user's behalf, consistent with PRD §12's asymmetry between read and write surfaces.
- MCP exposes `list_pending_writes` as a read-only adapter so K9 `/wrap-up` (and any future review UI) can surface staged proposals during the human-approval gate without needing direct DB access.
- `reject` is also CLI-only, with the user-supplied reason captured in `writes_log` rather than as a column on `pending_writes`.

## 2026-05-12 - Pending writes age out explicitly, not automatically on read

- `memhub review expire` is the only path that transitions a `pending_writes` row to `status = 'expired'`. No read-shaped command (`review list`, `status`, MCP `list_pending_writes`) has expiry side effects.
- The default cutoff is `--older-than-days 30`, matching PRD §11.3. Users / cron jobs can override per invocation.
- `expire` emits a single summary `writes_log` row rather than one entry per expired row; the affected `pending_writes` rows are still inspectable directly because they retain their original `id`, `payload_json`, and `actor`.

## 2026-05-12 - `reviewed_at` column lives on `pending_writes`, not derived from `writes_log`

- Migration `0005_pending_write_reviewed_at` adds a nullable `reviewed_at TEXT` column on `pending_writes`.
- It is set on any transition out of `pending` (accepted, rejected, expired) and stays null while a proposal is still pending.
- Keeping it on the row directly makes "show me recent reviews" queryable without joining `writes_log`, and makes `review show` self-contained for human inspection.

## 2026-05-12 - Review acceptance is not a single transaction across `pending_writes` and the durable table

- `accept` delegates to existing `fact::add` / `decision::add`, each of which opens its own connection and transaction, and then runs a second update on `pending_writes` in a separate transaction.
- A failure between the durable insert and the pending-row update leaves the pending row in `pending` so a retry is safe; `fact::add`'s `(project_id, key)` upsert makes the fact path idempotent, and a duplicate decision created via retry is acceptable (the original-actor provenance still points back through `pending_writes`).
- The alternative — refactoring `fact::add` / `decision::add` to accept an existing transaction — was rejected to keep this slice narrow and avoid touching every existing caller for a corner case that is local-only and easy to recover from manually.
