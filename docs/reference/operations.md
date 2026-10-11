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
rule would forbid.

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
(ms-marco-MiniLM-L-6-v2) over the top `[retrieval] rerank_candidate_pool`
candidates before truncating to `max_results`. The re-ranker adds
~275 ms per recall at pool=20 in exchange for ~+17 percentage points of
Recall@1 on memhub's own golden set (decision 68).

Toggle per-call with `memhub recall <query> --no-rerank`, or globally
with `[retrieval] use_reranker = false` in `.memhub/config.toml`. The
re-ranker is bundled into the binary unconditionally (no Cargo feature
flag) — turning it off in config skips the inference cost but doesn't
strip the model from disk. FTS-only mode bypasses the re-ranker
entirely.

Candidates whose cross-encoder logit falls below
`[retrieval.scoring] min_rerank_score` (default 2.0) are dropped
after re-ranking, primarily to keep gibberish queries from surfacing
as answers (see the low-confidence fallback below for what a recall
returns when the floor drops everything). This replaces the legacy `min_vector_score` cosine floor
(decisions 70, 71) — the cosine band of nonsense overlapped borderline
semantic queries, so a vector-path floor had no safe sweet spot.
The rerank-score band is similarly noisy on memhub's own corpus, so
2.0 is a parity calibration rather than an improvement; override with
`memhub recall --min-rerank-score <F>` or `memhub eval retrieval
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
`rerank_candidate_pool` defaults are unchanged, and superseded or stale rows
are still demoted rather than excluded (decision 145).

**Low-confidence fallback (issue #225).** When the relevance floor drops every
reranked candidate, recall returns the top 3 reranked candidates (never more
than `max_results`) instead of an empty bundle, each marked `low_confidence`
(`"low_confidence": true` in `memhub recall --json` and the MCP recall hit,
omitted otherwise; a `[low-confidence]` tag in the CLI text), and still emits
the `rerank_floor_dropped_all` warning. Treat them as leads, not answers. A
recall in which at least one candidate clears its floor contains no
low-confidence hits, and a hit that clears its floor is never marked. Doc
chunks that joined the default bundle and missed `doc_min_rerank_score` are
never returned as fallback hits (decisions 90/91). Previously, a recall whose
candidates were all dropped by the floor returned an empty bundle (decision
33); this fallback supersedes that rule by user decision of 2026-10-08. In the
eval harness an `empty` probe now passes when it returns no hit that cleared
the floor (low-confidence hits allowed), and each match outcome reports
`matched_low_confidence`. The golden set carries `kw-` keyword-bag variants
and `near-` related-but-no-answer probes for this.

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
  `use_reranker` on, more than one candidate). No relevance floor applies when
  it did not.
- `dropped_by_floor`: rows whose rerank score was under `min_rerank_score`
  (every row except default-included doc chunks). Low-confidence fallback hits
  are among these rows, since the floor dropped them before the fallback
  returned them.
- `dropped_by_doc_floor`: default-included doc chunks under
  `doc_min_rerank_score`.
- `stale_excluded`: stale rows excluded because stale rows were excluded
  (`include_stale` off).
- `accepted_only_excluded`: rows excluded by accepted-only.
- `superseded_demoted`: superseded rows kept but demoted by
  `superseded_penalty`.
- `stale_demoted`: stale rows kept but demoted by `stale_penalty` (the count
  behind the `stale_facts_demoted` warning).
- `rerank_pool_cut`: rows beyond `rerank_candidate_pool` that never reached the
  re-ranker.
- `result_limit_cut`: rows beyond `max_results` cut from the final bundle.
- `docs_dropped_no_rerank`: default-included doc chunks dropped because the
  re-ranker did not run to vet them.
- `docs_not_searched`: ingested doc chunks this call did not search (the same
  number as `available_docs`).
- `global_store_not_searched` (bool): a machine-global store exists but was not
  searched because this repo has not opted in (`[global] enabled`).

**Architecture sections (issue #232).** The latest architecture narrative
(`memhub arch set`) is split by markdown heading, with the same chunker
`memhub doc add` uses, into a derived `arch_sections` table (migration 0025),
and each section is a recall hit of source type `arch_section`, titled
`Architecture — <heading path>`. Sections are part of the default recall
bundle alongside facts, decisions, and tasks, with the same `min_rerank_score`
floor and the same low-confidence fallback eligibility; no config flag gates
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
for the run, the `min_rerank_score` floor, and the
`rerank_candidate_pool` size.
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
the low-confidence fallback. First recorded 2026-07-06 (issue #44) as 100% on
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
passes golden-file validation (`live_golden_file_passes_golden_validation`).
Its queries are the real agent recall queries from this repo's session history
(`seed-` ids), plus keyword bags (`kw-`), natural-language paraphrases
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
code is recoverable from git history.

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
is unaffected. `[retrieval.scoring]`'s `stale_penalty` (and
`superseded_penalty`, `age_half_life_days`, `min_rerank_score`) have no
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
the store is absent, recall is byte-identical to a pre-M9 build (the
eval-regression guarantee).

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
`[retrieval] include_docs_in_default` / its `[global]` mirror
(auto-flips true on the first `doc add` / `doc add --global`). The
`[global]` mirror flips **per repo**, gated on that repo's own current
config value — `doc add --global` in repo B still flips B's mirror on
B's own first call even when the shared global store already holds
docs from repo A (issue #123; the store's `documents` table spans every
opted-in repo, so store-emptiness is not "first add for this repo").

Because `config.toml` never travels through Drive sync — a sync
snapshot is this repo's `project.sqlite` only (a `VACUUM INTO`
snapshot + manifest; see "Cross-machine Drive sync" above) — a
machine that `sync adopt`s this repo's snapshot does not gain the
source machine's `[global] include_docs_in_default` mirror. The
machine-global store (`~/.memhub/global.sqlite`) is itself a separate
per-machine file that Drive sync never touches at all (same as
`memhub export`, above), so `sync adopt` cannot bring global docs
along either way. Run `doc add --global` (or set the flag by hand) on
each machine that should see those docs in its own default recall.

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

## Current Build Focus

The repository currently provides Milestone 1 scaffolding and a usable local CLI foundation. Future work should extend from these boundaries instead of replacing them.
