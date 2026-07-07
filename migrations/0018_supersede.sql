-- Migration 0018: supersession schema (Wave 3 L3, issue #46).
--
-- Q3 ruling (decision 145): a superseded row is demoted-with-link, never
-- dropped — present, tagged, and penalized in recall, but no-loss. Two
-- schema changes support that end to end:
--
-- 1. Add the `superseded_by` link column to `facts`. `decisions` already
--    carries an equivalent column: migration 0001 created
--    `decisions.superseded_by INTEGER REFERENCES decisions(id)` alongside a
--    `status` CHECK that already permits 'superseded'. Only `facts` lacked
--    the link. A fact is "superseded" exactly when `superseded_by IS NOT
--    NULL` (derived, like staleness from `verified_at`); no separate fact
--    `status` column is introduced. The added column is nullable with a
--    NULL default — the only shape SQLite permits for an `ALTER TABLE ADD
--    COLUMN` that carries a `REFERENCES` clause.
--
-- 2. Relax the `pending_writes.kind` CHECK to admit the new staged
--    `'supersede'` proposal kind (MCP `propose_supersede`, durable only on
--    `memhub review accept`). SQLite cannot ALTER a CHECK constraint in
--    place, so this rebuilds the table via the documented
--    create-copy-drop-rename dance, preserving every column, default, and
--    the lone index. `defer_foreign_keys` lets the drop/rename run inside
--    the migration runner's transaction; the data stays consistent
--    (project_id = 1 is unchanged), so the commit-time FK check passes.
--
-- Replay-safety: like every migration this is applied exactly once, gated by
-- the `schema_migrations` version ledger in `db::migrations::apply_all`
-- (SQLite has no `ADD COLUMN IF NOT EXISTS`, so the runner — not the SQL —
-- owns idempotency, mirroring 0017's plain `ALTER TABLE ... ADD COLUMN`).

ALTER TABLE facts ADD COLUMN superseded_by INTEGER REFERENCES facts(id);

PRAGMA defer_foreign_keys = ON;

CREATE TABLE pending_writes_new (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL DEFAULT 1 REFERENCES projects(id) ON DELETE CASCADE,
    kind TEXT NOT NULL CHECK (kind IN ('fact', 'decision', 'supersede')),
    payload_json TEXT NOT NULL,
    rationale TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'pending' CHECK (status IN ('pending', 'accepted', 'rejected', 'expired')),
    actor TEXT NOT NULL,
    actor_raw TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    provenance_json TEXT NOT NULL DEFAULT '{}',
    reviewed_at TEXT
);

INSERT INTO pending_writes_new (
    id, project_id, kind, payload_json, rationale, status,
    actor, actor_raw, created_at, provenance_json, reviewed_at
)
SELECT
    id, project_id, kind, payload_json, rationale, status,
    actor, actor_raw, created_at, provenance_json, reviewed_at
FROM pending_writes;

DROP TABLE pending_writes;

ALTER TABLE pending_writes_new RENAME TO pending_writes;

CREATE INDEX IF NOT EXISTS idx_pending_writes_status_created_at
    ON pending_writes(status, created_at DESC);
