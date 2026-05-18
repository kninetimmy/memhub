# Milestones

## Current Scaffold

This repository covers PRD §16 Milestones 1–4 in full, plus the memhub side of Milestone 5 K9 Claude Framework interop:

- Rust CLI scaffold
- SQLite schema and migrations
- Config loading and persistence (including deny-list and `[integrations.k9]`)
- Logging and error handling
- `init`, `status`, `sync-md`, `ingest-git`, `search`, `fact add|list`, `decision add|list`, `task add|list|done`, `command list|verify`, `export`, `import`, `review list|show|accept|reject|expire`, `integrations status|enable-k9|disable-k9|check-k9`
- Git ingestion into `commits`, `files`, and `commit_files` with path-based deny-list filtering
- FTS-backed decision search plus exact indexed file-history lookup, deny-list filtered
- Managed-block generation for `AGENTS.md` and `CLAUDE.md`
- Local stdio MCP server through `memhub serve`
- Thin MCP tools for status, search, task listing, recent decisions, latest command lookup, explicit verified command recording, staged fact/decision proposals, and read-only `list_pending_writes`
- Audit logging for writes with `--actor` attribution on every K9-targeted mutating command
- Portable version-tagged JSON `memhub export` and `memhub import` with `--force` overwrite, id preservation, FTS rebuild, and `sync-md` after restore
- Missing-DB safety with `memhub init --from-backup <path>` single-step recovery
- Derived per-command confidence and 90-day fact staleness flag
- v1 K9 `/wrap-up` shell-out contract with `--json` on every read and mutating command K9 needs

## Milestone 2: Git + Search

- Add git ingestion into `commits`, `commit_files`, and `files`
- Add FTS-backed text chunks
- Add a rule-based search path from the CLI
- Add query-plan-aware tests for hot queries
- Status: complete

## Milestone 3: MCP + Markdown Sync

- Add an MCP server with thin wrappers over read/write services
- Add explicit markdown managed-block sync for `AGENTS.md` and `CLAUDE.md`
- Start enforcing write-back policy boundaries for agent-originated data
- Status: complete under the narrowed repo plan; staged proposal writes, client alias normalization, and the `memhub review` promotion flow all shipped (the latter in Milestone 4)

## Milestone 4: Trust and Maintenance

- Portable export/import as the supported repo backup and restore path - shipped in `M4-001`
- Readable README backup/restore instructions - shipped with `M4-001`
- Missing-DB safety handling and `memhub init --from-backup <path>` single-step recovery - shipped in `M4-002`
- Review queue flows (`memhub review list|show|accept|reject|expire` plus MCP `list_pending_writes`) - shipped in `M4-003`
- Path-based deny-list enforcement for sensitive files - shipped in `M4-004`
- Confidence scoring and stale data handling (derived command confidence, 90-day fact staleness flag) - shipped in `M4-005`
- Status: complete

## Milestone 5: K9 Claude Framework Interop

- K9 detection on `memhub init`, plus `memhub integrations status|enable-k9|disable-k9` - shipped in `M5-001`
- v1 K9 `/wrap-up` contract with `memhub integrations check-k9` gate, plus `--json` / `--actor` on every mutating command K9 needs - shipped in `M5-002`
- `--json` read surfaces on `memhub review list` and `memhub review show` - shipped in `M5-003`
- Status: memhub side complete. The K9-repo `/wrap-up.md` consumer edit that calls into the v1 contract end-to-end lives outside this repo.

## Milestone 8: SQL+RAG hybrid recall

- Design anchor: [`docs/reference/memhub-prd-addendum-m8-retrieval.md`](../reference/memhub-prd-addendum-m8-retrieval.md)
- Status: shipped. Hybrid FTS5 + BGE-small embeddings + cross-encoder rerank, `memhub recall` / `memhub.recall`, eval harness over `tests/retrieval_golden.json`.

## Milestone 9: Machine-global memory

- Design anchor: [`docs/reference/memhub-prd-addendum-m9-machine-global-memory.md`](../reference/memhub-prd-addendum-m9-machine-global-memory.md)
- Optional, off-by-default machine-global store at `~/.memhub/global.sqlite`, merged into per-repo recall with `scope` provenance. User-gated writes only; agents may propose but never write global. Origin: task 45.

## Milestone 10+

Speculative until a mini-PRD exists per feature: continuous confidence decay, `memhub.log_session_note` MCP tool, `memhub stats` success-metric command, broader indexed retrieval over command history, desktop UI, file watchers, and network-backed ingestion.
