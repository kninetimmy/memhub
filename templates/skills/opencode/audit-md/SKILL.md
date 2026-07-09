---
name: audit-md
description: >
  Run and interpret memhub's read-only CLAUDE.md/AGENTS.md drift-and-bloat audit in OpenCode; use when the user asks whether CLAUDE.md is bloated, drifted, or out of date. Trigger on: "audit CLAUDE.md", "check for CLAUDE.md drift", "is CLAUDE.md bloated", "run the md audit", "/audit-md".
compatibility: opencode
---

# Skill: audit-md

Run `memhub audit md --json` and add the judgment layer on top of its
raw findings. Read-only — never edit `CLAUDE.md`, `AGENTS.md`, or any
other file; report and recommend only.

Workflow:
- Verify `.memhub/` exists and `memhub` is on PATH (as `/check-init` does).
- Run `memhub audit md --json` and parse the wrapped `{"audit_md": {...}}` object (`exit_code`, `count`, `findings[]` — each `{id, severity, message, detail?}`).
- Clean (`count: 0`): report "No findings — CLAUDE.md/AGENTS.md are in good shape" and stop.
- Otherwise, for each finding recommend a concrete fix by `id`:
  `claude_md_size` → move prose into `docs/reference/operations.md`;
  `agents_md_drift` → regenerate with `MEMHUB_REGEN=1 cargo test skill_parity` and commit, never hand-edit `AGENTS.md`;
  `claude_md_malformed` → restore the `# memhub` H1 / `## ` section structure;
  `managed_block_missing` / `managed_block_version` → add or update the versioned `memhub:managed-block` (see `src/managed_block.rs`);
  `keystone_phrases` → restore the exact phrases listed in `detail`;
  `claude_md_missing` → check the repo root or run `memhub init`;
  `user_md_size` / `user_md_unreadable` → trim or fix the opt-in `[audit] user_md_path` file;
  any other id → report `message`/`detail` verbatim rather than guessing (issue #32 owns the check set, which can grow independently of this skill).
- Group findings by `severity` (`error` first, then `warn`) and close with a one-line summary; remind the user no fix is applied automatically.
- Never invent severities, never pass `--strict`, never write to any file from this skill.
