---
name: wrap-up
description: Summarize this memhub session, route updates into the database, then re-render PROJECT.md and PROJECT_LEDGER.md
framework: memhub
framework_version: 1.0.0
last_updated: 2026-05-13
---

Wrap up the current session against the memhub repo. Memhub's SQLite
database is the source of truth; the rendered markdown files
(`.memhub/rendered/PROJECT.md` and
`.memhub/rendered/PROJECT_LEDGER.md` by default) are local output of
`memhub render`, not a parallel narrative. Your job is to
draft updates, get approval per item, write to the DB, then re-render.

This is the Codex counterpart to the Claude Code `/wrap-up` skill.
Both invoke the same `memhub` CLI against the same `.memhub/project.sqlite`;
they differ only in the agent identifier passed via `--actor` and
`--source`. Writes from this skill are attributed as
`agent:codex` / `codex:wrap-up`.

## Detection

Run this once at the top. Use the result to gate the rest of the
command.

**Check 1 — `.memhub/` exists.**
Run: `test -d .memhub && echo "present" || echo "absent"`
- `absent` → stop. Tell the user: "No `.memhub/` in this repo. Run
  `memhub init` (or `/init-project`) first." Do not proceed.
- `present` → continue.

**Check 2 — memhub binary on PATH.**
Run: `command -v memhub >/dev/null 2>&1 && echo "present" || echo "absent"`
- `absent` → stop. Tell the user to put `memhub` on PATH, then re-run.
- `present` → proceed.

**Check 3 — schema is current.**
Run: `memhub status` and confirm "Schema version" shows the latest
applied migration. If not, surface the gap and stop — running
`/wrap-up` against a stale schema is the kind of thing that produces
silently wrong rows.

All subsequent memhub invocations in this command pass
`--actor codex:wrap-up` so `writes_log` distinguishes wrap-up writes
from raw CLI use, and `--source user+agent:codex` on fact and
decision adds so the durable rows preserve both the user-approval
signal and the mediating agent.

## Read window

Capture the boundary of "this session" implicitly: the most recent
`project_state` row's `created_at` is the previous wrap-up timestamp.
Anything newer in the DB or git history is in-window.

Run, in order, and keep the JSON for draft assembly:

1. `memhub state show --json` — the current state narrative (or
   `null` for a fresh repo).
2. `memhub arch show --json` — the current architecture narrative.
3. `memhub note list --since-days 7 --json` — recent session notes.
4. `memhub review list --status pending --json` — staged proposals
   (facts and decisions) that earlier MCP sessions queued but no
   human has reviewed.
5. `memhub task list --status open` — open work items.
6. From the state row's `created_at`, run
   `git log --since="<that timestamp>" --oneline`. If there is no
   prior state row, fall back to the last 10 commits.
7. `git status --porcelain` — uncommitted changes worth surfacing.

If `memhub status --json`'s `k9_detected` is `true` and the operator
hasn't explicitly migrated, also note that this repo still has K9
markdown files and that they are no longer the source of truth —
surface as informational, not blocking.

## Draft assembly

Synthesize eight things, drafted separately so each can be approved or
rejected on its own:

1. **New `state` body.** Currently building / next up / open
   questions / brief mention of last session. Keep it tight — the
   render produces a long file; the state blob should stay under ~100
   lines. If the prior state row is still accurate, propose an update
   only if there's a real change.

2. **New decisions.** Architectural / workflow / contract decisions
   locked this session. Each is title + rationale. Ask the user
   whether each candidate is settled enough to record vs. still
   actively referenced (in which case it stays in the state narrative
   for now).

3. **Backlog changes.** New tasks discovered, status changes on
   existing tasks. For each existing task you'd close, look up its
   id from step 5 above.

4. **New facts.** Build / test / run commands or other durable
   key-value records that surfaced. Skip anything that's already in
   the facts table with the same value. Tag with `--kind` when the fact
   clearly fits one of: `gotcha`, `env`, `preference`, `command`,
   `constraint` — optional, skip it if nothing fits.

5. **Pending-write triage.** For each row from step 4 of the read
   window, propose accept or reject with a one-line reason.

6. **Session-summary note.** Two to four sentences on what actually
   shipped this session, anchored to commit hashes where possible.
   This goes into `session_notes` via `memhub note add`. Bias toward
   truth — if the session was exploratory with no concrete outcome,
   say so. Don't invent accomplishments.

7. **Architecture drift.** Touch only if a real architectural shift
   occurred (new subsystem, schema change, invariant change). Default
   is no arch update.

8. **Stale-fact re-verify candidates.** Run `memhub fact list --json`
   and pick up to 5 facts ordered oldest-first by `verified_at`
   (`null` sorts as oldest), preferring rows already flagged
   `is_stale`. Skip this draft entirely if there are none. Present
   each candidate as its own item — "Still true: `<key>` = `<value>`
   (last verified <verified_at, or 'never'>)?" — never a single
   grouped "re-verify all N?" prompt. A "no" means the fact is
   probably wrong; tell the user so they can fix or remove it instead
   of verifying it.

## Approval gate

Show all drafts in one block, grouped by kind. The user approves,
edits, or rejects each item individually. Wait for explicit approval
per item or a clear "all good" before moving on.

If the user rejects a draft, drop it. Do not retry without their
saying so.

## DB writes — first, atomic per item, halt on failure

Once approved, invoke each write in this order. Every command takes
`--json --actor codex:wrap-up` so the response is parseable and the
audit row is correctly attributed. Fact and decision adds also pass
`--source user+agent:codex` so the durable `source` column records
both the user approval and the mediating agent.

```
# 1. State (only if changed)
memhub state set "<approved body>" --json --actor codex:wrap-up

# 2. Pending-write promotions / rejections
memhub review accept <id> --json --actor codex:wrap-up
memhub review reject <id> --reason "<reason>" --json --actor codex:wrap-up

# 3. New decisions
memhub decision add "<title>" --rationale "<rationale>" --source user+agent:codex --json --actor codex:wrap-up

# 4. New tasks + closures
memhub task add "<title>" --notes "<notes>" --json --actor codex:wrap-up
memhub task done <id> --json --actor codex:wrap-up

# 5. New facts (--kind is optional: gotcha | env | preference | command | constraint)
memhub fact add "<key>" "<value>" [--kind <kind>] --source user+agent:codex --json --actor codex:wrap-up

# 6. Session summary (always, unless the user rejected it)
memhub note add "<two-to-four-sentence summary>" --json --actor codex:wrap-up

# 7. Architecture (only if approved this session)
memhub arch set "<approved body>" --json --actor codex:wrap-up

# 8. Stale-fact re-verifications — one invocation per approved fact,
#    never a bulk "verify all" pass
memhub fact verify <id> --json --actor codex:wrap-up
```

For multi-line state or arch bodies, write the body to a temp file
and pass `--from-file <path>` instead of inlining.

**Halt on first non-zero exit.** Do not retry, do not skip, do not
proceed to render. Tell the user which command failed and what
stderr said. The writes that succeeded are durable in `writes_log`
and the target tables — they can fix the cause and re-run `/wrap-up`
to pick up the rest.

## Render

After all approved DB writes succeed, run:

```
memhub render
```

This emits fresh local `PROJECT.md` and `PROJECT_LEDGER.md` files from
the new DB state, backing up the prior versions under
`.memhub/backups/rendered/<stamp>/`.

Surface what got written and any backup paths.

## Cross-machine sync push (only if enabled)

If this repo syncs across machines, push the freshly-updated DB into
the synced Drive folder so other machines can `/catch-up`. The
transport is an OS-level synced folder (Google Drive for Desktop, or an
rclone mount on Linux); memhub just writes a local path and Google's
app uploads it.

1. `memhub sync status --json`. If `enabled` is false, **skip this
   section silently**. If `remote_dir_error` is set (usually an empty
   `drive_subpath`), ask the user once for the absolute path of the
   synced folder and tell them to set `[sync] drive_subpath`. Otherwise
   the resolved `remote_dir` is the target.
2. **Check the remote first so you never clobber a newer push:** `memhub
   sync check`. If the verdict is `drive-ahead` or `diverged`, **stop and
   `/catch-up` first** — do not push. `up-to-date`, `local-ahead`, and
   `no-remote` are safe.
3. Snapshot **directly into the synced folder**: `memhub sync snapshot`
   — omit the path; it defaults to the canonical
   `<drive_subpath>/memhub/<project_id>` (emits `project.sqlite` +
   `manifest.json`; `VACUUM INTO` writes a complete file, so no
   half-written upload). It **refuses** on a drive-ahead/diverged remote;
   if that happens, `/catch-up` first rather than passing `--force`.
4. Record the baseline so the next `/catch-up` reads `up-to-date`:
   `memhub sync commit` (same path-less default).

If `sync snapshot` fails or refuses (remote ahead/diverged, or the
synced folder doesn't exist yet), say so and **do not** run `commit`.
The local DB is unaffected.

## Reminder, not a commit

Tell the user:
- They can audit what landed via
  `memhub stats --window 7d` (writes by actor and table) or
  `memhub note list --since-days 1`.
- The rendered files are local generated output and are not meant to
  be committed unless this repo explicitly opts into a tracked render
  path.
- They can start a new session whenever (`/clear` or restart).

**Do not run `git commit` yourself.** That is the user's call. The
local-vs-shared boundary is intentional.

## Notes

- Bias toward less content. A tight, true summary beats a padded one.
- Summarizing unsupervised is where hallucinated accomplishments creep
  in. The approval gate is the defense — never skip it.
- Session boundary is implicit (latest `project_state.created_at`).
  No explicit `memhub session` command exists, by design.
- The PRD principle "agents are untrusted writers" still applies even
  inside wrap-up: every approved item flows through a write that
  records the actor and the compound source.
- Source vocabulary lives at
  `docs/reference/memhub-prd-source-vocabulary-addendum.md` if a
  question arises about which value to pass.
- This skill is user-level; it fires in any repo that has `.memhub/`.
  In a repo without `.memhub/`, the Detection step stops here.
