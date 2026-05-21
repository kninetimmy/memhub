---
name: check-init
description: Check memhub project health in OpenCode; use when the user asks whether memhub is initialized or healthy.
compatibility: opencode
---

# Skill: check-init

Run the read-only memhub health check for the current repo. Do not write or repair anything.

Workflow:
- Verify `.memhub/` exists and `memhub` is on PATH.
- Run `memhub status` and prefer JSON output if the installed binary supports it.
- Verify `.memhub/rendered/PROJECT.md` and `.memhub/rendered/PROJECT_LEDGER.md` exist, start with `<!-- memhub:rendered -->`, and are not obviously stale.
- Check pending writes with `memhub review list --status pending --json` and recent activity with `memhub stats --window 7d --json`.
- Report Green, Yellow, or Red with concrete next action. Never run `memhub render` or mutate files from this skill.
