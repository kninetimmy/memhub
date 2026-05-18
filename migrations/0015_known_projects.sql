-- Migration 0015: machine-wide upgrade registry (`memhub upgrade`).
--
-- A self-maintaining list of every repo memhub has actually opened on
-- this machine, so `memhub upgrade` can enumerate instances
-- deterministically instead of scanning the filesystem (which is
-- non-reproducible, hangs on cloud/network mounts, and silently skips
-- permission-denied subtrees). See the decision recorded alongside
-- this migration.
--
-- This table is embedded in the shared MIGRATIONS list, so it is
-- created in BOTH per-repo DBs and the machine-global store. It is
-- only ever *read* from the global store (`~/.memhub/global.sqlite`),
-- which is the natural machine-scoped registry location; its presence
-- as an empty table in a repo DB is inert. There is no `project_id`
-- column: rows are absolute repo root paths, machine-scoped, not
-- project-scoped. Registry membership is NOT M9 global-memory opt-in —
-- recall never reads this table and stays gated on the repo's own
-- `[global] enabled`.
--
-- `known_projects` is machine-local: it is NOT part of `memhub export`
-- (a re-derivable cache, like embeddings), so older exports import
-- cleanly and the registry simply re-populates as repos are opened on
-- the new machine.

CREATE TABLE IF NOT EXISTS known_projects (
    root_path   TEXT PRIMARY KEY,
    last_seen   TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    last_schema TEXT
);
