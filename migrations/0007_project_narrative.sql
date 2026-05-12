CREATE TABLE IF NOT EXISTS project_state (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL DEFAULT 1 REFERENCES projects(id) ON DELETE CASCADE,
    body TEXT NOT NULL,
    actor TEXT NOT NULL,
    actor_raw TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_project_state_created_at ON project_state(created_at DESC);

CREATE TABLE IF NOT EXISTS project_arch (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL DEFAULT 1 REFERENCES projects(id) ON DELETE CASCADE,
    body TEXT NOT NULL,
    actor TEXT NOT NULL,
    actor_raw TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_project_arch_created_at ON project_arch(created_at DESC);
