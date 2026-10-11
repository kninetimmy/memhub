-- Migration 0026: optional `summary` on facts and tasks, summaries in
-- full-text search, and porter stemming on every recall FTS table
-- (issue #349).
--
--   1. `facts.summary` and `tasks.summary`: the same optional "what this
--      answers" paraphrase decisions carry since 0011 (decision 72).
--      Nullable; a row without one keeps exactly its old embed text, so
--      no existing embedding goes stale. Agents may write it directly
--      (decision 217): it changes how a row is found, not what memhub
--      asserts.
--   2. The six FTS tables recall reads (facts_fts, decisions_fts,
--      tasks_fts, doc_chunks_fts, session_notes_fts, arch_sections_fts)
--      are recreated with tokenize='porter unicode61', so a query word
--      matches other inflections of a stored word ('migrating' finds
--      'migration'). Before this migration they used the default
--      unicode61 tokenizer and matched exact tokens only.
--   3. facts_fts, decisions_fts and tasks_fts gain a `summary` column.
--      Before: 0011 deliberately left decisions_fts without the summary
--      ("a follow-up migration can index summary if FTS-side paraphrase
--      matching turns out to matter"); this is that follow-up.
--
-- A tokenizer cannot change in place, so each table's sync triggers are
-- dropped, the table is dropped and recreated, the triggers are
-- recreated naming the new column list, and 'rebuild' re-indexes every
-- existing row from its content table (FTS is derived state, decision
-- 20). The non-FTS `*_delete_embeddings` triggers are left alone.
--
-- Untouched: the legacy git-search chunk_fts (0002) and the code index's
-- code_chunks_fts (separate DB) keep the unicode61 tokenizer.

ALTER TABLE facts ADD COLUMN summary TEXT;
ALTER TABLE tasks ADD COLUMN summary TEXT;

DROP TRIGGER IF EXISTS facts_fts_ai;
DROP TRIGGER IF EXISTS facts_fts_ad;
DROP TRIGGER IF EXISTS facts_fts_au;
DROP TRIGGER IF EXISTS decisions_fts_ai;
DROP TRIGGER IF EXISTS decisions_fts_ad;
DROP TRIGGER IF EXISTS decisions_fts_au;
DROP TRIGGER IF EXISTS tasks_fts_ai;
DROP TRIGGER IF EXISTS tasks_fts_ad;
DROP TRIGGER IF EXISTS tasks_fts_au;
DROP TRIGGER IF EXISTS doc_chunks_fts_ai;
DROP TRIGGER IF EXISTS doc_chunks_fts_ad;
DROP TRIGGER IF EXISTS doc_chunks_fts_au;
DROP TRIGGER IF EXISTS session_notes_fts_ai;
DROP TRIGGER IF EXISTS session_notes_fts_ad;
DROP TRIGGER IF EXISTS session_notes_fts_au;
DROP TRIGGER IF EXISTS arch_sections_fts_ai;
DROP TRIGGER IF EXISTS arch_sections_fts_ad;
DROP TRIGGER IF EXISTS arch_sections_fts_au;

DROP TABLE IF EXISTS facts_fts;
DROP TABLE IF EXISTS decisions_fts;
DROP TABLE IF EXISTS tasks_fts;
DROP TABLE IF EXISTS doc_chunks_fts;
DROP TABLE IF EXISTS session_notes_fts;
DROP TABLE IF EXISTS arch_sections_fts;

CREATE VIRTUAL TABLE facts_fts USING fts5(
    key,
    value,
    summary,
    content='facts',
    content_rowid='id',
    tokenize='porter unicode61'
);

CREATE VIRTUAL TABLE decisions_fts USING fts5(
    title,
    rationale,
    summary,
    content='decisions',
    content_rowid='id',
    tokenize='porter unicode61'
);

CREATE VIRTUAL TABLE tasks_fts USING fts5(
    title,
    notes,
    summary,
    content='tasks',
    content_rowid='id',
    tokenize='porter unicode61'
);

CREATE VIRTUAL TABLE doc_chunks_fts USING fts5(
    heading_path,
    body,
    content='doc_chunks',
    content_rowid='id',
    tokenize='porter unicode61'
);

CREATE VIRTUAL TABLE session_notes_fts USING fts5(
    text,
    content='session_notes',
    content_rowid='id',
    tokenize='porter unicode61'
);

CREATE VIRTUAL TABLE arch_sections_fts USING fts5(
    heading_path,
    body,
    content='arch_sections',
    content_rowid='id',
    tokenize='porter unicode61'
);

CREATE TRIGGER facts_fts_ai AFTER INSERT ON facts BEGIN
    INSERT INTO facts_fts(rowid, key, value, summary)
        VALUES (new.id, new.key, new.value, new.summary);
END;
CREATE TRIGGER facts_fts_ad AFTER DELETE ON facts BEGIN
    INSERT INTO facts_fts(facts_fts, rowid, key, value, summary)
        VALUES ('delete', old.id, old.key, old.value, old.summary);
END;
CREATE TRIGGER facts_fts_au AFTER UPDATE ON facts BEGIN
    INSERT INTO facts_fts(facts_fts, rowid, key, value, summary)
        VALUES ('delete', old.id, old.key, old.value, old.summary);
    INSERT INTO facts_fts(rowid, key, value, summary)
        VALUES (new.id, new.key, new.value, new.summary);
END;

CREATE TRIGGER decisions_fts_ai AFTER INSERT ON decisions BEGIN
    INSERT INTO decisions_fts(rowid, title, rationale, summary)
        VALUES (new.id, new.title, new.rationale, new.summary);
END;
CREATE TRIGGER decisions_fts_ad AFTER DELETE ON decisions BEGIN
    INSERT INTO decisions_fts(decisions_fts, rowid, title, rationale, summary)
        VALUES ('delete', old.id, old.title, old.rationale, old.summary);
END;
CREATE TRIGGER decisions_fts_au AFTER UPDATE ON decisions BEGIN
    INSERT INTO decisions_fts(decisions_fts, rowid, title, rationale, summary)
        VALUES ('delete', old.id, old.title, old.rationale, old.summary);
    INSERT INTO decisions_fts(rowid, title, rationale, summary)
        VALUES (new.id, new.title, new.rationale, new.summary);
END;

CREATE TRIGGER tasks_fts_ai AFTER INSERT ON tasks BEGIN
    INSERT INTO tasks_fts(rowid, title, notes, summary)
        VALUES (new.id, new.title, new.notes, new.summary);
END;
CREATE TRIGGER tasks_fts_ad AFTER DELETE ON tasks BEGIN
    INSERT INTO tasks_fts(tasks_fts, rowid, title, notes, summary)
        VALUES ('delete', old.id, old.title, old.notes, old.summary);
END;
CREATE TRIGGER tasks_fts_au AFTER UPDATE ON tasks BEGIN
    INSERT INTO tasks_fts(tasks_fts, rowid, title, notes, summary)
        VALUES ('delete', old.id, old.title, old.notes, old.summary);
    INSERT INTO tasks_fts(rowid, title, notes, summary)
        VALUES (new.id, new.title, new.notes, new.summary);
END;

CREATE TRIGGER doc_chunks_fts_ai AFTER INSERT ON doc_chunks BEGIN
    INSERT INTO doc_chunks_fts(rowid, heading_path, body)
        VALUES (new.id, new.heading_path, new.body);
END;
CREATE TRIGGER doc_chunks_fts_ad AFTER DELETE ON doc_chunks BEGIN
    INSERT INTO doc_chunks_fts(doc_chunks_fts, rowid, heading_path, body)
        VALUES ('delete', old.id, old.heading_path, old.body);
END;
CREATE TRIGGER doc_chunks_fts_au AFTER UPDATE ON doc_chunks BEGIN
    INSERT INTO doc_chunks_fts(doc_chunks_fts, rowid, heading_path, body)
        VALUES ('delete', old.id, old.heading_path, old.body);
    INSERT INTO doc_chunks_fts(rowid, heading_path, body)
        VALUES (new.id, new.heading_path, new.body);
END;

CREATE TRIGGER session_notes_fts_ai AFTER INSERT ON session_notes BEGIN
    INSERT INTO session_notes_fts(rowid, text) VALUES (new.id, new.text);
END;
CREATE TRIGGER session_notes_fts_ad AFTER DELETE ON session_notes BEGIN
    INSERT INTO session_notes_fts(session_notes_fts, rowid, text)
        VALUES ('delete', old.id, old.text);
END;
CREATE TRIGGER session_notes_fts_au AFTER UPDATE ON session_notes BEGIN
    INSERT INTO session_notes_fts(session_notes_fts, rowid, text)
        VALUES ('delete', old.id, old.text);
    INSERT INTO session_notes_fts(rowid, text)
        VALUES (new.id, new.text);
END;

CREATE TRIGGER arch_sections_fts_ai AFTER INSERT ON arch_sections BEGIN
    INSERT INTO arch_sections_fts(rowid, heading_path, body)
        VALUES (new.id, new.heading_path, new.body);
END;
CREATE TRIGGER arch_sections_fts_ad AFTER DELETE ON arch_sections BEGIN
    INSERT INTO arch_sections_fts(arch_sections_fts, rowid, heading_path, body)
        VALUES ('delete', old.id, old.heading_path, old.body);
END;
CREATE TRIGGER arch_sections_fts_au AFTER UPDATE ON arch_sections BEGIN
    INSERT INTO arch_sections_fts(arch_sections_fts, rowid, heading_path, body)
        VALUES ('delete', old.id, old.heading_path, old.body);
    INSERT INTO arch_sections_fts(rowid, heading_path, body)
        VALUES (new.id, new.heading_path, new.body);
END;

-- Re-index every existing row from its content table.
INSERT INTO facts_fts(facts_fts) VALUES ('rebuild');
INSERT INTO decisions_fts(decisions_fts) VALUES ('rebuild');
INSERT INTO tasks_fts(tasks_fts) VALUES ('rebuild');
INSERT INTO doc_chunks_fts(doc_chunks_fts) VALUES ('rebuild');
INSERT INTO session_notes_fts(session_notes_fts) VALUES ('rebuild');
INSERT INTO arch_sections_fts(arch_sections_fts) VALUES ('rebuild');
