# PRD addendum: M10 cross-machine Drive sync

**Author:** Elswick
**Status:** Addendum to [`memhub-prd.md`](memhub-prd.md) (Draft v2). Authoritative for the items it modifies.
**Last updated:** 2026-05-22

This document supplements `memhub-prd.md` rather than replacing it.
The PRD stays verbatim per the project guardrail in `CLAUDE.md`.
Where this addendum and the PRD disagree on the items called out
below, this addendum is authoritative; everything not addressed here
continues to read from the PRD as-written.

This addendum is the design anchor for **Milestone 10: cross-machine
Drive sync**. It picks the **snapshot (whole-DB, last-writer-wins)**
model and explicitly defers the row-level **merge** model to a possible
M11 — see §8.

The origin is a concrete pain: a question asked to Claude on one
machine never reached the DB the agent reads on another machine, so
the second machine had no memory of the first session. The existing
manual `export` → move-the-file → `import` workflow (`CLAUDE.md`
"Cross-machine workflow") solves this in principle but is too manual to
run every session. M10 makes that workflow a one-command session ritual.

---

## What this addendum modifies

| PRD section | Status after addendum | Reason |
|---|---|---|
| §3.2 "Local-first" / §3.5 "boring tech" | **Unchanged — load-bearing.** memhub itself stays 100% offline. It never makes a network call, never holds a Drive credential, never speaks OAuth. The sync *transport* is the agent's existing Drive access; memhub only reads and writes **local files**. | M10 §1. |
| §3.6 "One DB file = one repo" / "per-machine" | **Relaxed (narrow).** A repo's `.memhub/project.sqlite` may now be pushed to / pulled from an out-of-band Drive folder as an opt-in snapshot. Still one DB per repo per machine; the Drive copy is a transport artifact, not a second live store. | M10 §2. |
| §4 "Non-goals" — "no cloud" / "no multi-user sync" | **Clarified, not overturned.** This is **single-user, single-DB, multi-machine** sync via a folder the *user already controls*. It is not a server, not multi-user, not real-time, and not a memhub-hosted cloud. memhub stays offline; the agent is the courier. | M10 §1, §9. |
| §13 "CLI surface" | **Extended.** Adds `memhub sync snapshot\|status\|adopt\|commit`. All operate on local files only. | M10 §5. |
| §16 "Milestones" | **Extended.** "Milestone 10: cross-machine Drive sync" added. | M10. |

The PRD's design principles, other non-goals, export/import (§14),
security (§15), and the M8/M9 addenda are otherwise unchanged. M10
layers on top of M9: a snapshot carries whatever is in the repo DB
(facts, decisions, tasks, docs, embeddings, session notes), but the
machine-global store `~/.memhub/global.sqlite` is a **separate** DB and
is **not** part of a per-project snapshot (§9).

---

## 1. Architecture: memhub offline, the agent is the courier

The defining constraint: **memhub does not grow a network client.** A
built-in Google Drive client would mean OAuth, token storage, refresh
handling, an HTTP stack, and a live network dependency — a direct
assault on "local-first, offline-capable, intentionally boring."

It is also unnecessary. Claude Code and Codex already have Google Drive
access (MCP). So the division of labor is:

- **memhub (offline core)** provides local-file commands: produce a
  clean snapshot, compare a snapshot against the local DB, adopt a
  snapshot, and record that a sync happened. Every one of these reads
  or writes only the local filesystem.
- **The skills (couriers)** use the agent's Drive access to move one
  file in each direction, and call the memhub commands around it.

memhub never knows or cares *how* the file reached the local disk —
Drive MCP, `rclone`, or a synced Drive desktop folder all work
identically. This is also what makes the design agent-neutral: it
answers OpenCode for free, since any agent that can move a file can
drive the same offline commands.

```
┌─ /catch-up (skill) ──────────────────────────────────────┐
│  agent Drive-MCP: download Drive/<id>/project.sqlite → tmp │
│  memhub sync status <tmp>        # offline: compare        │
│  → verdict: up-to-date | drive-ahead | local-ahead |       │
│             diverged                                       │
│  on operator y:  memhub sync adopt <tmp>   # offline       │
└────────────────────────────────────────────────────────────┘

┌─ /wrap-up (skill, extended) ─────────────────────────────┐
│  ...normal wrap-up + memhub render...                     │
│  memhub sync snapshot <tmp>       # offline: clean backup  │
│  agent Drive-MCP: upload <tmp> → Drive/<id>/project.sqlite │
│  memhub sync commit <tmp>         # offline: update marker │
└────────────────────────────────────────────────────────────┘
```

## 2. Sync model: snapshot, last-writer-wins, divergence-gated

The whole `.sqlite` file is the unit of sync. There is **no row-level
merge** in M10 (see §8 for why, and what a merge model would cost).

State is tracked with a **last-sync marker** stored locally under
`.memhub/` (gitignored, per-machine). The marker records the logical
version (§3) of the DB the last time this machine successfully synced,
for both sides. `sync status` then reduces to git-style fast-forward
logic:

| local changed since marker? | Drive changed since marker? | verdict | action |
|---|---|---|---|
| no | no | `up-to-date` | nothing |
| no | yes | `drive-ahead` | safe pull → adopt |
| yes | no | `local-ahead` | push on wrap-up |
| yes | yes | `diverged` | **operator-gated** overwrite y/N |

`diverged` is the only case that can lose data, and it never does so
silently: the operator is told both sides changed and must confirm
which side wins. This is the deliberate cost of the snapshot model —
accepted in exchange for shipping the cross-machine win without the
distributed-systems work a real merge requires.

## 3. Change detection is logical, not byte-level (load-bearing)

SQLite files are **not byte-stable** for identical logical content:
page reordering, `VACUUM`, and incremental autovacuum all rewrite bytes
without changing a single row. A raw file checksum would therefore
report `diverged` on essentially every comparison and make the gate
meaningless.

So divergence is computed from a **logical version**, recorded in the
manifest (§4) and the marker. It has two parts:

- the `writes_log` high-water mark + row count (a cheap monotonic human
  signal), and
- a **digest**: a SHA-256 over the **durable content tables**
  themselves (facts, decisions, tasks, commands, pending_writes,
  session_notes, project_state, project_arch), each row's columns
  rendered in id order.

Equality hinges on the digest. Two subtler approaches were tried and
rejected: row *count* collides (two repos that each added one fact look
identical), and a digest over the `writes_log` collides too — the log
records *that* a fact was added, not its key/value, so two repos adding
different facts log near-identical rows differing only by a
second-granularity timestamp. Hashing the content tables means only
genuinely identical content compares equal, independent of timing or
SQLite page layout. Because the digest is over content, not the file,
it is stable across the `VACUUM INTO` snapshot and the byte-copy adopt.

A file checksum is still recorded in the manifest, but only as an
integrity check against a torn/partial download — never as the
divergence signal.

## 4. Drive layout and project identity

```
Drive/memhub/<project-id>/
  project.sqlite      # the snapshot (clean, single-file)
  manifest.json       # machine_id, logical_version, schema_version,
                      #   file_sha256, created_at, memhub_version
```

**`<project-id>` is derived from the git remote URL** (e.g. a short
hash of the normalized remote). Both machines clone the same repo from
the same remote, so this is a stable cross-machine identity that needs
**no synced config**. Repo *paths* differ per machine (`~/memhub` on
Mac, `C:\…\memhub` on Windows) and so cannot be the key. A repo with no
git remote falls back to an operator-set `[sync] project_id`.

The manifest's `schema_version` enables the safety check in §6; its
`file_sha256` guards against partial downloads; `machine_id` and
`created_at` make "who pushed last, and when" legible to the operator.

## 5. CLI surface (offline; local files only)

| Command | Behavior |
|---|---|
| `memhub sync snapshot <out>` | Emit a **consistent** single-file copy of the DB (SQLite backup API / `VACUUM INTO`, never a raw `cp` of a live WAL'd file — see §7) plus `manifest.json`. |
| `memhub sync status <snapshot>` | Compare the given snapshot against the local DB and the marker; print `up-to-date` / `drive-ahead` / `local-ahead` / `diverged` and the schema-version verdict (§6). `--json` for skills. |
| `memhub sync adopt <snapshot>` | Replace the local DB with the snapshot. **Refuses without `--yes`** (mirrors `import --force`). Records the new marker. |
| `memhub sync commit <snapshot>` | After a successful push, record that the local DB == this snapshot in the marker, so the next `status` is `up-to-date`. |

`[sync]` config, **off by default**, baseline ships disabled in
`config.example.toml` (mirrors `[metrics]` and `[global]` opt-in).
Stores the Drive folder identifier and the optional `project_id`
fallback.

## 6. Schema-version coupling

A raw `.sqlite` carries a schema version, and migrations are
**forward-only** — an older binary cannot open a newer-schema DB. So
`sync status` reads the manifest's `schema_version`:

- snapshot schema **≤** local → fine; `open_project` auto-migrates
  forward on adopt.
- snapshot schema **>** local → **refuse to adopt** with a clear
  message: run `memhub upgrade` first, then retry `/catch-up`.

This turns the one genuinely confusing failure mode into an actionable
instruction, and ties M10 to the existing machine-wide upgrade story.

## 7. WAL consistency

A live DB in WAL mode has `-wal` / `-shm` sidecars; copying the main
file mid-session can capture a torn state. `sync snapshot` therefore
**must** use SQLite's online backup API (or `VACUUM INTO`) to emit one
internally-consistent file, and never shells out to a filesystem copy.
This is the same discipline the existing `export` path already relies
on for a consistent read.

## 8. Why snapshot now, and what merge would cost (deferred to M11)

The snapshot model was chosen over row-level merge for one concrete
reason: **memhub rows use autoincrement integer primary keys.** Fact
`#5` on the Mac is a *different* fact than Fact `#5` on Windows, so two
databases cannot be unioned by ID. A real merge needs **content-hash or
UUID row identity**, which the current schema does not have — that is
new schema work plus per-row conflict resolution, i.e. a real
distributed-systems milestone, not a weekend.

The snapshot model delivers the cross-machine win immediately and, by
keeping the Drive layout and marker design stable, leaves a clean path
to layer merge on later (a future `export.json` alongside
`project.sqlite` in the same `<project-id>` folder) **without** a format
break. M10 ships snapshot; M11 may add merge if last-writer-wins proves
too lossy in practice.

## 9. Non-goals and explicit deferrals

- **Not a server, not multi-user, not real-time.** Single user, single
  DB, multiple machines, manual cadence.
- **Auto-sync-on-session-start is out.** Sync is an explicit
  `/catch-up` (pull) and `/wrap-up` (push) ritual — a network/file op
  stays operator-visible, matching the wrap-up cadence.
- **The machine-global store is not synced in M10.**
  `~/.memhub/global.sqlite` is a separate DB; a per-project snapshot
  does not touch it. Syncing global memory across machines is a
  possible M10.1 follow-up with its own design.
- **Embeddings *do* travel** (they are in the `.sqlite`), which is a
  win over JSON export. This is safe only because both machines run the
  same memhub build after `memhub upgrade`; mismatched embedding models
  would surface as the existing `stale_embeddings` warning, handled the
  usual way.
- **Dated snapshot history / undo is explicitly out.** A `diverged`
  overwrite discards the losing side's unique rows with no recovery,
  and that is **accepted**: the gated y/N prompt is sufficient for the
  single-user reality this is built for. Retaining N dated snapshots in
  Drive as an undo buffer was considered and rejected as scope creep.
- **Single user, permanently.** M10 assumes one operator across their
  own machines. Anyone wanting a genuine multi-user / multi-writer
  setup should fork memhub and add it, or use a tool designed for that
  — it is a different product, not a memhub backlog item.

## 10. Risks

| Risk | Mitigation |
|---|---|
| `diverged` overwrite loses the older side's unique rows. | Detected, never silent; operator-gated y/N with both sides' logical version + timestamp shown. Accepted cost of the snapshot model (§2). |
| Byte-unstable SQLite files make divergence detection nonsense. | Logical version, not file checksum, is the divergence signal (§3). |
| Torn/partial Drive download adopted as truth. | `file_sha256` in the manifest verified before adopt (§4). |
| Older binary pulls a newer-schema snapshot. | `schema_version` check refuses adopt and points at `memhub upgrade` (§6). |
| Live-WAL snapshot captures a torn DB. | Backup API / `VACUUM INTO`, never a raw copy (§7). |
| memhub accreting a network stack. | memhub stays offline; the agent is the only network actor (§1). |
