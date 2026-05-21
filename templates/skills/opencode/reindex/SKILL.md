---
name: reindex
description: Rebuild memhub embeddings in OpenCode; use after stale-embedding warnings or embedding model changes.
compatibility: opencode
---

# Skill: reindex

Rebuild memhub embeddings only after user approval.

Workflow:
- Ask before running; do not reindex automatically.
- Run `memhub index rebuild --actor opencode:reindex`.
- Then run `memhub index status` and report missing/stale counts.
- If the command fails, surface stderr and stop.
