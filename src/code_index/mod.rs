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

pub mod chunker;
pub mod schema;
pub mod walker;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use rusqlite::{Connection, Transaction, params};
use sha2::{Digest, Sha256};

use crate::Result;
use crate::config::PathMatcher;
use crate::db::{self, MEMHUB_DIR};

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

/// Outcome of a [`refresh`] pass over the working tree.
#[derive(Debug, Default, Clone)]
pub struct RefreshSummary {
    /// Tracked files in the index after this refresh (includes binaries,
    /// which are tracked with zero chunks).
    pub files_total: usize,
    /// Total code chunks in the index after this refresh.
    pub chunks_total: usize,
    /// Files newly added to the index this pass.
    pub new_files: usize,
    /// Files whose content changed and were re-chunked this pass.
    pub changed_files: usize,
    /// Files left untouched (metadata or content matched the index).
    pub unchanged_files: usize,
    /// Index rows dropped because the file is no longer tracked.
    pub deleted_files: usize,
    /// Subset of new/changed files that were non-UTF-8 (tracked, but
    /// produced no chunks — a line-window chunker can't window binary).
    pub binary_skipped: usize,
    /// Indexed `HEAD`, if resolvable.
    pub head: Option<String>,
}

struct ExistingFile {
    id: i64,
    mtime: i64,
    size: i64,
    content_hash: String,
}

/// Bring the code index in line with the current working tree.
///
/// Lazy and git-aware (decision 107): the tracked set comes from `git
/// ls-files` minus the deny-list, and per file we diff `(mtime, size)`
/// against the index, confirming with a content hash only when metadata
/// moved. Unchanged files are never read; changed/added files are
/// re-chunked; files no longer tracked have their rows (and, via cascade,
/// chunks) dropped. The whole pass is one transaction.
pub fn refresh(start: &Path) -> Result<RefreshSummary> {
    // open_project gives us the canonical repo root + the deny-list. We
    // never touch its connection — the code index is a separate DB.
    let ctx = db::open_project(start)?;
    let repo_root = ctx.paths.repo_root.clone();
    let matcher = PathMatcher::from_patterns(&ctx.config.deny_list.patterns)?;
    drop(ctx);

    let tracked = walker::list_tracked_files(&repo_root, &matcher)?;
    let head = walker::current_head(&repo_root);

    let db_path = code_index_db_path(&repo_root);
    let mut conn = open_code_index(&db_path)?;
    let tx = conn.transaction()?;

    let mut existing = load_existing_files(&tx)?;
    let mut summary = RefreshSummary {
        head: head.clone(),
        ..Default::default()
    };

    for path in &tracked {
        let abs = repo_root.join(path);
        let meta = match fs::metadata(&abs) {
            Ok(m) => m,
            // Tracked but absent in the worktree (e.g. sparse checkout).
            // Leave any prior row intact; don't index, don't delete.
            Err(_) => {
                existing.remove(path);
                continue;
            }
        };
        let mtime = mtime_millis(&meta);
        let size = meta.len() as i64;

        if let Some(prev) = existing.remove(path) {
            // Fast path: identical metadata => assume unchanged, no read.
            if prev.mtime == mtime && prev.size == size {
                summary.unchanged_files += 1;
                continue;
            }
            let bytes = fs::read(&abs)?;
            let hash = sha256_hex(&bytes);
            if hash == prev.content_hash {
                // Content identical (touched / re-checked-out); refresh stat only.
                tx.execute(
                    "UPDATE indexed_files
                     SET mtime = ?1, size = ?2, last_indexed_at = CURRENT_TIMESTAMP
                     WHERE id = ?3",
                    params![mtime, size, prev.id],
                )?;
                summary.unchanged_files += 1;
                continue;
            }
            // Real content change: re-chunk in place.
            tx.execute("DELETE FROM code_chunks WHERE file_id = ?1", params![prev.id])?;
            let is_binary = insert_chunks(&tx, prev.id, path, &bytes)?;
            tx.execute(
                "UPDATE indexed_files
                 SET mtime = ?1, size = ?2, content_hash = ?3,
                     language = ?4, last_indexed_at = CURRENT_TIMESTAMP
                 WHERE id = ?5",
                params![mtime, size, hash, infer_language(path), prev.id],
            )?;
            summary.changed_files += 1;
            if is_binary {
                summary.binary_skipped += 1;
            }
        } else {
            let bytes = fs::read(&abs)?;
            let hash = sha256_hex(&bytes);
            tx.execute(
                "INSERT INTO indexed_files(path, mtime, size, content_hash, language)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![path, mtime, size, hash, infer_language(path)],
            )?;
            let file_id = tx.last_insert_rowid();
            let is_binary = insert_chunks(&tx, file_id, path, &bytes)?;
            summary.new_files += 1;
            if is_binary {
                summary.binary_skipped += 1;
            }
        }
    }

    // Anything still in the map is no longer tracked: drop it (the
    // ON DELETE CASCADE on code_chunks.file_id removes its chunks).
    for prev in existing.values() {
        tx.execute("DELETE FROM indexed_files WHERE id = ?1", params![prev.id])?;
        summary.deleted_files += 1;
    }

    if let Some(h) = &head {
        tx.execute(
            "INSERT INTO index_meta(key, value) VALUES ('last_head', ?1)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![h],
        )?;
    }

    summary.files_total =
        tx.query_row("SELECT COUNT(*) FROM indexed_files", [], |r| r.get::<_, i64>(0))? as usize;
    summary.chunks_total =
        tx.query_row("SELECT COUNT(*) FROM code_chunks", [], |r| r.get::<_, i64>(0))? as usize;

    tx.commit()?;
    Ok(summary)
}

fn load_existing_files(tx: &Transaction<'_>) -> Result<HashMap<String, ExistingFile>> {
    let mut stmt =
        tx.prepare("SELECT id, path, mtime, size, content_hash FROM indexed_files")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, String>(1)?,
            ExistingFile {
                id: row.get(0)?,
                mtime: row.get(2)?,
                size: row.get(3)?,
                content_hash: row.get(4)?,
            },
        ))
    })?;
    let mut map = HashMap::new();
    for row in rows {
        let (path, file) = row?;
        map.insert(path, file);
    }
    Ok(map)
}

/// Chunk `bytes` and insert the chunks for `file_id`. Returns `true` when
/// the file was non-UTF-8 (tracked, zero chunks) so callers can count it.
fn insert_chunks(tx: &Transaction<'_>, file_id: i64, path: &str, bytes: &[u8]) -> Result<bool> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Ok(true);
    };
    let chunks = chunker::chunk_file(path, text);
    let mut stmt = tx.prepare(
        "INSERT INTO code_chunks(
            file_id, start_line, end_line, symbol, kind, content_hash, embed_text
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for c in &chunks {
        stmt.execute(params![
            file_id,
            c.start_line as i64,
            c.end_line as i64,
            c.symbol,
            c.kind,
            sha256_hex(c.body.as_bytes()),
            c.embed_text,
        ])?;
    }
    Ok(false)
}

fn mtime_millis(meta: &fs::Metadata) -> i64 {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

/// Best-effort language hint from the file extension. Mirrors the set in
/// `ingest_git`; PR2's grammar registry keys off this.
fn infer_language(path: &str) -> Option<&'static str> {
    let extension = path.rsplit('.').next()?;
    match extension.to_ascii_lowercase().as_str() {
        "rs" => Some("rust"),
        "toml" => Some("toml"),
        "md" => Some("markdown"),
        "json" => Some("json"),
        "yaml" | "yml" => Some("yaml"),
        "ts" | "tsx" => Some("typescript"),
        "js" | "jsx" => Some("javascript"),
        "py" => Some("python"),
        "sql" => Some("sql"),
        _ => None,
    }
}

fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
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
