---
name: locate
description: Find where a feature, symbol, or behavior lives in this checkout before reading or editing it.
---

For code by intent, use the attached memhub MCP server's `locate` with the
behavior as a natural-language query. If MCP is unavailable, use
`memhub locate "<query>" --json`. Use the ranked path and line ranges as
breadcrumbs, then inspect the actual file. Check checkout binding, tracked
file coverage, indexed HEAD, and file hashes/freshness when present; the
index is not a snapshot of dirty or untracked code. If the result is empty,
try more specific wording and name any fallback or uncovered path. Use
`/skill:recall` instead for project history and prior decisions.
