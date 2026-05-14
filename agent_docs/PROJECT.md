<!-- memhub:rendered -->
<!-- DO NOT EDIT. Generated from .memhub/project.sqlite. -->
<!-- To change content, use memhub CLI; then re-run `memhub render`. -->
<!-- Generated at: 2026-05-14T03:32:00Z by memhub 0.1.0 -->

# memhub

## Currently building

M8 retrieval is closed. The follow-up min-score gap from the prior state
is now shipped: `min_vector_score` floors the hybrid vector path so
pure-nonsense queries return an empty bundle even in hybrid mode.

`cc78a18 Add min_vector_score floor to hybrid recall` filters vector
candidates below a configurable cosine threshold in `vector_lookup`,
before scoring. FTS hits are unaffected. Default is 0.7, calibrated
against the live corpus (nonsense ceiling ~0.67, legitimate top-1 ≥0.78).
The eval harness under `--mode hybrid` now reports Recall@3 = 11/11 AND
safety = 1/1 (was 0/1 before).

The task note's "default around 0.4" was actually the blended `final_score`
(0.5*0 + 0.5*0.67 ≈ 0.33), not the raw cosine. Fact #4
(`retrieval.min_vector_score-calibration`) records the empirical
calibration so a future bump (e.g., model swap) starts from the right
number.

## Next up

- **Dogfood Codex skills end-to-end** (task #10) is the last named
  follow-up. Exercise /wrap-up, /init-project, /check-init from the
  Codex skill templates in fresh and existing repos.
- **Free-AI-SSD golden set** remains nice-to-have once that repo wants
  its own Recall@K baseline.

## Open questions

- `min_vector_score` is a raw-cosine floor. If BGE-small is ever
  swapped for a model with a different similarity-noise floor, the 0.7
  default needs re-tuning — fact #4 captures the calibration method.
- `accepted_only_by_default` still false; task provenance/acceptance
  rules remain deferred from the prior state.

_Last updated 2026-05-14 02:46:23 by claude:wrap-up._

## Architecture

# Project Architecture

## Purpose

`memhub` is a local-first per-repo memory CLI that gives Codex and Claude Code one shared durable source of project context. The SQLite database under `.memhub/project.sqlite` is the source of truth; rendered markdown is the human-readable view.

By PRD milestone coverage, the current implementation includes the Milestone 1 foundation, Milestone 2 retrieval, the shipped markdown-sync and narrowed MCP/write-policy slices of Milestone 3, Milestone 4 recovery/review/deny/staleness work, the memhub side of K9 interop and deprecation, memhub-native narrative storage and render output, session notes, the Claude/Codex skill templates used to operate those primitives from agent sessions, and the M8 retrieval layer end-to-end (bundled BGE-small embedder, embeddings table, contentless FTS5 over source tables, opt-in eager-embed write path, hybrid `recall` query surface as CLI and MCP tool, the `index status` + `index rebuild` admin commands, and the `eval retrieval` Recall@K acceptance harness with its `/eval-recall` skill).

## Stack and Versions

- Rust 2024 edition
- `clap` for CLI parsing
- `rusqlite` with bundled SQLite (FTS5 enabled)
- `serde` + `toml` for config
- `globset` for deny-list glob matching
- `env_logger` + `log` for lightweight logging
- `tokio` for the local async runtime used by the MCP server
- `rmcp` for the local stdio MCP server and tool wiring
- `fastembed` for embedding inference via the bundled BGE-small model
- `ort` (with `download-binaries` feature) for the underlying ONNX Runtime
- `sha2` for `embeddings.content_hash` and build-time model integrity checks
- `ureq` as a build-time HTTP client for fetching the embedding model files

## Layout

- `src/cli/` - top-level CLI command definitions and output formatting
- `src/commands/` - command handlers, including export/import, review, narrative state/arch storage, session notes, render, index rebuild/status, and the eval retrieval harness
- `src/config/` - per-repo config model and read/write helpers, including `[render].output_dir`, `[retrieval].mode`, and `[retrieval.scoring]` weights
- `src/db/` - path discovery, connection bootstrap, migrations, and `.gitignore` handling
- `src/export/` - version-tagged on-disk export/import format types
- `src/mcp/` - stdio MCP server and thin tool adapters over existing services
- `src/models/` - small data structs used by the CLI layer
- `src/render/` - DB snapshot to `PROJECT.md` and `PROJECT_LEDGER.md`
- `src/retrieval/` - bundled BGE-small embedding wrapper (`embeddings.rs`), the eager-embed write-path helper (`persist.rs`), and the hybrid SQL+RAG recall engine (`recall.rs`)
- `src/sync_md/` - managed-block markdown sync helpers shared by render for backups and temp replacement writes
- `migrations/` - numbered SQL files embedded into the binary
- `build.rs` - downloads and SHA256-verifies the BGE-small ONNX and tokenizer files into `OUT_DIR` so the main crate can pick them up via `include_bytes!`
- `docs/` - preserved PRD, implementation notes, architecture, roadmap, and PRD addenda
- `templates/skills/claude/` - installable Claude Code command templates
- `templates/skills/codex/` - installable Codex skill templates
- `tests/retrieval_golden.json` - checked-in 12-query starter golden set used by `memhub eval retrieval` and the `/eval-recall` skill
- `tests/mcp_protocol.rs` - subprocess regression test that asserts the MCP server actually exposes its tool surface to external clients via the JSON-RPC handshake

## Key Subsystems

- Project bootstrap resolves or creates `.memhub/` in a repository root. `db::open_project` and `db::init_project` refuse to silently rebuild `project.sqlite` inside an existing `.memhub/`, returning `MemhubError::MissingDatabase`; `db::init_project_for_recovery` is the explicit recovery-mode entry point used by `memhub init --from-backup`.
- The DB layer applies embedded migrations and maintains a single `projects` row. The current schema is `0010_embeddings_delete_triggers`, which extends the M8 retrieval scaffolding from 0009 with cascading DELETE triggers from facts/decisions/tasks into embeddings.
- Command handlers perform writes for facts, decisions, tasks, explicit command verification, git ingestion, staged pending writes, session notes, narrative state/arch blobs, render, review promotion/rejection, and embedding index rebuild. Mutations append rows to `writes_log`.
- Durable claim provenance is intentionally separate from audit attribution. Facts and decisions carry a `source` value such as `user`, `agent:<id>`, `user+agent:<id>`, `git`, or `observed`; `writes_log.actor` records the writer that performed the mutation. Wrap-up skills therefore pass `--source user+agent:<agent>` for user-approved fact/decision claims and `--actor <agent>:wrap-up` for audit rows.
- Search uses SQLite FTS5 over decision chunks plus contentless FTS5 over `facts(key, value)`, `decisions(title, rationale)`, and `tasks(title, notes)` (migration 0009, kept in sync by per-source AI/AU/AD triggers), and exact indexed lookups for file history through `files` and `commit_files`.
- Retrieval (M8) lives under `src/retrieval/`. The BGE-small-en-v1.5 ONNX model and its tokenizer files are bundled into the binary at build time via `build.rs` (auto-download from Hugging Face, SHA256-pinned, cached in `OUT_DIR`). `embeddings.rs` exposes lazy `embed_one` / `embed_batch` over a `OnceLock<Mutex<TextEmbedding>>` with CLS pooling. `persist.rs` exposes `eager_embed_in_tx(tx, mode, source_type, source_id, text)`, a no-op for `RetrievalMode::Fts` that otherwise hashes the embed text with SHA256, looks up the existing embedding for the active model, short-circuits on hash match, or embeds and UPSERTs the row inside the caller's transaction. The fact/decision/task `add` paths and `review::accept` all flow through this helper. Mode is read from `[retrieval]` in `.memhub/config.toml`; `fts` is the default and never loads the model.
- `recall.rs` is the M8 query surface. It UNIONs FTS5 lookups per source table with brute-force cosine over the active-model embeddings (hybrid mode only, with vector candidates whose raw cosine is below `[retrieval.scoring].min_vector_score` filtered out before scoring so pure-nonsense queries don't surface low-confidence noise; FTS hits are unaffected), applies filters (source-type allowlist, `include_stale` for fact freshness, `accepted_only` mapped to `source IN ('user', 'user+agent:%')`), blends scores via the `[retrieval.scoring]` knobs (`0.5×fts + 0.5×vec − 0.3×stale_penalty` by default) after min-max FTS normalization, and returns a ranked evidence bundle. Recall is read-only in practice and in contract: it never syncs legacy chunks, writes durable rows, stages pending writes, or logs to `writes_log`. Completed tasks and inactive decisions remain recallable as historical evidence unless a future explicit status filter is added. Stale-embedding detection re-hashes the current source body per candidate and surfaces a `stale_embeddings` warning when the active-model embedding is missing or its content_hash drifts; the warning is informational, never auto-fixed.
- `commands::index` exposes `memhub index status` and `memhub index rebuild`. Status returns per-source-type counts (total vs. currently hash-matching embeddings), the active model name, and a missing/stale coverage count; rows whose `content_hash` no longer matches current source text are treated as not covered. Rebuild ignores `[retrieval] mode` so it works as the one-shot backfill for `fts → hybrid` migrations and for refreshing rows after a model upgrade. It embeds a source snapshot outside the write transaction, then prunes orphaned active-model embeddings and conditionally UPSERTs only rows whose current source hash still matches that snapshot, preserving newer eager embeddings from concurrent writes. The writes_log gets one summary row per rebuild (not per source row).
- `commands::eval` provides the M8 Recall@K acceptance gate. `run_retrieval` loads a versioned golden JSON file, runs each query through the shared `retrieval::recall` engine with `max_results = k`, and evaluates per-query matchers (`title_contains` / `body_contains` case-insensitive substring checks, plus optional `source_type`). `kind: match` queries contribute to Recall@K; `kind: empty` queries fail as safety violations if recall returns any hit. The harness is read-only — no `writes_log` rows, no durable mutation. Exposed as `memhub eval retrieval [--golden PATH] [--k N] [--mode fts|hybrid] [--json]` and via the `/eval-recall` Claude / Codex skill.
- The MCP layer serves a local stdio server through `memhub serve`. The `impl ServerHandler for MemhubServer` block carries the rmcp `#[tool_handler(router = self.tool_router)]` attribute — without it the default `list_tools` returns empty even with tools registered via `#[tool_router]`, and clients see a working initialize but a zero-tool surface; `tests/mcp_protocol.rs` is the subprocess regression test that asserts the full handshake. Read tools: `status`, `search`, `recall`, `list_tasks`, `list_decisions`, `list_facts`, `list_pending_writes`, `get_command`. Write tools split by trust: `task_add`, `task_done`, `render`, `record_command`, and `log_session_note` write directly; `propose_fact` and `propose_decision` stage to `pending_writes` for human review. Client identity is derived from `clientInfo.name`, normalized for known aliases, sanitized before logging, and preserved raw where useful. The server-info hint teaches agents to prefer `recall` over reading the ledger mid-session.
- Markdown sync rewrites only explicit managed sections in `AGENTS.md` and `CLAUDE.md`, validates managed-block pairing, creates timestamped backups under `.memhub/backups/markdown/`, and uses temp-file replacement writes. It can run explicitly or after writes when `auto_sync_md` is enabled.
- Export/import provides the supported recovery path. `memhub export` writes a version-tagged JSON file for durable rows; derived git/search data is excluded. `memhub import` validates the format, refuses on non-empty targets unless forced, restores durable IDs, regenerates decision chunks, logs the restore, and runs sync-md after commit.
- `memhub init --from-backup <path>` is the single-step recovery convenience UX for clean clones or missing-database cases. It refuses when `.memhub/project.sqlite` already exists.
- `commands::review` provides the explicit review and promotion flow for staged MCP proposals. Accept delegates to the fact/decision add paths so accepted rows reuse existing audit, FTS, eager-embed, and sync-md plumbing; reject and expire mutate only the pending row and `writes_log`.
- The deny-list subsystem compiles repo patterns with `globset`, fails closed on invalid globs, skips denied paths during git ingestion, and post-filters file-history search results. Current scope is path-based only.
- `memhub stats` is a read-only dogfood metrics surface over facts, decisions, tasks, commands, commits, files, chunks, pending writes, and `writes_log`. Windowed activity comes from `writes_log`; read counters are deliberately not instrumented.
- `session_notes` provides free-form agent scratch space through `memhub note add`, `memhub note list`, and the MCP `log_session_note` tool. Notes are not promoted automatically, not FTS-indexed, and not included in the v1 export format.
- `project_state` and `project_arch` store append-only narrative blobs. `memhub state|arch set|show|history` share the same implementation and support inline text or `--from-file`, `--json`, and `--actor`.
- `memhub render` emits `agent_docs/PROJECT.md` and `agent_docs/PROJECT_LEDGER.md` from the database. Render is DB-wins-with-backup: existing rendered files are copied to `.memhub/backups/rendered/<stamp>/` before temp+rename replacement. Render is on-demand in v1.
- User-level memhub skills implement the agent-facing workflow around CLI primitives. `/wrap-up`, `/init-project`, `/check-init`, `/recall`, `/reindex`, and `/eval-recall` exist for both Claude and Codex via checked-in templates under `templates/skills/`; installed dotfile copies are runtime artifacts, not repo source. The wrap-up skill gates on `.memhub/`, reads the since-last-state window, drafts updates, waits for per-item approval, writes DB-first with the correct actor/source attribution, then runs `memhub render`. It never commits. The `/recall` skill drives `memhub.recall` / `memhub recall` and is the preferred mid-session read; `/reindex` drives `memhub index rebuild` and always asks before mutating, per the stale-embedding UX rule; `/eval-recall` drives `memhub eval retrieval` to report a Recall@K baseline and surface failing queries verbatim.

## Security Invariants

- Runtime state stays local to the repository under `.memhub/`.
- No network behavior is part of the product runtime. Build-time network is used by `build.rs` (to fetch the BGE-small model files from Hugging Face, SHA256-verified against pinned hashes) and by `ort`'s `download-binaries` feature (prebuilt ONNX Runtime). Once a build is done, the resulting binary contains the model and runs offline.
- Agent-authored durable truth requires an explicit review or user-approval signal.
- Rendered markdown is output, not an alternate source of truth.
- Recall is read-only and never writes to `writes_log` or any durable table.
- The eval retrieval harness is read-only on the same terms as recall.

## Runtime Layout

Single local CLI process with an embedded SQLite database plus an on-demand stdio MCP server. No background services, listening ports, or external APIs.

## Known Gaps / Out of Scope

- Continuous confidence decay, a persisted command-confidence column, decision-level confidence, and configurable fact staleness remain out of v1 scope.
- Read-counter instrumentation for PRD section 17 is not implemented; `memhub stats` reports write activity only.
- Content-scanning deny rules are out of scope; deny matching is path-based.
- Garbage collection of already-ingested denied paths after a pattern change is not implemented.
- `session_notes` are omitted from v1 export; a v2 export format can include them if notes become durable.
- Switching `[retrieval].mode` from `fts` to `hybrid` on a populated DB requires running `memhub index rebuild` (or invoking `/reindex`) to backfill embeddings for pre-existing rows. The rebuild itself is shipped; the mode flip is a user action.
- Loading the bundled model clones ~130 MB out of `.rodata` into a `Vec<u8>` (fastembed's `UserDefinedEmbeddingModel::new` takes `Vec<u8>`, not `&[u8]`). Peak transient memory at first embed is roughly 2× the model size; revisit if it becomes a constraint.

_Last updated 2026-05-14 02:46:44 by claude:wrap-up._

## Recent session notes

- **2026-05-14 03:30:23** (claude:planning) — Closed planning session for tasks #19 (memhub viz dashboard) and #20 (cross-encoder re-ranker). Six decisions (#58-63) capture the dashboard design forks: ephemeral lifecycle, project-scoped discovery in v1 / registry in v2, read-only contract, listening-port invariant clarification, recall inspector as client-side replay of a verbose server response, and v1 scope (5 panels + PCA + polling activity + ephemeral launcher, ~3-5 days). Visual style is dark mode + neon purples per fact #5 (viz.theme). AM resumption: read task #19 notes plus decision #63 (v1 scope) for the build list. Task #20 (re-ranker) is independent and can be picked up in parallel — model size/quality trade-off between ms-marco-MiniLM-L-6-v2 (~80MB) and bge-reranker-v2-m3 (~280MB) still needs an eval-harness pass before the build.rs bundle decision. No production code shipped this session beyond the cc78a18 calibration commit earlier.
- **2026-05-14 02:46:38** (claude:wrap-up) — Shipped the recall min-score floor in cc78a18, closing the last named M8 follow-up. min_vector_score (default 0.7, configurable in [retrieval.scoring]) gates the raw cosine of the hybrid vector path so nonsense queries return an empty bundle without affecting FTS hits. Calibrated empirically against the live corpus — fact #4 records the method so a future model swap re-derives correctly. Eval harness under --mode hybrid now reports Recall@3 11/11 + safety 1/1 (was 0/1). Test suite 86+ lib tests green; commit is local, not pushed.
- **2026-05-14 00:37:40** (codex:wrap-up) — Codex shipped RAG consistency hardening in 90fda15: recall no longer mutates legacy chunks, done tasks remain recallable by default, index status detects content-hash drift, and rebuild avoids overwriting newer eager embeddings. The updated binary was installed to ~/.cargo/bin and copied over the active ~/.local/bin/memhub shadow, then verified in Free-AI-SSD with hybrid mode and 113/113 embeddings. This memhub repo was also switched to hybrid mode locally and backfilled to 73/73 embeddings, with hybrid recall verified.
- **2026-05-13 23:19:11** (claude:wrap-up) — Closed M8 end-to-end this session plus a load-bearing latent MCP fix. PR6 (eval harness + /eval-recall skills) landed in 6f89d2f — 12-query golden set, memhub eval retrieval CLI, Recall@3 = 100% baseline on the memhub repo. README refresh in 5fb4a02 promoted hybrid recall to the headline and reworked the agent-driven install prompts to ask FTS-vs-hybrid at install time. While verifying Codex MCP end-to-end, caught and fixed a latent-since-inception bug: impl ServerHandler was missing #[tool_handler] so tools/list returned {"tools":[]} for every client — fix in 3a3519c with a tests/mcp_protocol.rs subprocess regression test. Free-AI-SSD was brought to full hybrid mode out-of-tree (config flip + 113-row backfill + eager-embed and cascade-DELETE verified). Test suite 221 -> 238, all green, three commits pushed to origin/main.
- **2026-05-13 22:22:39** (claude:wrap-up) — Closed M8 PR4 (ceb012a — memhub recall CLI + memhub.recall MCP tool with hybrid scoring 0.5×fts + 0.5×vec − 0.3×stale_penalty, filters incl. --accepted-only mapped to source IN ('user', 'user+agent:%'); 13 new tests covering FTS normalization, cosine identities, accepted-only exclusion, empty bundle, missing-embedding warning) and M8 PR5 (204ff70 — /recall and /reindex skill templates for both Claude and Codex, CLAUDE.md/AGENTS.md prefer-recall rule, memhub index status + rebuild CLI; 4 new tests). Test suite grew 204 → 221. Only PR6 (Recall@3 eval harness + /eval-recall skill) and the long-standing PATH-shadow binary reinstall remain before M8 is complete.
- **2026-05-13 21:50:10** (claude:wrap-up) — Shipped M8 PRs 1-3 end-to-end in three atomic commits: 3168c8c (bundle BGE-small-en-v1.5 via build.rs + fastembed-rs UserDefinedEmbeddingModel, 2 smoke tests confirming 384-dim L2-normalized output), cd1ae3f (migration 0009 — embeddings table + contentless FTS5 over facts/decisions/tasks with sync triggers and rebuild backfill, 10 schema tests; FTS5 hyphen-as-NOT gotcha caught and worked around in the test helper), 8d2c59f (eager-embed in fact/decision/task add paths gated on [retrieval] mode = hybrid, migration 0010 for DELETE cascade, SHA256 content_hash short-circuit, 9 embed tests). Test suite grew 175 → 204. The installed memhub binary on PATH still predates this session and needs reinstall before any consumer outside this repo sees the new schema.
- **2026-05-13 20:55:44** (claude:wrap-up) — Hardening pass before starting M8: validated and fixed all six findings from an external Codex code review of the memhub surface. Six commits, one per finding (605fd59 atomic accept, 57a5f69 MCP actor, e5be353 source validation, d91fc98 export reviewed_at, 3c74cad two-phase render, ae90719 strip leading heading), each with regression tests. Test suite grew from 154 to 175 tests, all green. Branch pushed to origin/main.
- **2026-05-13 20:14:27** (claude:wrap-up) — Planning session — no code shipped. Defined M8 (SQL+RAG hybrid recall) end-to-end: library (fastembed-rs), model (BGE-small-en-v1.5 bundled, ~140MB binary), vector storage (SQLite BLOB + brute-force cosine, no extension), schema (embed existing rows directly, no chunks table), index lifecycle (eager on writes, content_hash drift, agent-prompted /reindex on staleness), agent surface (MCP recall tool plus /recall and /reindex skills; agents prefer recall over PROJECT_LEDGER.md), and eval discipline (Recall@3, 12 starter golden queries). Routed 19 decisions and 6 PR-shaped tasks into the DB. PRD addendum at docs/reference/memhub-prd-addendum-m8-retrieval.md to follow in a separate turn.
- **2026-05-13 18:40:30** (claude:wrap-up) — This session shipped 4 new MCP tools (task_add, task_done, list_facts, render) in e67167e, closing the four 'mid-session must Bash the CLI' gaps for agents while preserving the trust split — facts and decisions still stage for /wrap-up approval, but tasks and render now go direct. README's 'typical session' was reframed to lead with the agent-driven 'you say X / agent does Y' flow, demoting CLI to a fallback. Binary reinstalled so Codex's MCP client sees the new tool surface.
- **2026-05-13 18:23:57** (codex:wrap-up) — Since the previous wrap-up, committed the K9 artifact cleanup and user-level /wrap-up lift in f97bcbf, added Codex CLI provenance symmetry plus migration 0008 in 7671f07, and rewrote the README while adding Claude/Codex skill templates in 5e9a0c6. The current wrap-up found no pending reviews, no open tasks, and a clean worktree before drafting these DB updates.
