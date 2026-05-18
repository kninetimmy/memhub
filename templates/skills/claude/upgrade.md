---
name: upgrade
description: Rebuild + install memhub and bring every memhub instance on this machine (each known repo DB + the machine-global store) to head schema, resync installed agent skill wrappers, with a one-time fix for the ~/.local/bin PATH shadow. Run from the memhub source repo.
framework: memhub
framework_version: 1.0.0
last_updated: 2026-05-18
---

`memhub upgrade` is the one dependable command to make **every** memhub
install on this machine coherent after a code change — not just
whichever repo you happened to rebuild from. It rebuilds + installs the
binary, fixes the stale-`~/.local/bin` PATH shadow once and for all,
resyncs the installed slash-command skill wrappers, then migrates and
smoke-tests every known repo DB plus the machine-global store, printing
a per-instance ready table.

It exists because the old hand-run recipe (`cargo install --path .
--force` + manual `cp`) silently left a stale binary on PATH across
many sessions, and M9 multiplied the problem (N per-repo DBs + one
global store, each possibly on an old schema).

## When to reach for it

After pulling memhub changes that bump the schema or change behavior,
and you want **all** repos + the global store ready, not just one.
This is a machine-state-changing maintenance action (it reinstalls the
binary and may rewrite a symlink) — **suggest it and let the user run
it**; do not run it autonomously.

## Preconditions

- Run from the **memhub source repo** (it rebuilds from source; it
  checks `Cargo.toml` is the `memhub` package and errors otherwise).
- `cargo` on PATH.

## Enumeration is a registry, not a scan

memhub keeps a self-maintaining list of every repo it has actually
opened, in `~/.memhub/global.sqlite` (`known_projects`). `upgrade`
iterates that — deterministic and reproducible, never a filesystem
scan (scans hang on cloud mounts, skip permission-denied subtrees, and
migrate repos you never meant to touch).

Bootstrap gap: a repo memhub has **never opened** since this feature
landed is absent from the first run's report — but it self-migrates on
its next `open_project` anyway, so only a report row is lost, not
correctness. Seed it explicitly with `--also`:

```bash
memhub upgrade --also /path/to/untouched/repo   # repeatable; also persists it
```

## Usage

```bash
memhub upgrade            # rebuild+install, fix PATH shadow, resync skills, migrate+verify all
memhub upgrade --dry-run  # show what would happen; NO install/symlink/migration/skill copy
memhub upgrade --json     # machine-readable instance table + skills array
memhub upgrade --yes      # don't prompt before replacing a non-symlink shadow
memhub upgrade --no-skills # skip the skill-wrapper resync; binary + DB migrate still run
memhub upgrade --allow-self-stage # Windows + no TTY (CI/agent): permit the staged relaunch
```

Always show the user `--dry-run` output first if there is any doubt
about what will change.

## What it does, in order

1. `cargo install --path . --force` (aborts the whole run on build
   failure — no half-upgrade).
2. **PATH-shadow fix (closes the recurring stale-binary bug):** if
   `~/.local/bin/memhub` is a *regular file* shadowing
   `~/.cargo/bin/memhub`, it is replaced **once** with a symlink so
   every future install just works. Already-a-symlink → idempotent
   no-op. A non-symlink shadow is replaced only after a y/N
   confirmation (or `--yes`); declined → the exact manual `ln -sf`
   command is printed.
3. **Skill-wrapper resync (decision 97):** for each agent dir that
   *already exists* — `~/.claude/commands/` (flat `*.md`),
   `~/.codex/skills/` (dir-per-skill) — copies the source repo's
   `templates/skills/{claude,codex}/` over the installed wrappers so
   they never lag the binary. Additive (a skill removed from
   `templates/` leaves a harmless installed orphan; it does **not**
   prune shared user-global dirs), idempotent, best-effort (a
   partial/permission error is a `warn` row, never fatal). An agent dir
   that does not exist is skipped, never created. `--no-skills` skips
   this step. Internalizes the old manual `cp` recipe, which becomes a
   fallback only.
4. Re-execs the **freshly installed** binary for the migrate+verify
   pass, so migrations run under new code (old code only knows old
   migrations).
5. For each instance: open (auto-applies migrations) → compare schema
   → tiny FTS recall smoke → row in the table.

## Reading the table

```
  INSTANCE                    SCHEMA                       STATUS
  ~/memhub                    0014_documents -> 0015_...   migrated
  ~/src/projalpha             0015_known_projects          ready
  ~/work/legacy               (none)                       skipped (no memhub project)
  <global store>              0015_known_projects          ready

  skills: claude   ~/.claude/commands     synced 11
  skills: codex    ~/.codex/skills        synced 11
  3/4 instances ready
```

- `ready` — already at head. `migrated` — was behind, now at head.
- `skipped` — known/asked-for but nothing to do (path gone, or the
  global store is absent because no repo opted into M9).
- `ERROR` — opened/verified and failed; the command exits non-zero so
  scripts notice. Investigate that instance.
- `skills:` rows — per-agent: `synced N`, `skipped (reason)` (agent dir
  absent / not a directory / `--no-skills`), or `warn (...)` for a
  best-effort partial copy. A skills `warn` never fails the run.

## Notes

- The global store is **migrated only if it already exists**. `upgrade`
  never creates it — opting into machine-global memory stays the
  explicit `memhub global enable` choice (see `/global`).
- The registry (`known_projects`) is machine-local and **not**
  exported by `memhub export`; on a new machine it simply re-populates
  as repos are opened.
- Registry membership is **not** M9 opt-in — recall never reads it and
  stays gated on each repo's own `[global] enabled`.
- Skill resync only writes into an agent dir that **already exists** —
  same conservative rule as the PATH-shadow and global-store steps. It
  never creates `~/.claude/commands` or `~/.codex/skills`.
- On Windows, symlink creation may need privilege; the shadow fix
  falls back to a copy (re-run `memhub upgrade` after each install).

## Windows: the staged relaunch (and what agents must do)

Windows locks a running `.exe`, so the process that invokes `memhub
upgrade` cannot have its own binary replaced by `cargo install`. The
command handles this automatically: it copies itself to a `%TEMP%`
shim, relaunches that with `--staged`, and the original exits so its
lock releases. **Consequence: the invoking shell receives exit code 0
before the upgrade has actually finished or failed.**

- **Interactive (a TTY is attached):** staging happens automatically.
  Watch the staged run's streamed output and its final
  `memhub upgrade: SUCCESS|FAILED` line.
- **Non-interactive (CI, or an agent invoking it — no TTY):** the
  command **refuses by default** with an explanatory error rather than
  losing the exit code silently. Re-run with `--allow-self-stage` to
  permit it.

Because exit code 0 is not a success signal on a staged run, **do not
report success from the exit code**. The real outcome is durably
recorded at `~/.memhub/last_upgrade.json`
(`{"ok":bool,"summary":"...","unix_ms":...}`): a fresh `ok:true` is
success; `ok:false` with "completion not yet recorded" means it is
still running or was killed mid-run; any other `ok:false` is a real
failure. Poll that file (and check `unix_ms` is newer than when you
launched) to determine the result.

Non-Windows platforms never stage — this whole section is inert there.
