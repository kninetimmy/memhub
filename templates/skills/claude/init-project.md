---
name: init-project
description: Bootstrap memhub in this repo — initialize the SQLite store, seed starter state and architecture narratives, render PROJECT.md and PROJECT_LEDGER.md
framework: memhub
framework_version: 1.0.0
last_updated: 2026-07-14
---

Set up memhub in the current repository: create `.memhub/project.sqlite`,
seed `project_state` and `project_arch` with starter narratives, and
render the two agent_docs files.

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
the bootstrap can note it rather than ignoring prior work.

1. **Context files.** Check whether `CLAUDE.md` or `AGENTS.md` exists
   at repo root. Note presence; do not flag as foreign on its own.
2. **Other systems.** Probe for `.cursor/`, `.aider*`,
   `memory-bank/`, `.gaai/`, `ai_docs/`, `context/`. Informational.

## Detection + interview

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

## Draft starter narratives

Produce two drafts, kept separate so each can be approved on its own.

1. **`state` body.** Currently building / next up / open questions /
   last session. Under ~50 lines for a fresh project: one "Initialized
   memhub on YYYY-MM-DD" entry and 1–3 tentative next-up items pulled
   from the interview.

2. **`arch` body.** Purpose / stack / layout / key subsystems /
   known gaps. Sparse is fine for a fresh project — purpose + stack +
   folder inventory is enough. Match the section style used by
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

## Render

```bash
memhub render
```

Verify both `.memhub/rendered/PROJECT.md` and
`.memhub/rendered/PROJECT_LEDGER.md` exist, unless the project config
sets a different `[render].output_dir`. Surface the paths to the user.

## Runtime toggles

memhub ships several opt-in toggles off by default. Offer each one at
a time now, while the repo is fresh — skip any the user waves off.

1. **Retrieval mode.** Ask whether to turn on hybrid recall (semantic
   + keyword, recommended) or stay on the default FTS-only (lighter,
   keyword-only search).
   - Hybrid: set `mode = "hybrid"` under the existing `[retrieval]`
     table in `.memhub/config.toml` (memhub init already wrote that
     table), then run `memhub index rebuild --actor
     claude:init-project` to backfill embeddings for existing rows.
     Note that switching between `fts` and `hybrid` later always
     needs a rebuild to bring rows in sync — a config edit alone
     isn't enough. Hybrid mode already runs a bundled cross-encoder
     re-ranker over the blended results by default, nothing extra to
     turn on.
   - FTS: nothing to do; it's already the default.
2. **Code index.** Offer to run `memhub code index` now so
   `memhub locate` (and the `/locate` skill) can answer "where is X"
   with ranked file:line breadcrumbs right away instead of on first
   use.
3. **Machine-global memory.** Ask whether to opt this repo into the
   shared `~/.memhub/global.sqlite` store — a second SQLite for
   machine/toolchain facts and standing engineering policy, distinct
   from this repo's project memory. Off by default and per-repo
   opt-in. If yes, run `memhub global enable` and report the store
   path; note that writing to global is always a deliberate human
   action, never something to do on your own. If no, just note
   `/global` and `memhub global enable` are available anytime.
4. **Doc ingestion.** Ask whether there's a reference doc (design
   spec, API contract) to ingest now. After the first doc add,
   relevant chunks automatically surface in plain recall, gated by a
   relevance threshold so off-topic docs stay silent. If given a
   path, run `memhub doc add "<path>" --json` and report the chunk
   count; if not, just note `/doc` is available anytime.
5. **Drive sync.** Ask whether the user works across machines. If
   yes, run `memhub sync enable`, then ask for the absolute path of a
   folder that already syncs (Google Drive for Desktop, an rclone
   mount on Linux, etc.) and set it as `[sync] drive_subpath` in
   `.memhub/config.toml`. Run `memhub sync status` and report the
   resolved remote dir. Note that `/catch-up` pulls at session start
   and `/wrap-up` pushes at the end. If no, just note `/catch-up`,
   `/wrap-up`, and `memhub sync enable` are available anytime.

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

- **One exists already.** Leave it alone. memhub never writes to it —
  the only local generated output lives at `.memhub/rendered/PROJECT.md`
  and `.memhub/rendered/PROJECT_LEDGER.md`, written by `memhub render`.

## Summary

Tell the user:
- `.memhub/project.sqlite` was created at schema version X. Row
  counts: state=1, arch=1 (or 0 if skipped).
- Which runtime toggles were turned on (retrieval mode, code index,
  global memory, doc ingestion, Drive sync) and which were left for
  later.
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
