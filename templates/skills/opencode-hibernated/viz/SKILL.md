---
name: viz
description: Launch the memhub dashboard from OpenCode; use when the user asks for the local visual memory dashboard.
compatibility: opencode
---

# Skill: viz

Launch the read-only memhub visualization dashboard.

Workflow:
- Run `memhub viz` from the repo.
- Report the local URL if printed.
- If launch fails, surface stderr and suggest checking that the repo has `.memhub/` and that the binary is current.
