---
name: audit-md
description: Audit CLAUDE.md or AGENTS.md for drift, size, and managed-block problems.
---

From the project root run `memhub audit md --json`. Report each finding with
its file, evidence, and smallest fix. Distinguish an unavailable audit from a
clean result. If generated AGENTS.md drifted, recommend updating its source and
running the repo's generator. This skill only reports; do not edit the files.
