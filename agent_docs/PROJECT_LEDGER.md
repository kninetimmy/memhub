<!-- memhub:rendered -->
<!-- DO NOT EDIT. Generated from .memhub/project.sqlite. -->
<!-- To change content, use memhub CLI; then re-run `memhub render`. -->
<!-- Generated at: 2026-05-14T03:32:00Z by memhub 0.1.0 -->

# memhub — Ledger

## Decisions

_63 decision(s). Most recent first._

### D63 — memhub viz v1 scope: ephemeral launcher + 5 panels + PCA + polling activity feed

**Status:** active • **Decided:** 2026-05-14 03:30:04 • **Source:** user+agent:claude-code

v1 ships: 'memhub viz' ephemeral one-shot launcher; panels for Overview (counts + schema + recent writes), Embedding map (PCA projection — boring, deterministic, no UMAP dependency), Recall inspector with replay animation, Activity feed (polling at 2s tail of writes_log), Audit (provenance views over source and confidence); localhost bind + one-time token in URL; new src/dashboard/ module; optional feature flag (--features viz) so the default binary stays slim. Estimated 3-5 days of work. v2 adds UMAP, the projects registry + multi-project sidebar, SSE push for the activity feed, and the 'pin a row, see neighbors' interaction in the embedding map. v3 nice-to-haves (workspace cross-project view, decisions force-directed graph, eval timeline) are explicitly out of scope until v1+v2 land and produce real usage signal.

---

### D62 — Recall inspector replays pipeline state client-side from a verbose response

**Status:** active • **Decided:** 2026-05-14 03:29:54 • **Source:** user+agent:claude-code

When the user submits a query to the recall inspector panel, the server runs the full recall pipeline server-side and returns a verbose response containing every intermediate stage's state — tokenization, fts hits, vector candidates, min_vector_score filter, blend, top-K. The SPA animates through these stages client-side. Simpler than streaming intermediate state from a long-running query, more useful in practice (pause, scrub, inspect a specific stage), and avoids needing to introduce checkpointing into the live recall code path. The replay shape composes cleanly with the existing recall::run; the only change there is an opt-in 'verbose' mode that retains intermediate values for the response. Live tap into in-flight queries is explicitly out of scope.

---

### D61 — Listening-port invariant extended to allow user-initiated localhost binds

**Status:** active • **Decided:** 2026-05-14 03:29:45 • **Source:** user+agent:claude-code

The arch's 'No background services, listening ports, or external APIs' line was true when 'memhub serve' MCP was the only port-binding command. With 'memhub viz' it becomes 'No background services or external APIs; listening ports are localhost-only, started explicitly by the user — memhub serve for MCP, memhub viz for dashboard.' Both bind 127.0.0.1, both are user-initiated foreground processes, both are token-or-stdio gated. The product is still local-first and offline; this is a clarifying refinement of the invariant, not a posture shift.

---

### D60 — memhub viz dashboard is read-only and never writes to writes_log

**Status:** active • **Decided:** 2026-05-14 03:29:36 • **Source:** user+agent:claude-code

Every panel reads through existing read paths (state show, arch show, note list, recall, plus new SELECTs over embeddings and writes_log). No endpoint stages pending writes, mutates durable rows, or records to writes_log. Mirrors the existing read-only contract on recall and on the eval retrieval harness, and keeps the dashboard from accumulating its own audit obligations. If a future panel needs to mutate (e.g. 'accept pending write from the UI'), that is a v3 decision worth re-litigating; v1 stays strictly read-only.

---

### D59 — memhub viz discovery is project-scoped in v1, registry in v2

**Status:** active • **Decided:** 2026-05-14 03:29:27 • **Source:** user+agent:claude-code

v1 anchors 'memhub viz' to the cwd's .memhub/ the same way 'memhub render' and 'memhub recall' do — one server, one project, no global state. Multi-project viewing in v1 means launching the command in another shell against another project on a different port. v2 will add a ~/.memhub/projects.toml registry populated explicitly via 'memhub viz register <path>' (or auto-appended on 'memhub init'), and 'memhub viz --all' will open a sidebar-switcher view across registered projects. Filesystem scanning was rejected on privacy and noise grounds — a user should opt in to having a project visible in the dashboard, not have it auto-discovered from a $HOME walk.

---

### D58 — memhub viz is ephemeral, not a persistent daemon

**Status:** active • **Decided:** 2026-05-14 03:29:17 • **Source:** user+agent:claude-code

The dashboard server runs as a one-shot foreground process: 'memhub viz' binds a random localhost port, prints the URL with a one-time token, optionally opens the browser, and lives until Ctrl-C. No PID files, no auto-restart, no lock files, no log rotation. Matches how 'memhub render' and 'memhub recall' are invoked (anchored to a CLI call) and minimizes the departure from the existing no-persistent-listening-ports arch invariant. A daemon mode can be added later if always-on observability proves valuable, but on-demand value is enough for v1 and the global state cost of a daemon is real (PID management, restart-after-reboot, port reuse, log rotation).

---

### D57 — Hybrid min_vector_score gates raw vector cosine, not blended score

**Status:** active • **Decided:** 2026-05-14 02:46:32 • **Source:** user+agent:claude-code

Two designs were on the table: (A) a floor on the blended final_score (single knob, one number to reason about) or (B) a floor on raw vector cosine that gates the vector candidate set before scoring. Picked B. Rationale: the failure mode is specifically the vector path solo-producing low-confidence candidates against nonsense queries — gating raw cosine targets that mode directly, leaves FTS hits untouched, and avoids conflating two independent knobs (vector_weight and a separate min_score) into a single mental model. A blended-score floor would either over-filter legit hybrid hits or under-filter nonsense depending on the relative weight of fts_score vs vector_score. The numeric default (0.7) is recorded in fact #4, calibrated empirically against the live corpus.

---

### D56 — Index rebuild preserves newer eager embeddings

**Status:** active • **Decided:** 2026-05-14 00:37:22 • **Source:** user+agent:codex

Rebuild embeds from a snapshot but verifies the current source hash before each upsert and prunes only orphaned active-model embeddings. This avoids overwriting a fresher eager embedding written by a concurrent fact/decision/task update.

---

### D55 — Recall staleness filtering applies to fact freshness, not task/decision lifecycle

**Status:** active • **Decided:** 2026-05-14 00:37:15 • **Source:** user+agent:codex

include_stale=false should hide stale facts, not completed tasks or inactive decisions. Done tasks and superseded/draft decisions can still be relevant historical evidence and should remain recallable unless a future explicit status filter is added.

---

### D54 — Recall must not maintain legacy chunks

**Status:** active • **Decided:** 2026-05-14 00:37:09 • **Source:** user+agent:codex

memhub recall is a read surface and must not mutate durable or derived tables. Legacy decision chunks maintenance belongs to write/import/search-maintenance paths, not recall/eval, so read-only behavior remains true even on read-only DB mounts.

---

### D53 — Golden queries are keyword-style (3-6 specific terms), not natural-language sentences

**Status:** active • **Decided:** 2026-05-13 23:18:54 • **Source:** user+agent:claude-code

build_fts_match in src/retrieval/recall.rs splits queries on whitespace and ANDs every token in the FTS5 MATCH expression. Natural-language sentences embed stop words ('how', 'does', 'and', 'for') the FTS5 index doesn't carry, so the AND gate produces zero candidates. Golden queries are written as terse 3-6 keyword phrases to play well with the matcher. Documented in tests/retrieval_golden.json description. If recall's tokenizer ever grows stop-word filtering, the golden set can be rewritten in natural form; until then, this is the contract.

---

### D52 — MCP ServerHandler impl requires #[tool_handler] to expose tools

**Status:** active • **Decided:** 2026-05-13 23:18:51 • **Source:** user+agent:claude-code

Without rmcp's #[tool_handler] macro on the impl ServerHandler block, the default list_tools and call_tool methods return empty regardless of how many tools #[tool_router] has registered. This was a latent bug since the MCP server was first written, surfaced and fixed in 3a3519c. Fix passes router = self.tool_router to dispatch through the stored router field rather than re-constructing it per call. tests/mcp_protocol.rs is the regression test — it spawns memhub serve as a subprocess and asserts the full 15-tool surface is enumerable. Future MCP refactors must keep the attribute or replace it with manual list_tools + call_tool overrides.

---

### D51 — memhub eval retrieval is read-only and never writes to writes_log

**Status:** active • **Decided:** 2026-05-13 23:18:48 • **Source:** user+agent:claude-code

Like memhub.recall (decision 48), eval drives the shared retrieval::recall engine, applies golden-query matchers, and returns a Recall@K plus per-query outcome bundle. It never mutates durable rows, never stages pending writes, never logs to writes_log. Codified because the natural temptation for an 'eval ran on these queries' audit trail would distort memhub stats and the writes_log activity metric. If we ever want eval history, it belongs in a separate eval_runs table, not commingled with mutations.

---

### D50 — CLAUDE.md and AGENTS.md teach agents to prefer recall over the ledger and to ask before /reindex

**Status:** active • **Decided:** 2026-05-13 22:22:28 • **Source:** user+agent:claude-code

Decision 34 (agents prefer recall over reading PROJECT_LEDGER.md) and decision 35 (PROJECT.md stays thin; ledger becomes lazy-loaded) only materialize as a token-savings win if the per-repo agent instructions actually encode the rule. PR5 rewrites the Session Continuity section in both CLAUDE.md and AGENTS.md: read PROJECT.md at session start, prefer memhub.recall mid-session, treat PROJECT_LEDGER.md as fallback only. Stale-embedding warnings get surfaced and asked-about; agents must not auto-invoke /reindex (per decision 29). This is the load-bearing user-facing artifact that makes the M8 retrieval surface change agent behavior, not just exist as a tool.

---

### D49 — memhub index rebuild ignores [retrieval] mode and logs one writes_log row per rebuild

**Status:** active • **Decided:** 2026-05-13 22:22:20 • **Source:** user+agent:claude-code

Rebuild forces embedding generation regardless of mode = fts or hybrid so it can backfill fts -> hybrid migrations and re-embed after a model upgrade. Single transaction: wipe embeddings for the active model, embed in source-type batches via embed_batch, UPSERT fresh rows. The writes_log gets one summary row attributed to the passed actor, not one per source row, because rebuild is a single semantic action even when it touches dozens of embeddings. Per-row logging would explode the audit table for a use case the user explicitly triggered.

---

### D48 — memhub recall is read-only and never writes to writes_log

**Status:** active • **Decided:** 2026-05-13 22:22:12 • **Source:** user+agent:claude-code

Recall fetches FTS hits per source table, computes brute-force cosine over the active-model embeddings (hybrid only), blends them via the scoring config, and returns a ranked bundle. No row in writes_log, no durable mutation, no pending_writes entry. Addendum §8 says 'read-only' but codifying it as a decision because the natural temptation when adding observability would be to log every recall call, and that would distort memhub stats and writes_log activity metrics. Logging belongs to writers; recall is a reader.

---

### D47 — Recall --accepted-only filters to source IN ('user', 'user+agent:%')

**Status:** active • **Decided:** 2026-05-13 22:22:06 • **Source:** user+agent:claude-code

The M8 addendum's loose phrasing 'status=accepted decisions' does not map to any existing column. The concrete mapping is by source vocabulary: rows whose source is 'user' (CLI-authored) or 'user+agent:<id>' (passed through review acceptance). Consistent across fact/decision/task and matches the user-touched durability story. Rejected coupling to decision.status='active' because it conflates two filter concerns into one flag; if active-only ever becomes useful, ship it as a separate filter.

---

### D46 — Embed text format: fact key+value, decision title+rationale, task title (+notes when present)

**Status:** active • **Decided:** 2026-05-13 21:49:46 • **Source:** user+agent:claude-code

Fact embed text is 'key: value' so queries can match the key as label and value as body. Decision embed text is 'title\n\nrationale' so the tokenizer sees title and body as distinct segments. Task embed text is 'title\n\nnotes' when notes is present, else 'title' alone. content_hash is computed over the same string the model embeds, so any change to either side triggers re-embed on the next add. Chosen for short-sentence input shape that matches how BGE was trained.

---

### D45 — Embedding deletion cascades via SQL triggers, not app-level cleanup

**Status:** active • **Decided:** 2026-05-13 21:49:43 • **Source:** user+agent:claude-code

embeddings.(source_type, source_id) is a polymorphic key SQLite cannot express as a real FK. Addendum §3.2 suggested 'the application layer is responsible for orphan cleanup,' but v1 has no fact/decision/task delete command, and 0009 already used SQL triggers for FTS sync. Three AFTER DELETE triggers (one per source table) mirror that pattern and stay correct even when raw SQL deletes a row. Rejected requiring an app-level delete helper because no v1 caller exists; the trigger keeps the contract local to the schema.

---

### D44 — Eager-embed is gated by [retrieval] mode in config.toml

**Status:** active • **Decided:** 2026-05-13 21:49:41 • **Source:** user+agent:claude-code

Decision 21 specified an install-time toggle but did not say which side of the write path it controls. The split: mode = fts (default) makes fact/decision/task add a no-op for embeddings — the model never loads, the embeddings table stays empty. mode = hybrid runs eager_embed_in_tx inside each source-write transaction. Rejected always-embed because the BGE-small load cost would push every test through model init. Rejected mode-in-DB because TOML stays canonical and avoids drift. Switching fts → hybrid will require a one-shot memhub index rebuild to backfill — not shipped in PR3.

---

### D43 — SHA256-hex over UTF-8 embed text for embeddings.content_hash

**Status:** active • **Decided:** 2026-05-13 21:49:39 • **Source:** user+agent:claude-code

Addendum §3.2 said 'BLAKE3 (or similar)'. sha2 was already a build-dep for build.rs verification; promoting it to runtime adds no new crate. content_hash is only used for drift detection (skip re-embed when text unchanged), not for cryptographic guarantees. SHA256 also matches the verification mechanism used by build.rs and HF's x-linked-etag.

---

### D42 — Bundle BGE-small via build.rs auto-download into OUT_DIR

**Status:** active • **Decided:** 2026-05-13 21:49:31 • **Source:** user+agent:claude-code

build.rs fetches model.onnx + 4 tokenizer files from BAAI/bge-small-en-v1.5@main on first build, verifies each against a pinned SHA256, and short-circuits on cache hit. Rejected the manual-fetch-script alternative because contributor ergonomics favor zero-setup cargo build. model.onnx SHA256 (828e1496...cf35, 133 MB) came from HF's x-linked-etag; tokenizer file hashes computed locally over downloaded bytes. Decision 23 said 'bundled in binary' without specifying how; this is the implementation choice.

---

### D41 — MCP-originated writes log under the calling client identity, not cli:user

**Status:** active • **Decided:** 2026-05-13 20:54:51 • **Source:** user+agent:claude-code

command::verify and render::render_project previously hardcoded cli:user in their writes_log entries; when Codex or Claude called the record_command or render MCP tools, the audit log misattributed those writes as human CLI actions, distorting memhub stats. Both functions now accept an actor parameter. CLI entry points pass DEFAULT_ACTOR (preserves prior behavior). MCP wrappers pass the normalized client identity from the request context, the same pattern task_add / task_done / log_session_note already use.

---

### D40 — memhub render is two-phase: prepare all temps and backups before any destination rename

**Status:** active • **Decided:** 2026-05-13 20:54:45 • **Source:** user+agent:claude-code

Previously render walked the two output files one at a time, doing backup, remove, rename per file. A mid-loop failure could leave PROJECT.md replaced while PROJECT_LEDGER.md was missing or stale. Render is now split into phase 1 (backup plus write all temp files) and phase 2 (rename each temp into place). All failable preparation runs before any destination is touched; either both temps exist or no destination is at risk. write_with_replace also drops the redundant pre-rename fs::remove_file because fs::rename atomically replaces existing regular files on Unix and Windows. The irreducible inconsistency window is between the two atomic renames in phase 2.

---

### D39 — Source vocabulary is writer-enforced, schema stays unconstrained TEXT

**Status:** active • **Decided:** 2026-05-13 20:54:39 • **Source:** user+agent:claude-code

The source-vocabulary addendum already specified writer enforcement of the user / git / observed / agent:<id> / user+agent:<id> vocabulary; until this session, CLI and acceptance paths accepted any string, so typos like user+agnet:codex silently persisted. validate_source() now lives in commands::mod and is called from fact::add_in_tx and decision::add_with_decided_at_in_tx, so both CLI writes and review acceptance go through the same gate. The schema stays unconstrained TEXT on purpose: enforcement lives at the writer layer where it can evolve without migrations.

---

### D38 — Review acceptance promotes pending to durable in one Immediate transaction

**Status:** active • **Decided:** 2026-05-13 20:54:33 • **Source:** user+agent:claude-code

Previously the durable insert (via fact::add / decision::add) and the pending status update (via mark_status) lived in separate transactions on separate connections. A crash or concurrent acceptor between commits could leave a durable row with a still-pending pending_writes row, or produce duplicate durable rows. accept now opens one Immediate transaction, performs the durable insert via fact::add_in_tx or decision::add_with_decided_at_in_tx, updates pending_writes inside the same transaction, and commits once. Concurrent acceptors serialize at the write lock; if the row is no longer pending, the whole transaction rolls back with no durable side effect.

---

### D37 — /eval-recall skill runs the harness

**Status:** active • **Decided:** 2026-05-13 20:14:10 • **Source:** user+agent:claude-code

Agent-driven invocation of memhub eval retrieval. Surfaces the Recall@3 number without manual CLI invocation. Stays consistent with the agent-first UX pattern for everything else in M8.

---

### D36 — Eval metric: Recall@3 via tests/retrieval_golden.json

**Status:** active • **Decided:** 2026-05-13 20:14:10 • **Source:** user+agent:claude-code

Single-number test: across golden queries, what fraction had the expected row in the top 3 recall results? Simple to interpret, easy to track across scoring or model changes. 12 starter queries seeded covering decisions, facts, tasks, and negative cases.

---

### D35 — PROJECT.md stays as thin summary; PROJECT_LEDGER.md becomes lazy-loaded

**Status:** active • **Decided:** 2026-05-13 20:14:10 • **Source:** user+agent:claude-code

Render output unchanged in shape, but agent consumption pattern shifts. PROJECT.md is always in session-start context (small). PROJECT_LEDGER.md becomes a fallback the agent reads only when recall is insufficient.

---

### D34 — Agents prefer recall over reading PROJECT_LEDGER.md

**Status:** active • **Decided:** 2026-05-13 20:14:10 • **Source:** user+agent:claude-code

Load-bearing rule for the token-savings win. Encoded in CLAUDE.md and the existing skills: at session start read PROJECT.md only; reach for the ledger only after recall comes up short.

---

### D33 — Zero-result behavior: empty bundle, no automatic fallback

**Status:** active • **Decided:** 2026-05-13 20:14:10 • **Source:** user+agent:claude-code

When recall finds no matches it returns an empty results array, not an automatic dump of PROJECT_LEDGER.md. The agent decides whether to read the ledger as fallback based on the question.

---

### D32 — Output formats: JSON default for MCP, markdown default for CLI

**Status:** active • **Decided:** 2026-05-13 20:14:10 • **Source:** user+agent:claude-code

MCP always returns structured JSON for downstream agent parsing. CLI defaults to human-readable markdown with a --json flag for scripting. Same internal query path produces both.

---

### D31 — /recall and /reindex slash commands for direct user invocation

**Status:** active • **Decided:** 2026-05-13 20:14:10 • **Source:** user+agent:claude-code

Two new Claude Code skills mirror the MCP tools for cases where the user wants to ask memhub directly without going through chat. /reindex used after model upgrades or on stale-embedding warnings.

---

### D30 — memhub.recall MCP tool is the primary agent surface

**Status:** active • **Decided:** 2026-05-13 20:14:10 • **Source:** user+agent:claude-code

Agents call recall via MCP for context retrieval. CLI commands exist but are for humans and admin tasks. Keeps agent-driven UX consistent with the existing task_add/render/list_facts pattern.

---

### D29 — Stale-embedding UX: warning flag plus agent-prompted /reindex

**Status:** active • **Decided:** 2026-05-13 20:14:10 • **Source:** user+agent:claude-code

When recall detects stale embeddings (e.g. after model upgrade) it returns a warnings array. CLAUDE.md rule tells the agent to surface the warning and ask the user before invoking /reindex rather than auto-running.

---

### D28 — content_hash drift detection per embedding

**Status:** active • **Decided:** 2026-05-13 20:13:58 • **Source:** user+agent:claude-code

Store a hash of source body alongside each vector. Mismatch on read marks the embedding stale and triggers re-embed on next eager-embed pass or forces a /reindex prompt.

---

### D27 — Eager-embed on writes inside the same transaction

**Status:** active • **Decided:** 2026-05-13 20:13:58 • **Source:** user+agent:claude-code

Fact, decision, and task add paths re-embed the affected row synchronously. ~50ms write overhead is acceptable for the consistency guarantee. Avoids a background queue and stale-index window.

---

### D26 — FTS5 virtual tables attached to source tables

**Status:** active • **Decided:** 2026-05-13 20:13:58 • **Source:** user+agent:claude-code

Contentless FTS5 over facts.body, decisions.rationale, and tasks.body. Triggers keep FTS indexes synced with source on insert/update/delete. No data duplication.

---

### D25 — No memory_chunks table; embed source rows directly

**Status:** active • **Decided:** 2026-05-13 20:13:58 • **Source:** user+agent:claude-code

Facts, decisions, and tasks are already short enough to be single chunks. Skipping a chunk normalization table avoids the chunk-source-drift problem and a UNION-shaped retrieval query is straightforward.

---

### D24 — Vector storage: SQLite BLOB plus brute-force cosine

**Status:** active • **Decided:** 2026-05-13 20:13:58 • **Source:** user+agent:claude-code

At memhub scale (hundreds to low thousands of rows) brute-force search is sub-10ms. sqlite-vec extension would add loadable-extension packaging complexity across platforms for no real gain at this scale.

---

### D23 — Embedding model: BGE-small-en-v1.5, bundled in binary

**Status:** active • **Decided:** 2026-05-13 20:13:58 • **Source:** user+agent:claude-code

384-dim vectors, ~130MB model file, total binary ~140MB. Chosen over MiniLM (older, weaker semantic quality) and BGE-base (4x size for 2-5% quality gain that does not match memhub content profile).

---

### D22 — Embedding library: fastembed-rs

**Status:** active • **Decided:** 2026-05-13 20:13:58 • **Source:** user+agent:claude-code

Pure Rust plus ONNX runtime, bundles model loading and caching. Chosen over candle (more flexible but overkill) and Ollama (external dependency conflicts with local-first install story).

---

### D21 — Install-time retrieval mode toggle

**Status:** active • **Decided:** 2026-05-13 20:13:58 • **Source:** user+agent:claude-code

Config flag [retrieval] mode = fts | hybrid in config.toml. Both modes share the same recall API; hybrid adds vector scoring on top of FTS plus metadata filters.

---

### D20 — Retrieval stays SQL-first; RAG is a derived layer

**Status:** active • **Decided:** 2026-05-13 20:13:58 • **Source:** user+agent:claude-code

Canonical memory lives in facts/decisions/tasks tables. FTS and vector indexes are rebuildable derived state that never owns truth. Deleting derived state must leave memhub fully functional in FTS-only mode.

---

### D19 — M8 milestone: SQL+RAG hybrid recall

**Status:** active • **Decided:** 2026-05-13 20:13:58 • **Source:** user+agent:claude-code

Self-contained milestone for the retrieval layer with explicit acceptance criteria: recall MCP tool works, eval harness reports Recall@3 metric, agents prefer recall over reading PROJECT_LEDGER.md.

---

### D18 — State and arch narratives are wrap-up-only; no MCP tools for state_set or arch_set

**Status:** active • **Decided:** 2026-05-13 18:40:26 • **Source:** user+agent:claude-code

Letting agents rewrite the state narrative mid-session invites drift. The wrap-up approval gate exists for exactly this kind of write, where the agent's draft gets per-item human review before landing. Bigger trust surface than facts/decisions (one row replaces the whole narrative) and used rarely (once per session), so not worth a structured tool that bypasses the gate.

---

### D17 — MCP tool trust split: direct writes for intent, staged writes for claims

**Status:** active • **Decided:** 2026-05-13 18:40:19 • **Source:** user+agent:claude-code

Tasks are intent that the user prunes; session notes are scratch; render regenerates from the DB — all low-trust and worth direct MCP tools. Facts and decisions are claims about reality and need the user-approval staging gate; bypassing it via direct MCP fact_add / decision_add would erode the 'agents are untrusted writers' principle that makes memhub trustworthy as a multi-agent store. Codified by which MCP tools exist in e67167e (task_add, task_done, list_facts, render direct; propose_fact, propose_decision staged).

---

### D16 — Memhub-native skills ship as installable Claude and Codex templates

**Status:** active • **Decided:** 2026-05-13 18:23:38 • **Source:** user+agent:codex

templates/skills/claude/* and templates/skills/codex/* are now the distribution shape for /wrap-up, /init-project, and /check-init, while user-level installed copies are runtime artifacts. This keeps the repo as the source for skill prompts without making installed dotfiles part of the project worktree.

---

### D15 — Durable source provenance is separate from audit actor

**Status:** active • **Decided:** 2026-05-13 18:23:30 • **Source:** user+agent:codex

Migration 0008_decisions_source and the source vocabulary addendum establish that durable facts/decisions record the claim source (user, agent:<id>, user+agent:<id>, git, observed) while writes_log.actor records who performed the write. Codex and Claude wrap-up flows should pass --source user+agent:<id> for user-approved fact/decision writes and --actor <agent>:wrap-up for audit attribution.

---

### D14 — /wrap-up promoted to user-level skill placement (supersedes D13)

**Status:** active • **Decided:** 2026-05-13 17:32:55 • **Source:** user

D13 kept /wrap-up at project-level inside ~/memhub/.claude/commands/wrap-up.md on the reasoning that it only fires in memhub-initialized repos — but D13 itself acknowledged /init-project and /check-init are at user-level despite the same .memhub/ precondition. That asymmetry was a bug and caused a real UX gap this session: /wrap-up was not available in Free-AI-SSD, forcing a mid-task lift. Resolution: copied the skill to ~/.claude/commands/wrap-up.md and deleted the project-level copy. All three memhub-native skills now live at user-level (single source of truth, no project-vs-user drift); each gates on .memhub/ existing. The project-level .claude/commands/ directory in this repo is now empty.

---

### D13 — Memhub-native skill placement: /wrap-up is project-level; /init-project and /check-init are user-level

**Status:** active • **Decided:** 2026-05-13 03:28:07 • **Source:** user

/wrap-up only makes sense in memhub-initialized repos (requires .memhub/), so it ships at .claude/commands/wrap-up.md inside the memhub source repo and only fires there. /init-project creates memhub in any repo and /check-init diagnoses any memhub-using repo, so they must apply globally and live at user-level (~/.claude/commands/). The collision-rename pattern from M7-001 stays the same in both placements: rename the K9 file to -k9.md, drop in the memhub-native version at the original filename. Single rule: the memhub-native skill lives where its trigger makes sense; the K9 collision gets renamed regardless.

---

### D12 — PRD evolves via addendum docs; the PRD itself stays verbatim

**Status:** active • **Decided:** 2026-05-13 02:22:14 • **Source:** user

CLAUDE.md guardrail says 'Keep docs/reference/memhub-prd.md verbatim.' When PRD-level wording needs to change (e.g., the K9 deprecation track inverting §2's 'markdown is entry point' framing), the change ships as a separate addendum doc at docs/reference/memhub-prd-<topic>-addendum.md that is authoritative for the items it modifies. Future addenda follow the same naming pattern. The PRD's structure, design principles, non-goals, and section ordering all stay as they are. Captured in 7c162b2 as the first instance: docs/reference/memhub-prd-deprecation-addendum.md. This pattern keeps the original PRD as a stable canonical artifact while letting the project evolve under explicit, dated revisions readable by anyone tracing the PRD's history.

---

### D11 — Slash command collision resolution: rename the user-level skill

**Status:** active • **Decided:** 2026-05-13 01:50:34 • **Source:** user

Claude Code resolves /command names by filename and applies enterprise > personal > project precedence — personal shadows project. Documented at code.claude.com/docs/en/skills.md, not a bug. There is no /project:<name> namespace escape hatch for non-plugin commands. When a project-level memhub-native skill needs to claim a /command name already used by a user-level skill (typically K9), the resolution is to rename the user-level skill (e.g., wrap-up.md -> wrap-up-k9.md) plus update its frontmatter description. The forward-looking ergonomic (memhub) wins; the deprecating ergonomic (K9) takes the longer name. This pattern applies to any future memhub-native /init-project and /check-init equivalents and is reflected in cross-session memory at feedback_ux_mimic_k9.md. Captured operationally in the M7-001 backlog closure (103eea0).

---

### D10 — Wrap-up session boundary is implicit; no sessions table

**Status:** active • **Decided:** 2026-05-13 00:07:40 • **Source:** user

The most recent project_state row created_at is the previous wrap-up timestamp. Anything newer in decisions, tasks, facts, session_notes, pending_writes, or commits is in-window. Rejected an explicit sessions(id, started_at, ended_at, summary) table plus memhub session start/end CLI because that structure is bookkeeping until something queries it. If real use surfaces a need for explicit sessions, the table ships as a future migration. Until then, since-last-state-set is a free boundary that requires no schema change.

---

### D9 — Wrap-up routing brain is a Claude Code skill, not a CLI subcommand

**Status:** active • **Decided:** 2026-05-13 00:07:39 • **Source:** user

The session-end approval gate that orchestrates state set, decision add, task close, fact add, note add, review accept/reject, and render lives as .claude/commands/wrap-up.md in this repo, not as memhub wrap-up. Rejected a memhub wrap-up CLI subcommand because UX continuity matters: typing /wrap-up is the user muscle memory; memhub wrap-up is a different gesture and feels like a regression. The skill route also keeps the memhub binary as small primitives per the PRD boring tech principle, and skill prompts iterate without a Rust recompile. Trade-off accepted: users who do not run Claude Code do not get a wrap-up brain, only the CLI primitives. Captured in docs/roadmap/wrap-up-design.md section 1.

---

### D8 — Render conflict semantics: DB wins, prior file backed up

**Status:** active • **Decided:** 2026-05-13 00:07:39 • **Source:** user

memhub render unconditionally overwrites existing rendered files but first copies them under .memhub/backups/rendered/<stamp>/, mirroring the existing sync_md markdown-backup convention. Rejected refuse-on-divergence (require --force if file content diverges from prior render) because refusing punishes the user for a mistake the file own header already warns about; backup-and-overwrite preserves the edit content if they need it. Rendered files are generated artifacts. A human editing one is a category error: the change will not survive the next render and is not reflected in the DB.

---

### D7 — Render trigger is on-demand; auto_render is opt-in for later

**Status:** active • **Decided:** 2026-05-13 00:07:39 • **Source:** user

memhub render is an explicit CLI command. There is no auto_render config flag in v1. Rejected auto-firing after every mutating write (mirroring auto_sync_md) because render output is bigger than the small managed block sync-md produces and would clutter git status mid-session. The natural cadence is render-at-session-end, which is a wrap-up step. auto_render = true is reserved in [render] config as a future opt-in.

---

### D6 — State and arch durable storage uses single-blob tables, not decomposed columns

**Status:** active • **Decided:** 2026-05-13 00:07:39 • **Source:** user

Migration 0007_project_narrative.sql adds project_state and project_arch tables with (id, project_id, body, actor, actor_raw, created_at) shape. set always inserts a new row (append-only history); show returns the most recent; history lists prior. Rejected decomposing narrative into structured columns (currently_building, next_up, open_questions) because decomposition is a guess at structure that may not survive contact with how state actually evolves. Going decomposed to blob is harder than blob to decomposed if querying patterns later demand it, so blob ships first.

---

### D5 — Render output shape is memhub-native two-file (PROJECT.md + PROJECT_LEDGER.md)

**Status:** active • **Decided:** 2026-05-13 00:07:39 • **Source:** user

memhub render emits two files into the configured output dir (default agent_docs/): PROJECT.md for narrative (state, arch, recent session notes) and PROJECT_LEDGER.md for structured append-only content (decisions, backlog, facts, recent activity). Rejected K9-style four-file mirror because mirror would inherit K9's split decisions without earning them and would name-collide with K9 files during transition. Two files split along the natural seam in the DB: narrative-cadence (rewritten on state changes) versus ledger-cadence (appended on decisions/tasks/facts). Each rendered file leads with a memhub:rendered marker comment plus ISO timestamp and memhub version. Captured in docs/roadmap/memhub-render-design.md section 1.

---

### D4 — K9 framework deprecation: memhub becomes primary durable store

**Status:** active • **Decided:** 2026-05-12 22:40:23 • **Source:** user

End-state has memhub as the single durable source; K9 retires its slash commands, markdown templates, and routing brain. Direction committed in intent; implementation slices land individually with their own design docs. PRD §2 and k9-integration non-goals stay in force until each slice argues an explicit change. See docs/roadmap/k9-deprecation-plan.md for the four load-bearing slices and docs/roadmap/memhub-primary-evaluation.md as the origin record.

---

### D3 — K9 canonical conventions (H3 backlog items, em-dash decisions) are the parser target

**Status:** active • **Decided:** 2026-05-12 21:03:01 • **Source:** user

K9-Claude-Framework/docs/file-structure.md:156-208 is the authoritative K9 framework spec. project_backlog.md items use ### Title H3 delimiters with bulleted bolded-field bodies. The Status field uses vocabulary: triaged, planning, in-progress, blocked, or done. project_decisions.md entries use ## YYYY-MM-DD plus em-dash plus Title with a Unicode em-dash separator (U+2014). M6-001 and M6-002 target only this canonical format. Free-AI-SSD follows it; memhub agent_docs does not. M6-004 migrates memhub agent_docs files to canonical structural delimiters (em-dash separator on decisions, H3 headings on backlog) while preserving prose Notes paragraphs rather than decomposing into K9 bulleted slots. The lenient support-both-formats option was rejected because doubling the parser surface would bake memhub divergence into the supported contract; the narrower trailing Status line is retained only as a tolerated input recognition path, not a supported authoring convention.

---

### D2 — Evaluating memhub-primary is staged behind parser-fix evidence

**Status:** active • **Decided:** 2026-05-12 21:02:58 • **Source:** user

A working-session analysis report (local, not committed) identifies six bootstrap parser bugs in src/commands/bootstrap_k9.rs and a 12-gap roadmap for full memhub-primary replacement of the K9 framework. Direction is not committed. Evaluation plan lives in docs/roadmap/memhub-primary-evaluation.md: Phase 1 lands parser fixes (M6-001 and M6-002), Phase 2 re-runs bootstrap on Free-AI-SSD and captures findings (M6-003), Phase 3 routes to commissioning a separate memhub render design doc, extending the parser further, or closing the evaluation with memhub staying complementary. PRD section 2 (markdown files stay as the entry point), PRD section 4 non-goals, and docs/roadmap/k9-integration.md non-goals (no bidirectional sync, no DB mapping of project_state.md or project_arch.md) all remain in force. Reasoning: replacement is genuinely better only if parser fixes land cleanly AND a memhub render materializes acceptably; committing the roadmap before parser-fix evidence would be premature.

---

### D1 — One-shot K9 bootstrap is the narrow exception to the no-import non-goal

**Status:** active • **Decided:** 2026-05-12 19:38:52 • **Source:** user

Steady-state non-goal of no bulk K9 import stays. First-install bootstrap-k9 is the narrow carve-out for new-machine clones with populated K9 history. Refuses on non-empty target; writes through decision::add and task::add_with_status with actor=k9:bootstrap; skips project_state.md/project_arch.md and facts by design.

---

## Backlog

_20 task(s), 3 open. Open first, then by recency._

### T20 — Add cross-encoder re-ranker for hybrid recall

**Status:** open • **Updated:** 2026-05-14 03:15:08

Standard bi-encoder→cross-encoder pipeline. Bundle a re-ranker ONNX via build.rs same as BGE-small (model choice TBD: ms-marco-MiniLM-L-6-v2 ~80MB lower quality faster, or bge-reranker-v2-m3 ~280MB higher quality slower). New src/retrieval/rerank.rs module. recall::run flow: fetch top-N (default 20) by current blend, run pairs through reranker, sort by rerank score, truncate to K. Opt-in via [retrieval] use_reranker=false default, with [retrieval] rerank_candidate_pool=20. Validate via memhub eval retrieval — Recall@3 expected to climb, safety probes must still pass with the floor in place. Adds 50-200ms per recall in hybrid+rerank mode. Open question: model size/quality trade-off should be settled by running both candidates through eval harness against current golden set before committing the bundle.

---

### T19 — Add memhub viz dashboard — local visualization for the RAG database

**Status:** open • **Updated:** 2026-05-14 03:15:04

Localhost-only ephemeral webserver (axum + token-gated) that visualizes a project's memhub DB. Core panels: 2D embedding-space projection (UMAP/PCA) colored by source_type, writes_log timeline by actor, per-query recall inspector showing fts/vector/blend contributions, stale/drift canary, confidence histogram. Design questions to resolve in planning session: multi-project discovery model (filesystem scan vs registry file vs explicit add), tab/window model (one tab with sidebar switcher vs tab-per-project vs workspace cross-project view), refresh model (static vs polling vs WebSocket push), recall-replay interactive mode (animate the pipeline step by step), invocation lifecycle (ephemeral one-shot vs persistent daemon). Implementation: new src/dashboard/ module, axum + tower-http, static SPA bundled via include_bytes!, optional feature flag to keep default binary slim. Read-only on the same terms as recall — no writes_log entries. Caveat: first time memhub binds a port; arch's no-listening-ports invariant becomes no-persistent-listening-ports.

---

### T10 — Dogfood Codex memhub skills in fresh and existing repos

**Status:** open • **Updated:** 2026-05-13 18:23:52

Exercise /wrap-up, /init-project, and /check-init from the Codex skill templates after install; verify attribution, render output, and fresh-repo behavior.

---

### T18 — Add min-score threshold to memhub recall in hybrid mode

**Status:** done • **Updated:** 2026-05-14 02:39:25

Free-AI-SSD smoke test surfaced pure-nonsense queries returning low-similarity (~0.32) hits via vector path. Add a configurable cutoff (likely a new [retrieval.scoring] knob with a default around 0.4) so nonsense bundles are empty in hybrid mode the way they are in fts mode. The eval harness already has the shape to verify this — tests/retrieval_golden.json safety probes will start failing under hybrid mode until the threshold lands. Consider whether the threshold should apply to the blended final_score or just the vector_score component when fts_score is zero.

---

### T16 — PR6: eval harness — golden queries + /eval-recall skill

**Status:** done • **Updated:** 2026-05-13 23:18:59

tests/retrieval_golden.json with 12 seeded queries. memhub eval retrieval command computes Recall@3. /eval-recall skill invokes it and reports the number. Acceptance gate for M8: harness exists and reports a baseline.

---

### T15 — PR5: /recall and /reindex skills + CLAUDE.md lazy-ledger update

**Status:** done • **Updated:** 2026-05-13 22:22:32

New Claude Code skills under templates/skills/claude/. CLAUDE.md rule update: agents read PROJECT.md at session start, call memhub.recall mid-session, read PROJECT_LEDGER.md only as fallback. Codex skills mirror the Claude ones.

---

### T14 — PR4: recall CLI command + MCP tool with hybrid scoring

**Status:** done • **Updated:** 2026-05-13 22:22:32

memhub recall <query> command with filters (--source-type, --max-results, --json, --include-stale, --accepted-only). memhub.recall MCP tool. Hybrid scoring: 0.5 FTS + 0.5 vector + stale penalty + filters. Both modes (fts, hybrid) supported. Empty result returns empty bundle.

---

### T17 — Reinstall memhub binary on PATH (M8 PR1-PR3 outdated)

**Status:** done • **Updated:** 2026-05-13 21:55:37

~/.cargo/bin/memhub and the ~/.local/bin/memhub shadow still report schema 0008. Run cargo install --path . to rebuild ~/.cargo/bin/memhub, then copy the result over the ~/.local/bin shadow. Required before any consumer outside this repo (MCP clients, other-repo agents) sees the new embeddings/FTS surface from M8 PR1-PR3.

---

### T13 — PR3: eager-embed on writes (fact/decision/task add paths)

**Status:** done • **Updated:** 2026-05-13 21:49:59

Hook into fact/decision/task add handlers. Re-embed within the same transaction. content_hash short-circuits no-op writes. Target ~50ms write latency. Update paths handle delete-then-insert of the embedding row.

---

### T12 — PR2: schema migration — FTS5 virtual tables + embeddings table

**Status:** done • **Updated:** 2026-05-13 21:49:57

Migration 0009: add embeddings table (source_type, source_id, model_name, dimension, vector BLOB, content_hash, created_at, UNIQUE constraint). Add FTS5 virtual tables over facts.body, decisions.rationale, tasks.body with sync triggers. Backfill on first run.

---

### T11 — PR1: fastembed-rs integration + bundled BGE-small model

**Status:** done • **Updated:** 2026-05-13 21:49:55

Add fastembed-rs dependency. Bundle BGE-small-en-v1.5 ONNX model via include_bytes! (~130MB). Inference wrapper in src/retrieval/embeddings.rs with lazy model load. Smoke test: produce vector for known input, verify dimension=384.

---

### T9 — Migrate memhub's own agent_docs from K9 markdown to memhub-rendered files (M7-002)

**Status:** done • **Updated:** 2026-05-13 01:50:34

Blocked on M7-001. Once the memhub-native /wrap-up runs in this repo: (a) populate project_state and project_arch tables from current K9 narrative via memhub state set --from-file and memhub arch set --from-file; (b) run memhub render to produce PROJECT.md and PROJECT_LEDGER.md; (c) update CLAUDE.md Session Continuity to point at the new files instead of the K9 four-file set; (d) memhub integrations disable-k9; (e) decide whether to remove or archive the four K9 project_*.md files. Closes the M6-004 dogfood gap by replacing it. Validates the full memhub-primary loop end-to-end.

---

### T5 — M6-004 - Migrate memhub agent_docs to K9 canonical structural delimiters

**Status:** done • **Updated:** 2026-05-13 01:50:34

_No notes._

---

### T8 — Investigate .claude/commands/wrap-up.md override gap (M7-001)

**Status:** done • **Updated:** 2026-05-13 01:30:02

Project-level slash command at .claude/commands/wrap-up.md was expected to take precedence over user-level ~/.claude/commands/wrap-up.md when invoked inside the project. Empirically the user-level K9 /wrap-up fired instead during step 3 dogfood. Until this is understood and fixed, the memhub-native wrap-up cannot actually be dogfooded inside the memhub repo. Investigation: check Claude Code docs for skill resolution rules, verify file placement convention (commands/ vs agents/ vs skills/), test with a unique-named project skill to confirm whether project-level loading works at all, then either fix placement or rename to avoid collision while preserving slash-command ergonomics. Gates M7-002.

---

### T7 — M6-006 - Accept UTF-8 mojibake separator (â€" triple-codepoint sequence) in extract_date_and_title

**Status:** done • **Updated:** 2026-05-12 22:40:23

_No notes._

---

### T6 — M6-005 - Extend done-marker detection to recognize 'merged DATE' intervention and 'Shipped' vocabulary

**Status:** done • **Updated:** 2026-05-12 22:40:23

_No notes._

---

### T4 — M6-003 - Re-run bootstrap on Free-AI-SSD post-fix, capture findings, write Phase 2 results into the evaluation doc

**Status:** done • **Updated:** 2026-05-12 21:39:53

_No notes._

---

### T3 — M6-002 - Accept em-dash in strip_date_prefix and extract decision date into decided_at

**Status:** done • **Updated:** 2026-05-12 21:15:10

_No notes._

---

### T2 — M6-001 - Rewrite parse_backlog around K9 canonical H3 delimiters with structured done-detection

**Status:** done • **Updated:** 2026-05-12 21:03:14

_No notes._

---

### T1 — M5-006 - Add memhub integrations bootstrap-k9 first-install priming

**Status:** done • **Updated:** 2026-05-12 19:38:58

First-install-only memhub integrations bootstrap-k9 [--dry-run] [--json]. Refuses unless K9 enabled AND decisions+tasks empty; no --force. Parses project_decisions.md (heading-block, body becomes rationale) and project_backlog.md (bullets, Status: completed skipped, Status: blocked maps to blocked). Writes through decision::add and new task::add_with_status helper with actor=k9:bootstrap. project_state.md/project_arch.md not parsed; no fact extraction. 5 subprocess + 4 parser unit tests. Roadmap non-goal softened with carve-out. Discoverability: README install-prompt step 6 and checked-in audit prompt; K9 framework v1.2.1 shims outside this repo.

---

## Facts

_5 fact(s), 0 stale._

| Key | Value | Source | Confidence | Verified | Stale |
|-----|-------|--------|-----------|----------|-------|
| build-command | cargo build | user+agent:claude-code | 1.00 | 2026-05-13 20:55:34 | no |
| install-command | cargo install --path . --force && cp ~/.cargo/bin/memhub ~/.local/bin/memhub | user+agent:codex | 1.00 | 2026-05-14 00:37:33 | no |
| retrieval.min_vector_score-calibration | Default raw-cosine floor on the hybrid vector path is 0.7. Calibrated 2026-05-13 against the live .memhub/project.sqlite: nonsense queries (e.g. zxqv-pure-nonsense...) peak at cosine ~0.67; legitimate top-1 matches sit at >=0.78. 0.7 gives ~0.08 headroom on both sides while keeping Recall@3 at 11/11 and safety 1/1 in eval retrieval --mode hybrid. The task-note value of ~0.4 was actually the blended final_score (0.5*0 + 0.5*0.67), not the raw cosine — do not regress to 0.4. Re-tune if BGE-small is swapped for a model with a different cosine noise floor. | user+agent:claude-code | 1.00 | 2026-05-14 02:41:41 | no |
| test-command | cargo test | user+agent:claude-code | 1.00 | 2026-05-13 20:55:38 | no |
| viz.theme | Dark mode with neon purples. Aim for visually polished, not utilitarian — this is something the user looks at, not just a debug surface. Synthwave/cyberpunk-adjacent without being noisy; the embedding map should feel like a constellation, not a scatter plot. | user+agent:claude-code | 1.00 | 2026-05-14 03:30:13 | no |

## Recent activity (last 30 days)

| When | Actor | Table | Action | Reason |
|------|-------|-------|--------|--------|
| 2026-05-14 03:30:23 | claude:planning | session_notes | insert | mcp log_session_note |
| 2026-05-14 03:30:13 | claude:planning | facts | insert | fact add |
| 2026-05-14 03:30:04 | claude:planning | decisions | insert | decision add |
| 2026-05-14 03:29:54 | claude:planning | decisions | insert | decision add |
| 2026-05-14 03:29:45 | claude:planning | decisions | insert | decision add |
| 2026-05-14 03:29:36 | claude:planning | decisions | insert | decision add |
| 2026-05-14 03:29:27 | claude:planning | decisions | insert | decision add |
| 2026-05-14 03:29:17 | claude:planning | decisions | insert | decision add |
| 2026-05-14 03:15:08 | cli:user | tasks | insert | task add |
| 2026-05-14 03:15:04 | cli:user | tasks | insert | task add |
| 2026-05-14 02:46:50 | cli:user | render | render | memhub render |
| 2026-05-14 02:46:44 | claude:wrap-up | project_arch | insert | arch set |
| 2026-05-14 02:46:38 | claude:wrap-up | session_notes | insert | mcp log_session_note |
| 2026-05-14 02:46:32 | claude:wrap-up | decisions | insert | decision add |
| 2026-05-14 02:46:23 | claude:wrap-up | project_state | insert | state set |
| 2026-05-14 02:41:41 | cli:claude-code | facts | insert | fact add |
| 2026-05-14 02:39:25 | cli:user | tasks | update | task done |
| 2026-05-14 00:37:54 | cli:user | render | render | memhub render |
| 2026-05-14 00:37:46 | codex:wrap-up | project_arch | insert | arch set |
| 2026-05-14 00:37:40 | codex:wrap-up | session_notes | insert | mcp log_session_note |
| 2026-05-14 00:37:33 | codex:wrap-up | facts | insert | fact add |
| 2026-05-14 00:37:22 | codex:wrap-up | decisions | insert | decision add |
| 2026-05-14 00:37:15 | codex:wrap-up | decisions | insert | decision add |
| 2026-05-14 00:37:09 | codex:wrap-up | decisions | insert | decision add |
| 2026-05-14 00:36:59 | codex:wrap-up | project_state | insert | state set |
| 2026-05-13 23:58:52 | codex:configure-hybrid | embeddings | rebuild | index rebuild: model=bge-small-en-v1.5 facts=2 decisions=53 tasks=18 |
| 2026-05-13 23:19:16 | cli:user | render | render | memhub render |
| 2026-05-13 23:19:12 | claude:wrap-up | project_arch | insert | arch set |
| 2026-05-13 23:19:11 | claude:wrap-up | session_notes | insert | mcp log_session_note |
| 2026-05-13 23:19:02 | claude:wrap-up | tasks | insert | task add |
| 2026-05-13 23:18:59 | claude:wrap-up | tasks | update | task done |
| 2026-05-13 23:18:54 | claude:wrap-up | decisions | insert | decision add |
| 2026-05-13 23:18:51 | claude:wrap-up | decisions | insert | decision add |
| 2026-05-13 23:18:48 | claude:wrap-up | decisions | insert | decision add |
| 2026-05-13 23:18:41 | claude:wrap-up | project_state | insert | state set |
| 2026-05-13 22:23:41 | cli:user | render | render | memhub render |
| 2026-05-13 22:23:38 | claude:wrap-up | project_arch | insert | arch set |
| 2026-05-13 22:22:39 | claude:wrap-up | session_notes | insert | mcp log_session_note |
| 2026-05-13 22:22:32 | claude:wrap-up | tasks | update | task done |
| 2026-05-13 22:22:32 | claude:wrap-up | tasks | update | task done |
| 2026-05-13 22:22:28 | claude:wrap-up | decisions | insert | decision add |
| 2026-05-13 22:22:20 | claude:wrap-up | decisions | insert | decision add |
| 2026-05-13 22:22:12 | claude:wrap-up | decisions | insert | decision add |
| 2026-05-13 22:22:06 | claude:wrap-up | decisions | insert | decision add |
| 2026-05-13 22:21:59 | claude:wrap-up | project_state | insert | state set |
| 2026-05-13 22:21:04 | claude:wrap-up | project_state | insert | state set |
| 2026-05-13 21:55:37 | cli:user | render | render | memhub render |
| 2026-05-13 21:55:37 | claude:wrap-up | tasks | update | task done |
| 2026-05-13 21:50:25 | cli:user | render | render | memhub render |
| 2026-05-13 21:50:12 | claude:wrap-up | project_arch | insert | arch set |
