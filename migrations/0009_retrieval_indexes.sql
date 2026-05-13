-- Migration 0009: M8 SQL+RAG hybrid recall scaffolding.
--
-- Adds the embeddings table that stores per-row vector embeddings, plus
-- contentless FTS5 virtual tables over facts, decisions, and tasks. Source
-- tables remain authoritative; the FTS tables only carry the tokenized
-- index. Sync triggers and a one-shot backfill keep the index aligned with
-- existing and future rows.
--
-- The legacy `chunks` / `chunk_fts` tables (added in 0001_initial) are
-- intentionally untouched. They remain populated by the existing decision
-- writer and will be removed by a follow-up migration once the new
-- decisions_fts read path replaces them.

CREATE TABLE IF NOT EXISTS embeddings (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL DEFAULT 1 REFERENCES projects(id) ON DELETE CASCADE,
    source_type TEXT NOT NULL CHECK (source_type IN ('fact', 'decision', 'task')),
    source_id INTEGER NOT NULL,
    model_name TEXT NOT NULL,
    dimension INTEGER NOT NULL,
    vector BLOB NOT NULL,
    content_hash TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(source_type, source_id, model_name)
);

CREATE INDEX IF NOT EXISTS embeddings_lookup
    ON embeddings(source_type, source_id, model_name);
CREATE INDEX IF NOT EXISTS embeddings_model
    ON embeddings(model_name);

-- Contentless FTS5 virtual tables. The source table owns the row data;
-- FTS only stores the tokenized index keyed by the source row id.

CREATE VIRTUAL TABLE IF NOT EXISTS facts_fts USING fts5(
    key,
    value,
    content='facts',
    content_rowid='id'
);

CREATE VIRTUAL TABLE IF NOT EXISTS decisions_fts USING fts5(
    title,
    rationale,
    content='decisions',
    content_rowid='id'
);

CREATE VIRTUAL TABLE IF NOT EXISTS tasks_fts USING fts5(
    title,
    notes,
    content='tasks',
    content_rowid='id'
);

-- External-content sync triggers (per FTS5 docs §4.4.3). Delete uses the
-- 'delete' command to remove the index entry by rowid; insert mirrors the
-- new row. Updates do both.

CREATE TRIGGER IF NOT EXISTS facts_fts_ai AFTER INSERT ON facts BEGIN
    INSERT INTO facts_fts(rowid, key, value) VALUES (new.id, new.key, new.value);
END;
CREATE TRIGGER IF NOT EXISTS facts_fts_ad AFTER DELETE ON facts BEGIN
    INSERT INTO facts_fts(facts_fts, rowid, key, value)
        VALUES ('delete', old.id, old.key, old.value);
END;
CREATE TRIGGER IF NOT EXISTS facts_fts_au AFTER UPDATE ON facts BEGIN
    INSERT INTO facts_fts(facts_fts, rowid, key, value)
        VALUES ('delete', old.id, old.key, old.value);
    INSERT INTO facts_fts(rowid, key, value)
        VALUES (new.id, new.key, new.value);
END;

CREATE TRIGGER IF NOT EXISTS decisions_fts_ai AFTER INSERT ON decisions BEGIN
    INSERT INTO decisions_fts(rowid, title, rationale)
        VALUES (new.id, new.title, new.rationale);
END;
CREATE TRIGGER IF NOT EXISTS decisions_fts_ad AFTER DELETE ON decisions BEGIN
    INSERT INTO decisions_fts(decisions_fts, rowid, title, rationale)
        VALUES ('delete', old.id, old.title, old.rationale);
END;
CREATE TRIGGER IF NOT EXISTS decisions_fts_au AFTER UPDATE ON decisions BEGIN
    INSERT INTO decisions_fts(decisions_fts, rowid, title, rationale)
        VALUES ('delete', old.id, old.title, old.rationale);
    INSERT INTO decisions_fts(rowid, title, rationale)
        VALUES (new.id, new.title, new.rationale);
END;

CREATE TRIGGER IF NOT EXISTS tasks_fts_ai AFTER INSERT ON tasks BEGIN
    INSERT INTO tasks_fts(rowid, title, notes)
        VALUES (new.id, new.title, new.notes);
END;
CREATE TRIGGER IF NOT EXISTS tasks_fts_ad AFTER DELETE ON tasks BEGIN
    INSERT INTO tasks_fts(tasks_fts, rowid, title, notes)
        VALUES ('delete', old.id, old.title, old.notes);
END;
CREATE TRIGGER IF NOT EXISTS tasks_fts_au AFTER UPDATE ON tasks BEGIN
    INSERT INTO tasks_fts(tasks_fts, rowid, title, notes)
        VALUES ('delete', old.id, old.title, old.notes);
    INSERT INTO tasks_fts(rowid, title, notes)
        VALUES (new.id, new.title, new.notes);
END;

-- One-shot backfill of pre-existing rows. Triggers above only fire on
-- writes that happen after they exist; rebuild rescans the source tables.
INSERT INTO facts_fts(facts_fts) VALUES ('rebuild');
INSERT INTO decisions_fts(decisions_fts) VALUES ('rebuild');
INSERT INTO tasks_fts(tasks_fts) VALUES ('rebuild');
