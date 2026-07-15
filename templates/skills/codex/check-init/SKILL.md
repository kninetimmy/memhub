---
name: check-init
description: Read-only health check of the memhub project in this repo — schema version, render freshness, K9 coexistence, write-log activity
framework: memhub
framework_version: 1.0.0
last_updated: 2026-07-14
---

Verify this repo's memhub setup is healthy. Read-only — this skill
never writes, never fixes, and never runs `memhub render` itself.

This is the Codex counterpart to the Claude Code `/check-init` skill.
Both run the same read-only `memhub` commands; they differ only in
the agent identifier on whatever read-side telemetry the host
captures.

`/check-init` is now a thin wrapper: the health logic moved into
`memhub doctor` (issue #21) so the same checks — project, config, DB
integrity, retrieval/metrics, integrations — are available identically
from the CLI, a script, or any agent, not just this skill. This
skill's own job is just detection + reporting the doctor output.

## Detection

**Check 1 — `.memhub/` exists.**
- POSIX shell (bash/zsh): `test -d .memhub && echo "present" || echo "absent"`
- Windows PowerShell: `if (Test-Path .memhub -PathType Container) { "present" } else { "absent" }`

Run whichever matches the current shell — no bash or WSL required.
- `absent` → report **Red**. Suggest `/init-project` if the user
  wants to bootstrap memhub here. Stop here.

**Check 2 — memhub binary on PATH.**
- POSIX shell (bash/zsh): `command -v memhub >/dev/null 2>&1 && echo "present" || echo "absent"`
- Windows PowerShell: `if (Get-Command memhub -ErrorAction SilentlyContinue) { "present" } else { "absent" }`

Run whichever matches the current shell.
- `absent` → report **Red**. `.memhub/` exists locally but the CLI
  isn't on PATH; tell the user to install or rebuild memhub. Stop
  here.

## Run the health check

```bash
memhub doctor --json
```

Parse the wrapped `{"doctor": {...}}` object:
- `overall` — worst status across every check (`ok` / `warn` / `error`).
- `exit_code` — 0 unless an `error`-level check fired.
- `counts` — `{ok, warn, error, skipped}`.
- `checks[]` — each `{id, group, status, message, detail?}`, grouped
  by `group` (`project`, `config`, `integrity`, `retrieval_metrics`,
  `integrations`).

If `memhub doctor --json` itself exits non-zero for a reason other
than a reported `error` check (e.g. a crash), surface stderr and
report **Red** with that single finding.

If the installed binary doesn't support `doctor` yet (older build,
predates issue #21), fall back to `memhub status` and report on
schema version, K9 flags, and row counts from that instead — the
pre-doctor behavior this skill used to implement directly.

## Report

Summarize as one of:

- **Green** — `overall: ok`. One-line summary: project name and the
  `counts` line (e.g. "14 ok, 3 skipped, 0 warn, 0 error").
- **Yellow** — `overall: warn`. List every `warn` check with its
  message — most already name the fix (`run memhub render`, `run
  memhub index rebuild`, `run /catch-up`, ...). No writes are made
  from this skill.
- **Red** — `overall: error`, or `.memhub`/binary missing per the
  Detection step above. List every `error` check and its message.

End the report with:
- `memhub --version` output.
- The `counts` from `doctor --json` (or the human `Summary: ...`
  footer from plain `memhub doctor`).
- A one-line "next action" recommendation if Yellow or Red.

## Notes

- Read-only. Never write, never fix, never `memhub render` from this
  skill — `memhub doctor` itself never writes either (no table
  writes, no fix-it mode; it only reports).
- This skill is user-level; it fires in any repo. In a repo without
  `.memhub/`, it reports Red and points at `/init-project`.
- `memhub doctor --strict` (not used by this skill) promotes `warn`
  to a failing exit code for scripting; this skill already reads the
  full `checks[]` list itself, so it doesn't need the exit code to
  decide severity.
- memhub's PRD principle "intentionally boring" applies here too:
  this skill reports state. It does not heal, repair, or auto-fix.
