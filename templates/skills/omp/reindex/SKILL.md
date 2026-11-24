---
name: reindex
description: Rebuild memhub embeddings after a model change or missing/stale embedding warning, with approval.
---

From the initialized repo run `memhub index status --json`. Show the reason,
model, and counts. Explain that the rebuild touches only embeddings and may
also rebuild the global store when this repo is opted in. Wait for an
explicit yes, then run `memhub index rebuild --actor omp:reindex --json`.
Report counts and any global error, and confirm the remaining missing rows
with `memhub index status`. Recall remains usable while waiting for approval.
