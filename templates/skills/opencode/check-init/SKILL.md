---
name: check-init
description: Check memhub project health in OpenCode; use when the user asks whether memhub is initialized or healthy.
compatibility: opencode
---

# Skill: check-init

Run the read-only memhub health check for the current repo. Do not write or repair anything.

Workflow:
- Verify `.memhub/` exists and `memhub` is on PATH.
- Run `memhub doctor --json` and parse the wrapped `{"doctor": {...}}` object (`overall`, `exit_code`, `counts`, `checks[]` — each `{id, group, status, message, detail?}`).
- Report Green (`overall: ok`), Yellow (`overall: warn` — list each warn check and its message/fix), or Red (`overall: error`, or `.memhub`/binary missing) with concrete next action.
- Fall back to `memhub status` if the installed binary predates `doctor` (issue #21).
- Never run `memhub render`, `memhub doctor --strict`, or mutate files from this skill.
