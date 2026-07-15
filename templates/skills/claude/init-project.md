---
name: init-project
description: Bootstrap memhub in this repo — initialize the SQLite store, seed starter state and architecture narratives, render PROJECT.md and PROJECT_LEDGER.md
framework: memhub
framework_version: 1.0.0
last_updated: 2026-07-14
---

Set up memhub in the current repository: create `.memhub/project.sqlite`,
seed `project_state` and `project_arch` with starter narratives, render
the two agent_docs files, and (optionally) bootstrap from existing K9
markdown.

This skill replaces the K9 user-level `/init-project`.

## Detection

Run these in order. Stop and report if any fails.

**Check 1 — memhub binary on PATH.**
- POSIX shell (bash/zsh): `command -v memhub >/dev/null 2>&1 && echo "present" || echo "absent"`
- Windows PowerShell: `if (Get-Command memhub -ErrorAction SilentlyContinue) { "present" } else { "absent" }`

Run whichever matches the current shell — no bash or WSL required.
- `absent` → stop. Tell the user memhub isn't installed; point them at
  the memhub README for install instructions
  (`cargo install --path <path-to-memhub-source>` from a local clone).
  Do not proceed.

**Check 2 — already initialized.**
- POSIX shell (bash/zsh): `test -d .memhub && echo "present" || echo "absent"`
- Windows PowerShell: `if (Test-Path .memhub -PathType Container) { "present" } else { "absent" }`

Run whichever matches the current shell.
- `present` → refuse politely. The repo already has memhub. Suggest
  `/check-init` for diagnostics or `/wrap-up` to update state. Stop
  here.

**Check 3 — repo root sanity.**
Run: `git rev-parse --show-toplevel 2>/dev/null`
- If the output is a different directory than `pwd`, ask the user
  whether to init at the repo root or at the current directory. Wait
  for confirmation. memhub will create `.memhub/` wherever the
  command is invoked from; the user almost always wants the repo
  root.
- If the command fails (not a git repo), continue at `pwd` without
  asking — memhub does not require git.

## Existing-context-system scan

Look for signs the repo already uses a session-continuity system, so
the bootstrap can coexist or migrate rather than ignoring prior work.

1. **K9 markdown.** Check for `agent_docs/project_state.md` and
   `agent_docs/project_backlog.md`. If both exist, K9 is in play.
2. **Context files.** Check whether `CLAUDE.md` or `AGENTS.md` exists
   at repo root. Note presence; do not flag as foreign on its own.
3. **Other systems.** Probe for `.cursor/`, `.aider*`,
   `memory-bank/`, `.gaai/`, `ai_docs/`, `context/`. Informational.

If the K9 four-file set is present, ask the user which path to take:

- **(a) Coexist with K9.** Run `memhub init`, then
  `memhub integrations enable-k9 --agent-docs-path agent_docs` so
  memhub records that K9 also lives here. If K9 has populated
  history, offer `memhub integrations bootstrap-k9` to import
  decisions and backlog.
- **(b) Migrate fully.** Same flow as (a), plus a closing note
  suggesting the user archive or delete the K9 four files once
  they verify the migration in the rendered output.
- **(c) Memhub-only.** Treat as brand-new. Do not enable K9
  integration. Leave the K9 files alone (the user can clean them
  up manually if desired).

Wait for the user's choice before continuing.

## Detection + interview

For brand-new and (c) above:

- **Stack detection.** Lightweight repo scan:
  - Stack markers at root: `Cargo.toml`, `package.json`,
    `pyproject.toml`, `go.mod`, `Gemfile`, `pom.xml`, `*.csproj`,
    `*.sln`.
  - `README.md` if present — summarize purpose in one or two
    sentences.
  - Top-level folders, one level deep. Infer role from names.
  - CI config: `.github/workflows/` or equivalent.
  - Do NOT read every source file. Do NOT scan deep trees.

- **Interview.** Ask one question at a time. Skip anything the
  scan already answered; confirm rather than re-ask.
  1. Project name and a one-sentence purpose.
  2. Stack / language (or "use the detected one").
  3. Build / test / run commands (or "I'll add these later").
  4. Anything else worth recording in the starter narrative
     (constraints, deadlines, key collaborators).

For (a) and (b): read `agent_docs/project_state.md` and
`agent_docs/project_arch.md` to seed the starter narratives. Do NOT
parse `project_backlog.md` or `project_decisions.md` inline — those
flow through `memhub integrations bootstrap-k9` after `memhub init`.

## Draft starter narratives

Produce two drafts, kept separate so each can be approved on its own.

1. **`state` body.** Currently building / next up / open questions /
   last session. Under ~50 lines for a fresh project. For brand-new:
   one "Initialized memhub on YYYY-MM-DD" entry and 1–3 tentative
   next-up items pulled from the interview. For K9 coexist/migrate:
   distil the current focus from `project_state.md`.

2. **`arch` body.** Purpose / stack / layout / key subsystems /
   known gaps. Sparse for brand-new (purpose + stack + folder
   inventory is enough); richer when seeded from K9
   `project_arch.md`. Match the section style used by
   `memhub render` so PROJECT.md stays readable: short paragraphs,
   bullet lists for layout and subsystems.

## Approval gate

Show both drafts. Wait for explicit approval per draft. The user may
edit either before approving. Do not write anything until both are
approved (or the user explicitly says "skip arch for now" — in that
case, only state is set).

## Run init + writes

After approval, in order:

```bash
memhub init --json
```

Confirm the project was created (schema version, `.memhub/` path).
Halt on non-zero exit and surface stderr.

Write the state body to a temp file (multi-line bodies must use
`--from-file`, not inline):

```bash
memhub state set --from-file /tmp/memhub-init-state.md \
  --actor claude:init-project --json
memhub arch set --from-file /tmp/memhub-init-arch.md \
  --actor claude:init-project --json
```

For coexist/migrate paths, ask before each integration step:

```bash
memhub integrations enable-k9 --agent-docs-path agent_docs
# Offer bootstrap-k9 separately, only if K9 decisions/backlog have content
memhub integrations bootstrap-k9 --dry-run --json    # preview parsed rows
memhub integrations bootstrap-k9 --json              # apply
```

Halt on first non-zero exit and surface stderr. Do not retry, do not
skip. The rows that landed before the failure are durable in
`writes_log`; the user can fix and re-run.

## Render

```bash
memhub render
```

Verify both `.memhub/rendered/PROJECT.md` and
`.memhub/rendered/PROJECT_LEDGER.md` exist, unless the project config
sets a different `[render].output_dir`. Surface the paths to the user.

## Optional context-file update

memhub does not write `CLAUDE.md` or `AGENTS.md` itself. After render,
check whether one exists at repo root:

- **Neither exists.** Offer to write a minimal `CLAUDE.md` pointing at
  the rendered files for session continuity. Template:

  ```markdown
  # <project name>

  <one-sentence purpose>

  ## Session Continuity

  memhub is the source of truth at `.memhub/project.sqlite`.
  The rendered files under `.memhub/rendered/` are the local
  human-readable view. They are generated from the DB and ignored by
  Git by default. Re-render after `/wrap-up` with `memhub render`.

  ## Build / test / run

  <commands from interview, or "see Architecture in PROJECT.md">
  ```

  Approval gate before writing. If the user is on Codex, offer
  `AGENTS.md` with identical body.

- **One exists already.** Leave it alone. `memhub sync-md` does not
  edit it — it only writes the rendered twins at
  `.memhub/rendered/CLAUDE.md` and `.memhub/rendered/AGENTS.md`, the
  same local generated output as `PROJECT.md`.

## Summary

Tell the user:
- `.memhub/project.sqlite` was created at schema version X. Row
  counts: state=1, arch=1 (or 0 if skipped), facts=N, decisions=N,
  tasks=N (only N > 0 if `bootstrap-k9` ran).
- `.memhub/rendered/PROJECT.md` and
  `.memhub/rendered/PROJECT_LEDGER.md` are local generated output.
  `.memhub/` and the legacy `agent_docs/PROJECT*.md` render paths are
  gitignored by default.
- Suggested commit: `git add .gitignore CLAUDE.md`
  (drop CLAUDE.md if no new context file was written).
- `/check-init` runs a read-only health check anytime.
- `/wrap-up` is the session-end routing brain.

**Do not run `git add` or `git commit` yourself.** The user takes
that gesture. The local-vs-shared boundary is intentional.

## Notes

- Bias toward less content. Starter narratives can be thin; the
  first real `/wrap-up` fills them out from actual session work.
- Don't invent project history. If the user didn't tell you and
  there's no README, leave it blank rather than guess.
- Never overwrite an existing `.memhub/project.sqlite`. The check-2
  refusal is the guardrail. For recovery, the user runs
  `memhub init --from-backup <path>` directly, not this skill.
- This skill is user-level; it fires in any repo without a
  conflicting project-level `/init-project`.
