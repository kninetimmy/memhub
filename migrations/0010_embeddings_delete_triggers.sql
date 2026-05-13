-- Migration 0010: cascade source-row deletes into the embeddings table.
--
-- The embeddings table has a polymorphic (source_type, source_id) key
-- that SQLite cannot express as a real foreign key. Per addendum §3.2
-- the writer layer owns orphan cleanup; SQL triggers are the simplest
-- reliable mechanism, mirroring the FTS sync triggers from 0009.

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
