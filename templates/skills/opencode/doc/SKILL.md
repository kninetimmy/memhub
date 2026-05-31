---
name: doc
description: Ingest markdown reference docs into memhub from OpenCode; use for design specs, API contracts, and guides. Trigger on: "ingest this doc", "add this spec to memhub", "make this doc searchable", "remember this reference doc", "/doc".
compatibility: opencode
---

# Skill: doc

Ingest a user-pointed markdown reference document so recall can retrieve it.

Workflow:
- Confirm the path is a markdown file the user wants indexed.
- Run `memhub doc add "<path>" --json` for repo docs unless the user explicitly asks for global docs.
- Report created/updated/unchanged status and chunk count.
- Mention that docs are re-ingestable cache and are not exported by `memhub export`.
