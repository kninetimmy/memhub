---
name: init-project
description: Initialize memhub in a repo, seed approved project and architecture narratives, and render project memory.
---

Work from the intended repo root. If it's already initialized, report that
and suggest `/skill:check-init`. Inspect existing project instructions and
memory systems before proposing new ones; leave them intact. Ask for the
project purpose, stack, common commands, and near-term goals. Draft a short
current state and a separate architecture overview with headings. Present
both drafts and get approval before the first write; honor an explicit
request to skip architecture.

Run `memhub init --json` and check its exit status. For each approved body, use
`memhub state set --from-file <path> --json --actor omp:init-project` or
`memhub arch set --from-file <path> --json --actor omp:init-project`. Stop on
failure. Run `memhub render --actor omp:init-project` and verify the two
rendered paths. Ask separately before enabling global memory, sync, or
changing retrieval settings. Don't edit host MCP configuration. Point to
`/skill:wrap-up` and `/skill:catch-up` for session boundaries.
