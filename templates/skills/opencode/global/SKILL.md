---
name: global
description: Manage memhub machine-global memory in OpenCode; use for cross-repo facts, decisions, and docs.
compatibility: opencode
---

# Skill: global

Manage the optional machine-global memhub store.

Workflow:
- Explain that global memory is off by default and per-repo opt-in.
- Use `memhub global status`, `memhub global enable`, or `memhub global disable` as requested.
- Only route facts, decisions, and docs to global after explicit user approval.
- Never put repo-specific paths, symbols, or tasks into global memory.
- For MCP proposals, use `global=true` only when the user explicitly asks; durable writes still require review acceptance.
