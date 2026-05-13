<!-- memhub:rendered -->
<!-- DO NOT EDIT. Generated from .memhub/project.sqlite. -->
<!-- To change content, use memhub CLI; then re-run `memhub render`. -->
<!-- Generated at: 2026-05-13T18:24:09Z by memhub 0.1.0 -->

# memhub

## Currently building

## Currently building

Between tasks. The K9 -> memhub transition is complete on this machine and committed in `f97bcbf`: project-level K9-era artifacts were removed, `/wrap-up` was lifted to user-level, and the repo now uses memhub-rendered `PROJECT.md` + `PROJECT_LEDGER.md` as the human-readable view of `.memhub/project.sqlite`.

Since the previous wrap-up, the Codex-side bridge also landed in `7671f07`: decisions now have source provenance via migration `0008_decisions_source`, fact/decision writes can preserve `user+agent:<id>` source values separately from `writes_log.actor`, and `AGENTS.md` documents Codex-specific `--source` / `--actor` usage. `5e9a0c6` then rewrote the README around the current memhub-first install story and added installable Claude and Codex skill templates under `templates/skills/`.

## Next up

1. Dogfood the current user-level Codex skills (`/wrap-up`, `/init-project`, `/check-init`) in both this repo and a fresh memhub-initialized repo.
2. Decide whether the README/template install flow needs release packaging next (`cargo install`, one-shot installer hardening, Homebrew, or similar).
3. Otherwise: between tasks. No active milestone is in flight and the worktree is clean.

## Last session

2026-05-13 - committed the K9 artifact cleanup and user-level `/wrap-up` lift (`f97bcbf`), added Codex CLI provenance symmetry and compound source vocabulary (`7671f07`), then rewrote the README and shipped Claude/Codex skill templates (`5e9a0c6`).

## Open questions

- PATH ordering: the `~/.local/bin/memhub` shadow problem could recur after future local installs. Worth a docs note, Makefile target, or installer guard?
- State body schema: render currently dumps the state body under `## Currently building`, producing nested-looking `PROJECT.md`. Refactor schema or accept the styling quirk?
- `MEMHUB_ACTOR` env var as an alternative to repeated `--actor` flags for skills that fan out many CLI calls?
- `FACT_STALE_AFTER_DAYS` as a config knob?
- GC slice for already-ingested paths that later become denied?
- Additional `clientInfo.name` aliases from real MCP handshakes?
- Should v2 export include `session_notes`?

_Last updated 2026-05-13 18:23:25 by codex:wrap-up._

## Architecture

# Project Architecture

## Purpose

`memhub` is a local-first per-repo memory CLI that gives Codex and Claude Code one shared durable source of project context. The SQLite database under `.memhub/project.sqlite` is the source of truth; rendered markdown is the human-readable view.

By PRD milestone coverage, the current implementation includes the Milestone 1 foundation, Milestone 2 retrieval, the shipped markdown-sync and narrowed MCP/write-policy slices of Milestone 3, Milestone 4 recovery/review/deny/staleness work, the memhub side of K9 interop and deprecation, memhub-native narrative storage and render output, session notes, and the Claude/Codex skill templates used to operate those primitives from agent sessions.

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
- `src/commands/` - command handlers, including export/import, review, narrative state/arch storage, session notes, and render
- `src/config/` - per-repo config model and read/write helpers, including `[render].output_dir`
- `src/db/` - path discovery, connection bootstrap, migrations, and `.gitignore` handling
- `src/export/` - version-tagged on-disk export/import format types
- `src/mcp/` - stdio MCP server and thin tool adapters over existing services
- `src/models/` - small data structs used by the CLI layer
- `src/render/` - DB snapshot to `PROJECT.md` and `PROJECT_LEDGER.md`
- `src/sync_md/` - managed-block markdown sync helpers shared by render for backups and temp replacement writes
- `migrations/` - numbered SQL files embedded into the binary
- `docs/` - preserved PRD, implementation notes, architecture, roadmap, and PRD addenda
- `templates/skills/claude/` - installable Claude Code command templates
- `templates/skills/codex/` - installable Codex skill templates

## Key Subsystems

- Project bootstrap resolves or creates `.memhub/` in a repository root. `db::open_project` and `db::init_project` refuse to silently rebuild `project.sqlite` inside an existing `.memhub/`, returning `MemhubError::MissingDatabase`; `db::init_project_for_recovery` is the explicit recovery-mode entry point used by `memhub init --from-backup`.
- The DB layer applies embedded migrations and maintains a single `projects` row. The current schema is `0008_decisions_source`, which adds durable source provenance to decisions.
- Command handlers perform writes for facts, decisions, tasks, explicit command verification, git ingestion, staged pending writes, session notes, narrative state/arch blobs, render, and review promotion/rejection. Mutations append rows to `writes_log`.
- Durable claim provenance is intentionally separate from audit attribution. Facts and decisions carry a `source` value such as `user`, `agent:<id>`, `user+agent:<id>`, `git`, or `observed`; `writes_log.actor` records the writer that performed the mutation. Wrap-up skills therefore pass `--source user+agent:<agent>` for user-approved fact/decision claims and `--actor <agent>:wrap-up` for audit rows.
- Search uses SQLite FTS5 over decision chunks and exact indexed lookups for file history through `files` and `commit_files`.
- The MCP layer serves a local stdio server through `memhub serve`. It exposes status, search, task listing, decision listing, latest-command lookup, explicit verified command recording, staged fact/decision proposals, pending-write listing, and session-note logging. Client identity is derived from `clientInfo.name`, normalized for known aliases, sanitized before logging, and preserved raw where useful.
- Markdown sync rewrites only explicit managed sections in `AGENTS.md` and `CLAUDE.md`, validates managed-block pairing, creates timestamped backups under `.memhub/backups/markdown/`, and uses temp-file replacement writes. It can run explicitly or after writes when `auto_sync_md` is enabled.
- Export/import provides the supported recovery path. `memhub export` writes a version-tagged JSON file for durable rows; derived git/search data is excluded. `memhub import` validates the format, refuses on non-empty targets unless forced, restores durable IDs, regenerates decision chunks, logs the restore, and runs sync-md after commit.
- `memhub init --from-backup <path>` is the single-step recovery convenience UX for clean clones or missing-database cases. It refuses when `.memhub/project.sqlite` already exists.
- `commands::review` provides the explicit review and promotion flow for staged MCP proposals. Accept delegates to the fact/decision add paths so accepted rows reuse existing audit, FTS, and sync-md plumbing; reject and expire mutate only the pending row and `writes_log`.
- The deny-list subsystem compiles repo patterns with `globset`, fails closed on invalid globs, skips denied paths during git ingestion, and post-filters file-history search results. Current scope is path-based only.
- `memhub stats` is a read-only dogfood metrics surface over facts, decisions, tasks, commands, commits, files, chunks, pending writes, and `writes_log`. Windowed activity comes from `writes_log`; read counters are deliberately not instrumented.
- `session_notes` provides free-form agent scratch space through `memhub note add`, `memhub note list`, and the MCP `log_session_note` tool. Notes are not promoted automatically, not FTS-indexed, and not included in the v1 export format.
- `project_state` and `project_arch` store append-only narrative blobs. `memhub state|arch set|show|history` share the same implementation and support inline text or `--from-file`, `--json`, and `--actor`.
- `memhub render` emits `agent_docs/PROJECT.md` and `agent_docs/PROJECT_LEDGER.md` from the database. Render is DB-wins-with-backup: existing rendered files are copied to `.memhub/backups/rendered/<stamp>/` before temp+rename replacement. Render is on-demand in v1.
- User-level memhub skills implement the agent-facing workflow around CLI primitives. `/wrap-up`, `/init-project`, and `/check-init` exist for both Claude and Codex via checked-in templates under `templates/skills/`; installed dotfile copies are runtime artifacts, not repo source. The wrap-up skill gates on `.memhub/`, reads the since-last-state window, drafts updates, waits for per-item approval, writes DB-first with the correct actor/source attribution, then runs `memhub render`. It never commits.

## Security Invariants

- Runtime state stays local to the repository under `.memhub/`.
- No network behavior is part of the product runtime.
- Agent-authored durable truth requires an explicit review or user-approval signal.
- Rendered markdown is output, not an alternate source of truth.

## Runtime Layout

Single local CLI process with an embedded SQLite database plus an on-demand stdio MCP server. No background services, listening ports, or external APIs.

## Known Gaps / Out of Scope

- Broader indexed retrieval beyond exact file history, decision FTS, latest-command lookup, and the narrow MCP read tools is deferred until a real workflow demands it.
- Continuous confidence decay, a persisted command-confidence column, decision-level confidence, and configurable fact staleness remain out of v1 scope.
- Read-counter instrumentation for PRD section 17 is not implemented; `memhub stats` reports write activity only.
- Content-scanning deny rules are out of scope; deny matching is path-based.
- Garbage collection of already-ingested denied paths after a pattern change is not implemented.
- `session_notes` are omitted from v1 export; a v2 export format can include them if notes become durable.

_Last updated 2026-05-13 18:23:59 by codex:wrap-up._

## Recent session notes

- **2026-05-13 18:23:57** (codex:wrap-up) — Since the previous wrap-up, committed the K9 artifact cleanup and user-level /wrap-up lift in f97bcbf, added Codex CLI provenance symmetry plus migration 0008 in 7671f07, and rewrote the README while adding Claude/Codex skill templates in 5e9a0c6. The current wrap-up found no pending reviews, no open tasks, and a clean worktree before drafting these DB updates.
- **2026-05-13 17:32:55** (claude:wrap-up) — Lifted /wrap-up to user-level (~/.claude/commands/wrap-up.md) so it fires in any memhub-initialized repo, not just ~/memhub — supersedes D13's project-level placement. Migrated Free-AI-SSD's K9 narrative into memhub (state + arch tables) via --from-file and re-rendered. Fully removed K9-Claude-Framework from the machine end-to-end: framework directory, marker file, Codex and Agents skill copies, k9-named Claude command stubs, K9 archive files in this repo, K9 references in ~/.codex/config.toml and this repo's settings.local.json, plus the stale ~/src/memhub duplicate clone. Working tree holds 7 uncommitted changes ready to ship as a single 'remove K9 framework artifacts' commit.
- **2026-05-13 03:28:11** (claude:wrap-up) — Closed two prior 'Next up' items entirely outside the memhub source tree. (1) Installed memhub on PATH: cargo install --path . produced ~/.cargo/bin/memhub, but a stale ~/.local/bin/memhub shadowed it; copied the fresh binary over the shadow so state/arch/render resolve from any shell. (2) Shipped memhub-native /init-project and /check-init at user-level following the M7-001 rename pattern (lifted to user-level since init/check apply globally rather than inside-memhub-only). No commits this session — all artifacts live in ~/.local/bin/ and ~/.claude/commands/.
- **2026-05-13 02:22:14** (claude:wrap-up) — Wrote the PRD §2 addendum (docs/reference/memhub-prd-deprecation-addendum.md) closing slice 2 of the K9 deprecation plan. PRD itself stayed verbatim per CLAUDE.md guardrail; addendum is authoritative for the §2 inversion, §6.2 layout extension, §8 data model, and §13 CLI surface additions. Revised k9-integration.md non-goals inline and marked all four deprecation slices shipped in the plan doc. Shipped as 7c162b2. K9 deprecation track is now formally complete end-to-end.
- **2026-05-13 01:50:34** (claude:wrap-up) — Investigated and closed M7-001 (project-level slash command override gap). Root cause was documented Claude Code precedence (personal > project, filename-resolved), not a bug. Fix: renamed ~/.claude/commands/wrap-up.md to wrap-up-k9.md so the project-level memhub-native /wrap-up no longer collides. Verified via skills registry. Shipped as 103eea0; M7-002 then executed inline this session to fully migrate the repo to memhub-primary.
