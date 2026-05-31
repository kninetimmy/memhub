---
name: recall
description: Recall memhub project memory in OpenCode; use when project facts, decisions, tasks, or docs are needed mid-session. Trigger on: "what did we decide about X", "is there a fact/decision/task about Y", "recall X", "what do we know about Z", "look this up in memhub".
compatibility: opencode
---

# Skill: recall

Ask memhub for focused context instead of reading the full ledger.

Workflow:
- Prefer the MCP tool `memhub.recall` when available; otherwise run `memhub recall "$ARGUMENTS"` from the repo.
- Use source-type filters only when the user asks for docs/facts/decisions/tasks specifically.
- If recall returns stale-embedding warnings, surface the warning and ask before running `/reindex`.
- If recall is empty, say that clearly and only fall back to rendered files when there is a concrete reason.
