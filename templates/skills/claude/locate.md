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

Default: prefer the `memhub.locate` MCP tool when available — it
returns structured JSON and avoids shell quoting.

```
memhub.locate(query="<what the code does, in plain language>")
```

CLI fallback (no MCP, or to pipe to other shell tools):

```bash
memhub locate "<query>" --json
```

Drop `--json` for the human-readable form when the user wants to read
it directly.

## Freshness is automatic

Every locate first does a **lazy refresh** of the index against the
working tree. When nothing changed this is stat-only (no re-read, no
re-embed), so a warm index costs almost nothing; an edited or
never-indexed file is picked up transparently on the next call. You do
not need to run `memhub code index` first — that command is only an
explicit up-front warm-up / rebuild.

## Parameters

- `limit=N` / `--limit N`: max results. Default 10.
- `rerank=true` / `--rerank`: run the bundled cross-encoder over the
  candidate pool before truncating. **Off by default** — fusion
  (FTS + vector, no reranker) is the calibrated default and wins
  Recall@3 on memhub's own golden set (decisions 122/123); `--rerank`
  is a legitimate opt-in that instead wins single-best-guess Recall@1.
  Pass it when the user wants the single best guess rather than a
  fuller candidate set. Ignored in `fts` mode (no embedding pool to
  reorder).

## Interpreting the response

```json
{
  "query": "parse the manifest",
  "mode": "fts" | "hybrid",
  "results": [
    {
      "rank": 1,
      "path": "src/parser.rs",
      "start_line": 12,
      "end_line": 30,
      "symbol": "parse_manifest",
      "kind": "function",
      "score": 0.91,
      "fts_score": 1.0,
      "vector_score": 0.62,
      "rerank_score": null,
      "snippet": "pub fn parse_manifest() -> Result<Manifest> {…"
    }
  ],
  "candidate_count": 14,
  "returned_count": 1,
  "reranked": false,
  "files_total": 42,
  "chunks_total": 318,
  "head": "1f3bb5d…",
  "elapsed_ms": 47
}
```

- `path` is repo-relative, forward-slashed; `start_line`/`end_line`
  are 1-indexed inclusive. Cite hits as `path:start-end`.
- `symbol` is the function/struct/etc. name when the chunk is
  symbol-aware; `null` for plain line-window chunks.
- `kind` is the chunk tag (`function`, `struct`, `line-window`, …).
- `score` is the blended fusion rank; `fts_score` and `vector_score`
  are the normalized components. `rerank_score` is the cross-encoder
  logit, present only when `rerank` ran.
- `snippet` is a **clipped excerpt** (≤6 lines, ≤400 chars, trailing
  `…` when cut) — enough to confirm the hit, never the full chunk.
  Open the file at the line range to read the rest.
- `reranked` reports whether the cross-encoder actually ran this call.

## Empty results

When `results` is empty, say so and quote the exact query. Offer to
rephrase (the FTS side AND-matches tokens, so fewer / more central
terms often help) or, in `fts` mode, note that switching the repo to
`hybrid` would add semantic matching once embeddings are built.

Never invent a `file:line` to fill an empty result.

## When not to use locate

- For project memory (decisions, facts, tasks) use `/recall`.
- For exact git file history use `memhub search "file:src/foo.rs"`.
- To read or edit code, use your own file tools on the breadcrumbs
  locate returns — it is read-only and snippet-clipped by design.
