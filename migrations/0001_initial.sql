CREATE TABLE IF NOT EXISTS projects (
    id INTEGER PRIMARY KEY CHECK (id = 1),
    root_path TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    schema_version TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS facts (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL DEFAULT 1 REFERENCES projects(id) ON DELETE CASCADE,
    key TEXT NOT NULL,
    value TEXT NOT NULL,
    confidence REAL NOT NULL DEFAULT 1.0,
    source TEXT NOT NULL DEFAULT 'user',
    verified_at TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(project_id, key)
);

CREATE TABLE IF NOT EXISTS decisions (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL DEFAULT 1 REFERENCES projects(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    rationale TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'active' CHECK (status IN ('active', 'superseded', 'draft')),
    decided_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    superseded_by INTEGER REFERENCES decisions(id)
);

CREATE TABLE IF NOT EXISTS tasks (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL DEFAULT 1 REFERENCES projects(id) ON DELETE CASCADE,
    title TEXT NOT NULL,
    status TEXT NOT NULL DEFAULT 'open' CHECK (status IN ('open', 'done', 'blocked')),
    notes TEXT,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    updated_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE TABLE IF NOT EXISTS commands (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL DEFAULT 1 REFERENCES projects(id) ON DELETE CASCADE,
    kind TEXT NOT NULL,
    cmdline TEXT NOT NULL,
    last_exit_code INTEGER,
    last_run_at TEXT,
    success_count INTEGER NOT NULL DEFAULT 0,
    fail_count INTEGER NOT NULL DEFAULT 0
);

CREATE TABLE IF NOT EXISTS writes_log (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL DEFAULT 1 REFERENCES projects(id) ON DELETE CASCADE,
    actor TEXT NOT NULL,
    table_name TEXT NOT NULL,
    row_id INTEGER,
    action TEXT NOT NULL,
    reason TEXT,
    at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
);

CREATE INDEX IF NOT EXISTS idx_facts_key ON facts(key);
CREATE INDEX IF NOT EXISTS idx_decisions_status_decided_at ON decisions(status, decided_at DESC);
CREATE INDEX IF NOT EXISTS idx_tasks_status_updated_at ON tasks(status, updated_at DESC);
CREATE INDEX IF NOT EXISTS idx_commands_kind_last_run_at ON commands(kind, last_run_at DESC);
CREATE INDEX IF NOT EXISTS idx_writes_log_at ON writes_log(at DESC);
