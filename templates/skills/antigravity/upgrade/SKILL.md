---
name: upgrade
description: Rebuild + install memhub and bring every memhub instance on this machine (each known repo DB + the machine-global store) to head schema, resync installed agent skill wrappers, with a one-time fix for the ~/.local/bin PATH shadow. Run from the memhub source repo.
framework: memhub
framework_version: 1.0.0
last_updated: 2026-11-07
---

`memhub upgrade` is the one dependable command to make **every** memhub
install on this machine coherent after a code change — not just
whichever repo you rebuilt from. It rebuilds + installs the binary,
fixes the stale-`~/.local/bin` PATH shadow once and for all, resyncs
the installed slash-command skill wrappers, then migrates and
smoke-tests every known repo DB plus the machine-global store, printing
a per-instance ready table.

This is the Antigravity CLI counterpart to the Claude Code `/upgrade` skill; both
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
memhub upgrade --no-hooks  # skip the session-start hook install (Claude Code, Codex)
memhub upgrade --allow-self-stage # Windows + no TTY (CI/agent): permit the staged relaunch
```

Show the user `--dry-run` first if there is any doubt about what will
change.

## What it does, in order

1. `cargo install --path . --force --locked` (aborts the run on build
   failure — no half-upgrade).
2. **PATH-shadow fix:** a regular-file `~/.local/bin/memhub` shadowing
   `~/.cargo/bin/memhub` is replaced **once** with a symlink so future
   installs just work. Already-a-symlink → idempotent no-op. A
   non-symlink shadow is replaced only after y/N confirm (or `--yes`);
   declined prints the exact manual `ln -sf` command.
3. **Skill-wrapper resync:** refreshes existing Claude commands, Codex
   skills, OpenCode skills and commands, plus Antigravity skills at
   `~/.gemini/config/skills/`. Antigravity is installed only when its
   `~/.gemini/antigravity-cli/` directory already exists; `~/.gemini/`
   alone also belongs to Gemini CLI and is insufficient. The shared skill
   directory may be created for that configured host. Default builds
   skip the hibernated metrics wrapper. Copies are additive and owned
   files are updated only when the ownership manifest proves they are
   unchanged since memhub installed them. Customized/unowned files and
   obsolete wrappers are left in place and reported. A partial copy is
   a best-effort warning. `--no-skills` skips this step.
4. Re-execs the freshly installed binary for the migrate+verify pass
   (migrations must run under new code).
5. Per instance: open (auto-migrates) → compare schema → tiny FTS
   recall smoke → table row.
6. **Session-start hook:** `memhub upgrade` installs the session-start
   hook for Claude Code and Codex (in that same freshly installed pass):
   a user-scope SessionStart entry running `memhub hook session-start`,
   into `~/.claude/settings.json` and `~/.codex/hooks.json` (only for an
   agent whose dir exists; never duplicated; a deleted entry stays out).
   `--no-hooks` skips it.
   Codex runs a new or changed hook only after a one-time approval in
   its `/hooks` screen, so tell the user to approve it there. OpenCode
   and Antigravity get no hook.

## Reading the table

```
  INSTANCE                    SCHEMA                       STATUS
  ~/memhub                    0014_documents -> 0015_...   migrated
  ~/src/projalpha             0015_known_projects          ready
  ~/work/legacy               (none)                       skipped (no memhub project)
  <global store>              0015_known_projects          ready

  skills: claude   ~/.claude/commands     synced N
  skills: codex    ~/.codex/skills        synced N
  skills: antigravity ~/.gemini/config/skills synced N
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
- Skill resync leaves absent legacy host directories untouched.
  Antigravity's existing CLI setup directory permits creating its shared
  skill directory; without that marker, no Antigravity directories are created.
- On Windows the shadow fix falls back to a copy if symlink creation
  needs privilege (re-run after each install).

## Windows: the staged relaunch (and what agents must do)

Windows locks a running `.exe`, so a self-upgrade uses a staged shim.
The invoking process returns **exit 3** when that handoff is pending;
it has not yet finished upgrading. Non-interactive staging requires
`--allow-self-stage`. Interactive staging still needs the completion
result, not an assumed success from the launcher.

After a staged handoff, run `memhub upgrade --verify-last` (or add
`--json`) to inspect the durable result: exit 0 means success, exit 1
means failure or no completion record, and exit 3 means pending. Check
that the recorded launch/result belongs to this invocation. Report a
pending or failed result faithfully; do not rerun the upgrade merely
to poll it. Run the original upgrade as a plain foreground command so
its staged child can complete.

Non-Windows platforms never stage — this section is inert there.
