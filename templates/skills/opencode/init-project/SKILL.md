---
name: init-project
description: Initialize memhub for a repo in OpenCode; use when the user asks to bootstrap project memory.
compatibility: opencode
---

# Skill: init-project

Bootstrap memhub in the current repo, with an approval gate before writing starter narratives.

Workflow:
- Stop if `memhub` is absent from PATH or `.memhub/` already exists.
- Confirm the repo root with `git rev-parse --show-toplevel`; ask before initializing anywhere else.
- Scan lightly for README, stack markers, CI, `AGENTS.md`, `CLAUDE.md`, and other context systems.
- Interview for project name/purpose, stack, build/test/run commands, and constraints only when not already obvious.
- Draft separate state and architecture bodies; wait for explicit approval before writing.
- Run `memhub init --json`, then write approved bodies with `memhub state set --from-file ... --actor opencode:init-project --json` and `memhub arch set --from-file ... --actor opencode:init-project --json`.
- Run `memhub render` and report generated paths.
- Offer the runtime toggles one at a time, skipping any the user waves off:
  - Retrieval mode: hybrid recall (`mode = "hybrid"` under `[retrieval]` in `.memhub/config.toml`, then `memhub index rebuild --actor opencode:init-project` to backfill embeddings — a rebuild is required whenever this mode changes, not just the config edit) vs. the FTS-only default (nothing to do).
  - Code index: `memhub code index` to warm up `memhub locate`/`/locate` now instead of on first use.
  - Machine-global memory: off by default, per-repo opt-in via `memhub global enable` for the shared `~/.memhub/global.sqlite` store.
  - Doc ingestion: `memhub doc add "<path>" --json` to make a reference doc's chunks surface in plain recall.
  - Drive sync: off by default; `memhub sync enable` plus `[sync] drive_subpath` in `.memhub/config.toml` pointing at a folder that already syncs, then `memhub sync status` to confirm.
- If no `AGENTS.md` exists, offer a minimal one pointing at `.memhub/rendered/`; wait for approval before writing.
