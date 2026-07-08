---
name: locate
description: >
  Locate code in this repo by intent in OpenCode; use when you need to find where a symbol, function, or behavior lives before reading or editing it. Trigger on: "where is X", "where does X live", "find the code that does Y", "which file handles Z", "locate X", "where do I change Y".
compatibility: opencode
---

# Skill: locate

Ask memhub *where code is* instead of grepping the whole tree. Returns
ranked `file:line` breadcrumbs with clipped snippets — read-only,
never the full file.

Workflow:
- Prefer the MCP tool `memhub.locate` when available; otherwise run `memhub locate "$ARGUMENTS" --json` from the repo.
- The index lazily refreshes to the working tree on each call — no manual `memhub code index` needed.
- Use `limit=N` to widen/narrow. Leave `rerank` off (default) unless explicitly asked to A/B the cross-encoder.
- CLI only: `--no-refresh` skips the refresh (no `git ls-files`/stat/`rev-parse`) for fast warm repeat calls, but is stale-by-choice — only use it in a tight loop over a known-warm index. Default (no flag) refreshes every call.
- Cite hits as `path:start-end` and open the file with your own tools to read the rest.
- If results are empty, say so and quote the query; do not invent a location.
