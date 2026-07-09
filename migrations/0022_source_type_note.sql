-- Migration 0022: SourceType::Note — session notes join hybrid recall,
-- reachable only via explicit `source_types=["note"]` (Wave 6 W5, issue
-- #98, gate Q9). Notes stay write-only scratch by default: unlike docs
-- (migration 0014, decision 90), there is no `include_notes_in_default`
-- opt-in flip — notes never join the default recall bundle, full stop.
--
-- Two additions, mirroring 0009/0010/0014's shape for every other source
-- type. `session_notes` itself already exists (migration 0006); this only
-- adds the retrieval plumbing on top of it:
--
--   1. A contentless FTS5 index over `session_notes.text` — the only
--      searchable content column. `actor`/`actor_raw` are provenance
--      (who wrote the note), not content, so they stay out of the index,
--      the same reasoning that keeps `source` out of facts_fts/
--      decisions_fts.
--   2. Widen the `embeddings.source_type` CHECK to admit 'note'. SQLite
--      cannot ALTER a CHECK constraint, so this rebuilds the table via
--      the same create-copy-drop-rename dance 0014 used to add
--      'doc_chunk', copying every existing row so pre-existing fact/
--      decision/task/doc_chunk vectors survive. The four triggers that
--      name `embeddings` in their bodies (0010's three plus 0014's
--      `doc_chunks_delete_embeddings`) are dropped first and recreated
--      verbatim after, per SQLite's table-rebuild guidance (same as
--      0014).
--
-- `session_notes_delete_embeddings` is added alongside the other four for
-- symmetry even though no `session_note remove` command exists yet (notes
-- are add-only today) — so a future delete path is correct on day one
-- instead of silently leaking embedding rows.

DROP TRIGGER IF EXISTS facts_delete_embeddings;
DROP TRIGGER IF EXISTS decisions_delete_embeddings;
DROP TRIGGER IF EXISTS tasks_delete_embeddings;
DROP TRIGGER IF EXISTS doc_chunks_delete_embeddings;

CREATE TABLE embeddings_new (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL DEFAULT 1 REFERENCES projects(id) ON DELETE CASCADE,
    source_type TEXT NOT NULL CHECK (source_type IN ('fact', 'decision', 'task', 'doc_chunk', 'note')),
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
CREATE TRIGGER IF NOT EXISTS doc_chunks_delete_embeddings
    AFTER DELETE ON doc_chunks BEGIN
    DELETE FROM embeddings WHERE source_type = 'doc_chunk' AND source_id = old.id;
END;
CREATE TRIGGER IF NOT EXISTS session_notes_delete_embeddings
    AFTER DELETE ON session_notes BEGIN
    DELETE FROM embeddings WHERE source_type = 'note' AND source_id = old.id;
END;

-- Contentless FTS5 over session_notes.text (mirror 0009 / 0014).
CREATE VIRTUAL TABLE IF NOT EXISTS session_notes_fts USING fts5(
    text,
    content='session_notes',
    content_rowid='id'
);

CREATE TRIGGER IF NOT EXISTS session_notes_fts_ai AFTER INSERT ON session_notes BEGIN
    INSERT INTO session_notes_fts(rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER IF NOT EXISTS session_notes_fts_ad AFTER DELETE ON session_notes BEGIN
    INSERT INTO session_notes_fts(session_notes_fts, rowid, text)
        VALUES ('delete', old.id, old.text);
END;
CREATE TRIGGER IF NOT EXISTS session_notes_fts_au AFTER UPDATE ON session_notes BEGIN
    INSERT INTO session_notes_fts(session_notes_fts, rowid, text)
        VALUES ('delete', old.id, old.text);
    INSERT INTO session_notes_fts(rowid, text)
        VALUES (new.id, new.text);
END;

-- Backfill FTS for any rows that predate this migration's triggers.
INSERT INTO session_notes_fts(session_notes_fts) VALUES ('rebuild');
