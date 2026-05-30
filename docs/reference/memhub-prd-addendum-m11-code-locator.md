# PRD addendum: M11 code locator

Status: authoritative for the items it modifies, per decision 12 (the PRD
itself stays verbatim; design evolution ships as dated addenda).

Date: 2026-05-30 (covers decisions 107–120, shipped 2026-05-25 → 2026-05-29).

Design anchors: decisions 107 (locator spine), 108 (chunk/embedding
contract), 109 (auto-refresh), 110 (MCP surface), 114 (reranker-off
default, measured), 115–120 (multi-language AST chunking). Implementation
brief for the chunker hardening pass: `docs/handoff/m11-chunker-fixes.md`.

## What this addendum modifies

The PRD §4 non-goals list "becoming a general knowledge base / second
brain" and treat memhub as a *memory* store — facts, decisions, tasks —
not a codebase search engine. M11 adds a **code locator**: a cheap
semantic file/symbol search over the repo's own source. It brushes the
codebase-RAG line task 44 raised, so the design is deliberately bounded
to keep the non-goal intact:

- The locator is a **physically separate** database. It is never read by
  `memhub recall`, never in `memhub export`, never in M10 sync. The
  recall eval-regression guarantee is preserved *structurally*, not by
  convention — exactly the way M9 keeps registry membership out of recall.
- It is a **locator, not a retriever of code**. It returns ranked
  `path:line-range` breadcrumbs plus a clipped snippet. The agent then
  `Read`s that exact span. Code never enters a recall bundle and the
  locator never edits.

Everything below is additive. No PRD principle (local-first, offline,
intentionally boring) is weakened — the locator runs entirely offline and
reuses the existing embedding model.

## 1. Architecture: an isolated, disposable sibling database (D107)

The index lives at `.memhub/code_index.sqlite`, alongside but independent
of `.memhub/project.sqlite`. Three load-bearing invariants:

1. **Isolation.** Gitignored, per-machine. Never read by recall, never
   exported, never synced. Physical separation is the mechanism that
   guarantees the locator cannot regress recall — there is no code path
   from `recall` into this file.

2. **Derivable + disposable.** No migration framework. The schema is
   created with `CREATE TABLE IF NOT EXISTS` on open plus a
   `schema_version` row in an `index_meta` table; on a version mismatch
   the index is **dropped and rebuilt** rather than migrated. Because the
   whole thing is regenerable from the working tree, a rebuild is free,
   and `memhub upgrade` is a structural no-op for it.

3. **Index set = git ls-files ∩ (not deny-listed).** The walker enumerates
   git-tracked paths and filters them through the existing `PathMatcher`
   deny-list. Untracked scratch files are ignored for free; `.gitignore`
   is respected because git already applied it.

Tables: `indexed_files(path, mtime, size, content_hash, language,
last_indexed_at)`, `code_chunks`, `code_embeddings`, `code_chunks_fts`
(FTS5 mirror kept in sync by schema triggers), `index_meta`.

## 2. Chunking + embedding contract (D108)

The AST chunker emits **one chunk per top-level item** and **one
`Type::method` chunk per method** — the impl/class block is never a single
opaque chunk, so a method query has a precise home. Container types
(C#/Java classes, etc.) additionally emit a **header-only** chunk:
signature + fields + class doc, with each member's body excised to
`{ ... }`, so a "what is this type" query lands without duplicating every
method body (D116).

- **Embed text = `path + kind + name + body`.** Path and symbol kind are
  part of the embedding signal, not just the body.
- **Bodies are LF-normalized** before hashing/embedding, so a CRLF
  re-checkout on Windows does not churn embeddings or trip the staleness
  diff.
- **Embedding is a hybrid-only backfill that runs *after* the chunk
  transaction commits.** Model inference never holds the index write lock.
  A transient embed failure degrades to FTS for that file and self-heals
  on the next refresh — the index is never left in a half-written state.
- **FTS stays in sync via schema triggers, not writer code.** Writers
  touch `code_chunks`; the FTS mirror follows automatically.
- **A grammar-known file that fails to parse falls back to line-window
  chunks** — less precisely sliced, but still indexed. Files with no
  grammar are **excluded from the index** (task 69): the index set is
  scoped to grammar-known source languages (vendored/minified `*.min.*`
  bundles also excluded), because line-windowing docs/lockfiles/JSON let
  non-source files out-rank real code in `locate` (the dominant Recall
  error mode under decision 114). This reversed the earlier "nothing in
  the repo is invisible" property by design.

## 3. Multi-language AST chunking (D115–120)

Chunking started Rust-only behind a `Language → (grammar, node-kinds)`
registry seam, then generalized to six languages in one rollout: **Rust,
Go, Python, TypeScript/JavaScript, Java, C#**.

The generalization is a **hybrid `GrammarSpec`** — declarative role sets
(`function_kinds`, `type_container_kinds`, `method_container_kinds` with a
prefix field, `namespace_kinds`, `item_kinds`, `member_kinds`, comment and
attribute sets, `body_field`) for the uniform ~80%, plus **three
closed-set typed hooks** whose `Standard` defaults reproduce Rust
byte-for-byte:

- `method_naming`: `Standard` | `GoReceiver` (Go methods are top-level
  with a receiver field, so the type prefix comes from the receiver, not
  an enclosing block — D120).
- `function_naming`: `Direct` | `JsDeclarator` (JS/TS arrows and function
  expressions are named via their parent `variable_declarator` or class
  field — D118).
- `doc_fold`: `PrecedingSiblings` | `PythonDocstring` | `None` (Python
  decorators arrive through a `transparent_kind` climb and the docstring
  is the first body expression — D119).

One generic walker reads the spec. A frozen golden snapshot of the Rust
chunk output is the regression backbone: every spec/walker change must
leave Rust output byte-identical (the `Standard`/`Direct`/
`PrecedingSiblings` defaults exist precisely so it does).

Grammar bundling mirrors the reranker model: **all grammars compiled in
unconditionally**, no Cargo feature flag, **ABI-pinned with a per-language
load canary**. Language detection is **extension-only** via
`infer_language` (no content sniffing); `mjs/cjs/mts/cts` map to JS/TS.
No schema migration was needed — `code_chunks.kind` and `symbol` are free
`TEXT`, and the canonical symbol separator stays `::` internally for every
language.

## 4. Staleness: lazy and git-aware (D107, D109)

There is no "remember to reindex" step. Every `locate` first runs a lazy
staleness pass:

1. For each tracked file, diff `(mtime, size)` against `indexed_files`.
2. On a match, skip (stat-only — no read, no embed).
3. On a mismatch, confirm with a content hash, then re-chunk only the
   changed/added files.
4. Drop chunks for files deleted or renamed away.

The **first-ever index is the one expensive pass** — on memhub's own tree,
~171s cold in hybrid mode versus ~1.4s warm. `memhub code index` is the
explicit warm-up so the first interactive `locate` is fast.

Consequence (D109): `locate` is a **read-then-write** op, not strictly
read-only — it refreshes the sibling index before querying. This was
chosen over a query-only + stale-warning model so results always reflect
the working tree. It still writes nothing to `project.sqlite` and logs no
`writes_log` entry.

## 5. Retrieval: fusion default, reranker off (D110, D114, D122)

Recall over the index reuses `embed_one`, `EMBEDDING_MODEL_NAME`, and
`EMBEDDING_DIMENSION` plus FTS5 against the sibling DB. The default is
**fusion: FTS BM25 + vector**, with **the cross-encoder reranker OFF**.

This is a *measured* default, not a guess. PR5 built
`tests/code_locate_golden.json` (18 plain-English queries against memhub's
own tree + 2 nonsense safety probes) and a `memhub eval locate`
Recall@1/@K harness, then A/B-tested the bundled ms-marco cross-encoder on
code:

- The reranker is a **Recall@1 booster but a Recall@3 regressor**. Being
  NL-trained, it promotes prose (`AGENTS.md`, `CLAUDE.md`, `docs/*.md`,
  test files) over implementation files — exactly the unproven-on-code
  risk D107 flagged.
- The locator contract is **right-file-in-the-top-3** (precision is
  recovered by the agent's `Read`), so **Recall@3 governs** and fusion-only
  wins it decisively: 88.9 vs ≤77.8.
- **No safe nonsense floor exists.** True-match logits run as low as
  −5.44 and overlap the gibberish band, so any floor that rejects the
  probes also drops real matches.

So `locate` ships reranker-off with no `min_rerank_score` floor — the
validated default. `--rerank` remains available for a Recall@1-sensitive
single-best-guess caller, and the harness (`memhub eval locate --rerank
[--min-rerank-score]`) is retained so the call can be re-measured if the
index set or reranker model changes.

**Re-measured post-#69 (decision 122, task 75).** The numbers above were
taken on the pre-#69 index, where non-source prose contaminated the
candidate pool. After #69 scoped the index to grammar-known source, the A/B
was re-run on the same golden set: fusion Recall@1 44.4% / Recall@3 88.9%;
rerank (no floor) Recall@1 77.8% / Recall@3 88.9%. The **Recall@3 regression
disappears** — the two tie — because the reranker no longer has prose to
promote; what remains is a Recall@1 gain (44.4%→77.8%) at ~12× the latency.
Recall@3 still governs and is tied, so the **fusion default is unchanged**;
the *reason* shifts from "reranker regresses Recall@3" to "reranker is
Recall@3-neutral and not worth the latency under the top-3 contract." The
no-safe-floor finding is confirmed: a `min_rerank_score` of 0 rejects both
nonsense probes but also drops Recall@3 to 83.3% by killing the −5.44-logit
true match.

## 6. Surfaces (D107, D110)

- **CLI:** `memhub locate <query> [--limit N] [--rerank] [--json]`;
  `memhub code index | status | rm`. `memhub eval locate` is the Recall@K
  harness.
- **MCP (the agent-first surface):** `memhub.locate(query, limit, rerank)`
  returning `path / start_line / end_line / symbol / score / snippet`,
  with full CLI-JSON parity (kind + fts/vector/rerank score components +
  bundle meta). Read-only: clipped snippets only, never full code, never
  edits. `rerank` defaults off, exposed so the A/B can run over MCP
  without a rebuild.
- **Skill:** `/locate` (Claude Code / Codex / OpenCode).

## 7. Non-goals, deferrals, and known limitations

- **Not a recall source, ever.** The locator does not feed the memory
  bundle and is not a "second brain." The §4 non-goal stands.
- **No opt-in gate.** Unlike M9 global memory or M10 sync, the code index
  ships with no `enabled` flag. Because it is isolated + lazy + disposable
  it cannot pollute recall or cost anything until the first `locate`/`code
  index`, so a toggle would be ceremony for zero safety benefit (D107
  open-question (a), resolved to "no gate").
- **Index set is not yet scoped to source files (open, task 69).** The
  walker currently indexes *every* git-tracked, non-deny-listed path. PR5
  found the dominant Recall@3 error mode is non-source files —
  `AGENTS.md`, `CLAUDE.md`, `docs/*.md`, `Cargo.lock`, vendored minified
  JS — out-ranking real implementation files (and the reranker amplifies
  it, part of why it is off by default). Restricting the index set to
  source languages, or a code-index-specific deny-list, is open follow-up
  work and is likely a bigger Recall lever than the reranker ever was.
- **Cross-language eval sweep is open (task 75).** A single
  `memhub eval locate` Recall@K sweep across all six languages plus a
  no-Rust-regression check against the frozen snapshot is the final M11
  validation step.
- **Detection is extension-only by design.** A misnamed or
  extension-less source file line-windows rather than AST-chunks. No
  content sniffing is planned.
