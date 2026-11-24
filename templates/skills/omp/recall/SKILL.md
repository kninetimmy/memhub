---
name: recall
description: Find past memhub facts, decisions, tasks, architecture, and reference docs by intent.
---

Turn one only: read `.memhub/rendered/PROJECT.md` if present and not already
in context. Mid-session query the attached memhub MCP server's `recall` instead
of opening the ledger; if MCP is unavailable use
`memhub recall "<natural-language question>" --json`. Use `get_row` before
relying on a truncated hit. Report scope, IDs, and any stale or superseded
status. Try variants when wording is uncertain; use explicit doc or note
scope where appropriate (notes are not in default recall). On
`stale_embeddings` or `missing_embeddings`, surface the warning and ask before
running `/skill:reindex`. An empty bundle is not evidence that nothing was
recorded. Use `/skill:locate` for where code lives.
