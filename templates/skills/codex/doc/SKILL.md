---
name: doc
description: >
  Ingest an external markdown reference doc into memhub so it is RAG-searchable in chunks. The first ingest in a repo auto-enables doc chunks in default recall (decision 90). Trigger on: "ingest this doc", "add this spec to memhub", "make this doc searchable", "remember this reference doc", "/doc".
framework: memhub
framework_version: 1.0.0
last_updated: 2026-05-18
---

Ingest a local markdown file into this repo's `.memhub/project.sqlite`
as an external reference document, chunked by heading and embedded so
it is retrievable through the same SQL+RAG hybrid recall as facts,
decisions, and tasks. The first successful ingest in a repo
auto-enables `[retrieval] include_docs_in_default` (decision 90), so
afterward a plain recall call can surface a doc chunk directly — gated
by the `[retrieval.scoring] doc_min_rerank_score` relevance floor.

This is the Codex counterpart to the Claude Code `/doc` skill. Both
call into the same `memhub doc` CLI and `memhub.doc_add` MCP tool;
they differ only in the agent identifier on write-side telemetry.

Use this when the user wants to pull pieces of a long doc across a
session instead of loading the whole thing into context.

## Preconditions

- `.memhub/` exists in the working repo (run `/check-init` if unsure).
- `memhub` binary on PATH.
- The file is a local markdown file the user pointed at.

If a precondition is missing, surface it and stop.

## Invocation

Prefer the `memhub.doc_add` MCP tool when available:

```
memhub.doc_add(file="<absolute or repo-relative path>.md")
```

Optional `title="<override>"` — defaults to first heading or file name.

CLI fallback:

```bash
memhub doc add "<path>.md" --json
memhub doc ls --json
memhub doc show <id|path> --json
memhub doc rm <id|path> --json
```

Add `--global` to `ls`, `show`, or `rm` to manage docs in the
machine-global store instead of this repo's (mirrors `doc add
--global`; requires `memhub global enable` in this repo — see
`/global`). Without `--global` these only touch the repo store; global
doc ids are per-global-DB, independent of repo ids:

```bash
memhub doc ls --global --json
memhub doc show <id|path> --global --json
memhub doc rm <id|path> --global --json
```

## Re-ingest semantics

`doc add` on an already-ingested path: unchanged content (same
SHA-256) is a no-op (`status: "unchanged"`); changed content replaces
every chunk and refreshes embeddings/FTS (`status: "updated"`). Safe to
re-run after the user edits the file.

## Retrieving from the doc

Plain recall already surfaces ingested doc chunks once this repo's
first `doc add` has run (decision 90). To restrict a query to docs
alone, scope explicitly:

```
memhub.recall(query="<question>", source_types=["doc"])
```

Plain recall also returns an `available_docs` count — doc chunks that
did NOT surface this call; when it is non-zero and the question is
design/spec/architecture-flavored, a doc-scoped follow-up can still be
worth running (judgment, not reflex). Doc hits return with
`source_type: "doc_chunk"` and a `<title> — <section breadcrumb>`
title.

## Scope boundary

memhub is not a general knowledge base. Ingested docs are per-repo,
user-pointed reference material scoped to this repo's work. Ingest
what the user asks for, not arbitrary files.

## Notes

- `doc_add` is a direct write (no review queue); logged to
  `writes_log`.
- Docs are excluded from `memhub export` (disk-backed, re-ingestable
  cache) — re-run `doc add` on another machine.
- Embeddings populate only in `hybrid` mode; `fts` mode ingests
  chunks + FTS, vector recall starts after `memhub index rebuild`.
