ALTER TABLE facts ADD COLUMN retired_at TEXT;
ALTER TABLE facts ADD COLUMN retired_reason TEXT;
ALTER TABLE documents ADD COLUMN archived_at TEXT;
ALTER TABLE documents ADD COLUMN archived_reason TEXT;

-- SQLite cannot widen a CHECK in place. Preserve task IDs and the external
-- FTS index, and do not fire the embedding cleanup while copying tasks.
DROP TRIGGER tasks_fts_ai;
DROP TRIGGER tasks_fts_ad;
DROP TRIGGER tasks_fts_au;
DROP TRIGGER tasks_delete_embeddings;

CREATE TABLE tasks_new (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL DEFAULT 1 REFERENCES projects(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'done', 'blocked', 'dropped')),
    notes TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    dropped_reason TEXT,
    pre_drop_status TEXT CHECK (pre_drop_status IN ('open', 'blocked')),
    pre_drop_updated_at TEXT
);

INSERT INTO tasks_new(id, project_id, title, status, notes, created_at, updated_at)
SELECT id, project_id, title, status, notes, created_at, updated_at FROM tasks;
DROP TABLE tasks;
ALTER TABLE tasks_new RENAME TO tasks;

CREATE INDEX idx_tasks_status_updated_at ON tasks(status, updated_at DESC);

CREATE TRIGGER tasks_fts_ai AFTER INSERT ON tasks BEGIN
    INSERT INTO tasks_fts(rowid, title, notes)
        VALUES (new.id, new.title, new.notes);
END;
CREATE TRIGGER tasks_fts_ad AFTER DELETE ON tasks BEGIN
    INSERT INTO tasks_fts(tasks_fts, rowid, title, notes)
        VALUES ('delete', old.id, old.title, old.notes);
END;
CREATE TRIGGER tasks_fts_au AFTER UPDATE ON tasks BEGIN
    INSERT INTO tasks_fts(tasks_fts, rowid, title, notes)
        VALUES ('delete', old.id, old.title, old.notes);
    INSERT INTO tasks_fts(rowid, title, notes)
        VALUES (new.id, new.title, new.notes);
END;
CREATE TRIGGER tasks_delete_embeddings AFTER DELETE ON tasks BEGIN
    DELETE FROM embeddings WHERE source_type = 'task' AND source_id = old.id;
END;
