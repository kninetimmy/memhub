<!-- memhub:rendered -->
<!-- DO NOT EDIT. Generated from .memhub/project.sqlite. -->
<!-- To change content, use memhub CLI; then re-run `memhub render`. -->
<!-- Generated at: 2026-05-13T17:33:00Z by memhub 0.1.0 -->

# memhub

## Currently building

## Currently building

Between tasks. The K9 → memhub transition is fully complete on this
machine. K9-Claude-Framework is deleted, all auxiliary K9 artifacts
(marker file, Codex and Agents skill copies, k9-named Claude command
stubs, the four K9 archive files in this repo) are gone, and the
surviving config references in ~/.codex/config.toml and this repo's
.claude/settings.local.json have been cleaned out. The duplicate
~/src/memhub clone was removed as part of the sweep. Free-AI-SSD has
also been updated to the current memhub and the K9 narrative there
is now in the DB (state + arch tables) and rendered into
agent_docs/PROJECT.md.

The three memhub-native skills — /wrap-up, /init-project,
/check-init — all now live at user-level (~/.claude/commands/) so
they fire in any memhub-initialized repo. This session lifted
/wrap-up to match /init-project and /check-init's placement,
superseding D13 which had kept it project-level.

## Next up

1. Commit the 7 working-tree changes from this session as a single
   "remove K9 framework artifacts" commit: project-level wrap-up.md
   deletion, four K9 archive markdown deletions, and the two
   re-rendered files (PROJECT.md, PROJECT_LEDGER.md).
2. Optionally remove the now-empty .claude/commands/ directory in
   this repo (no project-level skills left).
3. Dogfood /check-init and /init-project in a fresh repo before the
   next memhub release — still un-exercised.
4. Otherwise: between tasks. No active milestone in flight.

## Last session

2026-05-13 — Lifted /wrap-up to user-level (supersedes D13);
migrated Free-AI-SSD's K9 narrative into memhub via state set / arch
set --from-file (~37 KB state + ~12 KB arch); ran the full K9
framework purge end-to-end. Phase 2 re-verification confirms no
remaining runtime dependencies on the K9 directory.

2026-05-13 (earlier) — Closed two prior "Next up" items entirely
outside the memhub source tree: installed memhub binary on PATH
(cargo install --path . plus the stale shadow override), shipped
memhub-native /init-project and /check-init at user-level with the
M7-001 rename pattern.

## Open questions

- Should .claude/commands/ in this repo be removed (now empty)?
- PATH ordering: the ~/.local/bin/memhub shadow problem could recur
  after any future cargo install --path .. Worth a docs note or a
  Makefile target?
- State body schema: render dumps the entire body under
  "## Currently building", producing nested-looking PROJECT.md.
  Refactor schema or accept the styling quirk?
- MEMHUB_ACTOR env var as alternative to --actor for skills that
  fan out many CLI calls?
- FACT_STALE_AFTER_DAYS as a config knob?
- gc slice for ingested denied paths?
- Additional clientInfo.name values in real handshakes?
- Should memhub ship a cargo install / homebrew tap?
- Should memhub migrate become explicit once external users adopt?
- Should v2 export format include session_notes?

_Last updated 2026-05-13 17:32:55 by claude:wrap-up._

## Architecture

# Project Architecture

## Purpose

`memhub` is a local-first per-repo memory CLI that aims to give Codex and Claude Code one shared durable source of project context. By PRD §16 milestones the project is at v1: the current implementation covers the Milestone 1 foundation, Milestone 2 retrieval (git ingestion + indexed search), the shipped markdown-sync and narrowed MCP/write-policy slices of Milestone 3, all of Milestone 4 (export/import recovery, missing-DB safety with `init --from-backup`, the `memhub review` flow, path-based deny-list filtering, and the 90-day fact staleness + derived command confidence pass), the memhub side of Milestone 5 K9 framework interop (detection + config, the v1 K9 `/wrap-up` shell-out contract, machine-readable `--json` on every read and mutating command K9 needs, plus PRD-§17 `memhub stats` dogfood tooling and PRD-§12 free-form session notes via the `log_session_note` MCP tool), and the K9-deprecation render slice shipped as `c3fbef0`: durable `project_state` and `project_arch` blob tables (migration `0007`), `memhub state|arch set|show|history` CLI, and `memhub render` emitting memhub-native `agent_docs/PROJECT.md` (narrative) and `agent_docs/PROJECT_LEDGER.md` (structured ledger) per `docs/roadmap/memhub-render-design.md`. The wrap-up slice shipped its first two steps (`5037033`, `588168b`): a `memhub note add` CLI primitive and a project-level `.claude/commands/wrap-up.md` skill orchestrating the session-end approval flow per `docs/roadmap/wrap-up-design.md`. The skill is currently blocked from dogfood use by `M7-001` (project-level skill override gap).

## Stack and Versions

- Rust 2024 edition
- `clap` for CLI parsing
- `rusqlite` with bundled SQLite
- `serde` + `toml` for config
- `globset` for deny-list glob matching
- `env_logger` + `log` for lightweight logging
- `tokio` for the local async runtime used by the MCP server
- `rmcp` for the local stdio MCP server and tool wiring

## Layout

- `src/cli/` - top-level CLI command definitions and output formatting
- `src/commands/` - core command handlers, including `export` and `import` for portable recovery, `narrative` (state/arch shared module), and `render` (thin shim over `src/render/`)
- `src/export/` - version-tagged on-disk format types (`v1`) for export/import
- `src/config/` - per-repo config model and read/write helpers; now includes `[render]` with `output_dir` defaulting to `agent_docs`
- `src/db/` - path discovery, connection bootstrap, migrations, `.gitignore` handling
- `src/models/` - small data structs used by the CLI layer
- `src/mcp/` - stdio MCP server and thin tool adapters over existing services
- `src/render/` - DB-snapshot-to-two-file markdown emitter (`PROJECT.md` + `PROJECT_LEDGER.md`); reuses backup/temp-write helpers lifted from `src/sync_md`
- `src/sync_md/` - markdown managed-block rendering and file rewrite logic; `create_backup`, `backup_stamp`, and `write_with_replace` are now `pub(crate)` so `src/render/` can share them
- `migrations/` - numbered SQL files embedded into the binary
- `docs/` - preserved PRD, implementation notes, architecture, roadmap

## Key Subsystems

- Project bootstrap resolves or creates `.memhub/` in a repository root. `db::open_project` and `db::init_project` both refuse to silently rebuild `project.sqlite` inside an existing `.memhub/`, returning `MemhubError::MissingDatabase`; `db::init_project_for_recovery` is the explicit recovery-mode entry point used by `memhub init --from-backup`.
- The DB layer applies migrations and maintains a single `projects` row.
- Command handlers perform real writes for facts, decisions, tasks, explicit command verification, git ingestion, and staged pending writes, plus the `commands::review` flow that promotes/rejects/expires staged proposals, and log those writes to `writes_log`.
- Search uses SQLite FTS5 over `chunks` for decision text and exact indexed lookups for file history through `files` and `commit_files`.
- The MCP layer serves a local stdio server through `memhub serve` and currently exposes thin tool adapters for status, search, task listing, decision listing, latest-command lookup, explicit verified command recording, staged fact/decision proposals, and read-only `list_pending_writes` for staged-proposal review surfaces (notably K9 wrap-up). It preserves the exact raw `clientInfo.name`, normalizes aliases from a trimmed copy, sanitizes client names before logging, and stores available MCP request/init provenance JSON with staged writes.
- Markdown sync rewrites only explicit managed sections in `AGENTS.md` and `CLAUDE.md`, validates that each file has at most one well-formed managed block pair, creates timestamped backups for changed existing files under `.memhub/backups/markdown/`, and uses temp-file replacement writes. It can run explicitly or after writes when `auto_sync_md` is enabled.
- Export/import provides the supported recovery path. `memhub export` writes a version-tagged JSON file covering facts, decisions, tasks, commands, pending writes, and the writes log; derived data (git ingestion, FTS chunks, schema migrations) is excluded. `memhub import` validates the format version, refuses on non-empty targets unless `--force` is passed, wipes durable tables plus decision chunks in a single transaction with `PRAGMA defer_foreign_keys = ON`, restores rows with their original IDs, regenerates decision chunks, appends a `writes_log` entry for the restore event, and runs `sync-md` after commit.
- `memhub init --from-backup <path>` is the single-step recovery convenience UX. It refuses to run when `.memhub/project.sqlite` already exists, then uses `db::init_project_for_recovery` to create `.memhub/` (if missing) and run migrations before delegating to the existing `commands::import::run` path. This covers both the clean-clone case and the missing-database case without making plain `memhub init` interactive.
- `commands::review` provides the explicit review and promotion flow for staged MCP proposals. `accept` delegates to `fact::add` / `decision::add` so promoted rows reuse all existing audit, FTS, and sync-md plumbing; `reject` and `expire` only mutate the pending row and `writes_log`. The `pending_writes` table gained a `reviewed_at` column in migration `0005_pending_write_reviewed_at` that is stamped on any transition out of `pending`. Acceptance runs against fresh database connections (not a single transaction), so a failure between durable promotion and pending-row update leaves the pending row in `pending` for safe retry, and `fact::add`'s `(project_id, key)` upsert keeps that retry idempotent.
- `config::deny` defines the per-repo path deny list. `ProjectConfig::deny_list` is a `serde(default)` field so existing `config.toml` files without the section automatically pick up the shipped defaults. `PathMatcher::from_patterns` compiles the patterns through `globset` and fails closed on invalid globs. `commands::ingest_git` builds the matcher at the start of a run and skips denied paths before any `files`/`commit_files` insert, surfacing `denied_files_skipped` in the summary. `commands::search` builds the same matcher and post-filters file-history hits (both prefixed `file:` lookups and inferred path lookups); denied direct lookups return a normal empty result without leakage. The current scope is path-based only; content scanning and historical-data cleanup remain out of scope.
- `commands::stats` is a pure read-only aggregation surface for PRD §17 dogfood metrics. It joins counts across `facts` (incl. stale ratio), `decisions`, `tasks`, `commands`, `commits`, `files`, `chunks`, `pending_writes`, and `writes_log` and produces a single `StatsSummary` shaped around a `StatsWindow` (`Days(i64)` or `All`). Windowed activity is sourced from `writes_log` alone — PRD §17's "simple read counter" is intentionally not instrumented; the omission is surfaced in both human and JSON output rather than being silently incomplete. No schema change, no migration.
- `commands::session_note` plus the `session_notes` table (migration `0006_session_notes`) provide free-form agent scratch space per PRD §12. The shape is `(id, project_id, actor, actor_raw, text, created_at)` indexed by `created_at DESC`. Writes go through the MCP `log_session_note` tool (which pulls actor identity from `clientInfo.name` via the same `ClientIdentity` plumbing as `propose_fact` / `propose_decision`) and through the new `memhub note add` CLI surface (`5037033`); both emit a `writes_log` row for audit. Notes remain write-only — no promotion path to facts or decisions, no FTS index, and no inclusion in the v1 `memhub export` format. Reads land in `memhub note list [--limit] [--actor] [--since-days] [--json]`. If notes ever start carrying durable value, a `v2` export format ships separately to include them.
- `commands::narrative` plus the `project_state` and `project_arch` tables (migration `0007_project_narrative`) provide single-blob durable storage for state and architecture narratives. Both tables share the shape `(id, project_id, body, actor, actor_raw, created_at)` indexed by `created_at DESC`; a single `commands::narrative` module dispatches on `NarrativeKind::State | Arch` to share the implementation. `set` always inserts a new row (append-only history); `show` returns the most recent; `history` lists prior. Bodies validate non-empty after trim and cap at 64K characters. CLI exposes `memhub state set|show|history` and `memhub arch set|show|history` with `--actor`, `--json`, and an inline-text-or-`--from-file` body input pattern (shared with `memhub note add` via `cli::resolve_text_input`). Storage decision rationale: blob-over-decomposed-columns is captured as a durable decision; revisit if querying patterns demand structure later.
- `src/render` plus `commands::render` build a `RenderSnapshot` from durable DB content (latest `project_state`, latest `project_arch`, all decisions ordered by `decided_at DESC`, all tasks with open-first ordering, all facts alphabetical with stale flag, recent session notes capped at 10, last-30-day `writes_log` slice capped at 50) and emit two files into the configured output dir (default `agent_docs/` per `[render].output_dir`): `PROJECT.md` (narrative — state, arch, recent session notes) and `PROJECT_LEDGER.md` (structured ledger — decisions, backlog, facts table, recent activity table). Each file leads with a `<!-- memhub:rendered -->` marker comment plus an ISO timestamp and the memhub package version. Conflict semantics are DB-wins-with-backup: existing rendered files are unconditionally copied to `.memhub/backups/rendered/<stamp>/` before being overwritten via the temp+rename pattern lifted from `sync_md`. Each `memhub render` invocation appends a `writes_log` row with `table_name = 'render'` for audit. Render is on-demand only in v1; auto-render-on-write is reserved as a future config opt-in. The two-file shape is the contract; broader output customization is out of scope.
- `~/.claude/commands/wrap-up.md` is the user-level Claude Code slash command implementing the wrap-up routing brain. Lifted from project-level per D14 so it fires in any memhub-initialized repo (gates on `.memhub/` existing). All three memhub-native skills (`/wrap-up`, `/init-project`, `/check-init`) follow the same placement. The skill prompt walks: detect `.memhub/` and `memhub` binary, capture an implicit since-last-`state set` boundary, read state/arch/notes/pending/git window, draft state + decisions + tasks + facts + session note + arch separately, gate on per-item approval, write DB-first with `--actor claude:wrap-up` halting on first failure, then `memhub render` to refresh the two output files. Never auto-commits — `git add` and `git commit` stay explicit user gestures. Memhub itself only gains primitives (`memhub note add` was the only one needed for the skill); the routing logic lives in markdown so it iterates without a Rust recompile. Design captured in `docs/roadmap/wrap-up-design.md`.

## Security Invariants

- Runtime state stays local to the repository under `.memhub/`.
- No network behavior is part of the product runtime in this scaffold.
- Agent-authored automation features are deferred until write policy is implemented.

## Runtime Layout

Single local CLI process with an embedded SQLite database plus an on-demand stdio MCP server. No background services, listening ports, or external APIs.

## Known Gaps / Out of Scope

- Broader indexed retrieval beyond exact file history, decision FTS, latest-command lookup, and the narrow MCP read tools (e.g. fact / task / session-note search) is deferred until a real workflow demands it
- Continuous confidence decay (exponential or otherwise); a persisted `commands.confidence` column; decision-level confidence; a configurable staleness threshold — all explicitly out of v1 scope per the 2026-05-12 decision
- Read-counter half of PRD §17 (`memhub stats` currently surfaces write activity from `writes_log` only)
- Content-scanning deny rules (current deny list is path-based only)
- Garbage collection of already-ingested denied paths after a pattern change
- Session notes in the export format (v1 export deliberately omits `session_notes`; a `v2` export would change this if notes become durable)

_Last updated 2026-05-13 17:32:55 by claude:wrap-up._

## Recent session notes

- **2026-05-13 17:32:55** (claude:wrap-up) — Lifted /wrap-up to user-level (~/.claude/commands/wrap-up.md) so it fires in any memhub-initialized repo, not just ~/memhub — supersedes D13's project-level placement. Migrated Free-AI-SSD's K9 narrative into memhub (state + arch tables) via --from-file and re-rendered. Fully removed K9-Claude-Framework from the machine end-to-end: framework directory, marker file, Codex and Agents skill copies, k9-named Claude command stubs, K9 archive files in this repo, K9 references in ~/.codex/config.toml and this repo's settings.local.json, plus the stale ~/src/memhub duplicate clone. Working tree holds 7 uncommitted changes ready to ship as a single 'remove K9 framework artifacts' commit.
- **2026-05-13 03:28:11** (claude:wrap-up) — Closed two prior 'Next up' items entirely outside the memhub source tree. (1) Installed memhub on PATH: cargo install --path . produced ~/.cargo/bin/memhub, but a stale ~/.local/bin/memhub shadowed it; copied the fresh binary over the shadow so state/arch/render resolve from any shell. (2) Shipped memhub-native /init-project and /check-init at user-level following the M7-001 rename pattern (lifted to user-level since init/check apply globally rather than inside-memhub-only). No commits this session — all artifacts live in ~/.local/bin/ and ~/.claude/commands/.
- **2026-05-13 02:22:14** (claude:wrap-up) — Wrote the PRD §2 addendum (docs/reference/memhub-prd-deprecation-addendum.md) closing slice 2 of the K9 deprecation plan. PRD itself stayed verbatim per CLAUDE.md guardrail; addendum is authoritative for the §2 inversion, §6.2 layout extension, §8 data model, and §13 CLI surface additions. Revised k9-integration.md non-goals inline and marked all four deprecation slices shipped in the plan doc. Shipped as 7c162b2. K9 deprecation track is now formally complete end-to-end.
- **2026-05-13 01:50:34** (claude:wrap-up) — Investigated and closed M7-001 (project-level slash command override gap). Root cause was documented Claude Code precedence (personal > project, filename-resolved), not a bug. Fix: renamed ~/.claude/commands/wrap-up.md to wrap-up-k9.md so the project-level memhub-native /wrap-up no longer collides. Verified via skills registry. Shipped as 103eea0; M7-002 then executed inline this session to fully migrate the repo to memhub-primary.
