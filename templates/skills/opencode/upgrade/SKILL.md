---
name: upgrade
description: Upgrade memhub and resync agent wrappers from OpenCode; use after code changes or stale installed skills.
compatibility: opencode
---

# Skill: upgrade

Run the machine-wide memhub upgrade flow from the memhub source repo.

Workflow:
- Confirm the current repo is the memhub source repo.
- Run `memhub upgrade --dry-run` first unless the user already requested a real upgrade.
- For real upgrades, run `memhub upgrade` and report binary, DB, GC, and skill/command sync results.
- OpenCode sync covers `~/.config/opencode/skills/` and `~/.config/opencode/commands/` when those directories already exist.
- `memhub upgrade` also installs the session-start hook for Claude Code and Codex (not OpenCode); `--no-hooks` skips it. Codex runs the hook only after a one-time approval in its `/hooks` screen, so tell the user to approve it there.
- Do not commit anything.
