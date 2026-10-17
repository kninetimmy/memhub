# memhub — operations reference

Operational detail for memhub's subsystems, moved out of the repo `CLAUDE.md` / `AGENTS.md` orientation files so those stay lean at session start (Wave 2 token diet, issue #30). Nothing here is load-bearing from turn one — the two orientation files keep the must-have-inline set (Session Continuity, Guardrails, Delegation, the `stale_embeddings` and `sync_adopt` safety gates, Build/Test/Run). Everything below is **memhub-recall-searchable**: this file is ingested with `memhub doc add`, so recall it on demand instead of loading it every session.

This document is content-preserving: the sections below are the full prose that used to live inline, verbatim, with every fact, decision number, and command name intact. When a subsystem changes, update it here and re-ingest.

## Cross-machine workflow

memhub state is **per-machine**. Each machine has its own
`.memhub/project.sqlite`, its own embeddings, and its own rendered
markdown under `.memhub/rendered/`. None of that is committed to git —
only code, migrations, and the static tracked `CLAUDE.md` / `AGENTS.md`
guardrails are.

**After `git pull` on a fresh or existing machine:**

```bash
cargo build --release
cargo run --release -- status   # first call auto-applies pending
                                # migrations from migrations/*.sql
```

`db::open_project` runs `migrations::apply_all` on every invocation;
migrations are idempotent against `schema_migrations`, so no manual
step is needed even if the schema bumped on another machine.

**To carry memory between machines (e.g. continue on Windows what you
started on Mac):**

```bash
# on the source machine
memhub export ~/transfer/memhub-<repo>-<date>.json

# move the file via Drive / USB / scp — memhub itself stays offline

# on the target machine, with an existing memhub project
memhub import ~/transfer/memhub-<repo>-<date>.json          # refuses
                                                            # if target
                                                            # has data
memhub import ~/transfer/memhub-<repo>-<date>.json --force  # overwrite

# or to bootstrap a target that has no DB yet
memhub init --from-backup ~/transfer/memhub-<repo>-<date>.json
```

After import, the target's embeddings are not yet built (only the
rows). Run `memhub index` to populate them — the import output
prints this hint. Until then, recall falls back to FTS-only and may
miss vector-similar matches.

If recall later surfaces a `stale_embeddings` warning (most likely
after an embedding-model upgrade on either machine), follow the same
rule as everywhere else: surface it and ask before invoking
`/reindex`.

The export format is JSON v1, additive: older exports import cleanly
into newer builds via `#[serde(default)]` on later-added fields. The
format is defined in `src/export/v1.rs`.

**Config baseline travels with the repo.** The canonical defaults
live in `.memhub/config.example.toml`, which is the **only** file
inside `.memhub/` that is tracked by git. A fresh `memhub init` (or
the first `open_project` call on a machine with no local config) copies
the example verbatim into `.memhub/config.toml`. The local file stays
gitignored and per-machine; edit the example to change the baseline
for every machine. Fields that should not drift (deny_list, retrieval
weights, render output dir, integrations) are documented at the top
of the example as commit-back-here fields.

## Maintenance-on-open contract

`db::open_project` performs full maintenance as a side effect of every
call, not only when a command is about to write (decision 161; audit
C6, task 120). On every open it: applies any pending schema migrations
(see above), upserts the project's `projects` row, records the open in
the machine-wide upgrade registry (best-effort, debounced; see
"Machine-wide upgrade" below), runs the opportunistic metrics
scrape/maintenance pass when the `metrics` feature is enabled, and
auto-expires aged `pending_writes` rows via the same logic `memhub
review expire` runs manually (Wave 3 Q6, PRD §11.3; see
`commands::review::auto_expire_best_effort`).

This is intentional, not an oversight: every command goes through the
same `open_project` path, including read-only commands — `recall`,
`status`, `doctor`, `eval` — so they perform the same maintenance a
write command would. The migration apply and the `projects` upsert are
load-bearing and fail-closed by design: an error from either one
propagates and fails the calling command, read or not — that is the
project's "fail loudly" guardrail, not an oversight to fix. Only the
upgrade-registry recording, the metrics scrape/maintenance, and the
pending-write auto-expiry are best-effort, either idempotent or
debounced, so none of those three can turn a read into a failure; a
`memhub status` or `memhub recall` call that happens to run right
after a schema bump, or with a 31-day-old pending write sitting
around, quietly brings the DB up to date as a byproduct once the
migration itself has succeeded. There is deliberately no
`open_project_readonly` / `open_project_maintained` split — one open
path keeps every command's behavior consistent and is simpler to
reason about than a two-path one, at the cost of a read command
occasionally doing the small amount of writing (a migration, a
registry upsert, an expiry pass) that a purist "reads never write"
rule would forbid. This contract covers the repo DB only: the
machine-global store follows the opposite rule for migrations (see
"Machine-global memory" below).

## Cross-machine Drive sync

Milestone 10 (design anchor:
[docs/reference/memhub-prd-addendum-m10-drive-sync.md](docs/reference/memhub-prd-addendum-m10-drive-sync.md))
makes one user's repo memory follow them between their own machines
through a synced folder, **without memhub ever going online**. It is
the export/import flow above, automated and made fast-forward-aware.

**Model (decisions 102/103/104).** Whole-DB **snapshot**, not row
merge: each push writes a consistent single-file DB copy (`VACUUM
INTO`) plus a `manifest.json` (the exact on-disk files and the
crash-safe write order are in "Remote layout and publication
atomicity" below). Divergence is decided from a **logical
version** (a digest of the durable content tables, never file bytes —
SQLite is byte-unstable), so `check` reports a git-style verdict:
`up-to-date` / `local-ahead` / `drive-ahead` / `diverged` /
`no-remote`. The single lossy case (both sides changed) is **operator-
gated**, never automatic. Scope is deliberately **single-user across
their own machines**: last-writer-wins on a diverged history is an
accepted cost; no snapshot-history/undo buffer, no multi-user plumbing
— do not re-propose either (decision 103).

**Transport is an OS-level synced folder, NOT the Drive MCP connector
(decision 104).** memhub stays fully offline and only reads/writes a
local path. Google Drive for Desktop (macOS/Windows) or an rclone
mount (Linux) does the byte movement out of band — so writing a
snapshot *into* the synced folder *is* the push. (The base64-over-MCP
courier framing in the addendum is superseded: a 2.8 MB snapshot is
~987K tokens per transfer.)

**Canonical remote path** is resolved in code (`sync::resolve_remote_dir`),
no longer hand-concatenated: `<drive_subpath>/memhub/<project_id>`,
where `drive_subpath` is the absolute synced-folder mount in `[sync]`
and `project_id` derives from the git remote (or an explicit `[sync]
project_id` override for a no-remote repo). CLI sync commands default
to it when the path arg is omitted.

**Per-repo opt-in** (mirrors `memhub global enable`): `memhub sync
enable`. `enabled` + `drive_subpath` live in `.memhub/config.toml`
`[sync]`; the tracked `.memhub/config.example.toml` baseline ships
`enabled = false`. When disabled, every sync command refuses.

Surfaces:
- CLI: `memhub sync enable|disable|status|snapshot|check|adopt|commit`.
  A push is just `snapshot`: writing to the resolved canonical remote
  dir records the push baseline itself, so a later `check` reads
  up-to-date on equal local/remote logical versions with no second
  step. (Snapshotting to a non-canonical destination — an inspection
  copy, a test fixture dir — leaves the marker untouched, the
  pre-existing fail-closed behavior.) `commit` is no longer part of
  the routine push; it exists to verify or repair a baseline after
  the fact, and **refuses when local's logical version does not equal
  the remote manifest's** — recording "local equals the pushed
  snapshot" as a baseline when that is false would let a later plain
  `snapshot` read `local-ahead` and clobber another machine's push
  through the gate (so `commit` on a diverged remote errors; reconcile
  with `adopt`/`snapshot` first). A pull is `check` then `adopt --yes`.
  `status` shows the resolved `remote dir`.
- MCP (the agent-first surface): `memhub.sync_status`,
  `memhub.sync_snapshot`, `memhub.sync_check`, `memhub.sync_commit`,
  and `memhub.sync_adopt`. All default the target to the canonical
  path; pass `remote` to override. **`sync_adopt` is gated**: it
  overwrites the local DB (the one destructive op), so without
  `confirm=true` it returns the would-change verdict and refuses —
  surface that to the user and only re-call with `confirm=true` after
  they approve. Hard refusals regardless of confirm: project-id
  mismatch, a snapshot schema newer than this binary (run `memhub
  upgrade`), or a checksum that disagrees with the manifest. (This
  gate is also kept inline in `CLAUDE.md` / `AGENTS.md` under Safety
  gates — it is the one destructive sync op.)
- Skill: `/catch-up` orchestrates the pull side (check → summarize →
  adopt with your approval).

How `adopt` installs the snapshot (F4/X6): it stages the remote
snapshot into a local `.memhub/project.sqlite.incoming` copy, hashes
**that** copy, and verifies it against the manifest — so a snapshot
Drive rewrites between hashing and install can never be installed
unverified. It then takes a `VACUUM INTO` safety copy of the current DB
to `.memhub/backups/sync/last-replaced.sqlite` (single slot,
WAL-inclusive) and restores the staged snapshot's pages into the live
DB **in place** through SQLite's online-backup API. It never deletes or
renames the DB (or its `-wal`/`-shm`) out from under a process that may
hold it open: on Windows that would raise a sharing violation; on POSIX
it would silently orphan another connection onto a stale inode/WAL.
Concurrent memhub processes therefore serialize through SQLite's own
locking. If the live DB is held open by another writer for the whole
restore window, adopt retries briefly and then refuses cleanly, leaving
the original DB — and the pre-adopt backup — intact; nothing torn or
half-replaced is ever observable.

**Remote layout and publication atomicity (audit F6/X5).** A push no
longer overwrites a single fixed `project.sqlite`. It writes an
**immutable, content-addressed** snapshot named `project-<sha256>.sqlite`
(the sha is the snapshot file's own hash), and `manifest.json` carries
the exact `snapshot_filename` it references. The write order is
crash-safe: `VACUUM INTO` a same-dir temp → rename it to the versioned
name (a *new* file, so the snapshot the live manifest still points at is
untouched) → write `manifest.json` via same-dir temp + atomic rename
**last**. That final rename is the single publication point; interrupt
anywhere before it and the remote still holds the *previous* valid
snapshot + manifest pair. After a successful publication, snapshots the
new manifest no longer names (older versioned files, a legacy bare
`project.sqlite`, interrupted temps) are garbage-collected best-effort.
Reads resolve the snapshot through the manifest's `snapshot_filename`;
the field is additive (`#[serde(default)]`), so a **legacy** Drive
folder — a bare `project.sqlite` with a manifest that lacks the field —
falls back to that legacy name and stays readable/adoptable, and an
older binary reading a new manifest simply ignores the extra field. The
`sync_marker.json` baseline is likewise written same-dir temp + atomic
rename, and a torn/unparseable marker degrades to a logged warning plus
"no baseline" (sync stays usable) rather than hard-erroring every op.

`known_projects`/registry membership and the M9 global store are
**unrelated** to sync. Sync state (`[sync]`, the `sync_marker.json`
baseline) is per-machine and **not** exported by `memhub export`.

**One-time baseline-stale note after a digest change (audit F2).** The
logical-version digest was widened to cover `documents`/`doc_chunks` and
`facts.kind`/`superseded_by`, and its row encoding was made unambiguous
(length-prefixed, NULL distinct from `''`). This changes the digest of
*every* DB, so the first `check` on each machine after upgrading past
this change compares the newly-computed local digest against a
`sync_marker.json` baseline written by the old encoding — they will not
match, so a repo that was `up-to-date` may read `diverged` (no shared
baseline) exactly once. This is expected and harmless: re-establish the
baseline the normal way — a `snapshot` into the canonical remote dir
(push records its own baseline) or a `check` → `adopt --yes` (pull), or
`commit` against the current remote — and subsequent checks read
correctly again. No data is lost; only the stale marker is replaced.

## Retrieval

Hybrid recall (the default) combines FTS5 BM25 and BGE-small embeddings,
then by default runs a bundled cross-encoder re-ranker
(ms-marco-MiniLM-L-6-v2) over `[retrieval] rerank_candidate_pool`
candidates, chosen as described under "Re-rank pool" below, before
truncating to `max_results`. A recall takes about 240 ms with the re-ranker
(about 380 ms with variants; medians of the live-set A/B at pool 20), in
exchange for ~+17 percentage points of Recall@1 on memhub's own golden set
(decision 68).

**Re-rank pool (issue #337).** The rows sent to the re-ranker are not the
top `rerank_candidate_pool` by blended score. Blended score min-max
normalizes BM25 over the hits and adds raw cosine, so a row with no word
overlap rarely made the cut, and default-included doc chunks crowded it.
Instead the pool is filled from two rankings in turn, vector similarity first
and FTS second, each skipping rows already taken, until it holds
`rerank_candidate_pool` distinct rows or both rankings run out; when one
ranking runs out the other fills the remaining slots. Every row in the top
half (rounded down) of either ranking therefore reaches the re-ranker
whatever its blended score. Each ranking applies the same superseded and
age-decay adjustments as the blended score, so a superseded row never
takes a slot ahead of an otherwise-equal current row. Staleness is not an
adjustment: a stale fact ranks exactly like a fresh one. The superseded penalty
comes off one unweighted retriever component, which carries only its weight (0.5 by
default) of the blended relevance, so in a pool ranking it counts for about half
its strength in the blended score.
This is intended: the pool is a wide net the re-ranker then judges, and the
penalty arithmetic is unchanged. In a default recall that includes doc chunks
(`include_docs_in_default`, no `--source-type`), `[retrieval] rerank_doc_cap`
(default 3) limits how many doc chunks may enter the pool; doc chunks over the
cap are not eligible, and a recall with explicit source types, including
`--source-type doc`, is not capped. Each pool passage is scored on its own, so
a row's `rerank_score` does not depend on which other rows are in the pool (the
bundled model is int8 dynamic-quantized, so in a shared batch a score shifts by
up to about 0.25 logits with its neighbours) and a large pool does not need
gigabytes of memory (one 50-row batch used 4.1 GB). The code locator
(`memhub locate`) chooses its own re-rank candidates as before.

Toggle per-call with `memhub recall <query> --no-rerank`, or globally
with `[retrieval] use_reranker = false` in `.memhub/config.toml`. The
re-ranker is bundled into the binary unconditionally (no Cargo feature
flag) — turning it off in config skips the inference cost but doesn't
strip the model from disk. FTS-only mode bypasses the re-ranker
entirely.

Candidates whose cross-encoder logit falls below `[retrieval.scoring]
min_rerank_score` (default 2.0) are dropped after re-ranking, primarily to keep
gibberish queries from surfacing as answers (see the low-confidence fill below
for what a recall returns in place of the dropped candidates). A candidate's
logit is its score against the query or, for a two-question query, its best
part (see "Two-question queries" below). This replaces the legacy
`min_vector_score` cosine floor (decisions 70, 71) — the cosine band of
nonsense overlapped borderline semantic queries, so a vector-path floor had no
safe sweet spot. The rerank-score band is similarly noisy on memhub's own
corpus, so 2.0 is a parity calibration rather than an improvement; override
with `memhub recall --min-rerank-score <F>` or `memhub eval retrieval
--min-rerank-score <F>` (a negative value disables the floor).

**Partial-token FTS (issue #225).** The recall FTS path first requires every
query token to match one row (a quoted FTS5 `AND`). When no row in the corpus
holds every token, which is the normal case for a keyword bag or a natural
question, recall retries with an `OR` of the query's content tokens (English
stopwords such as "the", "how", "does" are dropped) so rows matching some of
the tokens still contribute FTS signal. Before this, a multi-word query whose
tokens never co-occur in one row got no FTS signal at all, the rerank
candidate pool was picked by the narrow cosine band alone, and the right row
could fall outside it and never reach the cross-encoder. bm25 is min-max
normalized over the hits, so the best partial hit would otherwise blend as if
it matched every token (about 0.82 against about 0.36 for a vector-only row)
and crowd the pool; partial-token FTS scores are therefore multiplied by 0.5
(`PARTIAL_FTS_FACTOR`). The code locator (`memhub locate`) is unchanged and
still requires every token. `min_rerank_score`, `doc_min_rerank_score` and
`rerank_candidate_pool` defaults are unchanged, and superseded rows
are still demoted rather than excluded (decision 145).

**Low-confidence fallback (issue #225).** Since issue #323 this is a fill:
candidates under the relevance floor are not discarded outright. Recall returns every candidate that cleared
its floor first, in rerank order, and then fills the remaining result slots,
up to `max_results`, with the highest-reranked floor-cut candidates, each marked
`low_confidence` (`"low_confidence": true` in `memhub recall --json` and the MCP
recall hit, omitted otherwise; a `[low-confidence]` tag in the CLI text) and
carrying its `rerank_score`. Treat them as leads, not answers. A hit that clears
its floor is never marked, and marked hits always come after unmarked ones. When
no candidate clears the floor, recall returns up to `max_results` marked rows
and emits the `rerank_floor_dropped_all` warning, whose text states how many
marked rows were returned; a recall in which any candidate clears the floor, or
that returns a row named by number in the query (below), carries no such
warning. Doc chunks that joined the default bundle and missed
`doc_min_rerank_score` are never returned, marked or not (decisions 90/91, 189);
doc chunks in a recall scoped to doc chunks (`--source-type doc`) are not
default-included, so they take the normal floor and fill like every other row.
A candidate pool of a single row is re-ranked too, so the floor applies to it
(with or without variants) and a lone row under the floor is returned marked.
History: a recall whose candidates were all dropped by the floor first returned
an empty bundle (decision 33); issue #225 (decision 186) changed that to the top
3 marked candidates, and only when the floor dropped every candidate; issue #323
fills the bundle after the floor-clearing hits up to `max_results` instead. In
the eval harness an `empty` probe passes when it returns no hit that cleared the
floor (low-confidence hits allowed), and each match outcome reports
`matched_low_confidence`. The golden set carries `kw-` keyword-bag variants and
`near-` related-but-no-answer probes for this.

**Explaining an empty or thin result (issue #318).** `memhub recall <query>
--explain` (and `explain: true` on the MCP `recall` tool, an optional boolean)
reports how many candidate rows each stage dropped, demoted or cut, so a recall
that returns nothing, or less than expected, can be diagnosed. With `--json`
the CLI adds an `explain` object; without `--json` it prints an `Explain:`
block after the results; the MCP response gains an `explain` field. Without the
flag nothing is added and the output is unchanged. The counters are bookkeeping
only: the rows returned, their order and their scores are identical with and
without it. Counts are summed over every store the call searched (the repo
store, plus the machine-global store when this repo opted in). The reasons, by
JSON key:

- `reranker_ran` (bool): whether the cross-encoder re-ranker ran (hybrid mode,
  `use_reranker` on, at least one candidate). No relevance floor applies when
  it did not.
- `dropped_by_floor`: rows whose rerank score was under `min_rerank_score`
  (every row except default-included doc chunks). Low-confidence fill hits
  are among these rows, since the floor dropped them before the fill
  returned them.
- `dropped_by_doc_floor`: default-included doc chunks under
  `doc_min_rerank_score`.
- `accepted_only_excluded`: rows excluded by accepted-only.
- `superseded_demoted`: superseded rows kept but demoted by
  `superseded_penalty`.
- `rerank_pool_cut`: candidates not sent to the re-ranker: rows beyond
  `rerank_candidate_pool` in the by-retriever fill, and default-included doc
  chunks over `rerank_doc_cap`.
- `result_limit_cut`: rows beyond `max_results` cut from the final bundle. It
  counts only rows that cleared the relevance floor; floor-cut rows that the
  result limit trimmed from the low-confidence fill are counted only in
  `dropped_by_floor`.
- `docs_dropped_no_rerank`: default-included doc chunks dropped because the
  re-ranker did not run to vet them.
- `docs_not_searched`: ingested doc chunks this call did not search: the full
  chunk total (over the stores searched) when doc chunks were outside the
  searched source types, 0 when they were searched, whether by default
  inclusion or `--source-type doc`. A chunk that was searched but not surfaced
  does not count here, unlike in `available_docs`. The machine-global store's
  doc chunks are not counted when the repo has not opted in to the global
  store; `global_store_not_searched` reports that case.
- `global_store_not_searched` (bool): a machine-global store exists but was not
  searched because this repo has not opted in (`[global] enabled`). The call
  looks for the store only when `--explain` (or `explain: true`) was
  requested, so a plain recall does no filesystem check for it.

**Rows named by number (issue #340).** A recall whose main query contains the
word `decision`, `task` or `fact` (any letter case, a whole word) followed by
whitespace and/or `#` and a number (`decision 201`, `Task #99`, `fact 7`)
returns that row first. Only the main query is read, not the variants. The row
must exist in the repo store and pass the call's source-type and
accepted-only filters (accepted-only excludes a task, since tasks carry no `source`); a
superseded row is kept, as in ranked results. Named rows come in the order the
query names them, once each, ahead of every other hit, are never flagged
`low_confidence` and skip the relevance floor, and they count toward
`max_results` (named rows past the limit are dropped, last named first); the
remaining slots are filled by the normal ranking, which does not see them. When
the re-ranker ran, a named row carries its `rerank_score`. Rows of the
machine-global store are never routed (their ids are separate), and nothing else
is read as an id: plurals (`decisions 12`), shorthand (`D17`, `T164`, which spec
documents use for their own numbering), a number after any other word
(`issue 290`, `PR #328`, `run 2`), a number glued to letters or a decimal point
(`task 5b`, `fact 3.5`), a number followed by a unit or count word such as
`days` or `rows` (`in fact 3 rows were dropped`, `the decision 2 days ago`), a
number followed by `of them` (`task 3 of them`), a number hyphen-joined to a
word (`decision 2-day`) and a number that matches no row do nothing. A row only
the routing loaded is counted as if a retriever had found it: in
`candidate_count`, in the superseded counter, and in the accepted-only
exclusion; named rows cut by `max_results`
count in the `--explain` result-limit cut. A returned named row also keeps the
`rerank_floor_dropped_all` warning off the response.

**Query variants (issue #319).** The calling agent can send extra phrasings of
one question so rows the main wording misses can still enter the candidate
pool: `memhub recall <query> --variant <TEXT>` (repeatable) on the CLI, and an
optional `variants` list of strings on the MCP `recall` tool. Send them when a
plain call came back empty or thin, or the answer likely uses different words
than the question; useful phrasings are a keyword form of the question and a
sentence worded the way the answer would be. At most 4 variants per call, none
blank: a blank variant, or a fifth one, is rejected with an error saying why
(CLI error exit; MCP invalid-params). A call without variants gathers only the
main query (and, for a two-question query, its parts: see below), unchanged by
the variants feature.

Variants widen the candidate pool only. Each phrasing is embedded (the main
query and the variants in one batch) and gathered like the main query, in the
repo store and, when the repo opted in, the machine-global store. Each
phrasing's own `rerank_candidate_pool` rows (filled from its vector and FTS
rankings in turn, as under "Re-rank pool" above) are unioned, deduplicated by
row (scope, source type, id), and one re-rank pass then runs over that union. A
row found by several phrasings keeps the hit from its best-scoring phrasing, so
its blended `score`, `fts_score` and `vector_score` come from that phrasing,
while `rerank_score` is never scored against a variant. The re-ranker and the
`min_rerank_score` / `doc_min_rerank_score` floors score every candidate
against the **main query** (and, for a two-question query, each of its parts,
taking the highest: see below), never a variant, so each returned hit's
`rerank_score` is its score against the main query (or its best part) and the
floor keeps its meaning (a gibberish main query clears no floor whatever the
variants say, so its rows come back only flagged `low_confidence`). The main
query's own pool is always a subset of the union, so a variant can never push
out a row the main query would have re-ranked. Cost: the union has up to (1 +
variants + parts) x `rerank_candidate_pool` rows, so re-rank time grows with
the variants sent and the parts found (a recall takes about 240 ms with no
variants and about 380 ms with them, medians of the live-set A/B at pool 20),
plus, per variant or part, one more embedding and, in each searched store (the
repo store and, when the repo opted in, the machine-global store), one more set
of full-text lookups (one per searched source type, and a second set with the
partial-token retry when no row holds every token) and one more scan of the
stored vectors. When the re-ranker does not run (FTS-only mode, `--no-rerank`,
`use_reranker = false`) no pool is cut and variant-only rows have no main-query
score, so the merged set is ordered by each row's best blended score across
phrasings; the filters and the doc-chunk rule still apply as they do without
variants. With `--explain` each distinct row is counted once however many
phrasings found it, and `rerank_pool_cut` counts the rows outside every
phrasing's pool (doc chunks over the cap included). With `[global]` disabled
only the repo store is searched, as before M9; variants add rows from that
store alone.

**Two-question queries (issue #341).** A main query that asks two things
(`what does X measure, and where is Y set?`) is split when the re-ranker will
run (hybrid mode, re-ranker on). A split point is ` and `, with or without a
comma before it, immediately followed by one of `what`, `which`, `how`, `why`,
`where`, `when`, `who`, `do`, `does`, `did`, `is`, `are`, `can` or `should`
(any letter case, a whole word); the query is cut at each such point into
parts, each trimmed and without its trailing comma, the question word kept with
its part. `build and lint` and `and whatever` (not a whole question word) are
not split, and neither is a query where the question word would be a part on
its own (`build the index, and what`, `foo and why?`). Each part is gathered
like a caller variant (embedded in the same batch, its own
`rerank_candidate_pool` rows joined into the union), so a row only one half
matches can reach the re-ranker; parts do not count toward the 4-variant limit
and are not crossed with variants. The re-ranker then scores every pooled row
(a row named by number included) against the whole query and against each part
and keeps the highest, and that score is the row's `rerank_score`, used by the
floors, the `low_confidence` flag and the order. A caller variant is still
never scored against. Cost: one more re-ranker pass over the union per part,
plus per part the extra embedding and gather a variant costs. A query with no
split point, and any recall in FTS-only mode or with the re-ranker off, runs
exactly as before. The query log and `memhub eval` record the query as given;
parts are not logged.

**Architecture sections (issue #232).** The latest architecture narrative
(`memhub arch set`) is split by markdown heading, with the same chunker
`memhub doc add` uses, into a derived `arch_sections` table (migration 0025),
and each section is a recall hit of source type `arch_section`, titled
`Architecture — <heading path>`. Sections are part of the default recall
bundle alongside facts, decisions, and tasks, with the same `min_rerank_score`
floor and the same low-confidence fill eligibility; no config flag gates
them, unlike docs. Scope a recall to them alone with `memhub recall <query>
--source-type arch` (CLI) or `memhub.recall(query=..., source_types=["arch"])`
(MCP). Before this change the architecture narrative never entered recall; it
was only rendered into `PROJECT.md`. Only the latest body is searchable: every
`arch set` deletes the previous sections and re-derives them in the same
transaction, embedding them eagerly in hybrid mode. A DB whose latest body was
stored before migration 0025 gets its sections on the next open, FTS-only
(open never loads the embedding model); in hybrid mode `memhub index rebuild`
embeds them, and `memhub index status` counts them. A lone wrapper heading is
left out of section titles: when the body's first heading has nothing before
it and every other heading, of which there is at least one, is nested under it
(for example one `# memhub architecture` over the whole body), the wrapper is
dropped from every heading path, in both the `PROJECT.md` index entries and
the recall titles, and is dropped as a section of its own when it has no text
beyond the heading line (with text it stays as an untitled section). A second
top-level heading, any text before the first heading, or a body with only one
heading, means there is no wrapper and nothing is stripped. The rule runs when
sections are derived, while the `PROJECT.md` index is derived from the latest
body at render time; so sections stored before the rule existed keep
wrapper-prefixed recall titles beside stripped index entries until they are
re-derived on the next `memhub arch set` or `memhub import`. `memhub upgrade`
does not re-derive them. Sections are derived data: `memhub export` omits
them, `memhub import` re-derives them from the imported latest body, and Drive
sync snapshots carry them as part of the whole-DB copy while the sync digest
exempts them (`project_arch` itself is digested). This applies to the
architecture narrative only: the state narrative (`memhub state set`) still
never enters recall, and session notes remain reachable only through an
explicit `note` scope (gate Q9).

**PROJECT.md shape (issue #233).** `memhub render` writes `PROJECT.md` as a
compact frame, not a dump of the narratives: `## Currently building` carries
the latest state body in full; `## Architecture` lists the heading path of
every section of the latest architecture body (same chunker as the sections
above, consecutive repeats of a split section collapsed, text before the
first heading shown as an untitled opening entry) plus a pointer to
`memhub recall --source-type arch` and `memhub arch show`;
`## Recent session notes` keeps the latest 10 notes, each cut to at most 300
characters at a word boundary with an ellipsis, plus a pointer to
`memhub note list` and `memhub recall --source-type note`. Render only
reads the DB for this; stored architecture bodies and notes are untouched.
`PROJECT.md` is kept at or under 8,000 bytes (UTF-8; a fixed constant, so the
whole frame fits a session-start hook's output cap). When the full frame is
over budget, render removes the lowest-value content first: the
`## Token Accounting` section (shown only in a build with the metrics feature
and with metrics enabled in config), then session note stubs from the oldest,
then the architecture section index (the list of section headings); the
pointer sentences stay, reworded where they would otherwise describe content
that is no longer shown. The `## Currently building` text is never
shortened: if it alone keeps the file over budget, the file is written anyway.
Every render ends `PROJECT.md` with a one-line size meter stating the file's
exact byte count and the 8,000-byte budget, naming whatever was removed (the
Token Accounting section, all session note stubs, or how many of the oldest,
and the architecture section index), and, when still over budget, saying the
state text should be shortened.
`memhub state set` warns on stderr (exit 0, stdout unchanged) when a body
exceeds 4,000 characters, since PROJECT.md renders it in full; the 65,536
hard cap is unchanged.

Decisions can carry an optional natural-language `summary` (migration
0011, decision 72). When set, the summary is prepended to BOTH the
bi-encoder's embed text and the cross-encoder's rerank input, letting
jargon-titled decisions surface for plain-English queries. On memhub's
own golden set, backfilling summaries on four jargon-titled decisions
lifted Recall@3 from 76.5% to 100% with the safety probe still
passing. Set at write time with `memhub decision add --summary "..."`
or backfill an existing row with `memhub decision set-summary <ID>
"..."` (empty string clears it back to NULL).

For A/B testing in any repo: `memhub eval retrieval` vs
`memhub eval retrieval --no-rerank`.

**Judging a change per query (`memhub eval compare`).** `memhub eval
retrieval --json` reports the rank-1 pass count (`totals.match_passes_at_1`,
`recall_at_1`) alongside Recall@K, and records the settings the run used
under `settings`: retrieval `mode`, whether the `reranker` was enabled
for the run, the `min_rerank_score` floor, the
`rerank_candidate_pool` size, the `rerank_doc_cap`, and whether `variants` were
sent (a saved run from before the variants setting existed reads as `false`,
and one from before the doc cap existed reads as not recorded).
A golden query may carry an optional `variants` list (at most 4, none blank,
checked when the file loads); `eval retrieval` sends it with the query exactly as
an agent would (`recall --variant`), and `memhub eval retrieval --no-variants`
runs the same golden file on the main queries alone, so one file can be A/B
measured with `memhub eval compare`. Golden files without the field score as
before. Every query in `tests/retrieval_golden_live.json` and
`tests/retrieval_golden_holdout.json` carries two variants, all rewritten on
2026-10-11 (task 197) by writers who saw only the query text, with no
repository access and no answers: a keyword form (a question form for queries
that are already keyword bags) plus a sentence worded like the start of an
answer that leaves the unknown part generic.
`memhub eval compare <A> <B>` reads two such saved outputs and pairs them by
query id. For rank-1 and for found@K separately it reports each run's pass
count over the match queries, the ids that passed only in A and only in B,
and the exact two-sided McNemar p-value over those discordant pairs
(`min(1, 2 * P(X <= min(b, c)))` for `X ~ Binomial(b + c, 1/2)`, 1 when there
are none); it also prints each run's recorded settings and lists every
`empty` probe whose pass/fail differs, saying which run returned a hit
(`--json` gives the same as structured output). Because it compares saved
runs rather than config flags it also covers code changes: save run A with
the old binary and run B with the new one. It exits non-zero, printing no
comparison, when the runs' query id sets differ, a shared id has different
query text, or the runs used different K. Like all of `eval` it is read-only
(it only reads the two files, decision 51). A golden query may list several
acceptable answers: besides its own `source_type` / `title_contains` /
`body_contains`, an `also_accept` array holds further answers with the same
three fields, and the query passes when any top-K hit satisfies any one of
them. Queries without `also_accept` score exactly as before.

**Hermetic golden fixture (N28, issue #44).** `tests/retrieval_golden.json`'s
queries target memhub's own real decisions/facts/tasks (e.g. decision 34
"Agents prefer recall over reading PROJECT_LEDGER.md", decision 48 "recall is
read-only"), so running `memhub eval retrieval` from this repo's root scores
against *this machine's* live `.memhub/project.sqlite` — a corpus that drifts
as new rows land, making the golden-set contract a property of a given DB's
row population, not just of the code. `tests/retrieval/retrieval_golden_hermetic.rs`
(mounted from `tests/retrieval_harness.rs`) is the hermetic CI gate: it seeds a disposable tempdir project — switched to
hybrid mode *before* seeding so eager-embed (decision 27) actually fires —
whose rows reproduce the golden set's targets (copied verbatim from the live
decisions where one is cited, including the real backfilled `summary` text
on the four decisions that need it), then drives the compiled `memhub eval
retrieval --json` binary against it with `--golden` pointed at the real
shipped `tests/retrieval_golden.json`. That is the same pattern
`tests/retrieval/locate_polyglot.rs` already established for `eval locate` — a fixture
seeded fresh per run, independent of live `.memhub` state — applied to the
retrieval golden.

Baseline (hybrid mode, default rerank floor 2.0): the golden set is 36
queries (31 match, 5 empty) and the code scores Recall@3 = 31/31 match and
5/5 empty probes (0 safety failures), three of the keyword-bag matches via
the low-confidence fill. First recorded 2026-07-06 (issue #44) as 100% on
the then-smaller set, extended by issue #225. This is the reference other
Wave 3 lifecycle PRs (L2 staleness, L3 supersession, L6 age decay) compare
their own hermetic re-run against. There is no persisted fixture DB to
regenerate — the corpus is defined entirely by the `fact::add` /
`decision::add` calls in that test's `seed_hermetic_corpus`, rebuilt from
scratch every run. When `tests/retrieval_golden.json` legitimately changes,
update `seed_hermetic_corpus` to match and re-run
`cargo test retrieval_golden_hermetic`. The live-DB run from the repo
root (what `/eval-recall` still drives by default) remains a self-hosted
calibration signal, not the enforced gate.

**Live golden set (task 174).** `tests/retrieval_golden_live.json` is a
second, larger golden set that is scored against the repository's own memhub
project database rather than a seeded fixture. The hermetic set seeds about 19
rows, each one a query's answer, so it cannot tell close retrieval variants
apart; the live database has about 1,000 candidate rows with real distractors.
Any change to retrieval (tasks 160, 175 and 98, or an embedder swap) is
measured on this set before it lands. It is **not run in CI**, because CI has
no such database: `cargo test` only checks that the file is valid JSON and
passes golden-file validation (`live_golden_file_passes_golden_validation`,
and `holdout_golden_file_passes_golden_validation` for the held-out file
below).
Its queries are the real agent recall queries from this repo's session history
(`seed-` ids: MCP recall calls, and since 2026-10-11 CLI `memhub recall` calls
and later sessions), plus keyword bags (`kw-`), natural-language paraphrases
(`semantic-`), two-part questions (`two-`), a vocabulary-mismatch group
(`vm-`: phrased the way an agent asks without knowing the stored wording,
sharing no whole word of four or more letters with the title of the row that
answers it), and questions answered by an ingested doc chunk (`doc-`) or an
architecture section (`arch-`). Where several rows genuinely answer a query,
`also_accept` lists them. Its empty probes (`empty-`, `near-`) ask about
topics no row covers. Each query's `notes` name the answering row: a doc chunk
by its document and section heading, plus its matcher phrase wherever a query
accepts more than one chunk under the same heading (chunk ids change on every
re-ingest), any other row by type and id as of 2026-10-10; the matchers (title
and body substrings), not the ids, decide pass or fail.

**Held-out file (task 197).** `tests/retrieval_golden_holdout.json` holds
about one query in six of each id prefix of the combined set, taken by sorting
the ids within a prefix and moving every 6th query starting at the 4th out of
the main file on 2026-10-11 (38 queries, 35 match and 3 empty; the main file
keeps 194). It has the same format, matching rules and id prefixes as the
main file. It exists to check that a setting tuned on the main file holds on
queries the tuning never looked at. Use rule: while tuning (pool sizes,
weights, floors, model swaps), compare and inspect per-query results on the
main file only; score the held-out file once at the end, with the same
`eval retrieval` and `eval compare` procedure below, and report its totals.
Never tune against its per-query outcomes, and do not score it repeatedly
between changes: a held-out set stops being held out once its per-query
results steer the tuning.

First baseline, recorded 2026-10-10 with a debug build of main at 7e17fff
against this machine's database (live config: hybrid mode, re-ranker on,
`rerank_candidate_pool` 20, `min_rerank_score` 2.0; K = 5):

| Run | Rank-1 | Found@5 | Empty probes passed |
|---|---|---|---|
| default | 146/190 | 160/190 | 18/18 |
| `--no-rerank` | 110/190 | 153/190 | 0/18 |

For the 36 `vm-` queries alone, the default run found 16 within the top
5 (14 at rank 1) and the `--no-rerank` run 15 (9 at rank 1). With the
re-ranker off no relevance floor applies, so every empty probe returns hits;
that is why the second row fails all 18. `memhub eval compare` with A =
default and B = `--no-rerank` reports, for rank-1, 43 queries that only A
passed and 7 that only B passed (exact McNemar p = 2.1e-07), and for
found@5, 14 only A and 7 only B (p = 0.189); all 18 empty probes differ,
each with run B returning a hit. These numbers belong to one machine's
database on that date. Adding, superseding or editing a row, or running on
another machine's database, changes them, and they are not comparable to
the hermetic 36-query baseline above (memhub fact
`retrieval.live-eval-not-comparable`), so never compare a fresh run against
a number recorded elsewhere: always take run A and run B yourself.

**Recall query log (issue #324, opt-in).** Real agent queries are the best
source of new golden probes, so `[retrieval] log_queries = true` makes every
agent-facing recall append one JSON line to `.memhub/recall_queries.jsonl`,
beside `project.sqlite` and `config.toml`. It is **off by default**; with it
off no file is created and recall is untouched. It covers `memhub recall`
(surface `cli`) and the MCP recall tool (surface `mcp`) only: `memhub eval
retrieval`, hooks and every other internal recall never write it. The file is
a plain file, not a database table, and `/.memhub/*` is git-ignored, so Drive
sync snapshots, `memhub export` and the machine-global store never carry it.
Queries are stored in **plaintext**. A line holds `ts` (UTC, ISO 8601),
`surface`, `query`, `variants`, `source_types` (as requested; empty means the
default bundle), `max_results` (the effective value, so an omitted
`max_results` and an explicit default are logged the same), `results` (per
returned row: `source_type`, `source_id`, `scope`, `title`, `rerank_score`,
`low_confidence`) and `warnings` (the warning kinds):

```json
{"ts":"2026-10-11T17:40:02Z","surface":"cli","query":"cargo build","variants":["compile command"],"source_types":["fact"],"max_results":3,"results":[{"source_type":"fact","source_id":1,"scope":"repo","title":"build","rerank_score":null,"low_confidence":false}],"warnings":[]}
```

Each line is written with a single append. A failure to create or write the
file never fails or alters the recall; it prints
`memhub: recall query log not written: ...` on stderr only.
**The log is never rotated or size-capped and grows with every agent recall;**
delete or truncate it by hand when it gets large.

**A/B procedure for a code change.** An A/B is two back-to-back local runs
on the same database, compared with `memhub eval compare`:

1. Work from one repo root whose `.memhub/project.sqlite` is the database to
   score, and do nothing that writes memhub state between the two runs (no
   `add`, `accept`, `doc add`, `index rebuild`, `upgrade` or wrap-up on that
   repo, and no other agent session writing to it). If anything wrote,
   discard both runs and start again.
2. Build the old and the new binary, for example with `cargo build
   --release` in the base commit's checkout and in the changed one, and note
   both paths. For a settings-only change, one binary plus a flag
   (`--no-rerank`, `--min-rerank-score`, `--mode`) or a config edit is
   enough.
3. Run A with the old binary: `<old-memhub> eval retrieval --golden
   tests/retrieval_golden_live.json --k 5 --json > a.json`. Run the older
   binary first: every `eval retrieval` run applies the maintenance-on-open
   pass (see Maintenance-on-open contract above), so a binary with a newer
   schema migrates the database during its run, and an older binary then
   refuses to open it.
4. Immediately run B with the new binary and the same arguments: `<new-memhub>
   eval retrieval --golden tests/retrieval_golden_live.json --k 5 --json >
   b.json`. Save the files with a redirect that writes UTF-8 (Git Bash, zsh,
   bash, or PowerShell 7 and later); Windows PowerShell 5.1's `>` writes
   UTF-16, which `eval compare` rejects.
5. Run `memhub eval compare a.json b.json` (add `--json` for structured
   output). Read rank-1 and found@5 separately: each run's pass count, the
   queries only A or only B passed, the exact McNemar p-value, and any empty
   probe whose result differs. A small p-value (conventionally under 0.05)
   means the difference is unlikely to be chance; few discordant queries
   cannot reach significance, which is not evidence of no difference.
6. Record the date, both commits, the settings the runs used and the
   counts in the task or pull request, and say they come from this
   machine's database.

### Checking recall in any repository (`memhub eval self-check`)

The golden sets belong to memhub's own database, so they cannot say whether
recall works in a repository memhub was dropped into. `memhub eval
self-check [--sample N] [--json]` needs no golden set: after `memhub init`
(and `memhub index rebuild` in hybrid mode) it samples this repository's own
rows of each type — fact, decision, task, doc chunk and architecture section —
and asks recall for each one twice:

- the **title** query is the row's own title (a fact's key, a doc chunk's or
  section's heading path; a doc chunk with no heading uses its document title);
- the **passage** query is a 12-word span from the middle of the row's own
  text. A row with no text (a task without notes) has no passage query and is
  not counted for that form.

Each query is a plain `memhub recall` with an agent's defaults (default source
types and result count; the project's retrieval mode), and a
row "comes back" when the returned hits include that repository row.
`--sample` sets the rows sampled per type (default 10). The sample is spread
evenly across the rows ordered by id, so an unchanged database draws the same
sample on every run.

Reading the report: per type it shows the repository's row count, how many
were sampled, and per query form `first/top3 of queried` — how many sampled
rows came back at rank 1, how many within the top 3, out of the rows that
form could query. Below the table it lists every miss: each sampled row and
query form that did not come back within the top 3, with the query used and
either the rank it had (when it came back below the top 3) or "not returned".
`--json` carries the same counts per type (`title` and `passage` objects with
`queried`, `first`, `top3`) and the `misses` array (`source_type`,
`source_id`, `title`, `form`, `query`, `rank`, null when not returned).

A type with zero rows reports zero everywhere. A form that finds almost
nothing for a whole type points at a recall problem rather than at single
rows: for example doc chunks missing
because `include_docs_in_default` is off, or architecture sections missing
because they were never embedded. Passage misses on long, generic text are
less alarming than title misses on distinctive titles.

The command is read-only, like `eval retrieval` (decision 51): it adds,
changes or deletes no fact, decision, task, session note, document,
architecture section or `writes_log` entry, and, since it calls recall with
metrics logging off, appends nothing to `recall_metrics` or the
`recall_queries.jsonl` query log. It runs two recalls per sampled row, so the
default sample is up to 100 recalls. Test:
`tests/retrieval/self_check.rs`.

### Retrieval modes and configuration

Both modes are first-class; the install prompt asks you to pick one.

| | **`fts`** (default) | **`hybrid`** (recommended) |
|---|---|---|
| Scoring | FTS5 BM25 over title + body | 0.5 × FTS + 0.5 × cosine, then re-ranked |
| What it catches | Exact terms, stemmed variants | Exact + paraphrases (`"compile a release"` → `release_build` fact) |
| Per-write cost | 0 ms | ~50 ms eager-embed inside the source-write transaction |
| Per-recall cost | <10 ms | About 240 ms (brute-force cosine + a re-ranker that scores each pool passage on its own); about 380 ms with variants. Medians of the live-set A/B at pool 20 on one machine |
| Disk footprint | None beyond source rows | ~1.5 KB per row (384-dim f32 vector) |
| Network | Never | Never. Model is bundled. |
| Best for | Small projects, scripted use | Multi-month projects where you forget exact wording |

**Switching modes is non-destructive.** `fts → hybrid` needs one `memhub index rebuild`; `hybrid → fts` just stops consulting embeddings.

`memhub index rebuild` leaves each store holding vectors only for the bundled model: it re-embeds every row and deletes vectors written under any other model name. In a repo opted in to machine-global memory it does the same to the global store (the output reports both stores' counts); in a repo not opted in the global store is not touched. The code index does the same on its next hybrid refresh: chunks whose only vector is from another model or dimension are re-embedded and the foreign vectors removed.

The `[retrieval]` block in `.memhub/config.toml`:

```toml
[retrieval]
mode = "hybrid"                  # "fts" | "hybrid"
default_max_results = 6
accepted_only_by_default = false # filter to source IN ('user', 'user+agent:%')
fact_stale_after_days = 90       # facts older than this are flagged stale

[retrieval.scoring]
fts_weight = 0.5
vector_weight = 0.5

[global]
enabled = false                  # opt-in via `memhub global enable`
```

**Stale facts are returned and flagged, never excluded or demoted (decision
236).** A fact never verified, or last verified more than
`fact_stale_after_days` ago, ranks exactly like a freshly verified one and
carries `stale: true` for the caller to weigh; recall emits no stale warning.
`include_stale_by_default`, `[retrieval.scoring] stale_penalty`, the
`--include-stale` flag and the MCP `include_stale` parameter are retired: older
configs still load with them (every `memhub init` pinned both keys, some as
`include_stale_by_default = false`), and the flag and parameter are still
accepted, but none of them changes which rows come back or their order.
`memhub doctor` names the two config keys as ignored settings; deleting them is
safe. Optional age decay (`age_half_life_days`) is separate and unchanged.

`[global] enabled` is managed by `memhub global enable` / `disable` — it's per-machine, so the tracked `.memhub/config.example.toml` baseline ships `false`. The machine-global store has no `[retrieval]` block of its own; it inherits the active repo's, including `[retrieval] include_docs_in_default`, which alone decides whether global docs join that repo's default recall.

## Token Accounting

**Hibernated by default (Wave 7 Q30).** Normal builds preserve the metrics
schema, config, stored rows, and source implementation, but compile out all
collection, maintenance, rendering, CLI, MCP, calibration, and agent-skill
surfaces. A pre-existing `metrics.enabled = true` is inert and is not
rewritten or deleted. Reactivation is explicit: build with
`--features metrics`. Default skill installation also skips `/metrics`.
Transcript archiving remains independent and available. The web dashboard
(`memhub viz`, the `viz` feature, `/viz`) was deleted; `memhub upgrade` still
reports a leftover `viz` wrapper as an orphan without touching it, and the
code is recoverable from git history (last present at commit `db4da80`).

The retained feature behaves as follows when explicitly compiled in. Opt in
per machine with `memhub metrics enable` — this
auto-detects the Claude Code transcript directory and writes the resolved
path into `.memhub/config.toml`. Disable with `memhub metrics disable`.

Two independent sub-switches under `[metrics]`:
- `recall_proxy = true` (component A) — logs one row to `recall_metrics`
  per `memhub recall` call: actual bundle size vs a full-ledger
  counterfactual, tokenised with tiktoken cl100k.
- `session_accounting = true` (component B) — scrapes configured Claude Code
  and Codex transcript JSONL into `session_metrics` for real
  input/output/cache token totals. Scraping is incremental and never fatal;
  bad lines are skipped.

OpenCode transcript archiving does **not** add OpenCode session-accounting
ingestion. Before issue #214 the independent archiver accepted Claude/Codex
only; after issue #214 it can archive a complete OpenCode session export, but
the retained optional metrics component continues to support Claude Code and
Codex session accounting and writes no OpenCode rows to `session_metrics`.

**Proxy contract:** `bundle_tokens` is the token count of the recall bundle
actually returned. `ledger_tokens` (per row in `recall_metrics`) is the size
of `PROJECT_LEDGER.md` at recall time, measured in cl100k tokens. The
counterfactual is **session-scoped**: for each session that had at least one
non-empty recall, charge one ledger load (the minimum `ledger_tokens` across
that session's recalls, as a proxy for session-start size) and subtract all
non-empty bundle tokens. Empty-bundle recalls (no results returned) are not
savings events and contribute nothing to the offset. The rendered label is
"context offset vs full-ledger baseline" — not "tokens saved" — because the
agent would not necessarily have loaded the full ledger anyway.

**Empirical counterfactual baseline (task 64, decision 109).** The assumed
full-ledger baseline above is a guess. Task 64 adds a *measured* baseline
alongside it: `session_metrics.baseline_input_tokens` (migration 0017) records
the **full prompt of each session's first usage turn** — `input_tokens +
cache_read_input_tokens + cache_creation_input_tokens` — which approximates
everything loaded at session start (system prompt + CLAUDE.md + PROJECT.md +
any handoff md). It is `input + both cache fields`, **not `input_tokens`
alone**, because under prompt caching the bulk of startup context is billed as
cache_creation/cache_read; `input_tokens` alone undercounts startup ~10×. The
scraper sets it once per session, on the first usage line (`COALESCE` keeps the
earliest), so it pins the session-START cost. In `render_period_block` the
headline "Context offset" now prefers the **median `baseline_input_tokens`
across the window's no-recall sessions** (`recall_calls = 0`) as the
denominator (`recall_sessions × that median`), with the assumed full-ledger
percentage shown on an aligned line beneath so the gap is visible. The column
is **machine-local, not exported, and not applied retroactively** — existing
sessions are already past their first-turn offset, so the empirical baseline
accrues from new sessions forward (an uncalibrated/empty install renders only
the assumed line, byte-identical to before).

**Tokenizer caveat:** tiktoken cl100k is ±10% off Anthropic's real
tokenizer. Ratios stay sound because both sides of every comparison use the
same yardstick; treat absolute token counts as estimates, not ground truth.

**Tokenizer calibration (task 63, decision 109).** The ±10% above is a
fixed multiplier, so it can be corrected once. `memhub metrics calibrate`
sends a **fixed bundled corpus** — never your project's content — to
Anthropic's `count_tokens` endpoint, measures the cl100k→real ratio, and
writes it back to `[metrics] calibration_factor`. `tokenizer::tokens_of`
then scales every estimate by it (default `1.0` = uncalibrated
passthrough, so an uncalibrated install and every unit test is
byte-identical to before). It corrects *absolute* counts and the
ledger-vs-bundle *offset*; the context-offset **percentage** is a ratio
of two equally-scaled numbers and is unchanged. **This is the only
command in all of memhub that touches the network** (via `ureq`,
compiled in but never reached otherwise) — offline-first holds because
the call is explicit and one-time. It is **CLI-only ops housekeeping like
`gc`/`upgrade`, deliberately not an MCP/agent surface** (an agent must
not reach the network on its own). The factor is **per-machine** (a
property of the local binary's tokenizer, not the repo) and **not
applied retroactively** — rows written before calibration keep their
earlier scaling; re-run after a binary/tokenizer change. Needs
`ANTHROPIC_API_KEY` in the environment; refuses cleanly without it.

**Cache churn (task 62).** Each rendered period block also carries a
`Cache churn:` line — the share of cache tokens that were *creation*
(rebuilt prefix) rather than *read* (reused prefix). At a 1M-token
window the real recurring cost is cumulative per-turn `cache_read`, so a
high creation share is the honest "we kept rebuilding the cache" signal.
Two figures: the token-weighted window churn (dominated by the largest
sessions) and a per-session mean (each session weighted equally, so one
huge session can't dominate). Both derive from the already-logged
`cache_read_tokens` / `cache_creation_tokens` — no migration. The line
is omitted when a window had no cache activity. Rendered only in
`render_period_block` surfaces (the `/metrics` panel, MCP
`rendered_panel`, and the PROJECT.md digest); the plain `memhub metrics
status` CLI text keeps its leaner per-line layout.

Reactivated surfaces: `memhub metrics status` (CLI) · `memhub.metrics` (MCP
tool) · `/metrics` (skill). `memhub render` appends a 7-day digest to
`PROJECT.md` when enabled and ≥1 row exists; the section is omitted
entirely when disabled or when no data has been captured yet.

## Doc Ingestion

External markdown reference docs (design specs, API contracts) can be
ingested into `.memhub/project.sqlite` as opt-in retrieval material
(decision 86). The file is chunked by heading — fenced code blocks kept
intact — and each chunk is embedded, so it is retrievable through the
same SQL+RAG hybrid recall as facts, decisions, and tasks.

**Default after first ingest (decision 90, extends 86).** Docs are
opt-in by default; the first successful `memhub doc add` in a repo
flips `[retrieval] include_docs_in_default` on in that repo's local
config, so the user-pointed write that establishes docs also wires up
retrieval. After that, plain `memhub recall` surfaces a doc chunk only
when it clears the cross-encoder relevance boundary
(`[retrieval.scoring] doc_min_rerank_score`, default 0.0) — strong
topical matches in, off-topic docs out, so a UI style guide stays
silent on a backend query while a code style guide surfaces. The
doc floor is deliberately *below* `min_rerank_score`: doc chunks
rerank in a lower band than facts/decisions (an on-topic doc ≈ +1.6,
off-topic ≈ −11), so a higher floor would filter relevant docs.
Scope to docs alone with `memhub recall <query> --source-type doc`
(CLI) or `memhub.recall(query=..., source_types=["doc"])` (MCP);
explicit scoping keeps the normal floor and is unaffected by the
flag. Plain recall still returns `available_docs` — now the count of
ingested chunks that did *not* surface this call — so a doc-scoped
follow-up for the long tail stays a judgment call. Set
`include_docs_in_default = false` to revert to strict opt-in.

Surfaces: `memhub doc add|ls|show|rm` (CLI) · `memhub.doc_add` (MCP,
direct write — a doc is a user-pointed artifact, not an agent claim) ·
`/doc` (skill). Re-ingesting an unchanged file is a no-op; changed
content replaces every chunk and refreshes embeddings/FTS.

Doc content is **excluded from `memhub export`** — it is a disk-backed,
re-ingestable cache. On another machine, re-run `doc add` against the
same file. Embeddings populate only in `hybrid` mode; `fts` mode
ingests chunks + FTS and vector recall for docs starts after
`memhub index rebuild`.

## Code Index

Milestone 11 (design anchor:
[docs/reference/memhub-prd-addendum-m11-code-locator.md](docs/reference/memhub-prd-addendum-m11-code-locator.md))
adds a **code locator**: a cheap semantic file/symbol search over the
repo's own source, separate from project memory. `memhub locate <query>`
returns ranked `path:line-range` breadcrumbs plus a clipped snippet; the
agent then `Read`s that exact span. It never returns code into a recall
bundle and never edits.

**Isolated by construction (decision 107).** The index is a sibling DB at
`.memhub/code_index.sqlite` — gitignored, per-machine, and **never read by
`memhub recall`, never in `memhub export`, never in M10 sync**. That
physical separation is what preserves the recall eval-regression guarantee
(mirrors M9's registry-is-not-recall rule). It is also **derivable +
disposable**: no migration framework, just `CREATE TABLE IF NOT EXISTS` +
a `schema_version` in `index_meta`; a version mismatch drops and rebuilds,
so `memhub upgrade` is a no-op for it. The index set is `git ls-files`
filtered through the existing deny-list and scoped to grammar-known source
languages (task 69, below).

**Symbol-aware chunking (decisions 108, 115–120).** A tree-sitter AST
chunker emits one chunk per top-level item and one `Type::method` chunk per
method, with header-only chunks for container types. For Rust it also
emits one file-level `module-doc` chunk capturing the leading `//!` doc
comment, so a file whose purpose lives in its module prose stays
retrievable (a `ModuleDoc` grammar hook — task 85 added Rust inner-doc
capture; task 87 extended it to all six languages: Python docstring, Go
package doc, TS/JS file JSDoc, C#/Java file doc comment). Six languages get
real AST chunking — **Rust, Go, Python, TypeScript/JavaScript, Java, C#**
— via a hybrid `GrammarSpec` + typed hooks whose defaults reproduce Rust
byte-for-byte; a frozen snapshot test guards Rust output (the Rust freeze
is unchanged by task 87). Task 88 added a hermetic polyglot eval —
`tests/retrieval/locate_polyglot.rs` writes a six-language fixture repo and runs
`eval::run_locate` over `tests/code_locate_golden_polyglot.json` — so
non-Rust module-doc capture is held to the same Recall@K contract as Rust
(100% Recall@3), not just the chunker unit tests.
Grammars are bundled unconditionally and ABI-pinned with a per-language
load canary; detection is extension-only. A grammar-known file that fails
to parse falls back to line-window chunking; files of any other type are
excluded from the index entirely (task 69, below) rather than line-windowed.

**Lazy git-aware freshness (decision 109).** Every `locate` first diffs
`(mtime, size)` per tracked file against the index, confirms changes with a
content hash, re-chunks only what moved, and drops deleted/renamed files —
so results always reflect the working tree. A warm index is near-free
(stat-only); the first-ever index is the one expensive pass (~171s cold vs
~1.4s warm on memhub's tree), so `memhub code index` is the explicit
warm-up. `locate` is therefore a read-then-write op, but writes nothing to
`project.sqlite`. `--no-refresh` (issue #67) opts out of this pass
entirely for tight repeat-call loops on an already-warm index — no `git
ls-files`, no per-file stat, no `git rev-parse HEAD` — trading the
freshness guarantee for the lowest possible latency; `files_total` /
`chunks_total` / `head` then report the last-*indexed* state, not a fresh
recount. Stale-by-choice, explicit opt-in; default behavior (refresh every
call) is unchanged without the flag.

**Retrieval default: fusion, reranker OFF (decisions 114, 122, 123).**
Recall is FTS BM25 + vector fusion with the cross-encoder reranker off and
no score floor. On `tests/code_locate_golden.json` fusion now scores
**100% Recall@3** (18/18; Recall@1 61.1%) and the reranker holds at 88.9%
Recall@3 / 77.8% Recall@1. Task 85 (decision 123) lifted fusion past the
reranker on the governing metric by closing the last two misses at their
source: a 0.90 test-path down-weight (`[code_index] test_path_penalty`) so
`tests/` / `benches/` / `examples/` chunks stop out-ranking implementation,
and capturing the Rust file-level `//!` module doc as a chunk so a file
described by its module prose is retrievable. This supersedes decision
122's fusion≈rerank tie — fusion now
wins Recall@3 outright and runs ~12× faster, so it stays decisively the
default. `--rerank` remains the opt-in and still wins single-best-guess
Recall@1 (77.8% vs 61.1%). No nonsense floor is free: a `--min-rerank-score`
of 0 rejects both gibberish probes but also kills a true match (lowest
true-match logit −5.44), so the 2 nonsense-probe leaks under fusion are an
accepted no-floor cost. `memhub eval locate [--rerank]` is the A/B harness;
it indexes memhub's own (Rust) tree, so the non-Rust grammars are A/B'd by
the polyglot fixture eval in `tests/retrieval/locate_polyglot.rs` (task 88) instead.

Surfaces: `memhub locate` / `memhub code index|status|rm` (CLI) ·
`memhub.locate` (MCP, read-only — clipped snippets only, never full code) ·
`/locate` (skill).

**Scoring config is independent of recall (R11, issue #73).** Locate's
fusion weights and the test-path down-weight live under their own
`[code_index]` table — `fts_weight`, `vector_weight`, `test_path_penalty`
— rather than sharing `[retrieval.scoring]` with `memhub recall`. The two
tables used to be one struct, so tuning recall's blend silently retuned
locate's too (and vice versa); they are now separate, defaulting to the
same numeric values (0.5 / 0.5 / 0.90) so an untouched install's ranking
is unaffected. `[retrieval.scoring]`'s `superseded_penalty` (and
`age_half_life_days`, `min_rerank_score`) have no
`[code_index]` counterpart at all — the code index has no staleness or
supersession concept, and `--rerank` here has no score floor.

**Source-scoped index (task 69).** The index is scoped to grammar-known
source files only — the deny-list still applies on top, and vendored/
minified `*.min.*` bundles are excluded (a real `.js` extension that is not
hand-written code). The grammar registry is the single source of truth for
"indexable source", so a new language row is auto-included; non-source
files (docs, `Cargo.lock`, JSON/YAML/TOML, a vendored `*.min.js`) are dropped and
auto-pruned from any pre-task-69 index on the next `memhub code index`.
This deliberately reverses the earlier "index every tracked path" behavior:
on `tests/code_locate_golden.json` it lifted fusion Recall@1 0%→44% and
Recall@3 72%→89% by removing non-source files (notably the golden JSON
itself) that were out-ranking real code. The reranker A/B numbers recorded
in decision 114 predated this scoping; re-measured on the clean index they
showed fusion and rerank tied at 88.9% Recall@3 (decision 122), and task 85
(decision 123) then closed the two residual source-vs-source misses to take
fusion to 100% Recall@3 (see Retrieval default, above).

## Machine-global memory

Milestone 9 (design anchor:
[docs/reference/memhub-prd-addendum-m9-machine-global-memory.md](docs/reference/memhub-prd-addendum-m9-machine-global-memory.md))
adds an optional second store at `~/.memhub/global.sqlite`,
structurally identical to a repo DB (same embedded migrations;
`project_id = 1` is per-database, so zero new SQL migrations). It is
the global-vs-repo `CLAUDE.md` idea made retrievable.

Off by default and **per-repo**: a repo opts in with
`memhub global enable` (mirrors `memhub metrics enable|disable`).
`enabled` lives in `.memhub/config.toml` `[global]`; the tracked
`.memhub/config.example.toml` baseline ships `false`. When disabled or
the store is absent, only the repo store is searched, as before M9 (the
eval-regression guarantee for a query without variants).

Commands that only read the global store never migrate it: `recall`,
`doc ls --global`, `doc show --global` and `global status` open it
without applying migrations, because every memhub binary on the
machine, an experimental build included, can reach the same file.
`memhub upgrade` and the global writes (`global enable`, `fact|decision
add --global`, `fact|decision promote --global`, `review accept` of a
global-scoped proposal, `doc add|rm --global`) still migrate an older
store to the binary's schema and still refuse a newer one with the
"written by a newer memhub" error. The store's schema is read from
its applied-migration ledger (`schema_migrations`, the ledger
migrations themselves check), not from `projects.schema_version`,
which only ever ratchets upward and so can stay ahead of a store a
newer build once touched. Only a ledger version numbered above the
binary's newest migration counts as a newer schema; an unknown version
numbered at or below it is ignored. Absent such a newer version, the
schema is the last compiled migration before the first one the ledger
lacks; an empty ledger, or one missing the first migration, yields no
schema and is reported as "could not be read", the same as a ledger that
cannot be read. When that schema differs from the binary's newest migration
(older, newer, or could not be read), `recall` skips the
store: it returns the repo's rows only, with a `global_store_skipped`
warning naming both schemas and `memhub upgrade`. `doc ls|show
--global` fail with the same message, and `global status` reports the
ledger's schema and the store's counts. `memhub eval retrieval` reports a
skipped global store too: a "Global store skipped" line in the text output
and a `global_store_skipped` field in the JSON, both carrying the reason,
so a score computed without global rows is never read as a global-inclusive
one. `memhub upgrade` reads the same ledger for its `<global store>` row:
the row's schema, and its `ready` or `would migrate` verdict in a dry run,
come from `schema_migrations`, not from `projects.schema_version`.
Before issue #353 every one of those reads migrated an older store to
the binary's schema as a side effect, and failed outright (recall
included) on a newer one with the "written by a newer memhub" error.

When enabled, `recall` merges global hits with repo hits; every hit
carries `scope: "repo" | "global"`. **Precedence is
provenance-tag-only** — recall never drops a global hit and does no
automatic conflict resolution. Apply repo-overrides-global yourself
(exactly as repo `CLAUDE.md` overrides global `CLAUDE.md`).

Promotable to global: **facts, decisions, docs** only — machine/
toolchain truths, standing engineering policy, broadly-applicable
guides. Never global: tasks, rendered narrative, anything naming a
repo-specific path/symbol. Routing is **user-gated and never
agent-automatic** (one bad global write poisons every repo). Surfaces:
`memhub global enable|disable|status`, `--global` on
`fact|decision|doc add` and `doc ls|rm|show`,
`fact|decision promote <id> --global` (CLI) ·
`memhub.propose_fact|propose_decision(global=true)` (MCP — staged into
the repo's `pending_writes`, durable only on `memhub review accept`;
no `global` on MCP `doc_add`) · `/global` (skill). Global memory is
**not** exported by `memhub export` (per-machine; re-add on another
machine).

Onboarding exposes two explicit toggles — `[retrieval] mode` (fts vs
hybrid) and the machine-global store — plus two auto-followers with a
manual override: `[retrieval] use_reranker` (auto-on with hybrid) and
`[retrieval] include_docs_in_default` (auto-flips true on the first
repo `doc add`). That one key also decides whether global docs join the
repo's default recall; `doc add --global` never touches the config. A
`[global] include_docs_in_default` mirror used to exist but nothing read
it, so it was removed (issue #360); a leftover line in `config.toml` is
ignored and `memhub doctor` does not flag it.

Because `config.toml` never travels through Drive sync — a sync
snapshot is this repo's `project.sqlite` only (a `VACUUM INTO`
snapshot + manifest; see "Cross-machine Drive sync" above) — a
machine that `sync adopt`s this repo's snapshot does not gain the
source machine's `[retrieval] include_docs_in_default` value. The
machine-global store (`~/.memhub/global.sqlite`) is itself a separate
per-machine file that Drive sync never touches at all (same as
`memhub export`, above), so `sync adopt` cannot bring global docs
along either way. Add the global docs on each machine with `doc add
--global`, and set `[retrieval] include_docs_in_default` by hand where
needed, on each machine that should see those docs in its own default
recall.

## Machine-wide upgrade

`memhub upgrade` (decision 96; resolves task 48, subsumes the
recurring stale-PATH-binary problem) is the one dependable command to
bring **every** memhub install on a machine to a coherent state after
a code change — the binary on PATH, each known repo DB, the global
store, and the installed agent skill wrappers — not just whichever
repo you rebuilt from. Run it **from the memhub source repo**; it
errors elsewhere.

Flow: `cargo install --path . --force --locked` → one-time, order-independent
PATH-shadow fix (a regular-file `~/.local/bin/memhub` shadowing
`~/.cargo/bin/memhub` is replaced **once** with a symlink so future
installs always take effect; already-a-symlink is an idempotent no-op;
a non-symlink shadow is replaced only after a y/N confirm or `--yes`,
otherwise the manual `ln -sf` is printed) → installed-skill resync
(decision 97; below) → **re-exec the freshly installed binary** for the
migrate+verify pass so migrations run under new code → session-start
hook install (below; run by that re-exec'd binary) → per-instance
`ready/migrated/skipped/ERROR` table plus a per-agent skills line and a
per-file hooks line (`--json` carries `skills` and `hooks` arrays).
`--dry-run` reports the plan (including would-sync skill counts and
would-add hook entries) and changes nothing. Before issue #286 the flow
wrote no agent-CLI config file at all; it now merges one hook entry into
`~/.claude/settings.json` and `~/.codex/hooks.json` (MCP registration is
still manual, and `memhub doctor` still only reads agent-CLI config).

Skill resync (decision 97; resolves task 50, internalizes the fact-10
manual `cp`): the same `memhub upgrade` also refreshes the installed
slash-command wrappers so they never lag the binary. For each agent
dir that **already exists** — `~/.claude/commands/` (flat `*.md`),
`~/.codex/skills/` (dir-per-skill), `~/.config/opencode/skills/`
(dir-per-skill), and `~/.config/opencode/commands/` (flat `*.md`) — it
copies from the source repo's `templates/skills/{claude,codex,opencode}/`
and `templates/commands/opencode/`. It runs in the orchestrate phase
(the old binary, where `templates/` lives) and the result is rendered
by the re-exec'd child in one table. The copy is **additive**: a skill
removed/renamed in `templates/` leaves a harmless installed orphan —
settled against mirror-with-prune because pruning shared user-global
dirs risks a user's own same-named skill, while an orphan is just a
stale slash-command. Idempotent, best-effort (a partial/permission
error degrades to a `warn` row, never fails the upgrade — same posture
as the registry/metrics writes). `--no-skills` skips the step; the
binary + DB migrate still run. The manual `cp` recipe (fact 10) is now
a fallback only.

Instances are enumerated from a **self-maintaining registry**, never a
filesystem scan: every `db::open_project` does a single guarded,
debounced UPSERT into `known_projects` in `~/.memhub/global.sqlite`,
but **only if that store already exists** — the common
repo-with-no-global path pays one `stat`. A repo memhub has never
opened since this landed is absent from the first run but
self-migrates on its next open; seed it explicitly with `memhub
upgrade --also <path>` (repeatable; also persists it). Migration
`0015_known_projects` adds the table to the shared MIGRATIONS list; it
is read only from the global store.

Hard invariants: registry membership is **not** M9 global-memory
opt-in — recall never reads `known_projects` and stays gated on each
repo's own `[global] enabled` (a populated registry must not change
recall output: the eval-regression guarantee, tested in
`tests/upgrade_registry.rs`). `upgrade` migrates the global store only
if it already exists; it never creates it (opting in stays the
explicit `memhub global enable` choice). `known_projects` is
machine-local and **not** exported by `memhub export`. Skill resync
likewise only ever writes into an agent dir that already exists — it
never creates `~/.claude/commands`, `~/.codex/skills`,
`~/.config/opencode/skills`, or `~/.config/opencode/commands`, and a
non-directory at that path is a clean skip, not a clobber (mirrors the
PATH-shadow and global-store "only act on what exists" rule). The
session-start hook install follows the same rule at the directory level:
it never creates `~/.claude` or `~/.codex`, and skips an agent whose
directory is missing. It differs at the file level: inside a directory
that exists, it creates a missing `settings.json` / `hooks.json`.

**Session-start hook (issue #286).** `memhub hook session-start` is a
hidden command (not in `memhub --help`) that prints the current repo's
rendered `PROJECT.md` (`<repo>/<[render] output_dir>/PROJECT.md`) to
stdout byte for byte and nothing else, so an agent CLI gets the frame at
session start without the agent obeying a "read PROJECT.md" instruction.
It finds the repo from the working directory the same way every command
does (walking up to the nearest `.memhub/`), and it never breaks a
session: outside a memhub repo, or when the repo's config or `PROJECT.md`
cannot be read, it prints nothing and exits 0. It never opens or migrates
the DB and never writes a file, so what it prints is whatever the last
`memhub render` wrote.

`memhub upgrade` installs a user-scope SessionStart entry that runs it:

- `~/.claude/settings.json`, matcher `startup|resume|clear|compact`,
  handler `{"type": "command", "command": "memhub hook session-start",
  "timeout": 30}`;
- `~/.codex/hooks.json`, matcher `startup|resume`, handler `{"type":
  "command", "command": "memhub hook session-start", "timeout": 30,
  "additionalContextLimit": 4000}`.

`timeout` is in seconds in both tools. The Codex handler carries
`additionalContextLimit` because Codex shows the model at most about
2,500 tokens of a hook's output by default and, past that, saves the full
text to a file and hands the model only a head-and-tail preview;
PROJECT.md is capped at 8,000 bytes, which sits near that limit. (Before
#300 both agents got the same handler, without the field.) The Claude
Code handler does not carry it.

The install runs in the `--finish` pass (the freshly installed binary),
so the first upgrade from a binary that predates it still installs the
hook. It acts only on an agent whose directory exists: if `~/.claude` or
`~/.codex` does not exist, that row reads `skipped (no ~/.codex)` (or
`~/.claude`) and nothing is created for it; if the path exists but is
not a directory, the row reads `skipped (~/.codex is not a directory)`
(or `~/.claude`) and nothing is created or changed. No agent directory
is ever created, and `~/.claude.json` is never touched. Writing the
install record (below) may create `~/.memhub` if it is missing, so that
is the one directory the step can create. `memhub doctor`'s
MCP-registration verdicts are unaffected: its Codex check keys on
`~/.codex` existing, and its Claude check passes on a repo-scoped
`.mcp.json` that registers memhub before it reads `~/.claude.json`.
It is a merge, not an overwrite: every other key, setting, and hook
(including other SessionStart groups) keeps its value — key order may
change, because memhub's JSON writer does not preserve order, and that
also applies when an existing Codex entry is updated — and the write goes
to a temp file in the same directory and is renamed into place (a
pid-unique temp, fsynced; removed if any step fails; on Unix the
directory of the replaced file is fsynced after the rename). Inside an
existing agent directory a missing file is created.

What counts as the memhub entry: a command that is exactly one program
token followed by `hook session-start`, where the token is unquoted (no
whitespace, quotes, or shell characters) or wrapped whole in one pair of
`"` or `'`, and its last path component is `memhub` or `memhub.exe` in
any letter case. So `/home/me/.cargo/bin/memhub hook session-start` and
`"C:\Users\me\.cargo\bin\memhub.exe" hook session-start` count as
present; a command that runs another program (`echo /x/memhub hook
session-start`) or passes a memhub path as an argument does not. (Before
#300 only the exact string `memhub hook session-start` counted.) An
entry that counts is never duplicated, and neither agent's file is
rewritten when no handler changes. The only change made to an existing
memhub handler is adding `"additionalContextLimit": 4000` to a Codex
handler that lacks the field (any other value already there is left
alone); the row reads `updated
SessionStart entry` (`would update SessionStart entry` under
`--dry-run`) and says to approve the changed hook again in Codex's
`/hooks` screen, because Codex re-asks whenever a hook's definition
changes (it records trust against the hook's hash) and skips the hook
until you do.

A file that is not valid JSON, or whose `hooks` / `hooks.SessionStart` is
not an object / array, or that memhub cannot open for writing (e.g.
read-only), is left byte-for-byte unchanged and reported as `not
installed`; the rest of the upgrade still completes. So is a dangling
symlink at the hook file: nothing is created, and the row reads `not
installed` with "a symlink whose target does not exist; left
unchanged".

memhub records each file it added the entry to, or found it in, in
`~/.memhub/installed-hooks.json`, keyed by `~/.claude/settings.json` /
`~/.codex/hooks.json` rather than absolute paths (so a removed entry
stays removed however HOME is spelled; records written with absolute
paths are still read). If you later delete the entry, upgrade reports it
`left out` ("memhub's entry was in this file at an earlier upgrade and
has since been removed; not put back") and does not add it back (delete
that record to opt back in). `--no-hooks` skips the step (both rows read
`skipped`); `--dry-run` reports `would add` / `would update` without
writing either file or the record. The first upgrade from a binary older
than this change cannot take `--no-hooks`: that run is orchestrated by
the old binary, whose argument parser rejects the flag. To skip the hook
on that first upgrade, run `cargo install --path .` in the source repo
first, then `memhub upgrade --no-hooks`. Codex runs a new or changed
hook only after the user approves it once in its `/hooks` screen, so
whenever upgrade adds the Codex entry its report row says to do that.
OpenCode is not supported: it has no config-only session-start hook (it
would need a TypeScript plugin), so upgrade installs nothing for it.
`CLAUDE_CONFIG_DIR` and `CODEX_HOME` are not honored; the files are
always under the home directory.

Known limits of the write the installer makes when it adds or updates an
entry (it replaces the file through a temp file and a rename):

- hard links and Windows ACLs on the replaced file are not kept;
- a change another program writes to the same file during the install
  can be lost;
- the JSON writer is built without `preserve_order`,
  `arbitrary_precision`, or `float_roundtrip`, so integers outside the
  64-bit range and some floats may not round-trip exactly, a duplicated
  key keeps only its last value, and key order may change.

**Build-artifact GC (`memhub gc`).** Cargo's `target/<profile>/deps/`
is append-only — every rebuild writes a new hash-suffixed artifact and
never reclaims the old one; with memhub's `include_bytes!`'d ONNX
models each stale `libmemhub-<hash>.rlib` / test binary is ~1 GB, so a
few weeks of `cargo test` strands 100+ GB. `memhub gc` keeps only the
newest-mtime hash per **memhub-owned** stem (`memhub`, `libmemhub`,
each top-level `tests|benches|examples/*.rs` basename) and deletes the
superseded hashes plus their `.fingerprint/<stem>-<hash>` dirs.
Third-party dependency rlibs carry one hash, never balloon, and are
structurally never considered; `incremental/` is left alone (deleting
it only slows the next build). Worst case of pruning a superseded hash
is one rebuilt test binary — Cargo recovers, the current set is never
touched, so it cannot corrupt a tree. Runs **automatically inside
`memhub upgrade`** (best-effort, never fatal — same posture as the
skill/registry writes; `--no-gc` skips it, `--dry-run` reports it) and
standalone as `memhub gc [--dry-run] [--json]`. Pure `std::fs`,
OS-agnostic (macOS + Windows). Intentionally not a skill — ops
housekeeping like `upgrade` itself, surfaced via the CLI and the
upgrade flow.

## Attribution

Two columns split the work. `source` on `facts` and `decisions` is the
*origin of the claim*; `actor` on `writes_log` and `pending_writes` is *who
performed the write* (free-form, e.g. `cli:user`, `claude:wrap-up`). When an
agent proposal is staged the source is recorded as e.g. `agent:claude-code`;
when you accept it with `memhub review accept` (typically at `/wrap-up`), the
durable row's `source` becomes `user+agent:<actor>` automatically, so both
signals are preserved and you can always tell what was verified. Design
anchor:
[docs/reference/memhub-prd-source-vocabulary-addendum.md](memhub-prd-source-vocabulary-addendum.md).

Skill writes carry a fixed actor and source per agent:

- Claude Code: `actor=claude:wrap-up`, `source=user+agent:claude-code`
- Codex: `actor=codex:wrap-up`, `source=user+agent:codex`
- OpenCode: `actor=opencode:wrap-up`, `source=user+agent:opencode`

All three share one database, and every write is tagged, so you always know
who surfaced what:

```text
source                      Meaning
────────────────────────────────────────────────────────────────────
user                        You typed `memhub fact add` directly
agent:codex                 Codex proposed it (still in pending_writes)
agent:claude-code           Claude proposed it
agent:opencode              OpenCode proposed it
user+agent:codex            Codex surfaced via /wrap-up, you approved
user+agent:claude-code      Same, Claude-side
user+agent:opencode         Same, OpenCode-side
git                         Reserved for git ingestion
observed                    Reserved for observed signals
```

## Current Build Focus

The repository currently provides Milestone 1 scaffolding and a usable local CLI foundation. Future work should extend from these boundaries instead of replacing them.
