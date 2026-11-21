# memhub — operations reference

Operational detail for memhub's subsystems, moved out of the repo `CLAUDE.md` / `AGENTS.md` orientation files so those stay lean at session start (Wave 2 token diet, issue #30). Nothing here is load-bearing from turn one — the two orientation files keep the must-have-inline set (Session Continuity, Guardrails, Delegation, the `stale_embeddings` / `missing_embeddings` and `sync_adopt` safety gates, Build/Test/Run). Everything below is **memhub-recall-searchable**: this file is ingested with `memhub doc add`, so recall it on demand instead of loading it every session.

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
step is needed even if the schema bumped on another machine. Before
issue #405 an open with a pending migration migrated the DB in place
and kept no copy; it now first copies the DB to its pre-migration
backup (see "Safety backups" below). Before issue #441 an unlocked
pre-check could report no pending migration, then a concurrent restore
could install an older schema before the migration transaction's first
read; those newly pending migrations ran without a copy. Now every
file-backed caller of `apply_all` (repo opens and migration-capable
global opens, not `open_global_unmigrated`) takes SQLite's write lock
before the final schema checks, so that race is covered.

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

Before issue #405 an import that wiped the target's rows (`--force` on
a populated target, or a target holding only `writes_log` rows) kept no
copy of what it removed. It now first copies the target to its import
backup and prints that path (see "Safety backups" below). Before issue
#441 the emptiness/count checks and copy preceded a deferred import
transaction, so a concurrent committed row could be wiped while absent
from the copy. They now share one write-locked transaction.

Before issue #382, after import the target's embeddings were not yet
built (only the rows). You ran `memhub index` to populate them — the
import output printed this hint — and until then recall fell back to
FTS-only and could miss vector-similar matches. Now an import into a
hybrid-mode target (the default) embeds every imported row once the
import commits and reports the count; an fts-mode target still writes
no embeddings. If embedding fails after the commit, the import exits
with an error, the rows stay imported, and `memhub index rebuild`
embeds them.

If recall later surfaces a `stale_embeddings` warning (most likely
after an embedding-model upgrade on either machine) or a
`missing_embeddings` warning, follow the same rule as everywhere else:
surface it and ask before invoking `/reindex`.

Previously export wrote JSON v1, with defaults for later-added fields so
older files still imported. Export now writes v2 to preserve fact and task
lifecycle state. Import accepts both v1 and v2; v1-only builds refuse v2
rather than silently dropping that state. See the [export format](export-format.md)
and `src/export/{v1,v2}.rs`.

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

This is intentional, not an oversight: before prune review even `review
stale` went through the same `open_project` path as `recall`, `status`,
`doctor`, and `eval`. Now `prune scan` and its `review stale` alias use
an explicitly read-only SQLite connection and bypass all open maintenance.
Before, `review stale` seeded a missing `config.toml` during its maintained
open. Both scan entry points now use `config.example.toml` in place when
the live config is absent, or the built-in defaults when both are absent.
Neither entry point writes a config file or falls back from a broken config.
`recall`, `status`, `doctor`, and `eval` still use `open_project`, so they
perform the same maintenance a write command would. The migration apply
and the `projects` upsert are
load-bearing and fail-closed by design: an error from either one
propagates and fails the calling command, read or not — that is the
project's "fail loudly" guardrail, not an oversight to fix. Only the
upgrade-registry recording, the metrics scrape/maintenance, and the
pending-write auto-expiry are best-effort, either idempotent or
debounced, so none of those three can turn a read into a failure; a
`memhub status` or `memhub recall` call that happens to run right
after a schema bump, or with a 31-day-old pending write sitting
around, quietly brings the DB up to date as a byproduct once the
migration itself has succeeded. There is still no general-purpose
`open_project_readonly` / `open_project_maintained` switch — one maintained
open path keeps its callers' behavior consistent, at the cost of a read command
occasionally doing the small amount of writing (a migration, a
registry upsert, an expiry pass) that a purist "reads never write"
rule would forbid. This contract covers the repo DB only: the
machine-global store follows the opposite rule for migrations (see
"Machine-global memory" below).

**Paired MCP launches and the lookup-only profile preserve this contract
(issues #436 and #439).** When `memhub serve --memory-root <primary>
--code-root <checkout>` is used, every permitted memory-backed MCP operation
passes the fixed primary root into the same `open_project`-using command paths
as before. The full profile still includes the existing direct/staged writes,
sync, and transcript operations; lookup-only permits `status`, `recall`, and
`get_row`. Those semantic lookups can therefore still migrate, upsert
metadata, record/debounce registry or metrics maintenance, and expire
proposals.
Before prune review, MCP `status` also computed the old CLI stale queue,
including reading ingested document files, without exposing that count.
Now MCP `status` does neither; its response fields and maintained open
are unchanged in both MCP profiles.
The separate code root does not introduce a physically read-only memory
path. `locate` is the exception it already was: it reads the primary
config but opens only the selected checkout's disposable code cache.
The primary `.memhub` directory identity is pinned, while its DB may still
be atomically replaced by the existing confirmed sync-adopt path. On each
call the metadata directory, DB/config and SQLite sidecars must nevertheless
remain direct, single-link files at their canonical expected locations;
memory-directory junction/symlink or DB/config aliasing fails before open.

## Safety backups

Three operations replace data that reverting the code cannot bring
back, and each first writes a copy of the DB with SQLite `VACUUM INTO`
(a consistent copy that includes committed WAL state). Each copy is a
**single slot**: one fixed file that the next backup of the same kind
replaces, so only the most recent copy of each kind is kept. Each is
written under the directory that holds the DB file (`.memhub/` for a
repo DB, `~/.memhub/` for the machine-global store). If a copy cannot be
written, the operation fails before it changes the DB, and the error
names the backup path. `memhub gc` prunes only `backups/rendered/` and
`backups/markdown/`, so it never prunes any of these three. The new
`memhub prune backups` excludes all three safety-slot directories too;
delete one by hand once you no longer need it.

All three call `db::write_backup_slot` and therefore write the same way.
The copy first goes to a temp file beside its slot, named `.<slot file
name>.<process id>.<n>.tmp`, and is renamed over the slot only once it
is complete. A copy or publish failure leaves the earlier slot intact.
Before issue #407 sync adopt and import deleted the earlier copy before
writing the new one, so a failed copy left no copy at all; only the
pre-migration copy went through a temp file.

Before each copy, `write_backup_slot` attempts to remove **every**
matching temp sibling; the name alone cannot distinguish a killed
attempt from a live copy. Before issue #407 nothing removed abandoned
temps, and one at an exactly reused name made that open fail. Where the
OS permits unlinking an open file, cleanup can also remove a matching
live temp from a concurrent copy into the same slot; that copy later
cannot publish and its guarded operation fails closed. Where the OS
refuses to remove the open file (notably Windows), cleanup leaves it
alone. Same-DB import, adopt and migration copies are now serialized by
SQLite, so a live collision requires distinct DBs sharing one slot (the
home-root edge case below) or another direct caller. These cleanup and
fail-closed rules apply to every `write_backup_slot` caller, not only
pre-migration copies.

- **Sync adopt:** `backups/sync/last-replaced.sqlite`, written by `memhub
  sync adopt` (and the MCP `sync_adopt` with `confirm=true`) while the
  staged snapshot's in-place restore holds SQLite's destination write
  lock. Adopt starts the online-backup session with a zero-page step,
  copies the committed local DB, then restores through that same session.
  A writer that committed before the lock is in the copy; a later writer
  waits or gets the existing bounded contention error. Before issue #441
  adopt copied first, dropped that connection, then opened a separate
  restore, so a write could commit in between and be absent from both the
  replacement and copy. A copy failure performs no restore and preserves
  the earlier slot. A restore failure rolls SQLite's destination
  transaction back and leaves the completed copy. Adopt reports the path
  (`backup_path`). This was the only one of the three before issue #405.
- **Import:** `backups/import/last-replaced.sqlite`, written by `memhub
  import` under the import transaction's `BEGIN IMMEDIATE`, just before
  its wipe, whenever the wipe would remove at least one existing row of
  the target (a durable row, a `writes_log` row or a decision chunk).
  The no-`--force` decision, retained-doc and overwritten-audit counts,
  copy decision, copy and wipe all observe that protected state. A writer
  that commits first is counted and copied (or makes no-force import
  refuse); a later writer waits or gets the ordinary 5-second contention
  error, then may retry after replacement. Before issue #441 those reads
  and the copy happened before a deferred transaction, leaving a
  copy-to-wipe gap. The import output prints the path. An import that
  removes nothing, such as `memhub init --from-backup` into its freshly
  created DB, writes none. This applies to every
  `commands::import::run` path, including CLI import and
  `init --from-backup`.
- **Pre-migration:** `backups/migrations/pre-migration.sqlite`, written
  when a repo DB or the machine-global store that already has at least
  one applied migration is opened with one or more migrations pending
  (a missing tail or a gap in `schema_migrations`), before any of them
  runs. That is the first open after a binary update by any command:
  every repo command, `memhub init` on an existing DB, `memhub upgrade`,
  the MCP server and the global writes. Opening a DB at head or creating
  a new one writes none. The copy is logged to stderr only, so `--json`
  output and the MCP server's stdout are unchanged. Every file-backed
  caller of `apply_all` now first takes the DB's write lock, refuses a
  newer-than-supported schema and checks for pending migrations under
  that lock; migrations run in the same transaction. This covers every
  repo open and migration-capable global open, while the deliberately
  unmigrated global read path does not call `apply_all`. The independent
  reader used for the copy has an explicit 5-second busy timeout. So no
  other process can commit a write between the copy and migration, and
  when two processes open the same DB at once the loser takes no copy
  instead of overwriting the winner's pre-migration state with an
  already-migrated DB. Before issue #441 an unlocked pending check
  discarded the backup path and selected a deferred transaction when it
  saw a head DB. That made no-pending opens avoid this lock, but a
  concurrent older-schema restore before the first transaction read
  could then trigger an unbacked migration. After issue #441 even a
  no-pending file-backed `apply_all` call briefly takes the write lock;
  another writer waits up to its 5-second busy timeout, then fails
  explicitly. Fresh file DBs remain supported and write no pre-migration
  copy; in-memory migration callers still pass no backup path and use a
  deferred transaction. Before issue #407 the copy was written to its temp
  file before the write lock was taken and moved into the slot only if a
  migration was still pending once the lock was held, so a write another
  process committed between the copy and the lock was missing from the
  copy. A repo rooted at your home directory shares
  `~/.memhub/` with the global store, so their two pre-migration copies
  share one slot there; if both are copied at the same moment, one may
  delete the other's temp file, and that open then fails closed with
  nothing migrated (run the command again).

## Cross-machine Drive sync

Milestone 10 (design anchor:
[docs/reference/memhub-prd-addendum-m10-drive-sync.md](docs/reference/memhub-prd-addendum-m10-drive-sync.md))
makes one user's repo memory follow them between their own machines
through a synced folder, **without memhub ever going online**. It is
the export/import flow above, automated with baseline-relative checks.

**Model (decisions 102/103/104).** Whole-DB **snapshot**, not row
merge: each push writes a consistent single-file DB copy (`VACUUM
INTO`) plus a `manifest.json` (the exact on-disk files and the
crash-safe write order are in "Remote layout and publication
atomicity" below). Divergence is decided from a **logical
version** (the durable-content digest and the `writes_log` maximum ID and
row count, never file bytes — SQLite is byte-unstable), so `check` reports
a baseline-relative verdict:
`up-to-date` / `local-ahead` / `drive-ahead` / `diverged` /
`no-remote`. Before the baseline-safety correction, `drive-ahead` was
described as a safe fast-forward and `diverged` as the only lossy case
(both sides changed). Now unequal copies with regressed audit max ID or
count, or unchanged baseline counters but different durable content,
report `diverged` with `baseline_warning`. A returned older fork can
therefore diverge even when local still equals its last-push marker.
Equal local/remote logical versions still report `up-to-date` despite a
stale marker. Counters and content equality do not prove ancestry:
independent histories with higher counters remain undetectable by this
focused guard, so `drive-ahead` is not a guaranteed safe fast-forward.
Surface its warning and preserve both histories before deciding what to
replace. Adoption is **operator-gated**, never automatic.

**Lifecycle undo (#445).** Before, this section described the logical version
as the digest alone; equality has always required all three fields above.
Before 0026 there were no lifecycle undo pairs. Now, for example, retire then
unretire, archive then unarchive, drop then undrop, or keep then unkeep (with
no prior keep undo saved) restores the original content digest if nothing
else changed. Both actions append audit rows, so the logical version still
differs. If Drive is still at the shared baseline, `sync check` reports
`local-ahead` and `sync commit` refuses to record equality.

**Prune dismissals (#451).** Before 0027, there were no dismissals to
copy or compare. The whole-DB snapshot and adopt now carry
`prune_dismissals`, and a populated table contributes to the logical
digest as well as its logged inserts contributing to the audit counters.
An empty table does not change the pre-0027 digest or baseline. Other
memory, audit-cutoff rules, and schema/checksum/confirmation gates are
unchanged. `sync check --diff` shows dismissals as inserts into
`prune_dismissals` when the audit cutoff is comparable.

`sync check --diff` validates the audit cutoff against both actual
databases. An incompatible cutoff returns null sides with an explicit
`unavailable_reason`, never an available empty diff. A newer remote schema
also makes the diff unavailable, even with a comparable cutoff. Unavailable
does not mean there are no unique records; an available audit diff is not proof
of complete reconciliation or ancestry, either.

Scope is deliberately **single-user across
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
- **Mixed versions since migration 0026:** Two 0025 clients could adopt
  each other's compatible snapshots. A 0025 client now blocks adoption
  of a 0026 snapshot even when no lifecycle fields are populated and its
  logical version matches (`up-to-date`). The schema block is separate
  from the content verdict, and `/catch-up` stops on it. A 0026 client
  can still adopt a 0025 snapshot if the other guards pass; reopening
  migrates it and preserves its logical baseline. The push guard still
  checks `drive-ahead` and `diverged`, not the adopt-only schema flag.
- **Mixed versions since migration 0027:** The same existing rule now
  rejects 0027 snapshots for adopt and `sync check --diff` on a 0026
  client, even with an empty dismissal table. A 0027 client can still
  adopt a compatible 0026 snapshot and migrate on reopen.
- Skill: `/catch-up` orchestrates the pull side (check → summarize →
  adopt with your approval).

How `adopt` installs the snapshot (F4/X6): it stages the remote
snapshot into a local `.memhub/project.sqlite.incoming` copy, hashes
**that** copy, and verifies it against the manifest — so a snapshot
Drive rewrites between hashing and install can never be installed
unverified. It then starts SQLite's online-backup restore with a
zero-page step, which takes the live DB's destination write lock without
changing pages. While that lock is held, it writes the current committed
DB to `.memhub/backups/sync/last-replaced.sqlite` (single slot,
WAL-inclusive; see "Safety backups" above), then restores the staged
pages through the same backup session. Before issue #441 the copy and
restore were separate sessions, so a local write could commit in
between and be lost from both. It never deletes or renames the DB (or
its `-wal`/`-shm`) out from under a process that may hold it open: on
Windows that would raise a sharing violation; on POSIX it would silently
orphan another connection onto a stale inode/WAL. Concurrent memhub
processes therefore serialize through SQLite's own locking. Both backup
connections use zero SQLite busy timeouts and the restore has one
bounded retry loop (20 attempts, 100 ms apart). If contention persists,
adopt refuses cleanly before replacement; if restore fails after the
copy, SQLite rolls back the original DB and the valid copy remains.
Nothing torn or half-replaced is observable.

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
its strength in the blended score. Before lifecycle exclusions, a matching
fact or chunk could take a candidate slot. Now retired facts and archived
chunks are filtered before the keyword limit, vector scoring, and re-rank
pool fill. The limits, score adjustments, and fill order still apply to the
eligible pool.
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

**Compact hits in score order (decision 235).** After re-ranking, recall
returns hits in strict re-ranker score order, highest first, for every row
type, after any row named by number (below). Recall does not judge relevance:
the calling agent reads the hits and decides which ones answer the question.
One low cutoff, `[retrieval.scoring] rerank_cutoff` (default -7.0), drops only
nonsense: a row whose re-ranker score is below it is not returned, whatever
its type. A candidate's score is its score against the query or, for a
two-question query, its best part (see "Two-question queries" below). When
candidates existed but none cleared the cutoff and no named row is returned,
the response is empty and carries a `rerank_cutoff_dropped_all` warning that
names the cutoff and the highest score seen. Before lifecycle exclusions,
facts and chunks now out of circulation could participate in that decision.
If filtering leaves no candidates or named rows, the re-ranker does not run
and there is no cutoff warning. Override the cutoff per call with
`memhub recall --rerank-cutoff <F>` or `memhub eval retrieval --rerank-cutoff
<F>`. Calibration: the scores of right and wrong rows overlap, so no fixed
threshold separates them. The default was re-measured 2026-10-17 on 344 blind
questions across six repositories (questions written without the stored
wording, judged for multiple acceptable answers, with 9 off-topic probes per
repository): at -7.0 only 5 questions came back empty and 313 (91%) had an
answer in the top 3, against 19 and 297 (86%) at -5.0, while 46 of 54
off-topic probes still returned nothing (49 at -5.0). Without the re-ranker
(FTS-only mode, `--no-rerank`, `use_reranker = false`) there is no re-ranker
score: `rerank_score` is null, hits keep the blended FTS/vector order and no
cutoff applies (default-included doc chunks are still left out then, since
nothing vetted them). `rerank_cutoff` is a key of its own because every
existing config pins the retired recall-scoring keys it replaces; those keys
still load, have no effect, and `memhub doctor` names them under
`config_ignored_settings`.

Each hit is compact: `source_type`, `source_id`, `scope`, `title`,
`rerank_score`, `created_at`, `status` (tasks only: `open`, `done`, `blocked`
or `dropped`), `stale`, `superseded_by` and `text`, at most 300 characters of the
row's text (ending in `…` when cut) in place of its full body; a tagged fact
also carries `kind`. `memhub recall --json` adds the diagnostic `rank`,
`score`, `fts_score` and `vector_score`, and `memhub recall --full` adds each
hit's whole stored text as `body` (the text output prints it in place of the
cut text). The full text of any hit is one read-only lookup away: the MCP
`get_row` tool (`source_type`, `source_id`, and `scope`, default `repo`) or
`memhub get-row <source_type> <id> [--scope global] [--json]`. It reads repo
rows, and global rows when the repo enabled machine-global memory, and writes
no `writes_log` row, metrics row or query-log line (decision 51). The token
proxy (`recall_metrics`) counts each hit's title and compact text, what the
agent actually receives.

**Partial-token FTS (issue #225).** The recall FTS path first requires every
query token to match one row (a quoted FTS5 `AND`). Before retirement and
archiving, any full-token FTS hit in the searched types suppressed the retry.
Now a retired fact or archived chunk cannot suppress it. If no eligible FTS
hit matches every token in that store for that phrasing, recall retries with
an `OR` of the query's content tokens (English stopwords such as "the", "how",
"does" are dropped) so partial matches still contribute FTS signal. This is
common for keyword bags and natural questions. A full-token hit removed only
by the later accepted-only filter still suppresses the retry. Before
issue #225, a multi-word query whose tokens never co-occur in one row
got no FTS signal at all, the rerank candidate pool was picked by the narrow
cosine band alone, and the right row could fall outside it and never reach the
cross-encoder. bm25 is min-max
normalized over the hits, so the best partial hit would otherwise blend as if
it matched every token (about 0.82 against about 0.36 for a vector-only row)
and crowd the pool; partial-token FTS scores are therefore multiplied by 0.5
(`PARTIAL_FTS_FACTOR`). The code locator (`memhub locate`) is unchanged and
still requires every token. The `rerank_candidate_pool` default is unchanged,
and superseded rows are still demoted rather than excluded for supersession
alone (decision 145). Before retirement that applied to every otherwise
eligible superseded fact; now a separately retired fact is excluded.

**History of the empty and thin result.** A recall whose candidates all fell
under a fixed relevance threshold first returned an empty bundle (decision
33); issues #225 (decision 186) and #323 then returned the best cut candidates
marked as weak leads after every candidate that cleared the threshold, and
default-included doc chunks had a threshold of their own (decisions 90/91,
189). Decision 235 replaced that with the strict score order and the single
cutoff above, after the blind-question measurement showed the marking hid
answers: only 175 of the 265 correctly answered questions came back unmarked.
In the eval harness an `empty` probe now passes only when recall returns no
rows at all; probes whose id starts with `near-` (a related topic with no
stored answer) are reported apart as `near_queries` / `near_passes` and never
count as safety failures; and match needles are checked against each hit's
full stored text, not the cut text. The golden set carries `kw-` keyword-bag
variants and `near-` related-but-no-answer probes for this.

**Explaining an empty or thin result (issue #318).** `memhub recall <query>
--explain` (and `explain: true` on the MCP `recall` tool, an optional boolean)
reports how many candidate rows each stage dropped, demoted or cut, so a recall
that returns nothing, or less than expected, can be diagnosed. With `--json`
the CLI adds an `explain` object; without `--json` it prints an `Explain:`
block after the results; the MCP response gains an `explain` field. Without the
flag nothing is added and the output is unchanged. The counters are bookkeeping
only: the rows returned, their order and their scores are identical with and
without it. Counts are summed over every store the call searched (the repo
store, plus the machine-global store when this repo opted in). Before lifecycle
filters, any otherwise eligible fact or chunk could enter these stages.
Now these counters reflect the surviving inputs; there is no separate
explain counter for rows excluded by retirement or archiving. The reasons,
by JSON key:

- `reranker_ran` (bool): whether the cross-encoder re-ranker ran (hybrid mode,
  `use_reranker` on, at least one candidate). No cutoff applies when it did
  not.
- `dropped_by_cutoff`: re-ranked rows of any type whose score was under
  `rerank_cutoff`.
- `accepted_only_excluded`: rows excluded by accepted-only.
- `superseded_demoted`: superseded rows kept but demoted by
  `superseded_penalty`.
- `rerank_pool_cut`: candidates not sent to the re-ranker: rows beyond
  `rerank_candidate_pool` in the by-retriever fill, and default-included doc
  chunks over `rerank_doc_cap`.
- `result_limit_cut`: rows beyond `max_results` cut from the final bundle. Rows
  under the cutoff are counted only in `dropped_by_cutoff`.
- `docs_dropped_no_rerank`: default-included doc chunks dropped because the
  re-ranker did not run to vet them.
- `docs_not_searched`: ingested doc chunks this call did not search. Before
  archiving, any ingested chunk could count; now archived chunks never count.
  The count is the eligible chunk total (over the stores searched) when doc
  chunks were outside the searched source types, 0 when they were searched,
  whether by default inclusion or `--source-type doc`. A chunk that was
  searched but not surfaced does not count here, unlike in `available_docs`. The machine-global store's
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
superseded row is kept, as in ranked results. Before fact retirement, any
otherwise eligible repo fact could be named by ID. Now a retired fact is
skipped even when named; use `get-row` for direct inspection. Named rows come
in the order the query names them, once each, ahead of every other hit, are exempt from the
cutoff (a named row is an explicit lookup, not ranking), and they count toward
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
`rerank_cutoff_dropped_all` warning off the response.

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
cutoff score every candidate against the **main query** (and, for a two-question query, each of its parts,
taking the highest: see below), never a variant, so each returned hit's
`rerank_score` is its score against the main query (or its best part) and the
cutoff keeps its meaning (a gibberish main query's rows stay under it
whatever the variants say). The main
query's own pool is always a subset of the union, so a variant can never push
out a row the main query would have re-ranked. Cost: the union has up to (1 +
variants + parts) x `rerank_candidate_pool` rows, so re-rank time grows with
the variants sent and the parts found (a recall takes about 240 ms with no
variants and about 380 ms with them, medians of the live-set A/B at pool 20),
plus, per variant or part, one more embedding and, in each searched store (the
repo store and, when the repo opted in, the machine-global store), one more set
of full-text lookups (one per searched source type, and a second set with the
partial-token retry when no eligible FTS hit holds every token; before
lifecycle exclusions, a full-token hit from a now-retired fact or archived
chunk could suppress that extra set) and one more scan of the
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
cutoff and the order. A caller variant is still
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
bundle alongside facts, decisions, and tasks, ranked and cut the same way; no
config flag gates them, unlike docs. Scope a recall to them alone with `memhub recall <query>
--source-type arch` (CLI) or `memhub.recall(query=..., source_types=["arch"])`
(MCP). Before this change the architecture narrative never entered recall; it
was only rendered into `PROJECT.md`. Only the latest body is searchable: every
`arch set` deletes the previous sections and re-derives them in the same
transaction, embedding them eagerly in hybrid mode. A DB whose latest body was
stored before migration 0025 gets its sections on the next open, FTS-only
(open never loads the embedding model); in hybrid mode `memhub index rebuild`
embeds them, and `memhub index status` counts them. Before issue #382 nothing
else embedded them; now `memhub upgrade` embeds them in every hybrid-mode repo,
as does `memhub import` into one. A lone wrapper heading is
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
for the run, the `rerank_cutoff`, the
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

Baseline (hybrid mode, default rerank cutoff -7.0): the golden set is 36
queries (31 match, 1 gibberish empty probe and 4 `near-` probes) and the code
scores Recall@3 = 31/31 match, all 31 at rank 1; the gibberish probe returns
nothing (0 safety failures; its top re-ranker score is about -8.9), and 2 of
the 4 near-topic probes return nothing (`near-embedding-quantization` and
`near-mcp-auth-tokens` return related rows, topping out at -4.91 and -5.51,
which eval reports rather than fails; the test pins both ids). First recorded 2026-07-06 (issue #44) as 100% on
the then-smaller set, extended by issue #225, and re-recorded 2026-10-17 for
decision 235 (before it, all 5 empty-kind probes passed, three keyword-bag
matches only as weak leads after the rows that cleared the old threshold). This is the reference other
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
weights, the cutoff, model swaps), compare and inspect per-query results on the
main file only; score the held-out file once at the end, with the same
`eval retrieval` and `eval compare` procedure below, and report its totals.
Never tune against its per-query outcomes, and do not score it repeatedly
between changes: a held-out set stops being held out once its per-query
results steer the tuning.

First baseline, recorded 2026-10-10 with a debug build of main at 7e17fff
against this machine's database (live config: hybrid mode, re-ranker on,
`rerank_candidate_pool` 20, the re-ranker threshold of 2.0 that decision 235
later retired; K = 5):

| Run | Rank-1 | Found@5 | Empty probes passed |
|---|---|---|---|
| default | 146/190 | 160/190 | 18/18 |
| `--no-rerank` | 110/190 | 153/190 | 0/18 |

For the 36 `vm-` queries alone, the default run found 16 within the top
5 (14 at rank 1) and the `--no-rerank` run 15 (9 at rank 1). With the
re-ranker off no threshold applies, so every empty probe returns hits;
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
returned row: `source_type`, `source_id`, `scope`, `title`, `rerank_score`)
and `warnings` (the warning kinds):

```json
{"ts":"2026-10-11T17:40:02Z","surface":"cli","query":"cargo build","variants":["compile command"],"source_types":["fact"],"max_results":3,"results":[{"source_type":"fact","source_id":1,"scope":"repo","title":"build","rerank_score":null}],"warnings":[]}
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
   (`--no-rerank`, `--rerank-cutoff`, `--mode`) or a config edit is
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
and asks recall for each one, in up to two forms:

- the **title** query is the row's own title (a fact's key, a decision's or
  task's title, an architecture section's heading path). Doc chunks have no
  title query (see below);
- the **passage** query is a 12-word span from the middle of the row's own
  text. A row with no text (a task without notes) has no passage query and is
  not counted for that form.

Superseded facts and decisions are neither sampled nor counted in the per-type
row totals: recall ranks them below their replacements on purpose, so failing
to surface one would not be a recall fault. Done tasks are still sampled, as
recall keeps them visible, as it does dropped tasks. Before lifecycle states,
every non-superseded fact and every doc chunk could be sampled. Now retired
facts and archived chunks are also excluded from both the sample and the totals.

A doc chunk is checked by the passage form only, because a heading-path query
is not a fair known-item probe for one. A chunk's heading path starts with its
document's top heading and shares its words with the parent and sibling
sections of the same document. Only `rerank_doc_cap` (default 3)
default-included doc chunks enter the re-rank pool, so those siblings take the
slots and the target chunk never reaches the re-ranker, even though a worded
question about its content finds it.

Each query runs through the same recall code as `memhub recall`, with an
agent's defaults (default source types and result count; the project's
retrieval mode), against one open of the project for the whole run, so the
rank reported for a row is the rank `memhub recall` returns for that query on
the same database. A row "comes back" when the returned hits include that
repository row.
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
because `include_docs_in_default` is off, or architecture sections that were
never embedded. An architecture section without an embedding is still found
by keyword matching on its exact title (it has a full-text index) and loses
only meaning-based matching; recall warns `missing_embeddings` for it. Passage misses on long, generic text are
less alarming than title misses on distinctive titles.

Like `eval retrieval` (decision 51), the command adds or changes no fact,
decision, task, session note, document, architecture section or `writes_log`
entry, and, since it calls recall with metrics logging off, appends nothing to
`recall_metrics` or the `recall_queries.jsonl` query log. It is not literally
read-only: like every command it runs maintenance-on-open once (see
"Maintenance-on-open contract" above), which can apply a pending migration,
upsert the `projects` row and expire aged pending writes. It runs up to two
recalls per sampled row (one for doc chunks), so the default sample is up to
90 recalls. Test:
`tests/retrieval/self_check.rs`.

### Retrieval modes and configuration

Both modes are first-class; the install prompt asks you to pick one.
Since issue #382 hybrid is the default: a fresh `memhub init` writes
`mode = "hybrid"` (unless the repo tracks a
`.memhub/config.example.toml`, which is copied as is), and a config with
no `mode` key resolves to hybrid. Before it, `fts` was the default and
init wrote `mode = "fts"`. Set `mode = "fts"` under `[retrieval]` to opt
out to keyword-only recall; an fts repo writes no embeddings.

| | **`fts`** (opt-out; the default before issue #382) | **`hybrid`** (default, recommended) |
|---|---|---|
| Scoring | FTS5 BM25 over title + body | 0.5 × FTS + 0.5 × cosine, then re-ranked |
| What it catches | Exact terms, stemmed variants | Exact + paraphrases (`"compile a release"` → `release_build` fact) |
| Per-write cost | 0 ms | ~50 ms eager-embed inside the source-write transaction |
| Per-recall cost | <10 ms | About 240 ms (brute-force cosine + a re-ranker that scores each pool passage on its own); about 380 ms with variants. Medians of the live-set A/B at pool 20 on one machine |
| Disk footprint | None beyond source rows | ~1.5 KB per row (384-dim f32 vector) |
| Network | Never | Never. Model is bundled. |
| Best for | Small projects, scripted use | Multi-month projects where you forget exact wording |

**Switching modes is non-destructive.** `fts → hybrid` needs one `memhub index rebuild`; `hybrid → fts` just stops consulting embeddings.

**Rows without a vector are reported (issue #382).** Opening a DB never loads the embedding model, so rows can exist with no vector for the active model: architecture sections derived by migration 0025, rows written while the repo was in fts mode, or global-store rows written from an fts repo. Hybrid recall adds a `missing_embeddings` warning stating how many rows lack one and naming `/reindex` (`memhub index rebuild`); it counts the whole repo store and, when the repo is opted into machine-global memory and recall searches the global store (not skipped for a schema mismatch), the global store's rows too (issue #392; before, it counted the repo store only), while `stale_embeddings` (unchanged) counts only the candidates of that recall, so a recall can carry both. `memhub doctor`'s `embeddings_freshness` check states the same count. `memhub upgrade` fills these rows (see "Machine-wide upgrade"). The `stale_embeddings` safety gate covers both kinds: an agent surfaces either warning and asks the user before invoking `/reindex` (before this change the gate named `stale_embeddings` alone). Before this change nothing reported a row that had no vector unless it happened to be an FTS candidate.

`memhub index rebuild` leaves each store holding vectors only for the bundled model: it re-embeds every row and deletes vectors written under any other model name. In a repo opted in to machine-global memory it does the same to the global store (the output reports both stores' counts); in a repo not opted in the global store is not touched. If the global half fails for any reason (a store this binary refuses, a corrupt file, or an unresolvable home directory), the repo store, already committed, still has its counts reported: in `--json` `global` is null and `global_error` carries the message, the human output shows the message once, and the command exits non-zero. Re-running the rebuild retries both stores. The code index does the same on its next hybrid refresh: chunks whose only vector is from another model or dimension are re-embedded and the foreign vectors removed.

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
rerank_cutoff = -7.0             # rows re-ranked below this are not returned

[global]
enabled = false                  # opt-in via `memhub global enable`
```

**Staleness alone does not exclude or demote facts (decision 236).** Before
retirement, an otherwise eligible fact never verified, or last verified more
than `fact_stale_after_days` ago, ranked like a freshly verified one and
carried `stale: true`; recall emitted no stale warning. That still holds
unless the fact is retired, which excludes it independently.
`include_stale_by_default`, `[retrieval.scoring] stale_penalty`, the
`--include-stale` flag and the MCP `include_stale` parameter are retired: older
configs still load with them (every `memhub init` pinned both keys, some as
`include_stale_by_default = false`), and the flag and parameter are still
accepted, but none of them changes which rows come back or their order.
`memhub doctor` names the two config keys as ignored settings; deleting them is
safe. Optional age decay (`age_half_life_days`) is separate and unchanged.

`[global] enabled` is managed by `memhub global enable` / `disable` — it's per-machine, so the tracked `.memhub/config.example.toml` baseline ships `false`. The machine-global store has no `[retrieval]` block of its own; it inherits the active repo's. Before archiving, once that store was in scope, `[retrieval] include_docs_in_default` decided whether its docs could join default recall. It still does for non-archived docs; the archive filter applies to every searched store.

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
retrieval. After that, plain `memhub recall` ranks doc chunks with facts,
decisions and tasks in one score order (decision 235): at most `[retrieval]
rerank_doc_cap` chunks (default 3) are re-ranked per call, and a chunk is
dropped only when it scores under `[retrieval.scoring] rerank_cutoff` like
any other row. Off-topic chunks score far below it (about −11 against about
+1.6 for an on-topic chunk), so a UI style guide stays silent on a backend
query while a code style guide surfaces. Scope to docs alone with `memhub
recall <query> --source-type doc` (CLI) or `memhub.recall(query=...,
source_types=["doc"])` (MCP); explicit scoping is not capped by
`rerank_doc_cap` and is unaffected by the
flag. Plain recall still returns `available_docs` — the count of
eligible ingested chunks that did *not* surface this call — so a doc-scoped
follow-up for the long tail stays a judgment call. Set
`include_docs_in_default = false` to revert to strict opt-in.

Surfaces: `memhub doc add|ls|show|rm` (CLI) · `memhub.doc_add` (MCP,
direct write — a doc is a user-pointed artifact, not an agent claim) ·
`/doc` (skill). Re-ingesting an unchanged file is a no-op; changed
content replaces every chunk and refreshes embeddings/FTS.

Before document archiving, all ingested chunks could be searched and counted
as available, and re-ingesting any known path followed the unchanged/changed
rules above. Now only chunks of non-archived documents are eligible in both
default and doc-scoped recall (including `available_docs` and
`--explain`'s `docs_not_searched`); re-ingest refuses an archived path until it
is unarchived. The same rule applies to CLI and MCP ingestion.

**Drift hints in `memhub review stale`.** Before archiving, this command
reported changed or unreadable ingested files and suggested `memhub doc add
<path>` to re-ingest them once available. After archiving was introduced,
the command still printed that hint even for archived documents. Now
`review stale` uses the [prune scan](#prune-review): it offers re-ingest
for changed files and archive for missing or unreadable files, and
does not flag archived documents. Re-ingest still refuses an archived
path: to restore and re-ingest it, first run `memhub doc unarchive
<id-or-path>` and make sure the file is readable. This restriction still
applies to both CLI and MCP ingestion, not just the prune hint.

**Stale facts in review and wrap-up.** Before prune review, `memhub review
stale` suggested `memhub fact verify` for facts past the horizon that
were not superseded, even if they were retired. It now excludes retired
facts as well. `memhub status` still uses the same queue count.
Before retirement, both `/wrap-up` stale-fact re-verification paths could
pick such facts from the default `memhub fact list --json`. Both still use
that list, but it now omits retired facts, so they are no longer wrap-up
candidates. Direct `memhub fact verify <id-or-key>` still works on them.
The standalone stale-fact counts in `status` and `stats` still include
retired facts; the rendered ledger's stale-fact count does not.

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
the selected code checkout's `.memhub/code_index.sqlite` — gitignored,
per-machine, and **never read by any recall entry point (CLI, MCP, or
retrieval eval), never in any export/import payload, and never in any M10
sync operation**. These restrictions apply to every operation of each
named kind, not only the literal `memhub recall` symbol. That physical
separation is what preserves the recall eval-regression guarantee (mirrors
M9's registry-is-not-recall rule). It is also **derivable + disposable**:
no migration framework, just `CREATE TABLE IF NOT EXISTS` + a
`schema_version` in `index_meta`; a version mismatch drops and rebuilds, so
`memhub upgrade` is a no-op for it. The index set is `git ls-files`
filtered through the primary project's deny-list and scoped to
grammar-known source languages (task 69, below).

**Cache placement before and after issue #436 (2026-11-13).** Before,
“sibling” always meant beside the same checkout's `project.sqlite`, because
memory and code were rediscovered from one start path. After, ordinary CLI
and single-checkout MCP launches retain exactly that placement, while an
explicit paired MCP launch keeps `project.sqlite` and `config.toml` only
under the primary memory root and places one independent
`code_index.sqlite` under each selected checkout. No project DB or live
config is copied to a worktree.

The first issue #436 implementation validated checkout roots and source-file
containment but not cache storage. A worktree `.memhub` junction could
therefore point at the primary `.memhub`; locate reported the worktree path
while opening and overwriting the primary cache. A database symlink/hardlink
had the same aliasing class. The corrected contract validates cache storage
before status/open/remove: `.memhub` must resolve to its exact canonical
checkout path, and the database plus WAL/SHM/journal sidecars must be direct
regular files with one hard link. Any alias refuses without touching its
target. This applies to ordinary CLI/eval callers as well as paired MCP.

### MCP launch profile

Before issue #439, every `memhub serve` launch exposed and permitted the full
tool catalog compiled into that build. After issue #439, that behavior remains
the default and is also selected explicitly with `--profile full`; the
additive restricted form is:

```text
memhub serve --profile lookup-only [--memory-root <primary> --code-root <checkout>]
```

The profile works with ordinary project discovery and with the paired binding
below. Clap rejects any other profile value before binding or serving starts.
The existing public `mcp::serve` and `mcp::serve_bound` functions still launch
the full profile.

Lookup-only exposes and permits exactly `status`, `recall`, `get_row`, and
`locate`. `MemhubServer::build_tool_router` first builds the existing router,
merges the feature-enabled `metrics` router, then removes every registered
route outside that positive allowlist. The existing
`#[tool_handler(router = self.tool_router)]` therefore uses the same filtered
router for both `tools/list` and `tools/call`: excluded registered tools,
feature tools, and unknown names all return an explicit MCP error before
parameter deserialization or handler execution. This applies to every tool
route outside the four-name allowlist, not only to named write methods.
Skipping `tools/list`, changing `clientInfo`, request metadata or a claimed
role, passing `confirm=true`, or including a per-call profile/root field cannot
expand the launch-fixed router. A denied call performs none of its requested
semantic/admin writes or external filesystem actions.

This is lookup-only at the MCP surface, not a physically read-only server.
The shared project-open path still applies migrations and project-row upserts
fail-closed, and still attempts registry recording, feature-enabled metrics
maintenance, and pending-proposal expiry best-effort. Server startup still
warms bundled models. `locate` still reads primary config and may initialize
or refresh the selected checkout's disposable code cache; it does not gain a
semantic-memory write. The initialization instructions state these effects
and the unavailable write/admin categories without directing clients to an
unavailable tool or a shell workaround.

The four lookup tools advertise conservative `readOnlyHint: false` and
`idempotentHint: false`, because maintenance or cache refresh may change their
environment. Full-profile `sync_adopt` advertises `destructiveHint: true`; its
existing `confirm=true` gate remains the enforcement. All MCP annotations are
advisory metadata and do not implement the allowlist.

The profile controls one server process's MCP catalog only. The host owns
agent-role policy and any shell/CLI access, so this feature claims neither
authorization nor filesystem isolation outside MCP. Repository protocol
tests spawn the source-built binary over real stdio; they do not install or
register a binary, change host policy, or qualify a child-host launch. Those
installed-server and child-host checks remain separate.

### MCP launch binding

`memhub serve` keeps its ordinary discovery mode. Its additive paired form
is:

```text
memhub serve --memory-root <absolute-primary-root> --code-root <absolute-checkout-root>
```

The options are a pair: either both are present or neither is. Relative
paths are resolved by the OS at launch, so consumers that need
cwd-independent behavior pass absolute paths. The memory root must contain
the existing `.memhub/project.sqlite`; both supplied roots must resolve to
Git checkout roots with the same canonical common Git directory and both
must appear in `git worktree list`. Canonical root, checkout-admin and
common-directory filesystem identities are pinned (Unix device/inode;
Windows volume serial/file index) and the Git relationship is rechecked
before every tool call. A path alias is accepted only when canonicalization
proves it is that same pinned object. Missing/replaced roots, unrelated
repositories, deregistered worktrees, unverifiable paths, or changed Git
identity fail before a tool accesses another project/checkout. There is
still an unavoidable interval between revalidation and later filesystem
operations; this is local binding validation, not an atomic OS namespace
lease.

The alias rule above covers the supplied *roots*. It does not authorize
indirection below them. Primary-memory metadata and selected-checkout cache
storage use the stricter direct-storage rule described above; preserving
incidental symlink/junction/hardlink stores is intentionally removed in
favor of fail-closed project/cache identity.

Before issue #436, ordinary ancestor discovery could start inside a nested
Git worktree with no local `.memhub`, find the primary checkout's metadata
above it, and silently index/return primary-checkout code. Adding local
metadata made code correct but created a second memory location. After,
ordinary project-root/subdirectory discovery and non-Git memory-only
projects still work, but a launch directory whose Git top-level differs
from the discovered memory root fails with a binding error directing the
caller to the paired form. Binding is fixed for the server lifetime: a
later `.memhub` directory in the selected worktree cannot redirect it.

The `status`, `recall`, `get_row`, and `locate` outputs add:

- `binding.mode`: `discovered` or `explicit`;
- canonical `binding.memory_root` and `binding.code_root`;
- `binding.repository_relationship`: `same_checkout`,
  `registered_worktree`, or `memory_only`;
- `binding.git_common_dir` for verified Git bindings.

Their input schemas use closed objects and reject unsupported fields,
including attempted root overrides. In the full profile, sync `remote`, doc
allow-list inputs, proposal staging, archive/adopt confirmations, and
maintained-open behavior keep their existing meanings; none can alter the
launch binding. Lookup-only excludes those non-lookup tool routes while
retaining maintained-open behavior. This contract scopes data only. It does
not qualify child-role permissions, install host configuration, or automate
wrap-up.

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
content hash, re-chunks only what moved, and drops deleted/renamed files. A
warm index is near-free (stat-only); the first-ever index is the one
expensive pass (~171s cold vs ~1.4s warm on memhub's tree), so `memhub code
index` is the explicit warm-up. `locate` is therefore a read-then-write op,
but writes nothing to `project.sqlite`.

Before issue #435, this section described that default as “results always
reflect the working tree.” That was too strong: the scan, model inference,
query, and snippet read do not form an atomic filesystem snapshot, and two
processes could select the same missing embeddings before either inserted
them; one then failed with a duplicate `code_embeddings.chunk_id`. After
issue #435, every production Rust entry point that requests a refresh —
CLI `memhub locate` without `--no-refresh`, eval harness `memhub eval
locate`, MCP `memhub.locate`, and CLI `memhub code index` — reaches these
concurrency and recovery rules through the shared refresh implementation.
CLI locate and `eval::run_locate` call the public
`code_index::locate::locate`; MCP calls the additive
`code_index::locate::locate_bound` so its roots cannot be rediscovered.
`memhub code index` calls `code_index::refresh` directly. A CLI locate with
`--no-refresh` skips refresh but still opens and bootstraps the sibling
database before querying, so it shares the bootstrap serialization rule
below without running the refresh/backfill rules:

- Schema bootstrap and the chunk/FTS refresh take SQLite's writer slot
  before reading the state they will update. Concurrent cold refreshes
  therefore serialize that database phase and the later writer sees
  the earlier commit instead of trying to update a stale transaction.
- Missing-vector selection and model inference still run without a write
  transaction. Persistence rechecks the exact `(chunk_id, embed_text)` and
  ignores an already-inserted primary key. Stable overlapping backfills
  converge on one current-model vector per chunk; delayed work for a
  changed, deleted, or reused row id is discarded rather than attached to
  different text. A later refresh embeds any replacement chunk.
- A model failure happens after the chunk/FTS commit and before vector
  writes. A vector-write failure rolls back that whole pending batch.
  Already committed chunks, FTS rows, and current-model vectors remain
  usable, and the next successful refresh retries whatever is still
  missing. This maintenance remains confined to
  `.memhub/code_index.sqlite`; durable project memory, export, and sync
  contents are unchanged.

**Returned-content correction (issue #436).** Before, the query selected an
indexed chunk and then reread its line range from disk. A later edit,
unreadable file, or metadata fast-path collision could therefore pair an
indexed score with different/empty snippet bytes without saying so. After,
the snippet is clipped from the exact stored chunk body and its SHA-256 is
returned alongside the indexed chunk and file hashes. A default locate
also confines the current file to the selected checkout, reads it, and
requires its raw-byte hash to equal the indexed file hash. A hit that is
unreadable, escapes through a symlink/junction, changed since the index, or
has inconsistent index content is omitted and counted by reason under
`index.freshness.omitted_hits`; refresh-level skipped paths are also
reported and set `incomplete`.

The scored candidate now carries the path, file hash, chunk hash and
embed-text hash observed during candidate gathering. Hydration must see that
exact identity, and the per-file verification cache is keyed by
`(path, indexed_file_hash)`. Before this correction, two hits for one path
could straddle a concurrent index update and the later hit could reuse the
earlier file proof while publishing a different indexed hash. After, a
changed/reused candidate is an explicit `index_mismatch` omission;
internally unequal indexed/worktree hashes are never labelled
`worktree_file_hash`.

The limit is still observation of a moving working tree, not a filesystem
snapshot: enumeration, metadata/hash refresh, query and per-returned-file
verification do not share one transaction, and an edit can occur after the
last verification. Responses therefore say
`freshness.basis = metadata_with_hash_on_change` and
`atomic_tree_snapshot = false`; they never promote the indexed `HEAD` to a
dirty-content identity. The next default refresh converges again.

The first issue #435 implementation serialized schema bootstrap and refresh
but still ran `PRAGMA journal_mode=WAL` in the connection's startup batch.
During a competing cold open SQLite can return `SQLITE_BUSY` from that journal
transition without invoking the configured busy handler, so one process could
still fail immediately before reaching either serialized transaction. The
corrected startup separates that operation and retries only its
`SQLITE_BUSY`, bounded by the same five-second budget. Bootstrap and refresh
continue to use immediate transactions and the normal busy handler; other
journal-setup errors are not treated as transient. Contention that exhausts
the bound reports the existing retryable busy error rather than waiting
forever.

Manual `memhub code rm` and the removal step of `code index --rebuild` are not
coordinated with an active lookup, so run them when no locate/index process is
using the disposable cache and retry/rebuild if they race.

`--no-refresh` (issue #67) opts out of the freshness and maintenance pass
entirely for tight repeat-call loops on an already-warm index — no `git
ls-files`, no per-file stat, no `git rev-parse HEAD` — trading convergence
for the lowest possible latency; `files_total` / `chunks_total` / `head`
then report the last-*indexed* state, not a fresh recount. Stale-by-choice,
explicit opt-in; default behavior (attempt a refresh every call) is
unchanged without the flag. Its snippets now come from and are hash-checked
against the indexed chunk (`verification = indexed_only`), so this mode
still performs no current-file read and does not imply current bytes.

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
`memhub.locate` (MCP — clipped snippets only, never full code or source
edits; default calls may update the disposable cache) ·
`/locate` (skill).

`locate` preserves every pre-#436 response field and adds
`index.db_path`, `index.indexed_head`, typed freshness/incomplete evidence,
and per-hit `content` evidence:
`algorithm = sha256`, `snippet_sha256`, `indexed_chunk_sha256`,
`indexed_file_sha256`, optional `worktree_file_sha256`, and typed
`verification`. `head` and `index.indexed_head` identify the commit recorded
by the cache; only the content hashes identify dirty returned bytes.

### Issue #436 blast radius

| Structural element / caller | Before → after; prior behavior retained? |
| --- | --- |
| `TopLevelCommand::Serve`, `cli::run`, `mcp::serve` / `serve_bound` | The public one-argument `serve` and cwd discovery remain. The paired root flags, `serve_bound`, and launch validation are additive. |
| `MemhubServer` and all MCP tool implementations | One rediscovered `start` path became one launch-fixed, per-call-revalidated binding. Every existing tool still reaches its same command/retrieval implementation and the primary memory DB; no write capability was added. Primary metadata indirection is now refused, while confirmed sync-adopt may still replace the direct DB file. |
| MCP `status`, `recall`, `get_row`, `locate` request/response shapes | Existing fields retain their meanings. Closed request objects now reject unknown/root fields; typed `binding` is additive. Only these four project/code evidence surfaces changed; other tool schemas did not acquire a root override. |
| Proposal, task/command/note/doc/render, sync and archive MCP paths | Their staging/direct-write split, doc confinement, remote operation inputs, and confirmation gates still hold. Their fixed project argument is now the memory root. |
| `db::open_project` and maintenance-on-open | Unchanged. Paired semantic reads still run migrations/upserts and best-effort maintenance against the primary DB; no read-only split was added. |
| `code_index::locate::locate` (public ordinary API) | CLI locate and `eval::run_locate` still discover one checkout/config and keep default/`no_refresh` behavior. The response adds index/freshness/content evidence; snippets now come from verified indexed bodies instead of an unchecked disk reread. |
| `code_index::locate::locate_bound` (new API) | New MCP-only fixed-root entry point; it shares the same query implementation without a second discovery pass. |
| `code_index::refresh` / shared refresh implementation | CLI locate, eval locate and `code index` retain ordinary discovery; MCP supplies code root + primary config. PR #437's immediate transactions, conditional delayed embedding writes, five-second WAL-transition retry, and recovery semantics are unchanged. |
| Code cache open/bootstrap/schema | Same schema and bootstrap path, now under the selected code root. Every public cache operation validates direct checkout-local storage; the raw path opener is crate-private so library callers cannot bypass that check. A CLI `--no-refresh` still opens/bootstrap the cache; `code status` still uses its own connection and does not create a missing cache. |
| Code status and manual remove/rebuild | Existing behavior holds. Status still compares current/indexed HEAD; remove/rebuild remain uncoordinated with active lookups and must be retried if raced. |
| Git walker/source scope | Still `git ls-files` ∩ primary deny-list ∩ grammar-known source. Dirty tracked bytes are included; untracked bytes remain excluded. Final symlinks and ancestor symlink/junction escapes are now both refused. |
| `LocateResponse`, CLI JSON/human output, MCP locate output | Existing rank/path/line/scores/counts/`head` remain. Hash/freshness/omission evidence is additive; human output now warns when knowingly incomplete. Candidate identity is pinned through hydration, and cached file proofs are exact-version keyed. |
| Recall/export/sync isolation | The preexisting physical-separation rule still holds for every operation in those classes, not only one named function. Per-worktree caches are never durable memory, recalled, exported, imported, or synced. |
| Non-Git memory-only and ordinary single-checkout projects | Existing status/recall/write behavior holds. Code locate still needs Git; nested-checkout ancestor drift is the intentionally removed behavior recorded above. |

### Issue #439 blast radius

| Structural element / caller or input state | Before → after; prior behavior retained? |
| --- | --- |
| `TopLevelCommand::Serve`, clap parsing, and `cli::run` | Before, serve accepted only the optional paired roots and always launched the full catalog. After, `--profile full\|lookup-only` is parsed at launch; omission remains full, unknown values fail before server construction, and the roots still require each other and retain discovery/binding behavior. |
| Public `mcp::serve` and `mcp::serve_bound`; internal `serve_bound_with_profile` | Both public entry points still select full and retain their signatures and behavior. Only the CLI's new internal call can select lookup-only. Runtime setup, stdio transport, pre-initialize tolerance, and model warm-up still run for every profile. |
| `ServerProfile`, `MemhubServer::try_new_with_profile`, and the stored profile | The binding was already resolved once at launch and revalidated per call; that still holds. The new fixed enum selects only router contents and initialization instructions and cannot be changed by later requests. |
| `MemhubServer::build_tool_router`, the macro-generated full router, and the feature `metrics_tool_router` | Full still merges the same build-feature routes. Lookup-only filters that already-merged router to a four-name positive allowlist, so a present or future route is excluded unless it is one of those four; this restriction covers every tool route of that kind, not only one named writer or `metrics`. |
| `#[tool_handler(router = self.tool_router)]` for `tools/list` and `tools/call` | The existing rmcp router remains the only dispatch framework. Full listing/calls are unchanged. Lookup listing and direct calls both see the filtered router, so calls made before listing and unknown names fail before any excluded handler. |
| Caller's `clientInfo`, request `_meta`, role/profile/root arguments, and confirmation flags | These inputs still reach handlers where their existing full-profile schemas allow them; attribution still derives from client identity. None can mutate or bypass a lookup-only router. A denied route therefore cannot perform its requested semantic/admin write or filesystem operation. |
| MCP `status`, `recall`, `get_row`, and `locate` handlers and closed schemas | Existing command/retrieval implementations, bound roots, binding evidence, code-content hashes, omission/incomplete reporting, and root-override refusal all hold in both profiles. Only advisory annotations were added; all four conservatively advertise false read-only/idempotent hints because existing maintenance/refresh effects remain. |
| Every other base or feature MCP handler (search/list, direct/staged writes, render/doc, sync, transcript, metrics) | Full-profile reachability, attribution, write split, confinement, and errors remain as before. Lookup-only removes route reachability before deserialization/handler execution; handler implementations themselves were not redesigned. |
| Full-profile `sync_adopt` and `archive_transcript` | Existing confirmation refusals and confirmed behavior still hold. `sync_adopt` additionally advertises `destructiveHint: true`; the annotation is not its gate. Both routes are absent in lookup-only regardless of `confirm`. |
| `ServerHandler::get_info` initialization instructions | Full retains the established routing instructions. Lookup-only now names only its four available tools, unavailable capability classes, maintenance/cache effects, annotation limits, and the host-owned role/shell boundary. |
| `db::open_project`, registry/metrics maintenance, proposal expiry, and model initialization | Unchanged for every profile: migrations/project upserts fail closed; registry, feature metrics, and expiry remain best-effort; model warm-up remains launch-wide. Lookup-only did not add a read-only connection split. |
| `code_index::locate::locate_bound` and checkout-local code cache | Existing binding, primary-config use, tracked/denied-file scope, refresh, verification, and incomplete-result behavior hold. Lookup-only permits the same refresh writes to the disposable cache and no semantic memory-write route. |
| README/operations wire descriptions and stdio protocol fixtures | Before, broad catalog prose described all launches as full. After, the same prose records that removed lookup-only exposure as a before/after while retaining full compatibility, and tests distinguish source-level protocol verification from installation or child-host qualification. Host role policy and shell/CLI access are untouched. |

**Scoring config is independent of recall (R11, issue #73).** Locate's
fusion weights and the test-path down-weight live under their own
`[code_index]` table — `fts_weight`, `vector_weight`, `test_path_penalty`
— rather than sharing `[retrieval.scoring]` with `memhub recall`. The two
tables used to be one struct, so tuning recall's blend silently retuned
locate's too (and vice versa); they are now separate, defaulting to the
same numeric values (0.5 / 0.5 / 0.90) so an untouched install's ranking
is unaffected. `[retrieval.scoring]`'s `superseded_penalty` (and
`age_half_life_days`, `rerank_cutoff`) have no
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
"written by a newer memhub" error. The same forward-only rule holds for
normal repo opens. Before lifecycle states, 0025 was head. A 0026 build
now migrates a 0025 store on either of these migration-capable paths;
a 0025 build refuses to open a 0026 store through them. The global
store's schema is read from
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
With 0025 and 0026 builds, the read-only mismatch behavior applies in
both directions: recall skips the other schema, direct global `get-row`
and `doc ls|show --global` refuse it, and `global status` still reports
stored counts without migrating it. The same rules now apply across
0026 and 0027; the shared schema includes `prune_dismissals`, but prune
commands only read or write repo memory, even in a repo that opts in to
the global store.

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
still manual). `memhub doctor` reads agent-CLI config and reports whether
memhub's MCP server is registered with each CLI, including Antigravity CLI
(`mcp_registration_antigravity`: skipped unless `~/.gemini/antigravity-cli`
exists as a directory, since Gemini CLI also uses `~/.gemini`; otherwise OK when
`~/.gemini/config/mcp_config.json` has an enabled `mcpServers.memhub` entry, else a warning
naming `agy mcp add memhub memhub serve`). It also checks for active
Memhub entrypoint files in existing Claude commands, Codex skills, OpenCode
skills, OpenCode commands, and configured Antigravity shared skills install directories. Its embedded inventory
works outside the source checkout; default builds exclude hibernated metrics,
and metrics-enabled builds include it. Missing files warn with target paths
and source-checkout upgrade guidance. Absent legacy directories are not set up;
inaccessible or malformed paths warn. Customized regular files count as
present regardless of ownership records, and extra user files are ignored.
These checks do not verify content freshness or host discovery and never
repair files or change ownership or host configuration. Human and JSON
output carry the same findings; only `--strict` fails on warnings.

Before issue #427, Antigravity's registration check also accepted disabled
entries and rejected valid UTF-8 JSON with a BOM. It now warns for disabled
or malformed entries and accepts a leading BOM. Before that issue, doctor
did not check Antigravity skills; it now checks `~/.gemini/config/skills`
when `~/.gemini/antigravity-cli` is a directory, warning even if the entire
skill directory is missing. A machine with only Gemini CLI remains skipped.
Registration is a configuration check, not a live connection test.

Embedding fill (issue #382): after each instance's migrate and recall
smoke, the pass embeds, for the active model, every row that has no
current vector (none, or one whose text changed) — in each hybrid-mode
repo DB, and in the global store whatever mode wrote its rows. It never
re-embeds a current row or deletes another model's vectors (that is
`memhub index rebuild`), and it loads no model when nothing is missing.
The table's `EMBEDDED` column (`embedded` in `--json`) reports how many
rows it embedded in each instance; `-` (null) means no fill ran: an
fts-mode repo, or a skipped or failed instance. A fill failure marks the
instance `ERROR`. Before this change the pass wrote no embeddings.

Retired settings (issue #393): for each known repo, before its migrate,
the pass removes from `.memhub/config.toml` the settings `memhub doctor`
reports as ignored (`config_ignored_settings`, one list in
`src/commands/doctor.rs`: `[retrieval] include_stale_by_default` and
`[retrieval.scoring] stale_penalty`, `min_rerank_score` and
`doc_min_rerank_score`). It deletes only a line of the form `key = value`
(any spacing, an optional trailing comment) under that key's own table
header, line ending included; every other byte of the file, comments, blank
lines, key order and CRLF endings included, stays as it was. A removal is
kept only if the edited file parses to the old one minus that key. A key set
any other way (a dotted key such as `scoring.stale_penalty = 0.3` under
`[retrieval]`, an inline table, a quoted key, a value spanning lines) is
left and reported, and a commented-out key is not set, so it is neither
touched nor reported. A file that is not valid TOML, or cannot be read or
rewritten, is left unchanged and reported. The rewrite is a temp file in the
same directory renamed over the old one. A repo with no `config.toml` is
skipped silently. Text output prints a `config: <repo>: removed retired
setting(s) ...` line per repo (and `left ...` or the error); `--json`
carries `retired_settings`, one `{label, removed, left, error}` entry per
repo with something to report. After it, doctor reports `no ignored
settings` in a repo whose keys were all removed. Before this change upgrade
read each repo's `config.toml` (through `open_project`, which also created a
missing one from `config.example.toml` or the defaults) but never edited an
existing one, so doctor kept warning about these keys until they were deleted
by hand.

`--dry-run` previews both. Before this change it listed each instance's
schema and verdict only; it now also prints the `would remove` / `left`
lines (`retired_settings` in `--json`, same shape) and, for each instance
the fill would touch (a hybrid-mode repo, the global store whatever the
mode), how many rows have no vector at all for the active model: `fill: N
row(s) lack an embedding` after the verdict, `would_fill` and
`lacking_embeddings` in `--json` (on each instance and on `global_store`).
The count comes from a read-only connection and is taken only for an
instance already at head schema; one behind head reads `counted after
migrate` (`lacking_embeddings: null`), since its tables may not exist until
migrate. A row whose vector is out of date (its text changed) is re-embedded
by the fill but not counted here. The dry run still changes no config file
and no database.

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

The lifecycle release updates `/recall` and `/doc` guidance for all four
hosts and Claude's `/global` template through that same resync. Older
copies do not describe the new exclusions. Refreshing a wrapper alone
does not add CLI verbs or update MCP descriptions on an older binary.
The serving binary supplies the MCP descriptions, so reconnect after
upgrading it.

Before issue #427, skill resync had no Antigravity target. It now also
copies `templates/skills/antigravity/` into `~/.gemini/config/skills/`,
but only when `~/.gemini/antigravity-cli` is already a directory. That
host marker permits creating the shared skill directory; `~/.gemini`
alone is insufficient. The same ownership, dry-run, additive-copy and
orphan-reporting rules apply. A malformed or inaccessible setup/install
path is reported without clobbering it. Default builds skip metrics.
For the first upgrade adding this target, run the current source via
`cargo run --release -- upgrade` (preview with `--dry-run --no-gc`), or
install the new binary first: the invoking binary performs skill resync,
so an older installed binary cannot enumerate the new target yet.

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
likewise preserves the existing legacy-host rule: it
never creates `~/.claude/commands`, `~/.codex/skills`,
`~/.config/opencode/skills`, or `~/.config/opencode/commands`, and a
non-directory at that path is a clean skip, not a clobber (mirrors the
PATH-shadow and global-store "only act on what exists" rule). The
Antigravity setup-marker exception above permits its missing shared skill
directory to be created, but never creates the CLI setup directory. The
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

## Reviewing proposed facts and decisions

`memhub review accept <id>` makes a proposal durable. For repo proposals:

- A fact with a **different key** can trigger one similarity warning naming
  the existing row. Similarity can flag compatible observations as well as
  potential conflicts; it does not establish a contradiction. The accept
  succeeds without creating a supersession link or changing prior links.
  The warning does not recommend a supersession. If you independently
  determine that the proposed fact replaces another, you can explicitly
  select that row with `review accept <id> --supersede <old-id-or-key>`.
- If an active fact has the **same key but a different value**, a plain
  accept refuses without changing the fact or accepting the proposal. Use
  `--force` to overwrite intentionally; the prior value is recorded in
  `writes_log`. A fact cannot supersede itself.
- A similar existing decision still blocks a plain accept. Use `--force` to
  keep both decisions or `--supersede <decision-id>` to link the old one to
  its replacement. The similarity checks for both facts and decisions are
  skipped when the reranker is disabled or either flag is supplied.

Similarity warnings are best-effort on stderr, including with `--json`;
failure to print a warning does not affect acceptance or the success
output on stdout. Global-targeted accepts do not run the repo checks and
still reject `--supersede`.

## Reversible repo memory states

These repo-only commands take `--actor` and record successful changes in
`writes_log`. None is an MCP write tool. They do not delete rows, document
chunks, or embeddings. Drive snapshots retain the lifecycle fields and the
sync digest detects changes to them. Migrating an unchanged 0025 store does
not change its sync baseline.

**Audit readers:** Before, there were no lifecycle actions to display or
count. Now the ledger's Recent Activity uses its existing window and row
limit to show the new action, actor, and logged reason or undo detail.
`sync check --diff` counts each action after a comparable audit baseline as
an update, using its existing rule that every non-insert action is an
update. Status and stats include these entries in the existing write counts.
Doctor includes them in write recency. CLI `status` (human and JSON) and
`doctor` use the same render-freshness check: before, no lifecycle write
could count as a write since render; now any of the eight actions can
make current output stale. MCP `status` returns counts, not these checks.

**Wrap-up halt and resume:** Before, writing a fact, accepting a fact
proposal, or ingesting a document could fail, but not because a key was
retired or a path was archived. Now those refusals can halt `/wrap-up`'s
ordered approved writes: `fact add` and `review accept` can refuse a retired
key, and `doc_add` or `doc add` can refuse an archived path. The
halt-on-first-failure rule is unchanged: earlier successful writes remain
durable, but the refusal skips render (so the files can stay stale), the
cross-machine sync push, any approved session transcript archive, and the
normal closing report. Report the failure, resolve it, and resume the
remaining approved work without replaying successful writes.

The post-write steps and their gates are unchanged:

1. Render and report the output and backup paths. If only render fails,
   stop and resume at render after fixing it, without repeating the writes.
2. Check sync status. If enabled, check the verdict before pushing a
   snapshot; `drive-ahead` or `diverged` stops the push for `/catch-up`.
   The policy makes `sync commit` optional for baseline verification or
   repair; the OpenCode template lists it after the snapshot.
3. At `transcript` verbosity, Claude, Codex, and OpenCode archive the
   session transcript only after the writes and render succeed and the user
   explicitly approves the unredacted copy. Antigravity still reports the
   archive as unsupported. Standalone `memhub transcript archive` and
   `memhub transcript prune` are unchanged.
4. Give the closing report and audit reminder. OpenCode lists what was
   recorded, skipped, and left pending review. Rendered files are local by
   default, any commit is the user's call, and a new session can start anytime.

**Keyword index:** Before, otherwise eligible facts and chunks in the
full-text index could be returned as hits. Now recall filters retired facts
and archived chunks out before the keyword candidate limit, but leaves them
in the index. They still contribute to the BM25 corpus statistics used to
score other hits. This applies in both keyword-only and hybrid recall.
The candidate-based `stale_embeddings` check now sees the filtered pool,
including when checking for corrupt vectors. The store-wide
`missing_embeddings` count still includes rows out of circulation.

- **Retire a fact:** `memhub fact retire <id-or-key> --reason <text>`.
  `memhub fact list --include-retired` shows its retirement time and reason;
  `memhub get-row fact <id>` still reads its full text directly. `memhub fact
  unretire <id-or-key>` clears only the retirement metadata, without changing
  the value, verification time, or any supersession link.
  **Before:** all facts were in the default fact list and facts ledger, and
  eligible facts could be recalled even by naming their ID. Same-key
  `fact add` overwrote the existing row; accept-time comparison used
  non-superseded facts. **After:** every retired fact is excluded from
  ranked and named-ID recall (with or without filters), rendered fact
  views and the ledger's stale-fact count, the default CLI and MCP fact lists,
  and accept-time similarity candidates. All same-key add/accept
  variants, including `review accept --force`, refuse a retired key and
  name `memhub fact unretire`. Staleness and supersession alone still do
  not exclude a fact from recall; existing same-key rules for
  non-retired facts are unchanged.
  **Ledger supersession links:** Before retirement, a visible fact linked to
  its replacement as `→ key` when the replacement was present, with `→ #id`
  as a fallback. Now every visible fact whose replacement is retired uses
  the `→ #id` fallback. Visible replacements still show their keys.
- **Archive a document:** `memhub doc archive <id-or-path> --reason <text>`.
  `memhub doc ls` marks it archived, and `doc show` still reads its stored
  chunks. `memhub doc unarchive <id-or-path>` restores eligibility without
  re-ingesting or modifying those chunks. If the source file is gone,
  use the ID or the exact stored path shown by `doc ls`. See
  [doc ingestion](#doc-ingestion) for recall and re-ingest
  exclusions. `doc rm` still deletes, and portable export still excludes
  documents in either state.
- **Drop a task:** `memhub task drop <id> --reason <text>` changes an
  `open` or `blocked` task to `dropped`. `memhub task show <id>` shows why.
  `memhub task list --status dropped` and the existing MCP task-list
  status filter accept the new status. Dropped tasks are not open work
  in rendered views, but remain in the full backlog, task recall and
  exports. Before, the ledger said "Open first, then by recency" while
  grouping open, blocked, and done separately. It still groups those
  statuses in that order, then puts dropped after done regardless of
  recency. Within each group it sorts by latest update, then highest ID.
  `memhub task undrop <id>` restores the prior status and
  last-updated time and clears the drop reason. The former task states
  and their filters still work. Previously CLI `task done` and MCP
  `task_done` marked an open or blocked task done and refreshed its update
  time. Both still do that through the same handler and now also accept
  a dropped task directly. Marking a task done clears its drop reason and
  any saved drop or keep undo data; earlier actions stay in the audit log.
  `done` is terminal: `undrop` and `unkeep` cannot restore a done task.
  Once done, the task is subject to done-task recall age decay, which does
  not apply while dropped.
- **Keep a task:** `memhub task keep <id>` and `memhub task unkeep <id>` are
  both new. Keep refreshes the last-updated time shown for an open or
  blocked task and saves the previous time. Unkeep restores that time and
  logs the undo. Only the latest keep can be undone; unkeep refuses when
  there is no saved time or the current time no longer matches the keep.
  Dropping the task suspends unkeep until undrop restores the kept state.
  Marking it done clears the saved keep.

Both the minimal and shared standard/full/transcript `/wrap-up`
read windows still use `task list --status open --brief` to match closures
to IDs. Before, there was no drop to remove a task from that window or
keep to refresh its sort time. Now dropping an open task removes it and
keeping an open task can reorder it. Blocked tasks remain outside the
window, and `task show` still supplies full notes on demand.

## Prune review

`memhub prune scan` assembles a read-only review for the current repo.
Use `--json` for `{"prune_scan": {"total": ..., "counts": ..., "items":
...}}`. Each item names a category, affected row or files, a reason, a
stable identifier, and its available actions. An action's `command` is
an argument array including `memhub`; run it as separate arguments, not
through a shell. An action with `replace` needs the human's new value in
place of that argument. No action is run by the scan.
Human output quotes commands for PowerShell on Windows (including Windows
PowerShell 5.1) and for a POSIX shell elsewhere. Arguments containing
control characters are displayed as exact argv rather than a shell line.

| Category | Before in `review stale` | Now in both commands |
|---|---|---|
| `stale_fact` | Non-superseded facts past the configured horizon (including never-verified and retired facts); verify hint only. | Same `[retrieval] fact_stale_after_days` horizon; retired and superseded facts are excluded. Verify by ID, update value through same-key `fact add` (its usual source, confidence, and verification behavior applies), or retire by ID. |
| `idle_task` | Done tasks over 30 days old, with a `task list --status done` inspect hint. | Open or blocked tasks last updated more than 60 days ago. Keep with `task keep` to refresh the clock, or drop or mark done. Done and dropped tasks are not candidates. |
| `out_of_date_document` | Changed or unreadable files, including archived rows, with a single re-ingest hint. | Changed, missing or unreadable files for non-archived rows only. Re-ingest a changed file (keeping the title), archive a missing or unreadable one, or keep the item. Ingested documents can be external to the repo; drift checks the recorded source path, not a redirected symlink target. |
| `surplus_backups` | No backup candidates. `gc` separately reports/prunes rendered and legacy markdown backups under its own rules. | One item per backup directory over 20 eligible files, showing the surplus paths. Keep on any platform, or request explicit cleanup of that batch on supported Windows storage. |
| Expired pending writes | All expired proposals, with a `review show` inspect hint. | Not prune candidates. Inspect with `memhub review list --status expired` or `memhub review show <id>`. Proposal expiry on maintained open still holds. |

Each category shows up to 25 undismissed items, with an `overflow` count
for the rest. `total` counts all undismissed items, including overflow.
`memhub review stale` still answers only when asked; it now prints the
same report, including in JSON under its existing `review_stale` key.
Previously its items exposed an integer `id`, `message`, and one `verb`;
now they expose an opaque string `id`, `subjects`, `reason`, and `actions`.
JSON consumers should read `review_stale.total` instead of
`review_stale.counts.total`; `counts` now contains per-category totals
and overflow.
CLI `memhub status` replaces `Stale queue:` with `Prune review:` and adds
`prune_review_count` in JSON; the old `stale_queue` key mirrors the new
count. The CLI's separate stale-fact count and all other counts and checks
keep their existing definitions. MCP `status` still returns stored counts
and exposes neither the prune count nor any prune action.
The public Rust `StatusSummary` previously exposed `stale_queue`; it no
longer has that field. Use `commands::prune::scan` for a read-only total.
The old Rust `commands::review` queue entry points (`stale` and
`count_stale_queue`), `DONE_TASK_STALE_DAYS`, and `StaleCategory`,
`StaleItem`, `StaleCounts`, and `StaleReport` are also removed. The
replacement scanner and report types live under `commands::prune`.
When backups cannot be safely inspected (for example, a linked backup
root), both scan entry points and CLI status still report the other
categories once the project has opened. Human output names the omission;
JSON carries `backup_warning` in the scan report or `prune_backup_warning`
in `status`. This only changes the review count, not the maintained
open or its pre-migration backup.

`memhub prune dismiss <identifier> [--actor <actor>]` records a keep
decision without editing the source row or file. It matches a current
candidate and writes one `prune_dismissals` row and its `writes_log` entry
in the same transaction. Repeating an unchanged dismissal does not log
twice. The ID fingerprints the current row and, where applicable, the
on-disk file state. A changed source state still eligible for review
gets a new ID; presenting an old ID does not dismiss that changed item.
In particular, a dangling link at a previously missing document path is
an unreadable entry with a different ID; the link target is not inspected.
Returning to exactly the dismissed state matches the same ID again.
The scan does not prune or otherwise edit old dismissal records. It
reads schema 0026 or 0027 without applying migrations; `dismiss` and
`status` retain the ordinary maintained open. Dismissal audit entries
use the existing render Recent Activity and freshness, doctor recency,
status and stats write counts, and sync audit readers. Drive snapshots
carry the table; JSON export/import do not transfer dismissal state.

**Before (#451 implementation):** `memhub prune backups --yes` could
select all eligible batches regardless of dismissals, on any platform.
`--candidate <identifier>` was optional. Cleanup rechecked paths and
identity before each pathname unlink, but did not pin the verified file
through the unlink.

**After (#456):** Only `memhub prune backups --yes --candidate <identifier>`
can delete. It works without a `Cargo.toml` and does not open the database.
Linux and macOS list the surplus and offer keep, but direct cleanup also
refuses without moving or deleting anything. Both scan formats explain
when cleanup is report-only or the storage check fails.

Selection still uses regular, single-link `.bak` files under
`.memhub/backups/` (the extension used by render and the former markdown
backup writer). It excludes linked paths, directories, unrelated files,
the legacy K9 copy, and the `backups/{sync,import,migrations}` safety slots.
Retention still keeps the newest 20 per directory by modified time,
including times before 1970, with filename breaking ties. The Windows
action requires a current batch fingerprint covering the directory and
all eligible files, including the kept 20. The fingerprint now includes
filesystem identities, so replacing a file with an otherwise identical
copy requires a fresh review; prior backup candidate identifiers and
dismissals don't match the new format.

Cleanup requires a direct path on a fixed, local NTFS volume that
supports Transactional NTFS. Windows pins the directory chain against
rename and takes transactional locks on the eligible batch before
verifying names, identities, timestamps, sizes and contents, including
the 20 files that justify retention. It refuses changed or unverifiable
batches, redirected paths, sharing conflicts, read-only files, and
deletion targets with unreviewed alternate streams. It sets
`FileDispositionInfo` on each verified deletion handle and commits
only after all dispositions succeed; it never falls back to a pathname
unlink. If Windows cannot supply these guarantees, rescan and keep the
files rather than treating the delete action as guaranteed to succeed.
Microsoft has deprecated Transactional NTFS. Volumes that do not
advertise transaction support are report-only.

These restrictions apply only to `memhub prune backups`, not to all
cleanup commands. `memhub gc` retains its existing Rust-project
requirement, scope and `[gc] delete_stale_backups`/`--dry-run` behavior.

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
- Antigravity CLI: `actor=antigravity:wrap-up`, `source=user+agent:antigravity`

All four share one database, and every write is tagged, so you always know
who surfaced what:

```text
source                      Meaning
────────────────────────────────────────────────────────────────────
user                        You typed `memhub fact add` directly
agent:codex                 Codex proposed it (still in pending_writes)
agent:claude-code           Claude proposed it
agent:opencode              OpenCode proposed it
agent:antigravity           Antigravity proposed it
user+agent:codex            Codex surfaced via /wrap-up, you approved
user+agent:claude-code      Same, Claude-side
user+agent:opencode         Same, OpenCode-side
user+agent:antigravity      Same, Antigravity-side
git                         Reserved for git ingestion
observed                    Reserved for observed signals
```

## Current Build Focus

The repository currently provides Milestone 1 scaffolding and a usable local CLI foundation. Future work should extend from these boundaries instead of replacing them.
