---
name: eval-recall
description: Run the retrieval golden-set evaluation and interpret Recall@K or compare two runs.
---

Run `memhub eval retrieval --json` against the project's golden queries. Report
recall by cutoff and the specific misses; a missing corpus or golden set is
not a pass. To compare a baseline and candidate, save each JSON result and run
`memhub eval compare <baseline.json> <candidate.json>`. Use the same corpus and
queries for both and distinguish missing/new query IDs from rank changes. Do
not write memory or change retrieval settings to make a result pass.
