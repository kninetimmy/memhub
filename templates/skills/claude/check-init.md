---
name: check-init
description: Read-only health check of the memhub project in this repo — schema version, render freshness, K9 coexistence, write-log activity
framework: memhub
framework_version: 1.0.0
last_updated: 2026-05-13
---

Verify this repo's memhub setup is healthy. Read-only — no writes, no
fixes applied without explicit approval.

This skill replaces the K9 user-level `/check-init`. The K9 version is
still available as `/check-init-k9` for repos using the K9 markdown
four-file framework directly.

## Detection

**Check 1 — `.memhub/` exists.**
Run: `test -d .memhub && echo "present" || echo "absent"`
- `absent` → report **Red**. Suggest `/init-project` if the user
  wants to bootstrap memhub here, or `/check-init-k9` if this is a
  K9 markdown repo. Stop here.

**Check 2 — memhub binary on PATH.**
Run: `command -v memhub >/dev/null 2>&1 && echo "present" || echo "absent"`
- `absent` → report **Red**. `.memhub/` exists locally but the CLI
  isn't on PATH; tell the user to install or rebuild memhub. Stop
  here.

## Status read

Run, and parse the JSON output:

```bash
memhub status --json
```

Capture:
- `project.name`, `project.created_at`.
- Schema version. If the binary supports a `--latest-known` or
  equivalent flag, compare; otherwise treat any non-error `status`
  as schema-current and trust that migrations applied on connect.
- K9 integration flags (`integrations.k9.*` if present).
- Row counts: facts, decisions, tasks (broken down by status if
  available), session_notes, pending_writes, writes_log entries.

If `memhub status --json` exits non-zero, surface stderr and report
**Red** with that single finding.

If the binary doesn't expose `--json` on `status` (older build),
fall back to the human-readable `memhub status` output and parse
the labelled lines.

## Render output check

For each of `agent_docs/PROJECT.md` and `agent_docs/PROJECT_LEDGER.md`:

1. **File presence.** Missing → **Red**. Suggested fix:
   `memhub render`.
2. **Leading `<!-- memhub:rendered -->` marker.** Missing →
   **Yellow** (the file was hand-edited or written by something
   other than `memhub render`). Suggested fix: `memhub render` —
   the prior file will be backed up under `.memhub/backups/rendered/`.
3. **Freshness vs. DB.** Read the rendered file's
   "Generated at: <ISO>" header line. Then capture:
   - Latest `project_state.created_at` via `memhub state show --json`.
   - Latest `project_arch.created_at` via `memhub arch show --json`.
   - Most recent `writes_log` entry via `memhub stats --window 7d --json`
     (or the equivalent activity-summary field).

   If any of those is newer than the rendered file's "Generated at"
   timestamp → **Yellow**. Suggested fix: `memhub render`.

## K9 coexistence (informational)

If `memhub status` indicates K9 is enabled:

- Read `[integrations.k9].agent_docs_path` from
  `.memhub/config.toml` (or whatever the status JSON exposes).
- Confirm that directory exists. If not → **Yellow**: K9 enabled
  but pointed at a missing path. Suggested fix: either re-create
  the directory + `/init-project-k9`, or
  `memhub integrations disable-k9`.
- Confirm the four K9 files (`project_state.md`, `project_arch.md`,
  `project_decisions.md`, `project_backlog.md`) are present in
  that directory. Missing → **Yellow** with the same two
  suggested fixes.
- All four present → coexistence is intact. Informational only.

If K9 is disabled (`integrations.k9.enabled == false`) but the four
K9 files exist in `agent_docs/`, that's the post-migration archive
state (memhub-primary, K9 files retained for history). Informational
only.

If `.memhub/config.toml` has no `[integrations.k9]` section at all,
skip this section entirely — the repo is memhub-only.

## Pending writes triage

Run: `memhub review list --status pending --json` (or the equivalent
listing command).

- Any row older than 7 days → **Yellow**. Suggested fix: invoke
  `/wrap-up` to triage, or `memhub review list` + accept/reject
  manually.
- All pending rows under 7 days old → informational ("N pending,
  triage at next `/wrap-up`").
- No pending rows → skip.

## Writes log freshness

Run: `memhub stats --window 7d --json` and report:
- Last write actor and table (if surfaced by stats).
- Count of writes in the last 7 days.
- If the most recent `writes_log` entry is older than 14 days,
  surface informationally — the project may simply be idle, not
  unhealthy. Not a finding.

## Backups directory

If `.memhub/backups/` exists, report:
- Total size (e.g., `du -sh .memhub/backups/`).
- Most recent subdirectory name (e.g., `rendered/2026-05-13T*`).

Informational only — backups accumulate by design; pruning is the
user's call.

## Schema version detail

If the `memhub` binary version (from `memhub --version`) reports a
build newer than the schema version of `project.sqlite`, the binary
will apply pending migrations on next connect. That happens silently;
informational. If the binary is *older* than the schema — i.e.,
someone else upgraded the DB and this machine is on a stale build —
surface as **Red**: most commands will refuse. Suggested fix:
rebuild and reinstall the binary.

## Report

Summarize as one of:

- **Green** — `.memhub/` healthy, schema current, both rendered
  files fresh, no stale pending writes. One-line summary including
  project name and last-write timestamp.
- **Yellow** — minor issues. List each with a concrete suggested
  fix (one command). No writes are made from this skill. Render
  staleness, pending-write age, K9 path drift, and missing
  managed-block markers all surface here.
- **Red** — `.memhub/` missing, binary missing, schema/binary
  mismatch in the dangerous direction, or render output missing
  entirely. List the gaps; suggest the recovery path
  (`/init-project`, rebuild the binary, or
  `memhub init --from-backup <path>` if the user has an export).

End the report with:
- `memhub --version` output.
- Detected schema version.
- A one-line "next action" recommendation if Yellow or Red.

## Notes

- Read-only. Never write, never fix, never `memhub render` from this
  skill. The user makes that call.
- This skill is user-level; it fires in any repo. In a repo without
  `.memhub/`, it reports Red and points at `/init-project`.
- For K9 markdown framework health (the four `agent_docs/project_*.md`
  files), invoke `/check-init-k9` instead.
- Backups are informational. Pruning policy is the user's call;
  this skill doesn't surface them as findings.
- Memhub's PRD principle "intentionally boring" applies here too:
  this skill reports state. It does not heal, repair, or auto-fix.
