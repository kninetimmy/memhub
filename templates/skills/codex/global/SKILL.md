---
name: global
description: Manage memhub machine-global memory — opt this repo in/out, write or promote facts/decisions/docs that should be visible to every repo on this machine, and understand scope provenance in recall.
framework: memhub
framework_version: 1.0.0
last_updated: 2026-05-18
---

Machine-global memory (M9) is an optional second store at
`~/.memhub/global.sqlite`, structurally identical to a repo's
`.memhub/project.sqlite`. When this repo has opted in, `recall` merges
hits from the global store alongside repo hits, each tagged with a
`scope` of `"repo"` or `"global"`. It is the global-vs-repo
`AGENTS.md` idea, made retrievable instead of always-loaded.

This is the Codex counterpart to the Claude Code `/global` skill. Both
drive the same `memhub global` CLI and the same `global` flag on the
`memhub.propose_fact` / `memhub.propose_decision` MCP tools; they
differ only in the agent identifier on captured telemetry.

Local-only, per-machine, offline. The global store is never exported,
never synced. Each machine maintains its own.

## Preconditions

- `.memhub/` exists in the working repo (run `/check-init` if unsure).
- `memhub` binary on PATH.

## Enablement (per-repo, off by default)

```bash
memhub global enable        # opt this repo in; create the store if absent
memhub global disable       # opt out (non-destructive; store kept)
memhub global status --json # enabled?, path, schema version, row counts
```

When disabled (or the store is absent), recall is byte-identical to a
pre-M9 build and every global write refuses with a hint.

## What belongs in global (the anti-noise rule)

Routing is user-gated and never agent-automatic. A wrong write
pollutes every repo on the machine:

- **Global facts** — machine/toolchain truths, install/env commands,
  cross-repo personal conventions, agent-collaboration preferences.
- **Global decisions** — standing engineering policy applied
  everywhere, not a per-repo architecture call.
- **Global docs** — broadly-applicable guides (a universal style
  guide). A guide only one repo follows stays a plain repo `/doc`.
- **Never global** — tasks, rendered narrative state, anything naming
  a repo-specific path/symbol/architecture.

## Writing to global (user-gated, human-typed)

```bash
memhub fact add <key> <value> --global
memhub decision add "<title>" --rationale "<why>" --global
memhub doc add <path/to/guide.md> --global
memhub fact promote <id> --global       # copy, not move
memhub decision promote <id> --global
```

Fact keys are UNIQUE per store (re-promote updates). Decisions have no
natural key (re-promote duplicates; the CLI warns). The first global
write prints a one-time disclosure naming the store path.

## The agent path: propose, never write

Never write global directly. The only agent route is a staged proposal
a human accepts:

```
memhub.propose_fact(key=..., value=..., rationale=..., global=true)
memhub.propose_decision(title=..., rationale=..., global=true)
```

It lands in this repo's `pending_writes` tagged `target:"global"` and
becomes durable in `~/.memhub/global.sqlite` only on
`memhub review accept <id>` (and only while the repo is still
enabled). There is no `global` parameter on `memhub.doc_add`.

Propose global only when the user explicitly frames something as
machine-wide. Repo is the safe default.

## Scope provenance in recall

Every recall hit carries `scope`: `"repo"` or `"global"`. Precedence
is provenance-tag-only — recall never drops a global hit and does no
automatic conflict resolution. Apply repo-overrides-global yourself:
the repo answer is authoritative for this repo on conflict. Cite the
scope when it matters.

## Onboarding toggles (related)

Explicit install-time choices: `[retrieval] mode = fts|hybrid` and
machine-global store (`memhub global enable`). Auto-followers with a
manual override: `[retrieval] use_reranker` (auto-on with hybrid) and
`[retrieval] include_docs_in_default` plus its `[global]` mirror
(auto-flips true on the first `doc add` / `doc add --global`).

## Notes

- `enabled` lives in `.memhub/config.toml` `[global]`; the tracked
  `.memhub/config.example.toml` baseline ships `false`.
- Global embeddings populate only when the writing repo is in `hybrid`
  mode; fts-mode writes leave the global row FTS-only until a reindex.
  Recall degrades gracefully for such rows.
- Global memory is not exported by `memhub export`. On another
  machine, re-enable and re-add / re-ingest from source.
