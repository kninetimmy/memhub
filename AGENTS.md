# memhub — Codex / OpenCode / Antigravity instructions

Local-first Rust CLI for durable per-repo project memory shared between Codex, Claude Code, OpenCode, and Antigravity. Treat [docs/reference/memhub-prd.md](docs/reference/memhub-prd.md) as the product authority and do not silently diverge from it.

Operational detail for every subsystem — retrieval, token accounting, doc ingestion, the code index, machine-global memory, cross-machine workflow and Drive sync, and machine-wide upgrade/GC — now lives in [docs/reference/operations.md](docs/reference/operations.md) and is memhub-recall-searchable. Recall it on demand rather than loading it every session; this file keeps only what an agent needs inline from turn one.

<!-- memhub:managed-block v=1 -->
memhub-primary: true
db: .memhub/project.sqlite
rendered: .memhub/rendered/
config: .memhub/config.toml
<!-- /memhub:managed-block -->

This file is the Codex / OpenCode / Antigravity counterpart to `CLAUDE.md`, and is **generated** from it by `generate_agents_md` — do not hand-edit it; edit `CLAUDE.md` and regenerate with `MEMHUB_REGEN=1 cargo test skill_parity`. The two exist so Codex CLI, OpenCode CLI, Antigravity CLI, and Claude Code sessions get the same orientation when they open this repo; where they diverge it is intentional (a different H1, plus the Codex/OpenCode/Antigravity-only sections injected at the end).

## Session Continuity

This repo is memhub-primary as of M7-002 (2026-05-13). The DB at
`.memhub/project.sqlite` is the source of truth; rendered markdown is
the local human-readable view under `.memhub/rendered/`. At session
start, read `.memhub/rendered/PROJECT.md` if present, unless its frame
(starting `<!-- memhub:rendered -->`) is already in your context from
the session-start hook. It is a compact frame: the "currently
building / next up / open questions" state in full, an architecture
section index, and session note stubs; to keep it within its size
budget the architecture section index and the session note stubs may
be dropped. Pull the architecture text with
`memhub recall` (sections are recall-searchable) or `memhub arch show`,
and full notes with `memhub note list` or note-scoped recall. If
PROJECT.md is missing, fall back to `memhub recall` / `memhub status`.

The mid-session routing rules — prefer recall over the
`PROJECT_LEDGER.md` ledger, and the turn-1-only PROJECT.md read —
live in the memhub MCP server's own instructions (`src/mcp/mod.rs`)
and are not duplicated here. (Re-rendering after mid-session DB writes
is a `/wrap-up` step, not an MCP-instructions rule.)

If recall returns a `warnings[].kind == "stale_embeddings"` or
`"missing_embeddings"` entry, surface it and ask the user before
invoking `/reindex`. Recall results stay usable in the meantime —
the warning means hybrid scoring may be undercounting some rows,
not that retrieval is broken.

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

memhub is Orch-managed, so delegation follows Orch and this repo adds no
exception of its own:

- **Spawn only Orch's `orch-*` role agents.** Read-only exploration goes to
  `orch-scout`.
- **Tracked-file changes ship only through a Delivery run** whose plan gate the
  user approved. The engine routes each issue's executor and reviewer from
  `.orchestrator/config.toml`; never choose or override a subagent's model
  yourself.
- **The main session keeps** design decisions, scope, and every memhub write.
  Subagents execute and report back; they never write to memhub.
<!-- END MANAGED: delegation-policy -->

## Safety gates

Two agent-facing gates never move out of this file, because acting without them is destructive or misleading:

- **`stale_embeddings` / `missing_embeddings` (recall) — kept in Session Continuity above.** If `memhub recall` returns a `warnings[].kind == "stale_embeddings"` or `"missing_embeddings"` entry, surface it and ask the user before invoking `/reindex`. Recall stays usable meanwhile; scoring may undercount some rows, retrieval is not broken.
- **`sync_adopt` (Drive sync).** The MCP `memhub.sync_adopt` tool overwrites the local DB — the one destructive sync op — so without `confirm=true` it returns the would-change verdict and refuses. Surface that verdict to the user and only re-call with `confirm=true` after they approve. Hard refusals regardless of confirm: project-id mismatch, a snapshot schema newer than this binary (run `memhub upgrade`), or a checksum that disagrees with the manifest. Full Drive-sync model and surfaces: [docs/reference/operations.md](docs/reference/operations.md).

## Project state

Current project state is machine-local. `.memhub/rendered/PROJECT.md`
is the compact frame Session Continuity describes; for tasks, decisions
and facts use `memhub recall`. Run `memhub render` to refresh the local
view. Nothing under this section is committed to git — each machine
maintains its own DB and its own rendered view.

## Build / Test / Run

```bash
cargo build
cargo test
cargo run -- init
cargo run -- status
```

## Agent attribution (Codex / OpenCode / Antigravity)

When you write to memhub from the CLI, identify yourself so the row gets attributed correctly. Two flags matter:

- `--source` — origin of the claim. Pass `--source user+agent:<host>` on `memhub fact add` / `memhub decision add` writes that go through `/wrap-up` (agent surfaced, user approved). For direct CLI writes the user typed themselves, omit the flag and take the `user` default.
- `--actor` — who performed the write. Pass `--actor <host>:wrap-up` from the wrap-up skill; `--actor <host>:<skill-name>` from other skills.

Use your own host's ids (never another host's):

| Host | `--source` | `--actor` (wrap-up) |
|---|---|---|
| Codex | `user+agent:codex` | `codex:wrap-up` |
| OpenCode | `user+agent:opencode` | `opencode:wrap-up` |
| Antigravity | `user+agent:antigravity` | `antigravity:wrap-up` |

See [docs/reference/memhub-prd-source-vocabulary-addendum.md](docs/reference/memhub-prd-source-vocabulary-addendum.md) for the full vocabulary (`user`, `agent:<id>`, `user+agent:<id>`, `git`, `observed`).

Once `memhub serve` is registered with your host (Codex: `~/.codex/config.toml`; Antigravity: `agy mcp add memhub memhub serve`; a per-machine step — see README's "Register the MCP server"; `memhub doctor` reports current status), writes via MCP attribute automatically — the server reads `clientInfo.name` from `initialize` and tags writes with the bare host name (`codex` / `opencode` / `antigravity`) without you needing to pass anything.

## memhub routing (Codex / OpenCode / Antigravity)
memhub is this repo's project memory. When intent matches, use the memhub MCP tools — do
not fall through to Grep/Read/manual scan:
- past decisions / status / "is there a fact/task about X" → `recall`
- find code by what it does / "where is X" → `locate`
- session start (turn 1 only) → read `.memhub/rendered/PROJECT.md` once, unless its frame
  (starting `<!-- memhub:rendered -->`) is already in your context from the session-start hook
  (it may drop the architecture section index and the session note stubs to keep it within its size budget)
- new task / mark done → `task_add` / `task_done`; ingest a spec doc → `doc_add`
Never Grep for code by intent before `locate`. Never read `PROJECT_LEDGER.md` before `recall`
(it is the fallback). Never write facts/decisions directly — stage via `propose_fact` /
`propose_decision`, durable on `memhub review accept`.
(Carrier note: Claude Code gets these rules from the MCP `instructions` field; this block is
the Codex/OpenCode/Antigravity fallback pending the Wave 4 delivery spike.)

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
