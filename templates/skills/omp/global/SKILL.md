---
name: global
description: Inspect machine-global memory, manage repo opt-in, or propose a global fact or decision for review.
---

Run `memhub global status --json` first. Global memory is off per repo by
default and shares one machine-local store. Only enable or disable it with
explicit user approval (`memhub global enable` / `memhub global disable`).
Disable retains the store. Keep facts, decisions, and docs repo-scoped unless
the user explicitly says they apply across repos.

For an agent-mediated global fact or decision, use MCP `propose_fact` or
`propose_decision` with `global=true`. It stays in the repo's review queue;
only the user's approval through `memhub review accept <id>` makes it durable,
while global memory is enabled. Never fall back to direct global writes. A
human may type direct global commands, including global docs; there is no
agent-mediated global doc add. Label recall hits by scope and prefer repo
memory on a conflict.
