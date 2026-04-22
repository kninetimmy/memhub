ALTER TABLE pending_writes
    ADD COLUMN provenance_json TEXT NOT NULL DEFAULT '{}';
