---
name: locate
description: >
  Find where code lives in this repo by intent — SQL+RAG hybrid search over a sibling code index that returns ranked file:line breadcrumbs with clipped snippets. Read-only; never returns full code. Trigger on: "where is X", "where does X live", "find the code that does Y", "which file handles Z", "locate X", "where do I change Y".
framework: memhub
framework_version: 1.0.0
last_updated: 2026-05-26
---

Ask memhub *where code is* before you read it. `locate` runs SQL+RAG
hybrid search (FTS5 BM25 + cosine when the repo is in `hybrid` mode)
over a sibling code index at `.memhub/code_index.sqlite` and returns
ranked breadcrumbs — `path`, line range, symbol, and a **clipped**
snippet. It is a locator, not a reader: it never returns whole files
and never edits.

This is the Codex counterpart to the Claude Code `/locate` skill. Both
call into the same `memhub locate` CLI and `memhub.locate` MCP tool;
they differ only in the agent identifier on any read-side telemetry
the host captures.

Use it to turn "where is the thing that does X?" into concrete
`file:start-end` targets, then open those with your own file tools.
It complements `recall` (project facts/decisions/tasks) — `recall` is
for *what we decided*, `locate` is for *where the code is*.

## Preconditions

- `.memhub/` exists in the working repo (run `/check-init` if unsure).
- `memhub` binary on PATH.
- The repo is a git repo — the index refresh is git-aware.

If a precondition is missing, surface that and stop.

## Invocation

Prefer the `memhub.locate` MCP tool when available — it returns
structured JSON and avoids shell quoting.

```
memhub.locate(query="<what the code does, in plain language>")
```

CLI fallback:

```bash
memhub locate "<query>" --json
```

Drop `--json` for the human-readable form.

## Freshness is automatic

Every locate first does a **lazy refresh** of the index against the
working tree. When nothing changed this is stat-only (no re-read, no
re-embed), so a warm index costs almost nothing; edited or
never-indexed files are picked up transparently. You do not need to
run `memhub code index` first — that is only an explicit warm-up /
rebuild.

## Parameters

- `limit=N` / `--limit N`: max results. Default 10.
- `rerank=true` / `--rerank`: run the bundled cross-encoder over the
  candidate pool. **Off by default** — fusion (FTS + vector, no
  reranker) is the calibrated default and wins Recall@3 on memhub's
  own golden set (decisions 122/123); `--rerank` is the opt-in that
  instead wins single-best-guess Recall@1. Ignored in `fts` mode.

## Interpreting the response

Each hit carries `path` (repo-relative, forward-slashed),
`start_line`/`end_line` (1-indexed inclusive), `symbol` (the
function/struct name, or `null` for line-window chunks), `kind`, the
blended `score` with its `fts_score`/`vector_score` components, an
optional `rerank_score` (present only when `rerank` ran), and a
**clipped** `snippet` (≤6 lines — enough to confirm the hit, never the
full chunk). Cite hits as `path:start-end` and open the file to read
the rest. The bundle also reports `mode`, `candidate_count`,
`returned_count`, `reranked`, `files_total`, `chunks_total`, `head`,
and `elapsed_ms`.

## Empty results

When `results` is empty, say so and quote the exact query. Offer to
rephrase (the FTS side AND-matches tokens, so fewer / more central
terms often help), or note that switching the repo to `hybrid` adds
semantic matching once embeddings are built. Never invent a
`file:line` to fill an empty result.

## When not to use locate

- For project memory (decisions, facts, tasks) use `/recall`.
- For exact git file history use `memhub search "file:src/foo.rs"`.
- To read or edit code, use your own file tools on the breadcrumbs
  locate returns — it is read-only and snippet-clipped by design.
