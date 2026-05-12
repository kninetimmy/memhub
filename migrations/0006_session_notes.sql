CREATE TABLE IF NOT EXISTS session_notes (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL DEFAULT 1 REFERENCES projects(id) ON DELETE CASCADE,
    actor TEXT NOT NULL,
    actor_raw TEXT NOT NULL,
    text TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_session_notes_created_at ON session_notes(created_at DESC);
