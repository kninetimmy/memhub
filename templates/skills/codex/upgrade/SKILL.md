---
name: upgrade
description: Rebuild + install memhub and bring every memhub instance on this machine (each known repo DB + the machine-global store) to head schema, resync installed agent skill wrappers, with a one-time fix for the ~/.local/bin PATH shadow. Run from the memhub source repo.
framework: memhub
framework_version: 1.0.0
last_updated: 2026-05-18
---

`memhub upgrade` is the one dependable command to make **every** memhub
install on this machine coherent after a code change — not just
whichever repo you rebuilt from. It rebuilds + installs the binary,
fixes the stale-`~/.local/bin` PATH shadow once and for all, resyncs
the installed slash-command skill wrappers, then migrates and
smoke-tests every known repo DB plus the machine-global store, printing
a per-instance ready table.

This is the Codex counterpart to the Claude Code `/upgrade` skill; both
drive the same `memhub upgrade` CLI and differ only in agent telemetry.

## When to reach for it

After pulling memhub changes that bump the schema or change behavior,
when you want **all** repos + the global store ready. This reinstalls
the binary and may rewrite a symlink — **suggest it and let the user
run it**; do not run it autonomously.

## Preconditions

- Run from the **memhub source repo** (it rebuilds from source and
  errors if `Cargo.toml` is not the `memhub` package).
- `cargo` on PATH.

## Enumeration is a registry, not a scan

memhub keeps a self-maintaining list of every repo it has actually
opened in `~/.memhub/global.sqlite` (`known_projects`). `upgrade`
iterates that — deterministic, never a filesystem scan. A repo memhub
has never opened since this feature landed is absent from the first
run's report but self-migrates on its next open anyway. Seed one
explicitly:

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

Show the user `--dry-run` first if there is any doubt about what will
change.

## What it does, in order

1. `cargo install --path . --force` (aborts the run on build failure —
   no half-upgrade).
2. **PATH-shadow fix:** a regular-file `~/.local/bin/memhub` shadowing
   `~/.cargo/bin/memhub` is replaced **once** with a symlink so future
   installs just work. Already-a-symlink → idempotent no-op. A
   non-symlink shadow is replaced only after y/N confirm (or `--yes`);
   declined prints the exact manual `ln -sf` command.
3. **Skill-wrapper resync (decision 97):** for each agent dir that
   *already exists* — `~/.claude/commands/` (flat `*.md`),
   `~/.codex/skills/` (dir-per-skill) — copies the source repo's
   `templates/skills/{claude,codex}/` over the installed wrappers so
   they never lag the binary. Additive (a skill removed from
   `templates/` leaves a harmless installed orphan; shared user-global
   dirs are never pruned), idempotent, best-effort (a partial/permission
   error is a `warn` row, never fatal). A non-existent agent dir is
   skipped, never created. `--no-skills` skips it. Internalizes the old
   manual `cp` recipe, now a fallback only.
4. Re-execs the freshly installed binary for the migrate+verify pass
   (migrations must run under new code).
5. Per instance: open (auto-migrates) → compare schema → tiny FTS
   recall smoke → table row.

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

- `ready` already at head; `migrated` was behind, now at head.
- `skipped` nothing to do (path gone, or global store absent because
  no repo opted into M9).
- `ERROR` opened/verified and failed; the command exits non-zero.
- `skills:` rows — per-agent `synced N`, `skipped (reason)` (agent dir
  absent / not a directory / `--no-skills`), or `warn (...)` for a
  best-effort partial copy. A skills `warn` never fails the run.

## Notes

- The global store is migrated **only if it already exists**. `upgrade`
  never creates it — that stays the explicit `memhub global enable`
  choice.
- `known_projects` is machine-local and **not** exported by
  `memhub export`; it re-populates as repos are opened on a new
  machine.
- Registry membership is **not** M9 opt-in — recall never reads it and
  stays gated on each repo's own `[global] enabled`.
- Skill resync only writes into an agent dir that **already exists** —
  same conservative rule as the PATH-shadow and global-store steps; it
  never creates `~/.claude/commands` or `~/.codex/skills`.
- On Windows the shadow fix falls back to a copy if symlink creation
  needs privilege (re-run after each install).

## Windows: the staged relaunch (and what agents must do)

Windows locks a running `.exe`, so the process invoking `memhub
upgrade` cannot have its own binary replaced by `cargo install`. The
command auto-handles this: it copies itself to a `%TEMP%` shim,
relaunches that with `--staged`, and the original exits to release its
lock. **The invoking shell therefore gets exit code 0 before the
upgrade has finished or failed.**

- **Interactive (TTY attached):** staging is automatic; watch the
  streamed output and the final `memhub upgrade: SUCCESS|FAILED` line.
- **Non-interactive (CI, or an agent — no TTY):** refuses by default
  with an explanatory error instead of losing the exit code silently.
  Re-run with `--allow-self-stage` to permit it.

Do **not** treat exit 0 as success on a staged run. The real outcome
is durably recorded at `~/.memhub/last_upgrade.json`
(`{"ok":bool,"summary":"...","unix_ms":...}`): fresh `ok:true` is
success; `ok:false` "completion not yet recorded" means still
running/killed; any other `ok:false` is a real failure. Poll that file
and check `unix_ms` is newer than your launch time.

Non-Windows platforms never stage — this section is inert there.
