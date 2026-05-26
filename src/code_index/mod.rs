//! Code locator index (M11, decision 107).
//!
//! A cheap semantic locator over the repo's own source. It lives in a
//! **sibling DB** at `.memhub/code_index.sqlite`, physically separate
//! from `project.sqlite`, and is NEVER read by recall, NEVER exported,
//! NEVER synced. That physical separation is what preserves the recall
//! eval-regression guarantee structurally (mirrors the M9
//! registry-is-not-recall split).
//!
//! PR1 ships the spine: the sibling-DB schema + bootstrap, plus (in
//! later commits of this PR) a git-aware walker, the lazy staleness
//! diff, a line-window placeholder chunker, and the `memhub code
//! index|status` CLI. The tree-sitter chunker, embedding, and FTS
//! population arrive in PR2; the `memhub locate` query path in PR3.

pub mod schema;

use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::Result;
use crate::db::MEMHUB_DIR;

/// Filename of the sibling code-index DB inside `.memhub/`.
pub const CODE_INDEX_DB_FILENAME: &str = "code_index.sqlite";

/// Resolve the sibling DB path for a repo root: `<root>/.memhub/code_index.sqlite`.
pub fn code_index_db_path(repo_root: &Path) -> PathBuf {
    repo_root.join(MEMHUB_DIR).join(CODE_INDEX_DB_FILENAME)
}

/// Open (creating + bootstrapping if necessary) the sibling code-index DB
/// at the given path. This connection is independent of the project DB —
/// callers must never run code-index DDL/DML against `project.sqlite`.
pub fn open_code_index(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(db_path)?;
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA busy_timeout = 5000;",
    )?;
    schema::bootstrap(&conn)?;
    Ok(conn)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::params;
    use schema::CODE_INDEX_SCHEMA_VERSION;
    use tempfile::tempdir;

    fn table_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT 1 FROM sqlite_master WHERE type IN ('table','view') AND name = ?1",
            params![name],
            |row| row.get::<_, i64>(0),
        )
        .is_ok()
    }

    #[test]
    fn bootstrap_creates_schema_at_current_version() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("code_index.sqlite");
        let conn = open_code_index(&path).expect("open");

        for table in [
            "index_meta",
            "indexed_files",
            "code_chunks",
            "code_embeddings",
            "code_chunks_fts",
        ] {
            assert!(table_exists(&conn, table), "missing table: {table}");
        }
        assert_eq!(
            schema::stored_version(&conn).expect("version"),
            Some(CODE_INDEX_SCHEMA_VERSION)
        );
    }

    #[test]
    fn embeddings_and_fts_start_empty_in_pr1() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("code_index.sqlite");
        let conn = open_code_index(&path).expect("open");

        let embeddings: i64 = conn
            .query_row("SELECT COUNT(*) FROM code_embeddings", [], |r| r.get(0))
            .expect("count embeddings");
        let fts: i64 = conn
            .query_row("SELECT COUNT(*) FROM code_chunks_fts", [], |r| r.get(0))
            .expect("count fts");
        assert_eq!(embeddings, 0);
        assert_eq!(fts, 0);
    }

    #[test]
    fn reopen_is_idempotent() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("code_index.sqlite");
        open_code_index(&path).expect("first open");
        let conn = open_code_index(&path).expect("second open");
        assert_eq!(
            schema::stored_version(&conn).expect("version"),
            Some(CODE_INDEX_SCHEMA_VERSION)
        );
    }

    #[test]
    fn version_mismatch_triggers_drop_and_rebuild() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("code_index.sqlite");

        // Bootstrap, then plant a row and stamp a stale version.
        {
            let conn = open_code_index(&path).expect("open");
            conn.execute(
                "INSERT INTO indexed_files(path, mtime, size, content_hash)
                 VALUES ('src/main.rs', 1, 2, 'deadbeef')",
                [],
            )
            .expect("seed file row");
            conn.execute(
                "UPDATE index_meta SET value = ?1 WHERE key = 'schema_version'",
                params![(CODE_INDEX_SCHEMA_VERSION - 1).to_string()],
            )
            .expect("stale version");
        }

        // Reopening sees the mismatch and rebuilds from scratch.
        let conn = open_code_index(&path).expect("reopen");
        assert_eq!(
            schema::stored_version(&conn).expect("version"),
            Some(CODE_INDEX_SCHEMA_VERSION)
        );
        let files: i64 = conn
            .query_row("SELECT COUNT(*) FROM indexed_files", [], |r| r.get(0))
            .expect("count files");
        assert_eq!(files, 0, "stale rows should be dropped on rebuild");
    }
}
