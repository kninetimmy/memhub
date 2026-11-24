---
name: doc
description: Ingest, list, show, or remove memhub reference documents such as specs and guides.
---

Use the attached memhub MCP tool `doc_add` for a user-requested Markdown file,
or `memhub doc add "<path>" --json --actor omp:doc`. Report the document ID,
chunk count, and whether it was unchanged. A changed document replaces its
chunks; an archived one must be explicitly restored first. Respect path and
deny-list failures. For lookup and review use `memhub doc ls --json` and
`memhub doc show <id-or-path> --json`. For explicitly approved removal use
`memhub doc rm <id-or-path> --json --actor omp:doc`. A global write is a
human-typed operation; keep agent ingestion repo-scoped. Retrieve ingested
text through recall, using `source_types=["doc"]` when the intent is doc-only.
