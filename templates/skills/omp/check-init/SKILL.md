---
name: check-init
description: Check memhub project health, schema, and OMP registration or skill presence.
---

From the project, run `memhub doctor --json`. Report material findings by
check ID and distinguish skipped checks from passes. The OMP rows are static
inspection, not proof of discovery or connectivity. If the project isn't
initialized, suggest `/skill:init-project`; if the binary is unavailable,
report that instead. Do not repair or reconfigure anything from this skill.
