-- Migration 0014: external reference-document ingestion.
--
-- Adds the `documents` + `doc_chunks` tables and wires `doc_chunks` into
-- the existing retrieval machinery: a contentless FTS5 index (mirroring
-- 0009) and a delete trigger that cascades chunk deletes into the
-- polymorphic `embeddings` table (mirroring 0010).
--
-- Documents are per-repo, explicitly user-ingested reference material
-- (e.g. a design spec). They are RAG-searchable through the same hybrid
-- recall path as facts/decisions/tasks, but are deliberately OPT-IN:
-- `doc_chunk` is never in the default recall source-type set. See the
-- scope decision recorded alongside this migration.
--
-- The `embeddings` table (0009) carries a CHECK constraint that allows
-- only ('fact','decision','task'). SQLite cannot ALTER a CHECK, so this
-- migration rebuilds the table with a widened constraint, copying every
-- existing row so pre-existing fact/decision/task vectors survive. No
-- real foreign key references `embeddings.id` (its key is the polymorphic
-- (source_type, source_id) pair).
--
-- The 0010 delete triggers (facts/decisions/tasks_delete_embeddings)
-- name `embeddings` in their bodies. Dropping/renaming the table forces
-- SQLite to re-validate those trigger definitions and it errors with
-- "no such table: embeddings". Per SQLite's official table-rebuild
-- guidance, the migration drops those three triggers first and recreates
-- them (verbatim from 0010) after the swap.

CREATE TABLE IF NOT EXISTS documents (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL DEFAULT 1 REFERENCES projects(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    title TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    byte_len INTEGER NOT NULL,
    source TEXT NOT NULL DEFAULT 'user',
    ingested_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(project_id, path)
);

CREATE TABLE IF NOT EXISTS doc_chunks (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL DEFAULT 1 REFERENCES projects(id) ON DELETE CASCADE,
    doc_id INTEGER NOT NULL REFERENCES documents(id) ON DELETE CASCADE,
    ord INTEGER NOT NULL,
    heading_path TEXT NOT NULL,
    body TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(doc_id, ord)
);

CREATE INDEX IF NOT EXISTS idx_doc_chunks_doc ON doc_chunks(doc_id, ord);

-- Drop the 0010 triggers that reference `embeddings` so the table swap
-- below does not trip SQLite's trigger re-validation. Recreated verbatim
-- after the rename.
DROP TRIGGER IF EXISTS facts_delete_embeddings;
DROP TRIGGER IF EXISTS decisions_delete_embeddings;
DROP TRIGGER IF EXISTS tasks_delete_embeddings;

-- Rebuild `embeddings` with a widened source_type CHECK. Column order is
-- identical to 0009 so `INSERT ... SELECT *` lines up positionally.
CREATE TABLE embeddings_new (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL DEFAULT 1 REFERENCES projects(id) ON DELETE CASCADE,
    source_type TEXT NOT NULL CHECK (source_type IN ('fact', 'decision', 'task', 'doc_chunk')),
    source_id INTEGER NOT NULL,
    model_name TEXT NOT NULL,
    dimension INTEGER NOT NULL,
    vector BLOB NOT NULL,
    content_hash TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(source_type, source_id, model_name)
);

INSERT INTO embeddings_new SELECT * FROM embeddings;
DROP TABLE embeddings;
ALTER TABLE embeddings_new RENAME TO embeddings;

CREATE INDEX IF NOT EXISTS embeddings_lookup
    ON embeddings(source_type, source_id, model_name);
CREATE INDEX IF NOT EXISTS embeddings_model
    ON embeddings(model_name);

-- Recreate the 0010 delete triggers verbatim against the rebuilt table.
CREATE TRIGGER IF NOT EXISTS facts_delete_embeddings
    AFTER DELETE ON facts BEGIN
    DELETE FROM embeddings WHERE source_type = 'fact' AND source_id = old.id;
END;
CREATE TRIGGER IF NOT EXISTS decisions_delete_embeddings
    AFTER DELETE ON decisions BEGIN
    DELETE FROM embeddings WHERE source_type = 'decision' AND source_id = old.id;
END;
CREATE TRIGGER IF NOT EXISTS tasks_delete_embeddings
    AFTER DELETE ON tasks BEGIN
    DELETE FROM embeddings WHERE source_type = 'task' AND source_id = old.id;
END;

-- Contentless FTS5 over doc_chunks (mirror 0009). heading_path is indexed
-- alongside the body so a section breadcrumb ("Components > Buttons")
-- contributes to keyword matching.
CREATE VIRTUAL TABLE IF NOT EXISTS doc_chunks_fts USING fts5(
    heading_path,
    body,
    content='doc_chunks',
    content_rowid='id'
);

CREATE TRIGGER IF NOT EXISTS doc_chunks_fts_ai AFTER INSERT ON doc_chunks BEGIN
    INSERT INTO doc_chunks_fts(rowid, heading_path, body)
        VALUES (new.id, new.heading_path, new.body);
END;
CREATE TRIGGER IF NOT EXISTS doc_chunks_fts_ad AFTER DELETE ON doc_chunks BEGIN
    INSERT INTO doc_chunks_fts(doc_chunks_fts, rowid, heading_path, body)
        VALUES ('delete', old.id, old.heading_path, old.body);
END;
CREATE TRIGGER IF NOT EXISTS doc_chunks_fts_au AFTER UPDATE ON doc_chunks BEGIN
    INSERT INTO doc_chunks_fts(doc_chunks_fts, rowid, heading_path, body)
        VALUES ('delete', old.id, old.heading_path, old.body);
    INSERT INTO doc_chunks_fts(rowid, heading_path, body)
        VALUES (new.id, new.heading_path, new.body);
END;

-- Cascade chunk deletes into the polymorphic embeddings table (mirror
-- 0010). recursive_triggers is pinned OFF (see `open_connection`), so
-- this fires ONLY on direct `doc_chunks` deletes — NOT on chunks removed
-- by the FK cascade when a `documents` row is deleted. The writer
-- therefore deletes chunks explicitly (on re-ingest and on `doc rm`)
-- before the parent row, so every removal routes through this
-- direct-delete trigger and no embedding is orphaned.
CREATE TRIGGER IF NOT EXISTS doc_chunks_delete_embeddings
    AFTER DELETE ON doc_chunks BEGIN
    DELETE FROM embeddings WHERE source_type = 'doc_chunk' AND source_id = old.id;
END;

-- Backfill FTS for any rows that somehow predate the triggers (none on a
-- fresh migration, but rebuild is idempotent and matches 0009's pattern).
INSERT INTO doc_chunks_fts(doc_chunks_fts) VALUES ('rebuild');
