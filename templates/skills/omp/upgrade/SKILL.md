---
name: upgrade
description: Preview or run the machine-wide memhub upgrade from the memhub source checkout, including OMP skills.
---

Use the memhub source checkout, not the project that requested the upgrade.
Preview with `memhub upgrade --dry-run --no-gc` and show the install target,
known projects and active OMP skill path. It uses a registry, not a disk
scan. For a profile selected with OMP's `--profile` flag in another process,
set `OMP_PROFILE` for the memhub command as well. Ask before machine-wide
changes. With approval run `memhub upgrade`, using `--no-skills`, `--no-hooks`,
or `--no-gc` if the user opts out. OMP registration is neither created nor
repaired. Preserve customized files and report protected and orphaned paths.

On Windows, honor a self-stage refusal. Use `--allow-self-stage` only with
approval when non-interactive staging is required. Exit 3 is a handoff, not
success; resolve it with `memhub upgrade --verify-last`. Report actual
verification rather than the cargo install result alone.
