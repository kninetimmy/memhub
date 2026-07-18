# memhub — Codex CLI instructions

Local-first Rust CLI for durable per-repo project memory shared between Codex, Claude Code, and OpenCode. Treat [docs/reference/memhub-prd.md](docs/reference/memhub-prd.md) as the product authority and do not silently diverge from it.

Operational detail for every subsystem — retrieval, token accounting, doc ingestion, the code index, machine-global memory, cross-machine workflow and Drive sync, and machine-wide upgrade/GC — now lives in [docs/reference/operations.md](docs/reference/operations.md) and is memhub-recall-searchable. Recall it on demand rather than loading it every session; this file keeps only what an agent needs inline from turn one.

<!-- memhub:managed-block v=1 -->
memhub-primary: true
db: .memhub/project.sqlite
rendered: .memhub/rendered/
config: .memhub/config.toml
<!-- /memhub:managed-block -->

This file is the Codex / OpenCode counterpart to `CLAUDE.md`, and is **generated** from it by `generate_agents_md` — do not hand-edit it; edit `CLAUDE.md` and regenerate with `MEMHUB_REGEN=1 cargo test skill_parity`. The two exist so Codex CLI, OpenCode CLI, and Claude Code sessions get the same orientation when they open this repo; where they diverge it is intentional (a different H1, plus the Codex/OpenCode-only sections injected at the end).

## Session Continuity

This repo is memhub-primary as of M7-002 (2026-05-13). The DB at
`.memhub/project.sqlite` is the source of truth; rendered markdown is
the local human-readable view under `.memhub/rendered/`. At session
start, read `.memhub/rendered/PROJECT.md` if present for the
"currently building / next up / open questions" state, the
architecture narrative, and recent session notes; if it is missing,
fall back to `memhub recall` / `memhub status`.

The mid-session routing rules — prefer recall over the
`PROJECT_LEDGER.md` ledger, and the turn-1-only PROJECT.md read —
live in the memhub MCP server's own instructions (`src/mcp/mod.rs`)
and are not duplicated here. (Re-rendering after mid-session DB writes
is a `/wrap-up` step, not an MCP-instructions rule.)

If recall returns a `warnings[].kind == "stale_embeddings"` entry,
surface it and ask the user before invoking `/reindex`. Recall
results stay usable in the meantime — the warning means hybrid
scoring may be undercounting some rows, not that retrieval is
broken.

The K9 integration was removed entirely (task 123, PR #165,
2026-07-18); nothing in memhub detects, reads, or configures K9
anymore. The four legacy K9 files (`agent_docs/project_state.md`,
`project_arch.md`, `project_decisions.md`, `project_backlog.md`) are
inert historical artifacts — last accurate at commit `366cc1c`. Do
not write to them; ignore them.

## Project Guardrails

- Local-first, offline-capable, and intentionally boring.
- Milestone 1 stays lean: CLI, DB, migrations, config, logging, and real CRUD for facts, decisions, and tasks.
- Agents are untrusted writers; do not promote agent claims to durable truth without a concrete signal or explicit user action.
- Prefer narrow milestones and explicit TODOs over speculative subsystems.
- Do not pretend MCP, markdown sync, git ingestion, routing, or confidence decay are implemented before they exist.
- Keep `docs/reference/memhub-prd.md` verbatim.

<!-- BEGIN MANAGED: delegation-policy (per-repo; delete this whole block to revert) -->
## Delegation (this repo)

Scoped exception to the global Subagent Dispatch Policy, for memhub only:
once an implementation plan is approved, the main Opus session MAY delegate
implementation work to subagents without re-asking for each spawn. This
license is limited to executing the approved plan — it is not open season.

- **Default to Sonnet.** Delegate implementation tasks to the `implementer`
  agent (Sonnet). Use `cargo-test-runner` (Sonnet) for test runs/diagnosis.
- **Escalate a subtask to Opus** (spawn with `model: opus`) only when it needs
  cross-file architectural reasoning, subtle concurrency/correctness work, or
  it already failed once on Sonnet. Otherwise stay on Sonnet.
- **Main thread keeps** architecture/design decisions and anything outside the
  approved plan. Subagents execute; they don't decide scope.
- Outside an approved plan, the global Subagent Dispatch Policy applies as
  written (ask before spawning for multi-file/architectural work).
<!-- END MANAGED: delegation-policy -->

## Safety gates

Two agent-facing gates never move out of this file, because acting without them is destructive or misleading:

- **`stale_embeddings` (recall) — kept in Session Continuity above.** If `memhub recall` returns a `warnings[].kind == "stale_embeddings"` entry, surface it and ask the user before invoking `/reindex`. Recall stays usable meanwhile; scoring may undercount some rows, retrieval is not broken.
- **`sync_adopt` (Drive sync).** The MCP `memhub.sync_adopt` tool overwrites the local DB — the one destructive sync op — so without `confirm=true` it returns the would-change verdict and refuses. Surface that verdict to the user and only re-call with `confirm=true` after they approve. Hard refusals regardless of confirm: project-id mismatch, a snapshot schema newer than this binary (run `memhub upgrade`), or a checksum that disagrees with the manifest. Full Drive-sync model and surfaces: [docs/reference/operations.md](docs/reference/operations.md).

## Project state

Current project state (active tasks, durable decisions, known quirks)
is machine-local and lives in `.memhub/rendered/PROJECT.md`. Use
`memhub recall` mid-session and `memhub render` to refresh the local
view. Nothing under this section is committed to git — each machine
maintains its own DB and its own rendered view.

## Build / Test / Run

```bash
cargo build
cargo test
cargo run -- init
cargo run -- status
```

## Agent attribution (Codex-specific)

When you write to memhub from the CLI, identify yourself so the row gets attributed correctly. Two flags matter:

- `--source` — origin of the claim. Pass `--source user+agent:codex` on `memhub fact add` / `memhub decision add` writes that go through `/wrap-up` (agent surfaced, user approved). For direct CLI writes the user typed themselves, omit the flag and take the `user` default.
- `--actor` — who performed the write. Pass `--actor codex:wrap-up` from the wrap-up skill; `--actor codex:<skill-name>` from other skills.

See [docs/reference/memhub-prd-source-vocabulary-addendum.md](docs/reference/memhub-prd-source-vocabulary-addendum.md) for the full vocabulary (`user`, `agent:<id>`, `user+agent:<id>`, `git`, `observed`).

Once `memhub serve` is registered in `~/.codex/config.toml` (a per-machine step — see README's "Register the MCP server"; `memhub doctor` reports current status), writes via MCP attribute automatically — the server reads `clientInfo.name` from `initialize` and tags writes as `codex` / `codex:wrap-up` without you needing to pass anything.

## memhub routing (Codex / OpenCode)
memhub is this repo's project memory. When intent matches, use the memhub MCP tools — do
not fall through to Grep/Read/manual scan:
- past decisions / status / "is there a fact/task about X" → `recall`
- find code by what it does / "where is X" → `locate`
- session start (turn 1 only) → read `.memhub/rendered/PROJECT.md` once
- new task / mark done → `task_add` / `task_done`; ingest a spec doc → `doc_add`
Never Grep for code by intent before `locate`. Never read `PROJECT_LEDGER.md` before `recall`
(it is the fallback). Never write facts/decisions directly — stage via `propose_fact` /
`propose_decision`, durable on `memhub review accept`.
(Carrier note: Claude Code gets these rules from the MCP `instructions` field; this block is
the Codex/OpenCode fallback pending the Wave 4 delivery spike.)

<!-- orchestrator:managed:start version=1 -->
This file is partially managed by Orch (see `.orchestrator/config.toml`).
- In **Assist** mode, tracked-file changes are mechanically denied; a mutating
  request triggers read-only planning instead.
- In **Delivery** mode, work happens in an isolated per-issue worktree, never in
  this checkout directly.
- Model/effort routing, concurrency, and host plugin setup live in
  `.orchestrator/config.toml` — edit that file, not this block.
- Orch upgrades this block through Delivery. Do not hand-edit it; a hand edit
  blocks the next install/upgrade until reverted or removed.
<!-- orchestrator:managed:end -->
