---
name: metrics
description: Show memhub token metrics in OpenCode; use when the user asks about recall/session token accounting.
compatibility: opencode
---

# Skill: metrics

Display memhub's token-accounting dashboard.

Workflow:
- Prefer the MCP tool `memhub.metrics` when available; otherwise run `memhub metrics status`.
- Print the rendered dashboard panel verbatim when provided.
- Explain that session accounting currently depends on supported transcript scrapers; recall proxy rows still work independently.
- Do not enable or disable metrics unless the user explicitly asks.
