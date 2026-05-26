//! Sibling code-index DB schema + bootstrap (M11 PR1, decision 107).
//!
//! The code index lives in its own file at `.memhub/code_index.sqlite`,
//! physically separate from `project.sqlite`. It is derivable and
//! disposable, so it carries **no migration framework**: bootstrap is
//! `CREATE TABLE IF NOT EXISTS` plus a single `schema_version` row in
//! `index_meta`. When the stored version disagrees with the version this
//! binary knows, every table is dropped and recreated — a rebuild is
//! free because the whole index is regenerable from the working tree.
//! This is what makes `memhub upgrade` a no-op for the code index.
//!
//! `code_embeddings` and `code_chunks_fts` are created here but stay
//! EMPTY in PR1: embedding + FTS population is PR2's job (the tree-sitter
//! chunker lands there). They exist now only so the schema is stable.

use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;

/// Schema version stamped into `index_meta`. Bumping this triggers a
/// DROP+REBUILD on the next open — intentional and cheap, since the index
/// is rebuilt from tracked files.
pub const CODE_INDEX_SCHEMA_VERSION: i64 = 1;

const META_VERSION_KEY: &str = "schema_version";

/// Bring the sibling DB to the current schema version.
///
/// If a prior version is recorded and differs from
/// [`CODE_INDEX_SCHEMA_VERSION`], the whole index is dropped and rebuilt.
/// Otherwise the `CREATE TABLE IF NOT EXISTS` statements are idempotent.
pub fn bootstrap(conn: &Connection) -> Result<()> {
    let needs_rebuild = match stored_version(conn)? {
        Some(version) => version != CODE_INDEX_SCHEMA_VERSION,
        // `index_meta` exists but holds no parseable version: a corrupt or
        // partially-written prior state. Treat it as a mismatch and rebuild
        // — the index is regenerable, so a spurious rebuild is only cheap,
        // never lossy. A genuinely fresh DB has no `index_meta` table and
        // is reported as `false` here, so it is created, not dropped.
        None => meta_table_present(conn)?,
    };
    if needs_rebuild {
        drop_all(conn)?;
    }

    create_all(conn)?;

    conn.execute(
        "INSERT INTO index_meta(key, value) VALUES (?1, ?2)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        params![META_VERSION_KEY, CODE_INDEX_SCHEMA_VERSION.to_string()],
    )?;

    Ok(())
}

/// The schema version recorded in `index_meta`, or `None` when the index
/// has never been bootstrapped (the `index_meta` table is absent).
pub fn stored_version(conn: &Connection) -> Result<Option<i64>> {
    if !meta_table_present(conn)? {
        return Ok(None);
    }

    let raw: Option<String> = conn
        .query_row(
            "SELECT value FROM index_meta WHERE key = ?1",
            params![META_VERSION_KEY],
            |row| row.get(0),
        )
        .optional()?;
    Ok(raw.and_then(|v| v.parse::<i64>().ok()))
}

/// Whether the `index_meta` table exists — i.e. the DB has been
/// bootstrapped at least once (even if its version row is corrupt).
fn meta_table_present(conn: &Connection) -> Result<bool> {
    let present: Option<i64> = conn
        .query_row(
            "SELECT 1 FROM sqlite_master WHERE type = 'table' AND name = 'index_meta'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(present.is_some())
}

fn create_all(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS index_meta (
            key   TEXT PRIMARY KEY,
            value TEXT NOT NULL
        );

        CREATE TABLE IF NOT EXISTS indexed_files (
            id              INTEGER PRIMARY KEY,
            path            TEXT NOT NULL UNIQUE,
            mtime           INTEGER NOT NULL,
            size            INTEGER NOT NULL,
            content_hash    TEXT NOT NULL,
            language        TEXT,
            last_indexed_at TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );

        CREATE TABLE IF NOT EXISTS code_chunks (
            id           INTEGER PRIMARY KEY,
            file_id      INTEGER NOT NULL REFERENCES indexed_files(id) ON DELETE CASCADE,
            start_line   INTEGER NOT NULL,
            end_line     INTEGER NOT NULL,
            symbol       TEXT,
            kind         TEXT,
            content_hash TEXT NOT NULL,
            embed_text   TEXT NOT NULL,
            created_at   TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );

        CREATE INDEX IF NOT EXISTS idx_code_chunks_file ON code_chunks(file_id);

        -- Populated in PR2 (tree-sitter chunker + embed). Empty in PR1.
        CREATE TABLE IF NOT EXISTS code_embeddings (
            chunk_id     INTEGER PRIMARY KEY REFERENCES code_chunks(id) ON DELETE CASCADE,
            model_name   TEXT NOT NULL,
            dimension    INTEGER NOT NULL,
            vector       BLOB NOT NULL,
            content_hash TEXT NOT NULL,
            created_at   TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP
        );

        -- Contentless FTS5 over code_chunks (mirror of the doc_chunks_fts
        -- shape in migration 0014). Sync triggers + backfill are wired in
        -- PR2 alongside population; the table is intentionally empty now.
        CREATE VIRTUAL TABLE IF NOT EXISTS code_chunks_fts USING fts5(
            symbol,
            embed_text,
            content='code_chunks',
            content_rowid='id'
        );",
    )?;
    Ok(())
}

fn drop_all(conn: &Connection) -> Result<()> {
    // Drop children before parents; FTS vtable first. `DROP TABLE IF
    // EXISTS` is idempotent, so a partial prior schema drops cleanly.
    conn.execute_batch(
        "DROP TABLE IF EXISTS code_chunks_fts;
         DROP TABLE IF EXISTS code_embeddings;
         DROP TABLE IF EXISTS code_chunks;
         DROP TABLE IF EXISTS indexed_files;
         DROP TABLE IF EXISTS index_meta;",
    )?;
    Ok(())
}
