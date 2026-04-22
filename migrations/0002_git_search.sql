CREATE TABLE IF NOT EXISTS commits (
    sha TEXT PRIMARY KEY,
    project_id INTEGER NOT NULL DEFAULT 1 REFERENCES projects(id) ON DELETE CASCADE,
    author TEXT NOT NULL,
    committed_at TEXT NOT NULL,
    message TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS files (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL DEFAULT 1 REFERENCES projects(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    last_seen_commit TEXT REFERENCES commits(sha),
    language TEXT,
    UNIQUE(project_id, path)
);

CREATE TABLE IF NOT EXISTS commit_files (
    commit_sha TEXT NOT NULL REFERENCES commits(sha) ON DELETE CASCADE,
    file_id INTEGER NOT NULL REFERENCES files(id) ON DELETE CASCADE,
    change_type TEXT NOT NULL,
    PRIMARY KEY(commit_sha, file_id)
);

CREATE TABLE IF NOT EXISTS chunks (
    id INTEGER PRIMARY KEY,
    project_id INTEGER NOT NULL DEFAULT 1 REFERENCES projects(id) ON DELETE CASCADE,
    source_type TEXT NOT NULL,
    source_id TEXT NOT NULL,
    text TEXT NOT NULL,
    created_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
    UNIQUE(project_id, source_type, source_id)
);

CREATE VIRTUAL TABLE IF NOT EXISTS chunk_fts USING fts5(
    text,
    content='chunks',
    content_rowid='id'
);

CREATE TRIGGER IF NOT EXISTS chunks_ai AFTER INSERT ON chunks BEGIN
    INSERT INTO chunk_fts(rowid, text) VALUES (new.id, new.text);
END;

CREATE TRIGGER IF NOT EXISTS chunks_ad AFTER DELETE ON chunks BEGIN
    INSERT INTO chunk_fts(chunk_fts, rowid, text) VALUES ('delete', old.id, old.text);
END;

CREATE TRIGGER IF NOT EXISTS chunks_au AFTER UPDATE ON chunks BEGIN
    INSERT INTO chunk_fts(chunk_fts, rowid, text) VALUES ('delete', old.id, old.text);
    INSERT INTO chunk_fts(rowid, text) VALUES (new.id, new.text);
END;

CREATE INDEX IF NOT EXISTS idx_commits_project_committed_at
    ON commits(project_id, committed_at DESC);
CREATE INDEX IF NOT EXISTS idx_files_project_path
    ON files(project_id, path);
CREATE INDEX IF NOT EXISTS idx_files_last_seen_commit
    ON files(last_seen_commit);
CREATE INDEX IF NOT EXISTS idx_commit_files_file_id_commit_sha
    ON commit_files(file_id, commit_sha);
CREATE INDEX IF NOT EXISTS idx_commit_files_commit_sha_file_id
    ON commit_files(commit_sha, file_id);
CREATE INDEX IF NOT EXISTS idx_chunks_project_source
    ON chunks(project_id, source_type, source_id);
