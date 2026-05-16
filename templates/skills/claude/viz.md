---
name: viz
description: Launch the memhub viz dashboard for the current repo in a browser tab.
framework: memhub
framework_version: 1.0.0
last_updated: 2026-05-15
---

Launch the memhub web dashboard for this repo.

## Preconditions

- `.memhub/` exists in the working repo (run `/check-init` if unsure).
- `memhub` binary on PATH and compiled with the `viz` feature (default build).

## Invocation

```bash
memhub viz --open
```

This starts a local HTTP server (default port 4242) and opens the dashboard
in the default browser. The server runs until you stop it (Ctrl-C).

If the default port is taken, pass an explicit one:

```bash
memhub viz --port 4243 --open
```

## What to tell the user

1. Run the command above in their terminal.
2. The dashboard will open automatically. If it doesn't, navigate to
   `http://localhost:4242` (or the port chosen).
3. To stop the server, press Ctrl-C in the terminal where it's running.

## Error handling

- `memhub viz was compiled out` → the binary was built without the `viz`
  feature. Rebuild with `cargo build --release` (the feature is on by default).
- `No such file or directory: .memhub/project.sqlite` → memhub hasn't been
  initialized in this repo. Run `memhub init` first.
- Port already in use → retry with `--port <other>`.
