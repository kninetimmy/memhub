---
name: audit-md
description: >
  Run memhub's read-only CLAUDE.md/AGENTS.md drift-and-bloat audit (`memhub audit md`) and interpret the findings — size, generated-file drift, managed-block version, keystone phrases, opt-in user-global size — recommending concrete fixes without rewriting anything itself. Trigger on: "audit CLAUDE.md", "check for CLAUDE.md drift", "is CLAUDE.md bloated", "run the md audit", "/audit-md".
framework: memhub
framework_version: 1.0.0
last_updated: 2026-07-06
---

Run `memhub audit md` and add the judgment layer on top of its raw
findings: which ones matter, and what to actually do about them.
Read-only — this skill never edits `CLAUDE.md`, `AGENTS.md`, or any
other file. It reports and recommends; a human (or a separate,
explicit follow-up task) makes the edit.

`memhub audit md` (issue #32) is the machine-checkable half of the
token-diet regime — CLI-only, no MCP tool, no DB writes. This skill is
the other half: it does not add new checks (that's `audit md`'s own
job), it interprets the ones that already fired.

## Preconditions

- `.memhub/` exists in the working repo (run `/check-init` if unsure).
- `memhub` binary on PATH.

If a precondition is missing, report it and stop — same as `/check-init`.

## Invocation

CLI only (no MCP tool for this one):

```bash
memhub audit md --json
```

Parse the wrapped `{"audit_md": {...}}` object (Q29 convention, the
same shape family as `doctor`'s `{"doctor": {...}}`):

- `exit_code` — always `0` from a plain `--json` call; this skill never
  passes `--strict`, so exit code carries no signal here. Read
  `findings` directly instead.
- `count` — `findings.len()`, a cheap "anything at all?" check.
- `findings[]` — each `{id, severity, message, detail?}`. `severity` is
  `"warn"` or `"error"` (a display label — every entry is already a
  problem; there is no `"ok"` finding).

## Judgment: known finding ids and their fix

For each finding, look up its `id` below and recommend that fix. Quote
`message` (and `detail`, if present) verbatim so the user sees the
audit's own words, then add the recommendation:

- **`claude_md_size`** — `CLAUDE.md` is over the ~2,500-token target
  (or the 2,600 hard ceiling, if `severity: "error"`). Recommend moving
  subsystem prose into `docs/reference/operations.md` (or another
  referenced doc) rather than trimming content outright — the
  established pattern from the CLAUDE.md token diet (issue #30).
- **`agents_md_drift`** — `AGENTS.md` no longer matches
  `generate_agents_md(CLAUDE.md)`. Recommend regenerating it:
  `MEMHUB_REGEN=1 cargo test --test skill_parity`, then commit the
  updated `AGENTS.md`. Never hand-edit `AGENTS.md` — it's a pure
  derivative (decision Q21).
- **`claude_md_malformed`** — `CLAUDE.md` doesn't start with the
  required `# memhub` H1 or has no `## ` section, so `AGENTS.md`
  can't even be generated/verified. Recommend restoring that structure
  before anything else; every other check depends on it.
- **`managed_block_missing`** — no `memhub:managed-block` in
  `CLAUDE.md`. Recommend adding the versioned block (see
  `src/managed_block.rs` for the current schema) — it's how agents get
  `memhub-primary` / `db` / `rendered` / `config` without parsing prose.
- **`managed_block_version`** — the block is present but older than
  this build expects. Recommend updating its fields/version to match
  `managed_block::MANAGED_BLOCK_VERSION`.
- **`keystone_phrases`** — one or more N4 keystone phrases (safety
  gates / identity line / core guardrail) are missing from `CLAUDE.md`.
  `detail` lists exactly which ones. Recommend restoring them
  verbatim — these are load-bearing, not stylistic, and must survive
  any future token-diet edit.
- **`claude_md_missing`** — `CLAUDE.md` itself couldn't be read.
  Recommend checking the repo root, or running `memhub init` if this
  repo has never been set up.
- **`user_md_size`** / **`user_md_unreadable`** — only appear when
  `[audit] user_md_path` is configured (opt-in, Q25). Recommend
  trimming the user-global file, or fixing/clearing the configured
  path if it's unreadable.
- **Any other id** — `audit md`'s own check set can grow independently
  of this skill (issue #32 owns the checks). Report the `message` /
  `detail` verbatim and suggest running `memhub audit md` (human
  output) or checking `src/commands/audit_md.rs` for what the new id
  means, rather than guessing at a fix.

## Report

- **Clean** (`count: 0`) — one line: "No findings — CLAUDE.md/AGENTS.md
  are in good shape." Stop here.
- **Findings present** — group by `severity` (`error` first, then
  `warn`), one bullet per finding: its `message`, its recommended fix
  from the table above, and `detail` when present. Close with a
  one-line summary ("N finding(s): X error, Y warn") and remind the
  user this skill does not apply any of these fixes itself.

## Notes

- Read-only judgment layer, not an auto-fixer. It never rewrites
  `CLAUDE.md`/`AGENTS.md`; every recommendation above is something a
  human (or a separate, explicitly-scoped follow-up) applies.
- Do not invent new severity levels or re-classify a finding's
  `severity` — treat it as advisory context, not something to override.
- `memhub audit md --strict` exists for scripts/CI (nonzero exit iff
  ≥1 finding); this skill doesn't need it since it already reads the
  full `findings[]` list to decide what to report.
