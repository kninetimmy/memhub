---
name: upgrade
description: Rebuild + install memhub and bring every memhub instance on this machine (each known repo DB + the machine-global store) to head schema, with a one-time fix for the ~/.local/bin PATH shadow. Run from the memhub source repo.
framework: memhub
framework_version: 1.0.0
last_updated: 2026-05-18
---

`memhub upgrade` is the one dependable command to make **every** memhub
install on this machine coherent after a code change — not just
whichever repo you rebuilt from. It rebuilds + installs the binary,
fixes the stale-`~/.local/bin` PATH shadow once and for all, then
migrates and smoke-tests every known repo DB plus the machine-global
store, printing a per-instance ready table.

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
memhub upgrade            # rebuild+install, fix PATH shadow, migrate+verify all
memhub upgrade --dry-run  # show what would happen; NO install/symlink/migration
memhub upgrade --json     # machine-readable instance table
memhub upgrade --yes      # don't prompt before replacing a non-symlink shadow
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
3. Re-execs the freshly installed binary for the migrate+verify pass
   (migrations must run under new code).
4. Per instance: open (auto-migrates) → compare schema → tiny FTS
   recall smoke → table row.

## Reading the table

```
  INSTANCE                    SCHEMA                       STATUS
  ~/memhub                    0014_documents -> 0015_...   migrated
  ~/src/projalpha             0015_known_projects          ready
  ~/work/legacy               (none)                       skipped (no memhub project)
  <global store>              0015_known_projects          ready

  3/4 instances ready
```

- `ready` already at head; `migrated` was behind, now at head.
- `skipped` nothing to do (path gone, or global store absent because
  no repo opted into M9).
- `ERROR` opened/verified and failed; the command exits non-zero.

## Notes

- The global store is migrated **only if it already exists**. `upgrade`
  never creates it — that stays the explicit `memhub global enable`
  choice.
- `known_projects` is machine-local and **not** exported by
  `memhub export`; it re-populates as repos are opened on a new
  machine.
- Registry membership is **not** M9 opt-in — recall never reads it and
  stays gated on each repo's own `[global] enabled`.
- On Windows the shadow fix falls back to a copy if symlink creation
  needs privilege (re-run after each install).
