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
- Scan lightly for README, stack markers, CI, `AGENTS.md`, `CLAUDE.md`, K9 `agent_docs/`, and other context systems.
- Interview for project name/purpose, stack, build/test/run commands, and constraints only when not already obvious.
- Draft separate state and architecture bodies; wait for explicit approval before writing.
- Run `memhub init --json`, then write approved bodies with `memhub state set --from-file ... --actor opencode:init-project --json` and `memhub arch set --from-file ... --actor opencode:init-project --json`.
- Run `memhub render` and report generated paths.
- If no `AGENTS.md` exists, offer a minimal one pointing at `.memhub/rendered/`; wait for approval before writing.
