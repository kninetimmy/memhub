---
name: doc
description: Ingest an external markdown reference doc (design spec, API contract) into memhub so it is RAG-searchable in chunks. Opt-in to recall — never pollutes the default bundle.
framework: memhub
framework_version: 1.0.0
last_updated: 2026-05-16
---

Ingest a local markdown file into this repo's `.memhub/project.sqlite`
as an external reference document. The file is chunked by heading
(fenced code blocks kept intact) and each chunk is embedded, so it is
retrievable through the same SQL+RAG hybrid recall as facts, decisions,
and tasks — but **opt-in only**: doc chunks never appear in the default
recall bundle, so normal project recall stays clean.

Use this when the user wants to "pull bits and pieces" of a long doc
across a working session instead of pasting the whole thing into
context.

## Preconditions

- `.memhub/` exists in the working repo (run `/check-init` if unsure).
- `memhub` binary on PATH.
- The file is a local markdown file the user has pointed you at.

If a precondition is missing, surface it and stop.

## Invocation

Prefer the `memhub.doc_add` MCP tool when available — structured JSON,
no shell quoting:

```
memhub.doc_add(file="<absolute or repo-relative path>.md")
```

Optional `title="<override>"` — defaults to the first heading or the
file name.

CLI fallback:

```bash
memhub doc add "<path>.md" --json
```

Other verbs (CLI; MCP exposes only `doc_add`):

```bash
memhub doc ls --json                 # list ingested docs
memhub doc show <id|path> --json     # metadata + chunk breadcrumbs
memhub doc rm <id|path> --json       # remove a doc + its chunks
```

## Re-ingest semantics

`doc add` on a path already ingested:

- **Unchanged content** (same SHA-256) → no-op, `status: "unchanged"`.
- **Changed content** → every chunk is replaced (old embeddings + FTS
  cleaned), `status: "updated"`.

So re-running after the user edits the source file is safe and cheap;
just run it again.

## Retrieving from the doc

Docs are **opt-in**. To pull from an ingested doc, scope recall to
docs explicitly:

```
memhub.recall(query="<question>", source_types=["doc"])
```

In normal recall (no `source_types`), the response carries an
`available_docs` count. When it is non-zero and the question is
design/spec/architecture-flavored, the `/recall` skill's guidance
applies: run one follow-up doc-scoped recall before answering. Use
judgment — not every turn.

Doc hits return with `source_type: "doc_chunk"` and a `title` of
`<document title> — <section breadcrumb>`. Cite the document and
section when you use one.

## Scope boundary

memhub is not a general knowledge base. Ingested docs are per-repo,
user-pointed reference material scoped to this repo's work — that is
why they are opt-in and excluded from the default bundle. Don't ingest
arbitrary files speculatively; ingest what the user asks you to.

## Notes

- `doc_add` is a direct write (no review queue): a doc is a
  user-pointed artifact, not an agent claim. It is recorded in
  `writes_log` like any other write.
- Doc content is **not** included in `memhub export` — it is a
  disk-backed, re-ingestable cache. On another machine, re-run
  `doc add` against the same file.
- Embeddings populate only in `hybrid` retrieval mode. In `fts` mode
  ingestion still works (chunks + FTS index); vector recall for docs
  starts after `memhub index rebuild` once the repo is on hybrid.
