-- Migration 0016: replay-safe global pending-write accept.
--
-- Accepting a `target:"global"` pending write is a cross-database,
-- non-atomic operation: the durable row is committed to the
-- machine-global store (`~/.memhub/global.sqlite`) first, then the
-- repo-side `pending_writes` status flip commits separately in the
-- repo DB. If the process dies (or the repo-side commit fails) in
-- the window between those two commits, the proposal stays `pending`
-- and is safe to re-accept ONLY if the global write is idempotent.
--
-- Facts are already idempotent (key upsert in `facts`). Decisions are
-- NOT — `decision add` has no natural key, so a naive re-accept would
-- insert a SECOND global decision, and a bad global write poisons
-- every repo on the machine. This table is the acceptance marker:
-- the global durable write and a `(repo_key, pending_id)` marker row
-- commit together in one global-store transaction, so a replayed
-- accept detects the marker and returns the already-written row
-- instead of duplicating it.
--
-- Like `known_projects` (0015), this table is embedded in the shared
-- MIGRATIONS list and therefore created in BOTH per-repo DBs and the
-- machine-global store, but it is only ever read/written in the
-- global store; its presence as an empty table in a repo DB is inert.
-- There is no `project_id`: `repo_key` is the canonical absolute repo
-- root path (machine-scoped, matching the registry's keying), and
-- `pending_id` is that repo's local `pending_writes.id`. The table is
-- machine-local crash-recovery state, NOT durable memory: it is not
-- part of `memhub export` (re-derivable / meaningless on another
-- machine, since pending ids are repo-local), exactly like
-- `known_projects`.

CREATE TABLE IF NOT EXISTS global_accept_markers (
    repo_key      TEXT    NOT NULL,
    pending_id    INTEGER NOT NULL,
    durable_table TEXT    NOT NULL,
    durable_id    INTEGER NOT NULL,
    created_at    TEXT    NOT NULL DEFAULT CURRENT_TIMESTAMP,
    PRIMARY KEY (repo_key, pending_id)
);
