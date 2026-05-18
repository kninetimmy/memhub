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
`CLAUDE.md` idea, made retrievable instead of always-loaded.

Local-only, per-machine, offline. The global store is never exported,
never synced. Each machine maintains its own.

## Preconditions

- `.memhub/` exists in the working repo (run `/check-init` if unsure).
- `memhub` binary on PATH.

## Enablement (per-repo, off by default)

A repo opts in explicitly. This both creates the store on first enable
anywhere on the machine and gates this repo's global reads + writes.

```bash
memhub global enable        # opt this repo in; create the store if absent
memhub global disable       # opt out (non-destructive; store kept)
memhub global status --json # enabled?, path, schema version, row counts
```

When disabled (or the store is absent), recall is byte-identical to a
pre-M9 build and every global write refuses with a hint.

## What belongs in global (the anti-noise rule)

Routing is **user-gated and never agent-automatic.** A wrong write
pollutes every repo on the machine, so promote with judgment:

- **Global facts** — machine/toolchain truths, install/env commands,
  cross-repo personal conventions, agent-collaboration preferences.
- **Global decisions** — standing engineering policy applied
  everywhere ("always integration-test against a real DB"), not a
  per-repo architecture call.
- **Global docs** — broadly-applicable guides: a universal coding
  style guide, language idioms. A guide only one repo follows stays a
  plain repo `/doc` ingest.
- **Never global** — tasks, rendered narrative state, and anything
  naming a repo-specific path/symbol/architecture.

## Writing to global (user-gated, human-typed)

Born-global writes (require this repo enabled):

```bash
memhub fact add <key> <value> --global
memhub decision add "<title>" --rationale "<why>" --global
memhub doc add <path/to/guide.md> --global
```

Promote an existing repo row (copy, not move — the repo row stays and
still wins locally):

```bash
memhub fact promote <id> --global
memhub decision promote <id> --global
```

Manage global docs with the same `--global` flag on the other `doc`
verbs (global doc ids are per-global-DB, independent of repo ids):

```bash
memhub doc ls --global               # list global docs
memhub doc show <id|path> --global   # metadata + chunk breadcrumbs
memhub doc rm <id|path> --global     # remove a global doc + chunks
```

Fact keys are UNIQUE per store, so re-promoting a key updates the
global fact. Decisions have no natural key — re-promoting duplicates
and the CLI warns. The first global write prints a one-time disclosure
naming the store path.

## The agent path: propose, never write

An agent must never write global directly. The only agent route is a
**staged proposal a human accepts**:

```
memhub.propose_fact(key=..., value=..., rationale=..., global=true)
memhub.propose_decision(title=..., rationale=..., global=true)
```

This lands in *this repo's* `pending_writes` tagged `target:"global"`.
It becomes durable in `~/.memhub/global.sqlite` only when the user
runs `memhub review accept <id>` (and only while the repo is still
enabled). There is no `global` parameter on `memhub.doc_add` — a
born-global doc must be human-typed via the CLI / `/doc`.

Default to proposing global only when the user explicitly frames
something as machine-wide. Repo is the safe default; global is the
deliberate, user-confirmed exception.

## Scope provenance in recall

Every recall hit carries `scope`: `"repo"` or `"global"`. Precedence
is provenance-tag-only — recall never drops a hit for being global and
does no automatic conflict resolution. Apply **repo-overrides-global**
yourself: if a repo hit and a global hit conflict, the repo answer is
authoritative for this repo (exactly as repo `CLAUDE.md` overrides
global `CLAUDE.md`). Cite the scope when it matters.

## Onboarding toggles (related)

Two switches are explicit install-time choices:

- `[retrieval] mode = fts|hybrid` — hybrid adds embeddings + reranker.
- machine-global store — `memhub global enable` (this skill).

Two auto-follow with a manual override:

- `[retrieval] use_reranker` — auto-on with hybrid (FTS-only bypasses
  it). Turn off only to skip the rerank cost.
- `[retrieval] include_docs_in_default` (and its
  `[global]` mirror) — auto-flips true on the first `doc add` /
  `doc add --global`. Set false to keep docs strictly opt-in.

## Notes

- `enabled` lives in `.memhub/config.toml` `[global]`; the tracked
  `.memhub/config.example.toml` baseline ships `false`.
- The global store's embeddings populate only when the writing repo is
  in `hybrid` mode; an fts-mode write leaves the global row FTS-only
  until a future reindex. Recall degrades gracefully (FTS scoring) for
  such rows.
- Global memory is **not** exported by `memhub export`. On another
  machine, re-enable and re-add / re-ingest from source.
