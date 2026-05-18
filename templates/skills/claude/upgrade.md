---
name: upgrade
description: Rebuild + install memhub and bring every memhub instance on this machine (each known repo DB + the machine-global store) to head schema, with a one-time fix for the ~/.local/bin PATH shadow. Run from the memhub source repo.
framework: memhub
framework_version: 1.0.0
last_updated: 2026-05-18
---

`memhub upgrade` is the one dependable command to make **every** memhub
install on this machine coherent after a code change — not just
whichever repo you happened to rebuild from. It rebuilds + installs the
binary, fixes the stale-`~/.local/bin` PATH shadow once and for all,
then migrates and smoke-tests every known repo DB plus the
machine-global store, printing a per-instance ready table.

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
memhub upgrade            # rebuild+install, fix PATH shadow, migrate+verify all
memhub upgrade --dry-run  # show what would happen; NO install/symlink/migration
memhub upgrade --json     # machine-readable instance table
memhub upgrade --yes      # don't prompt before replacing a non-symlink shadow
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
3. Re-execs the **freshly installed** binary for the migrate+verify
   pass, so migrations run under new code (old code only knows old
   migrations).
4. For each instance: open (auto-applies migrations) → compare schema
   → tiny FTS recall smoke → row in the table.

## Reading the table

```
  INSTANCE                    SCHEMA                       STATUS
  ~/memhub                    0014_documents -> 0015_...   migrated
  ~/src/projalpha             0015_known_projects          ready
  ~/work/legacy               (none)                       skipped (no memhub project)
  <global store>              0015_known_projects          ready

  3/4 instances ready
```

- `ready` — already at head. `migrated` — was behind, now at head.
- `skipped` — known/asked-for but nothing to do (path gone, or the
  global store is absent because no repo opted into M9).
- `ERROR` — opened/verified and failed; the command exits non-zero so
  scripts notice. Investigate that instance.

## Notes

- The global store is **migrated only if it already exists**. `upgrade`
  never creates it — opting into machine-global memory stays the
  explicit `memhub global enable` choice (see `/global`).
- The registry (`known_projects`) is machine-local and **not**
  exported by `memhub export`; on a new machine it simply re-populates
  as repos are opened.
- Registry membership is **not** M9 opt-in — recall never reads it and
  stays gated on each repo's own `[global] enabled`.
- On Windows, symlink creation may need privilege; the shadow fix
  falls back to a copy (re-run `memhub upgrade` after each install).
