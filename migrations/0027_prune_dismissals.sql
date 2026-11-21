CREATE TABLE prune_dismissals (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL DEFAULT 1 REFERENCES projects(id) ON DELETE CASCADE,
    identifier TEXT NOT NULL,
    UNIQUE (project_id, identifier)
);
