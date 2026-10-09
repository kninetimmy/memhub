-- Migration 0025: SourceType::ArchSection — the latest architecture
-- narrative joins the DEFAULT recall bundle, one row per heading section
-- (issue #232).
--
-- `project_arch` (migration 0007) is append-only history whose latest
-- row is the current architecture body. Recall never saw it: the body
-- was a single markdown blob outside every source type. This adds a
-- derived `arch_sections` table holding the latest body split by heading
-- (the `doc add` chunker, `commands::doc::chunk_markdown`), plus the same
-- retrieval plumbing every other source type has:
--
--   1. `arch_sections` itself. Derived data, never authored directly:
--      `memhub arch set` and `memhub import` delete every section row and
--      re-derive from the latest `project_arch` row in the same
--      transaction, so only the current body is ever searchable. It is
--      excluded from `memhub export` and digest-exempt for Drive sync.
--      SQL cannot chunk markdown, so a DB whose latest body predates this
--      migration gets its sections from Rust right after the migration
--      batch (`db::migrations::apply_all`), FTS-only — no embedding
--      model load on open; `memhub index rebuild` embeds them.
--   2. A contentless FTS5 index over (heading_path, body), mirroring
--      0014's doc_chunks_fts so a heading breadcrumb contributes to
--      keyword matching.
--   3. Widen the `embeddings.source_type` CHECK to admit 'arch_section'
--      via the same create-copy-drop-rename dance 0014/0022 used,
--      copying every existing row so fact/decision/task/doc_chunk/note
--      vectors survive. Every trigger naming `embeddings` (0010's three,
--      0014's doc_chunks one, 0022's session_notes one, and this
--      migration's own) is dropped first and recreated after, per
--      SQLite's table-rebuild guidance.
--   4. `arch_sections_delete_embeddings`, so the explicit section delete
--      on every re-derive clears the matching vectors.
--      The FK cascade from `project_arch` would fire it too, even with
--      recursive_triggers OFF; writers still delete sections explicitly
--      so the cleanup does not depend on the cascade.

CREATE TABLE IF NOT EXISTS arch_sections (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL DEFAULT 1 REFERENCES projects(id) ON DELETE CASCADE,
    arch_id INTEGER NOT NULL REFERENCES project_arch(id) ON DELETE CASCADE,
    ord INTEGER NOT NULL,
    heading_path TEXT NOT NULL,
    body TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(arch_id, ord)
);

DROP TRIGGER IF EXISTS facts_delete_embeddings;
DROP TRIGGER IF EXISTS decisions_delete_embeddings;
DROP TRIGGER IF EXISTS tasks_delete_embeddings;
DROP TRIGGER IF EXISTS doc_chunks_delete_embeddings;
DROP TRIGGER IF EXISTS session_notes_delete_embeddings;
-- Absent on a first apply; dropped so a re-apply (a DB whose ledger row
-- went missing) does not trip the rename's trigger re-validation.
DROP TRIGGER IF EXISTS arch_sections_delete_embeddings;

CREATE TABLE embeddings_new (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL DEFAULT 1 REFERENCES projects(id) ON DELETE CASCADE,
    source_type TEXT NOT NULL CHECK (source_type IN ('fact', 'decision', 'task', 'doc_chunk', 'note', 'arch_section')),
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
CREATE TRIGGER IF NOT EXISTS arch_sections_delete_embeddings
    AFTER DELETE ON arch_sections BEGIN
    DELETE FROM embeddings WHERE source_type = 'arch_section' AND source_id = old.id;
END;

-- Contentless FTS5 over arch_sections (mirror 0014's doc_chunks_fts).
CREATE VIRTUAL TABLE IF NOT EXISTS arch_sections_fts USING fts5(
    heading_path,
    body,
    content='arch_sections',
    content_rowid='id'
);

CREATE TRIGGER IF NOT EXISTS arch_sections_fts_ai AFTER INSERT ON arch_sections BEGIN
    INSERT INTO arch_sections_fts(rowid, heading_path, body)
        VALUES (new.id, new.heading_path, new.body);
END;
CREATE TRIGGER IF NOT EXISTS arch_sections_fts_ad AFTER DELETE ON arch_sections BEGIN
    INSERT INTO arch_sections_fts(arch_sections_fts, rowid, heading_path, body)
        VALUES ('delete', old.id, old.heading_path, old.body);
END;
CREATE TRIGGER IF NOT EXISTS arch_sections_fts_au AFTER UPDATE ON arch_sections BEGIN
    INSERT INTO arch_sections_fts(arch_sections_fts, rowid, heading_path, body)
        VALUES ('delete', old.id, old.heading_path, old.body);
    INSERT INTO arch_sections_fts(rowid, heading_path, body)
        VALUES (new.id, new.heading_path, new.body);
END;

-- Idempotent and matches 0009/0014/0022; the table is empty here (the
-- Rust backfill runs after this batch, through the insert trigger).
INSERT INTO arch_sections_fts(arch_sections_fts) VALUES ('rebuild');
