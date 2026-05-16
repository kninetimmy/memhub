---
name: doc
description: Ingest an external markdown reference doc into memhub so it is RAG-searchable in chunks. Opt-in to recall — never pollutes the default bundle.
framework: memhub
framework_version: 1.0.0
last_updated: 2026-05-16
---

Ingest a local markdown file into this repo's `.memhub/project.sqlite`
as an external reference document, chunked by heading and embedded so
it is retrievable through the same SQL+RAG hybrid recall as facts,
decisions, and tasks — but **opt-in only**: doc chunks never appear in
the default recall bundle.

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

## Re-ingest semantics

`doc add` on an already-ingested path: unchanged content (same
SHA-256) is a no-op (`status: "unchanged"`); changed content replaces
every chunk and refreshes embeddings/FTS (`status: "updated"`). Safe to
re-run after the user edits the file.

## Retrieving from the doc

Docs are opt-in — scope recall to docs explicitly:

```
memhub.recall(query="<question>", source_types=["doc"])
```

Plain recall returns an `available_docs` count; when it is non-zero
and the question is design/spec/architecture-flavored, run one
follow-up doc-scoped recall before answering (judgment, not reflex).
Doc hits return with `source_type: "doc_chunk"` and a
`<title> — <section breadcrumb>` title.

## Scope boundary

memhub is not a general knowledge base. Ingested docs are per-repo,
user-pointed reference material — opt-in, excluded from the default
bundle. Ingest what the user asks for, not arbitrary files.

## Notes

- `doc_add` is a direct write (no review queue); logged to
  `writes_log`.
- Docs are excluded from `memhub export` (disk-backed, re-ingestable
  cache) — re-run `doc add` on another machine.
- Embeddings populate only in `hybrid` mode; `fts` mode ingests
  chunks + FTS, vector recall starts after `memhub index rebuild`.
