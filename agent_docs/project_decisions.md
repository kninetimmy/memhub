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

## 2026-05-12 - Deny list ships with `globset` and matches by path segments

- `memhub` uses the `globset` crate for deny-list pattern compilation so patterns like `secrets/**` work the same way `.gitignore` patterns do, without rolling our own glob engine.
- `PathMatcher::is_denied` matches the full normalized path *and* each `/`-separated suffix, so unrooted patterns like `*.pem` deny `config/server.pem` even though they don't start at the repo root. This matches the user expectation that "*.pem" means "any .pem file anywhere."
- The trade-off is one extra small dep (`globset` + transitive `aho-corasick`/`regex`), which is acceptable given how widely used these crates already are in the Rust ecosystem.

## 2026-05-12 - Deny-list pattern compilation fails closed

- If any user-supplied pattern fails to compile, `commands::ingest_git` and `commands::search` return `MemhubError::InvalidInput` and refuse to run.
- Fail-open (warn-and-skip) was rejected because a typo in a deny pattern is exactly the kind of mistake that silently regresses sensitive-data protection. Hard failure forces the user to fix the config before sensitive paths can flow through.

## 2026-05-12 - Deny list is filter-on-read for existing data; no auto-cleanup

- New ingestions skip denied paths. Search post-filters denied paths even if older `files` / `commit_files` rows still contain them.
- After a pattern change, previously-ingested denied paths stay in the database but never surface through search. Deletion is explicitly deferred to a future `memhub gc` slice that does not exist yet.
- The reasoning is that a destructive auto-cleanup on every pattern change is exactly the kind of surprise behavior that erodes trust. Filter-on-read is sufficient for the "agents reading memory can't read these" property the PRD requires.

## 2026-05-12 - Deny list is path-based, not content-based

- Patterns match the file path only. Content scanning for credential strings (e.g. AWS access key IDs, GCP service-account keys embedded in tracked files) is out of scope for this slice.
- The PRD's mention of "common AWS/GCP credential patterns" is interpreted as filenames (`.aws/credentials`, `.gcloud/credentials*`) rather than regex over file contents. Content scanning is a much larger design space (false positives, performance over commit history, what to do on partial matches) and would distract from the M4 trust theme.

## 2026-05-12 - Review acceptance is not a single transaction across `pending_writes` and the durable table

- `accept` delegates to existing `fact::add` / `decision::add`, each of which opens its own connection and transaction, and then runs a second update on `pending_writes` in a separate transaction.
- A failure between the durable insert and the pending-row update leaves the pending row in `pending` so a retry is safe; `fact::add`'s `(project_id, key)` upsert makes the fact path idempotent, and a duplicate decision created via retry is acceptable (the original-actor provenance still points back through `pending_writes`).
- The alternative — refactoring `fact::add` / `decision::add` to accept an existing transaction — was rejected to keep this slice narrow and avoid touching every existing caller for a corner case that is local-only and easy to recover from manually.

## 2026-05-12 - Fact staleness ships as a 90-day hardcoded threshold, not continuous decay

- A fact is stale when `verified_at` is null or older than 90 days. The threshold lives as `models::FACT_STALE_AFTER_DAYS = 90` in code, not as a config knob.
- Continuous decay (e.g. exponential half-life on `confidence`) was rejected for v1 because it adds tuning knobs and makes confidence harder to reason about; the PRD §11.4 wording is the simpler stale-flag model and matches v1 expectations.
- Staleness is computed at read time in SQL via `julianday('now') - julianday(verified_at) > 90`. There is no stored `is_stale` column — the result drifts as time passes, which is correct.
- Promotion to a config knob is deferred until a real workflow shows the default needs tuning.

## 2026-05-12 - Command confidence is derived from existing counters, not a persisted column

- `CommandRecord::confidence()` returns `Option<f64>` equal to `success_count / (success_count + fail_count)`, with `None` when there have been no runs.
- No new `commands.confidence` column. Adding one was considered and rejected: the counters are already the source of truth, `command verify` already updates them on every run, and a stored derived value would double the state we'd have to keep consistent.
- The trade-off is that an EMA / recency-weighted confidence isn't possible without schema change. If a real workflow needs that, it ships as a follow-on slice with its own migration.

## 2026-05-12 - Staleness and confidence updates rely only on existing write paths

- The only triggers that refresh `verified_at` on a fact are `memhub fact add` (insert or upsert, including the upsert path used by `review accept`) and any other caller of `fact::add`. There is no implicit auto-bump from search, list, or any other read.
- The only triggers that update command counters are `memhub command verify` and the MCP `record_command` adapter that delegates to it.
- Auto-bumping fact confidence on duplicate `propose_fact` arrivals was rejected for this slice — it would require a dedupe path we haven't designed. Treating every read as weak re-verification was rejected to stay consistent with PRD §11.2 ("agent claiming 'that worked' in chat is not a verifiable signal").
- This keeps the slice narrow and means no new write-side surface was added in `M4-005`.

## 2026-05-12 - K9 detection is a single-file existence probe at a configurable path

- `memhub` treats the K9 Claude Framework as present when `<agent_docs_path>/project_state.md` exists as a regular file. `agent_docs_path` is configurable per project (default `"agent_docs"`).
- A single canonical file was chosen over "all four canonical files" because mid-bootstrap K9 repos may not yet have all four; over "directory exists" because that produces false positives for repos using `agent_docs/` for unrelated purposes; and over "scan CLAUDE.md for a sentinel string" because that couples detection to documentation text that can drift.
- Detection is path-only — `memhub` never reads the contents of `project_state.md`. The marker file is treated as opaque.

## 2026-05-12 - `[integrations.k9]` is omitted from fresh configs when K9 isn't detected

- A fresh `memhub init` only writes the `[integrations.k9]` section when K9 is detected at init time. When K9 is not detected, the section is omitted entirely rather than written as `enabled = false`.
- This keeps default `.memhub/config.toml` files minimal and makes "K9 was detected at init time" visually obvious from the config file itself.
- Existing configs without the section continue to work because `IntegrationsConfig` is `#[serde(default)]` on `ProjectConfig` and `IntegrationsConfig::k9` defaults to `None`.

## 2026-05-12 - `memhub init` never modifies an existing config; toggling goes through `memhub integrations`

- `memhub init` writes `[integrations.k9]` only when it creates the config file. On a re-init against an existing config it leaves the file alone, preserving its prior idempotent contract.
- Toggling on already-initialized repos uses a new dedicated subcommand: `memhub integrations enable-k9 [--agent-docs-path <path>] [--force]` and `memhub integrations disable-k9`. `enable-k9` refuses to run when no K9 marker is detected unless `--force` is supplied, so the config can't quietly drift away from filesystem reality. `disable-k9` keeps the section and flips `enabled = false` rather than removing the section entirely, so the audit trail of "this project once integrated with K9" survives.
- Auto-merging the missing section on re-init was rejected because it would stretch `init`'s contract (it currently never modifies an existing config) and create a surprise when re-running init on a repo where the user intentionally chose not to enable K9.

## 2026-05-12 - K9 detection re-runs on `memhub status` but not on every `open_project`

- `commands::integrations::k9_state` runs the detection probe whenever `memhub status` (or the MCP `status` tool) executes. The result is reported as `K9 detected: yes/no` plus a `drift` message when config and filesystem disagree.
- `db::open_project` does not re-run detection. Adding a filesystem probe to every CLI invocation was rejected as unnecessary work for the common case and risky — it would create temptation to silently rewrite config on drift, which we explicitly don't want.
- Drift is surfaced, never auto-corrected. The user runs `memhub integrations enable-k9` / `disable-k9` to bring config and filesystem back into agreement.

## 2026-05-12 - MCP exposes K9 state on `status` only; no new tools

- The MCP `status` tool response gained `k9_detected`, `k9_enabled`, `k9_agent_docs_path`, and `k9_drift` so agents can condition behavior on the integration state.
- A dedicated `integrations` MCP tool was rejected as overkill — `status` already exists and the K9 state is small.
- Agents continue to use `propose_fact` / `propose_decision` identically regardless of K9 presence. The integration is a CLI / human-approval concern, not an agent concern at the MCP layer.

## 2026-05-12 - K9 wrap-up contract is locked by a versioned doc, not in-payload metadata

- The K9 `/wrap-up` shell-out contract lives at `docs/reference/k9-wrap-up-contract.md` and is the single source of truth for JSON output shapes, exit codes, actor conventions, and sequencing.
- JSON responses on mutating commands deliberately do not carry a `schema_version` field — the document itself is the version artifact. Breaking changes ship as a `v2` doc with K9 migrating explicitly.
- Considered embedding `schema_version: 1` in every response; rejected as noise that future-you would have to keep consistent across every command for no real consumer benefit (K9 consumes the contract as a whole, not per-response).

## 2026-05-12 - `memhub integrations check-k9` is a pure exit-code probe

- `memhub integrations check-k9` writes nothing to stdout and returns exit 0 only when `.memhub/project.sqlite` exists AND `[integrations.k9].enabled = true`. Any failure mode (no `.memhub/`, missing section, disabled, internal error) returns exit 1 silently.
- The implementation in `commands::integrations::check_k9` swallows `open_project` errors via `let Ok(ctx) = ...` rather than propagating them, because a missing-DB or no-project state is "not enabled" from K9's perspective — not a user-facing error.
- The clap-derived `CheckK9` handler calls `process::exit(0|1)` directly, bypassing the normal `Result<()>` error-printing path in `main.rs`. This is the only command that exits explicitly; everything else returns through the standard pipeline.

## 2026-05-12 - `--actor` is a free-form string with bounded length

- All mutating CLI commands accept `--actor <name>` (default `cli:user`, max 64 chars, non-empty). The internal API takes `actor: &str` as an explicit parameter on every write function.
- Validation lives in `commands::validate_actor` and runs at the CLI boundary via `resolve_actor`. Invalid values produce `MemhubError::InvalidInput` and exit 1.
- Considered adding a `MEMHUB_ACTOR` env var as the K9 convention; deferred. The explicit per-command flag is auditable in shell history and harder to leak across processes. Env-var support can be added later as an alternative without breaking the flag.

## 2026-05-12 - `review accept` propagates the supplied actor to durable writes

- `review::accept` was already calling `fact::add` / `decision::add` internally to promote staged proposals. With the new actor parameter it threads its own `actor` argument down into those calls, so a `memhub review accept <id> --actor k9:wrap-up` produces `writes_log` rows with `actor = "k9:wrap-up"` for both the pending-write status update AND the durable fact/decision insert.
- This keeps K9's audit trail coherent: a single `/wrap-up` invocation produces a contiguous block of `writes_log` rows all tagged with the same actor.

## 2026-05-12 - K9 wrap-up contract is memhub-side only; K9 repo edits are a separate slice

- `M5-002` delivers the contract document AND the CLI affordances K9 needs to consume it, but does not include the K9 repo's `/wrap-up.md` consumer change. That edit lives in the K9 repository and is owned separately.
- This split was chosen so the memhub side has a stable artifact to point at (`v1` contract) and can ship independently without lockstep coordination.
- The K9 repo edit, when it ships, must consume `v1` verbatim or bump the contract to a new version.
