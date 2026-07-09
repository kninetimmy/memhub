---
name: wrap-up
description: Summarize this memhub session, route updates into the database, then re-render PROJECT.md and PROJECT_LEDGER.md
framework: memhub
framework_version: 1.0.0
last_updated: 2026-07-09
---

Wrap up the current session against the memhub repo. Memhub's SQLite
database is the source of truth; the rendered markdown files
(`.memhub/rendered/PROJECT.md` and
`.memhub/rendered/PROJECT_LEDGER.md` by default) are local output of
`memhub render`, not a parallel narrative. Your job is to draft
updates, get approval per item, write to the DB, then re-render.

This is the memhub-native wrap-up, installed as a user-level skill
(decision 97) — it fires in any repo with `.memhub/`, not just this
one. It is a **thin executor** (Q10, issue #99): the step-by-step
policy — detection, read window, draft assembly, approval gate — is
rendered from one canonical source in the binary by
`memhub wrapup-policy`, not duplicated here. What follows is (1) the
pre-flight this skill needs before it can even run that command, (2) a
handful of repo-specific additions the generic policy text doesn't
know about yet, and (3) the concrete, Claude-attributed DB-write
sequence.

## Detection (pre-flight)

Run this once, before anything else — it only covers what has to be
true before `memhub wrapup-policy` can even run; schema currency is
checked as part of the policy text itself, not here.

1. `test -d .memhub && echo present || echo absent` — `absent` → stop.
   Tell me: "No `.memhub/` in this repo. Run `memhub init` first, or
   invoke a different wrap-up command if this repo uses K9 markdown
   directly." Do not proceed.
2. `command -v memhub >/dev/null 2>&1 && echo present || echo absent`
   — `absent` → stop. Tell me to put `memhub` on PATH, then re-run.

All memhub invocations below pass `--actor claude:wrap-up` so
`writes_log` distinguishes wrap-up writes from raw CLI use.

## Run the policy

Run `memhub wrapup-policy` (human-readable) or
`memhub wrapup-policy --json` (the wrapped
`{"wrapup_policy": {"verbosity": ..., "instructions": "..."}}` form —
no `--actor`, it's read-only and never opens the DB). It renders this
repo's full wrap-up policy — the schema-currency check, the read
window, the draft-assembly items for this repo's `[wrap_up] verbosity`
(`.memhub/config.toml`; `minimal` / `standard` / `full` /
`transcript`), and the approval-gate rule — from one source. **Follow
the returned `instructions` text for all of that.** It supersedes
anything you remember from a previous session or a different repo,
since both the level and the text can differ.

## Repo-specific additions to draft assembly

The policy text is agent-agnostic and, as of this build, doesn't yet
know about a few newer surfaces. Layer these on top of what it says —
they refine its draft assembly, they don't replace it:

- **Decision summaries (decision 72).** Whenever you draft a decision,
  also draft a one-sentence `--summary` — a natural-language
  paraphrase of the title. On memhub's own golden set this lifted
  Recall@3 on jargon-titled decisions from 76.5% to 100%; the policy
  text calls it mandatory at `full`/`transcript` verbosity, but draft
  one regardless of level — it's cheap and it's what actually moves
  the number. Facts have no `--summary` field; don't add one there.
- **Verified commands are not facts (Q11).** A build/test/run/lint
  command you actually executed this session, with an observed exit
  code, is not a fact — draft it for `memhub command verify` instead
  (DB writes, below). This is go-forward only: do not backfill
  existing command-shaped facts into the `commands` table.
- **New or revised reference docs.** If a durable spec/design doc was
  authored or materially revised this session, draft a
  `memhub doc add <path>` item alongside the others.
- **Global promotion (M9) — only when I say so.** Repo-scoped is
  always the default. If, and only if, I explicitly frame a drafted
  fact or decision as machine-wide during approval, stage it for the
  global store instead of writing it repo-scoped (see "Global writes"
  below). Never infer this yourself — same anti-noise rule as
  `/global`: one bad global write pollutes every repo on the machine.

## Approval gate

Non-negotiable at every verbosity level. Show all drafts in one block,
grouped by kind. I approve, edit, or reject each item individually —
wait for explicit approval per item, or a clear "all good", before
writing anything. A rejected draft is dropped; do not retry without my
saying so.

## DB writes — first, atomic per item, halt on failure

Once approved, invoke each write in this order. Every command takes
`--json --actor claude:wrap-up` so the response is parseable and the
audit row is correctly attributed.

```
# 1. State (only if changed)
memhub state set "<approved body>" --json --actor claude:wrap-up

# 2. Pending-write promotions / rejections (also where any global
#    proposal from step 3 becomes durable, once I've confirmed it)
memhub review accept <id> --json --actor claude:wrap-up
memhub review reject <id> --reason "<reason>" --json --actor claude:wrap-up

# 3. New decisions (repo-scoped) -- always draft --summary (decision 72)
memhub decision add "<title>" --rationale "<rationale>" --summary "<summary>" --source user+agent:claude-code --json --actor claude:wrap-up

# 4. New tasks + closures
memhub task add "<title>" --notes "<notes>" --json --actor claude:wrap-up
memhub task done <id> --json --actor claude:wrap-up

# 5. New facts (repo-scoped; --kind optional: gotcha | env | preference | command | constraint)
memhub fact add "<key>" "<value>" [--kind <kind>] --source user+agent:claude-code --json --actor claude:wrap-up

# 6. Verified commands (Q11 -- not facts; no --json/--actor on this one)
memhub command verify <build|test|run|lint|other> "<cmdline>" --exit-code <n>

# 7. New or revised reference docs (repo-scoped)
memhub doc add "<path>" [--title "<title>"] --json --actor claude:wrap-up

# 8. Session summary (always, unless I rejected it)
memhub note add "<two-to-four-sentence summary>" --json --actor claude:wrap-up

# 9. Architecture (only if approved this session)
memhub arch set "<approved body>" --json --actor claude:wrap-up

# 10. Stale-fact re-verifications -- one invocation per approved fact,
#     never a bulk "verify all" pass
memhub fact verify <id> --json --actor claude:wrap-up
```

For multi-line state or arch bodies, write the body to a temp file and
pass `--from-file <path>` instead of inlining.

**Halt on first non-zero exit.** Do not retry, do not skip, do not
proceed to render. Tell me which command failed and what stderr said.
The writes that succeeded are durable in `writes_log` and the target
tables — I can fix the cause and re-run `/wrap-up` to pick up the
rest.

## Global writes (M9)

For any fact or decision I explicitly flagged as global during
approval, do **not** run its repo-scoped line above. An agent never
writes `--global` directly (same rule as `/global`) — stage it
instead, and let step 2 above accept it once I've confirmed:

```
memhub.propose_fact(key=..., value=..., rationale=..., kind=..., global=true)
memhub.propose_decision(title=..., rationale=..., global=true)
```

This lands in this repo's `pending_writes` tagged `target:"global"`
and becomes durable in `~/.memhub/global.sqlite` only via
`memhub review accept <id>` (step 2), and only while this repo still
has `memhub global enable`d. `propose_decision` has no `--summary`
field yet — if I approved one for a global decision, backfill it after
acceptance: `memhub decision set-summary <id> "<summary>" --json --actor claude:wrap-up`.

Docs have no agent-mediated global path at all — there is no `global`
parameter on `memhub.doc_add`. If I want a doc promoted to global,
tell me to run `memhub doc add <path> --global` (or `/global`)
myself; you keep ingesting it repo-scoped as in step 7.

## After the writes

Once every approved write above succeeds, pick the policy text back up
for Render and Cross-machine sync — run `memhub render`, then the sync
steps if this repo has sync enabled, exactly as it describes them.
Nothing repo- or agent-specific to add there. It also covers the
closing reminder (audit trail, rendered files aren't a commit, start a
new session anytime) — including that **you never run `git commit`**,
which holds regardless of verbosity level; that's my call.

## Notes

- Bias toward less content — a tight, true summary beats a padded one.
  The approval gate is what stops hallucinated accomplishments from
  landing; never skip it, whatever the verbosity level.
- Session boundary is implicit (latest `project_state.created_at`). No
  explicit `memhub session` command exists, by design.
- The PRD principle "agents are untrusted writers" applies throughout:
  every approved item flows through a write that records the actor,
  and anything global gets an extra staged-review hop for the same
  reason.
- This skill is user-level; it fires in any repo that has `.memhub/`.
  In a repo without `.memhub/`, Detection stops here.
