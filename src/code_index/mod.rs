//! Code locator index (M11, decision 107).
//!
//! A cheap semantic locator over the repo's own source. It lives in a
//! **sibling DB** at `.memhub/code_index.sqlite`, physically separate
//! from `project.sqlite`, and is NEVER read by recall, NEVER exported,
//! NEVER synced. That physical separation is what preserves the recall
//! eval-regression guarantee structurally (mirrors the M9
//! registry-is-not-recall split).
//!
//! PR1 shipped the spine: the sibling-DB schema + bootstrap, a git-aware
//! walker, and the lazy `(mtime,size)`+hash staleness diff. PR2 (here)
//! adds the tree-sitter AST chunker ([`chunker`]/[`grammar`]), eager
//! embedding of chunks into `code_embeddings` (hybrid mode), and FTS
//! population via the schema's `code_chunks_fts` triggers. The `memhub
//! code index|status` CLI is PR3; the `memhub locate` query path also PR3.

pub mod chunker;
pub mod grammar;
pub mod locate;
pub mod schema;
pub mod walker;

use std::collections::HashMap;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::UNIX_EPOCH;

use rusqlite::{Connection, Statement, Transaction, params};

use crate::Result;
use crate::config::{PathMatcher, ProjectConfig, RetrievalMode};
use crate::db::{self, MEMHUB_DIR};
use crate::retrieval::embeddings::{EMBEDDING_DIMENSION, EMBEDDING_MODEL_NAME, embed_batch};
use crate::retrieval::util::{bytes_to_vector, sha256_hex, vector_to_le_bytes};

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
         PRAGMA synchronous = NORMAL;
         PRAGMA busy_timeout = 5000;
         PRAGMA recursive_triggers = OFF;",
    )?;
    schema::bootstrap(&conn)?;
    Ok(conn)
}

/// Outcome of a [`refresh`] pass over the working tree.
#[derive(Debug, Default, Clone)]
pub struct RefreshSummary {
    /// Indexable source files in the index after this refresh. Scoped to
    /// the grammar-known source languages (task 69); non-source files —
    /// docs, lockfiles, JSON/YAML/TOML, vendored `*.min.*` bundles — are
    /// excluded (see [`excluded_files`]). A source file that is non-UTF-8
    /// is still counted here but carries zero chunks.
    ///
    /// [`excluded_files`]: RefreshSummary::excluded_files
    pub files_total: usize,
    /// Total code chunks in the index after this refresh.
    pub chunks_total: usize,
    /// Files newly added to the index this pass.
    pub new_files: usize,
    /// Files whose content changed and were re-chunked this pass.
    pub changed_files: usize,
    /// Files left untouched (metadata or content matched the index).
    pub unchanged_files: usize,
    /// Index rows dropped because the file is no longer tracked (or was
    /// absent on disk this pass).
    pub deleted_files: usize,
    /// Tracked files examined but not indexed this pass: absent on disk,
    /// a symlink, or present-but-unreadable. Together with new/changed/
    /// unchanged this reconciles against the indexable set:
    /// `new + changed + unchanged + skipped == indexable tracked files`.
    pub skipped_files: usize,
    /// Tracked files excluded from the index because they are not an
    /// indexable source language, or are a vendored/minified bundle (task
    /// 69). These never enter the per-file loop; one previously indexed is
    /// dropped via the cleanup pass. With the four loop counters this
    /// reconciles against the full tracked set:
    /// `new + changed + unchanged + skipped + excluded == tracked total`.
    pub excluded_files: usize,
    /// Subset of new/changed files that were non-UTF-8 (tracked, but
    /// produced no chunks — a line-window chunker can't window binary).
    pub binary_skipped: usize,
    /// Chunks embedded this pass (hybrid mode only; 0 in fts mode or when
    /// every chunk already had a current vector).
    pub embedded_chunks: usize,
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
    // Resolve the canonical repo root + deny-list WITHOUT opening
    // project.sqlite: discover_paths + a config read are all we need, and
    // they skip open_project's heavy side effects (migrations, Claude
    // transcript scrape, registry upsert). The code index is a separate DB
    // and stays decoupled from project.sqlite (finding M1).
    let paths = db::discover_paths(start)?;
    let repo_root = paths.repo_root.clone();
    let config = if paths.config_path.exists() {
        ProjectConfig::load(&paths.config_path)?
    } else {
        let repo_name = repo_root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("memhub");
        ProjectConfig::default_for_repo_name(repo_name)
    };
    let matcher = PathMatcher::from_patterns(&config.deny_list.patterns)?;

    // `list_tracked_files` is the deny-list-filtered git set; scope it
    // further to indexable source files (task 69). Non-source files —
    // docs, lockfiles, JSON/YAML/TOML, vendored `*.min.*` bundles — are
    // dropped here so they can't out-rank real implementation files in
    // `locate`. Any such file already in the index is left in `existing`
    // and dropped by the cleanup pass below, so a re-index auto-prunes.
    let all_tracked = walker::list_tracked_files(&repo_root, &matcher)?;
    let tracked_total = all_tracked.len();
    let tracked: Vec<String> = all_tracked
        .into_iter()
        .filter(|p| is_indexable_source(p))
        .collect();
    let head = walker::current_head(&repo_root);

    let db_path = code_index_db_path(&repo_root);
    let mut conn = open_code_index(&db_path)?;
    let tx = conn.transaction()?;

    let mut existing = load_existing_files(&tx)?;
    let mut summary = RefreshSummary {
        head: head.clone(),
        excluded_files: tracked_total - tracked.len(),
        ..Default::default()
    };

    // Prepared once and reused across every file instead of re-preparing
    // per file (finding L2). Dropped before the cleanup writes + commit so
    // the transaction carries no outstanding borrow when consumed.
    let mut insert_chunk_stmt = tx.prepare(
        "INSERT INTO code_chunks(
            file_id, start_line, end_line, symbol, kind, content_hash, embed_text
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;

    for path in &tracked {
        let abs = repo_root.join(path);
        // symlink_metadata does NOT follow links, so a tracked symlink is
        // detected here rather than silently read by-target (finding M3),
        // and an absent file is distinguished from a present one.
        let meta = match fs::symlink_metadata(&abs) {
            Ok(m) => m,
            // Tracked but absent on disk: deleted-but-unstaged, or a sparse
            // checkout. Do NOT remove it from `existing` — leaving it there
            // lets the cleanup loop drop any stale row, which is the correct
            // staleness behavior (finding P2). A re-checkout re-indexes it.
            Err(_) => {
                summary.skipped_files += 1;
                continue;
            }
        };
        if meta.file_type().is_symlink() {
            // Never index a symlink — reading it would follow the link,
            // possibly outside the repo. Leave it in `existing` so a row
            // for a path that used to be a regular file is cleaned up.
            summary.skipped_files += 1;
            continue;
        }
        let mtime = mtime_millis(&meta);
        let size = meta.len() as i64;

        if let Some(prev) = existing.remove(path) {
            // Fast path: identical metadata => assume unchanged, no read.
            // Only valid when mtime is actually known; if the platform
            // can't report it we fall through to the content hash so a
            // same-size edit is never missed (finding M2).
            if let Some(mtime) = mtime
                && prev.mtime == mtime
                && prev.size == size
            {
                summary.unchanged_files += 1;
                continue;
            }
            let bytes = match fs::read(&abs) {
                Ok(b) => b,
                // Present but unreadable (permissions / TOCTOU vanish).
                // Skip and count; `prev` is already out of `existing`, so
                // the prior row survives the cleanup loop — don't wipe a
                // good entry over a transient blip (finding H1).
                Err(_) => {
                    summary.skipped_files += 1;
                    continue;
                }
            };
            let hash = sha256_hex(&bytes);
            if hash == prev.content_hash {
                // Content identical (touched / re-checked-out); refresh stat only.
                tx.execute(
                    "UPDATE indexed_files
                     SET mtime = ?1, size = ?2, last_indexed_at = CURRENT_TIMESTAMP
                     WHERE id = ?3",
                    params![mtime.unwrap_or(0), size, prev.id],
                )?;
                summary.unchanged_files += 1;
                continue;
            }
            // Real content change: re-chunk in place.
            tx.execute(
                "DELETE FROM code_chunks WHERE file_id = ?1",
                params![prev.id],
            )?;
            let is_binary = insert_chunks(
                &mut insert_chunk_stmt,
                prev.id,
                path,
                infer_language(path),
                &bytes,
            )?;
            tx.execute(
                "UPDATE indexed_files
                 SET mtime = ?1, size = ?2, content_hash = ?3,
                     language = ?4, last_indexed_at = CURRENT_TIMESTAMP
                 WHERE id = ?5",
                params![
                    mtime.unwrap_or(0),
                    size,
                    hash,
                    infer_language(path),
                    prev.id
                ],
            )?;
            summary.changed_files += 1;
            if is_binary {
                summary.binary_skipped += 1;
            }
        } else {
            let bytes = match fs::read(&abs) {
                Ok(b) => b,
                Err(_) => {
                    summary.skipped_files += 1;
                    continue;
                }
            };
            let hash = sha256_hex(&bytes);
            tx.execute(
                "INSERT INTO indexed_files(path, mtime, size, content_hash, language)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![path, mtime.unwrap_or(0), size, hash, infer_language(path)],
            )?;
            let file_id = tx.last_insert_rowid();
            let is_binary = insert_chunks(
                &mut insert_chunk_stmt,
                file_id,
                path,
                infer_language(path),
                &bytes,
            )?;
            summary.new_files += 1;
            if is_binary {
                summary.binary_skipped += 1;
            }
        }
    }

    drop(insert_chunk_stmt);

    // Anything still in the map is no longer tracked (or absent on disk):
    // drop it (the ON DELETE CASCADE on code_chunks.file_id removes its
    // chunks).
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

    summary.files_total = tx.query_row("SELECT COUNT(*) FROM indexed_files", [], |r| {
        r.get::<_, i64>(0)
    })? as usize;
    summary.chunks_total = tx.query_row("SELECT COUNT(*) FROM code_chunks", [], |r| {
        r.get::<_, i64>(0)
    })? as usize;

    tx.commit()?;

    // Phase 2: embed any chunk that lacks a current vector. Hybrid-only —
    // in fts mode `code_embeddings` stays empty and the locator (PR3) runs
    // FTS-only, mirroring the project DB's eager-embed gating. This runs
    // AFTER the chunk transaction commits (not inside it) so the heavy
    // model inference never holds the index write lock, and a transient
    // embed failure leaves a complete chunk+FTS index that degrades to FTS
    // rather than rolling the whole refresh back. Backfill semantics —
    // embed every missing chunk, not just this pass's new ones — means such
    // a failure self-heals on the next refresh.
    if config.retrieval.mode == RetrievalMode::Hybrid {
        summary.embedded_chunks = embed_missing(&mut conn)?;
    }
    Ok(summary)
}

/// A read-only snapshot of the sibling code index for `memhub code status`.
#[derive(Debug, Clone)]
pub struct CodeIndexStatus {
    /// Absolute path to the sibling DB.
    pub db_path: PathBuf,
    /// Whether the DB file exists yet (a never-indexed repo reports false
    /// with all-zero counts; status never creates the DB).
    pub exists: bool,
    /// Stored schema version, or `None` when absent/unparseable.
    pub schema_version: Option<i64>,
    /// Configured retrieval mode (governs whether vectors are populated).
    pub mode: RetrievalMode,
    pub files_total: i64,
    pub chunks_total: i64,
    pub embeddings_total: i64,
    /// `HEAD` recorded at the last refresh.
    pub last_head: Option<String>,
    /// Current repo `HEAD`, if resolvable.
    pub current_head: Option<String>,
}

impl CodeIndexStatus {
    /// True when the index was built against a different commit than the
    /// current `HEAD` — a coarse staleness cue. `false` when either side is
    /// unknown (no claim either way) or the index is empty.
    pub fn head_stale(&self) -> bool {
        match (&self.last_head, &self.current_head) {
            (Some(last), Some(current)) => last != current,
            _ => false,
        }
    }
}

/// Snapshot the sibling index without refreshing it. Reports `exists:
/// false` (and zero counts) when the DB has never been created, rather than
/// bootstrapping it — a status check must not have side effects.
pub fn status(start: &Path) -> Result<CodeIndexStatus> {
    let paths = db::discover_paths(start)?;
    let repo_root = paths.repo_root.clone();
    let config = if paths.config_path.exists() {
        ProjectConfig::load(&paths.config_path)?
    } else {
        let repo_name = repo_root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("memhub");
        ProjectConfig::default_for_repo_name(repo_name)
    };
    let db_path = code_index_db_path(&repo_root);
    let current_head = walker::current_head(&repo_root);

    if !db_path.exists() {
        return Ok(CodeIndexStatus {
            db_path,
            exists: false,
            schema_version: None,
            mode: config.retrieval.mode,
            files_total: 0,
            chunks_total: 0,
            embeddings_total: 0,
            last_head: None,
            current_head,
        });
    }

    let conn = Connection::open(&db_path)?;
    let schema_version = schema::stored_version(&conn)?;
    let files_total = conn.query_row("SELECT COUNT(*) FROM indexed_files", [], |r| r.get(0))?;
    let chunks_total = conn.query_row("SELECT COUNT(*) FROM code_chunks", [], |r| r.get(0))?;
    let embeddings_total =
        conn.query_row("SELECT COUNT(*) FROM code_embeddings", [], |r| r.get(0))?;
    let last_head: Option<String> = conn
        .query_row(
            "SELECT value FROM index_meta WHERE key = 'last_head'",
            [],
            |r| r.get(0),
        )
        .ok();

    Ok(CodeIndexStatus {
        db_path,
        exists: true,
        schema_version,
        mode: config.retrieval.mode,
        files_total,
        chunks_total,
        embeddings_total,
        last_head,
        current_head,
    })
}

/// Outcome of [`remove_index`].
#[derive(Debug, Clone)]
pub struct RemoveOutcome {
    pub db_path: PathBuf,
    /// True when the main DB file existed and was deleted.
    pub removed: bool,
}

/// Delete the sibling code-index DB (and its `-wal` / `-shm` companions).
/// The index is fully regenerable from the working tree (decision 107), so
/// this is a disposable-cache wipe, not a destructive data loss. A
/// `removed: false` means there was nothing to delete.
pub fn remove_index(start: &Path) -> Result<RemoveOutcome> {
    let paths = db::discover_paths(start)?;
    let db_path = code_index_db_path(&paths.repo_root);
    let removed = db_path.exists();
    for suffix in ["", "-wal", "-shm"] {
        let candidate = if suffix.is_empty() {
            db_path.clone()
        } else {
            let mut name = db_path.as_os_str().to_os_string();
            name.push(suffix);
            PathBuf::from(name)
        };
        if candidate.exists() {
            // A missing WAL/SHM is fine; only surface a real removal error.
            fs::remove_file(&candidate)?;
        }
    }
    Ok(RemoveOutcome { db_path, removed })
}

/// Embed every `code_chunks` row that has no `code_embeddings` row yet and
/// persist the vectors. Reuses the shared BGE model ([`embed_batch`]).
///
/// An in-run cache keyed by the `embed_text` hash means a chunk whose text
/// matches one already embedded — duplicated code, or an unchanged sibling
/// whose vector survived this refresh — reuses that vector instead of
/// re-running the model. Cache misses are gathered into a single batched
/// inference. Returns the number of chunks given an embedding row.
fn embed_missing(conn: &mut Connection) -> Result<usize> {
    // Chunks still missing a vector (new this pass, or a prior embed blip).
    // Queried FIRST so a warm `locate` — where nothing is missing — returns
    // before decoding the entire `code_embeddings` table into the cache.
    let missing: Vec<(i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT c.id, c.embed_text FROM code_chunks c
             LEFT JOIN code_embeddings e ON e.chunk_id = c.id
             WHERE e.chunk_id IS NULL",
        )?;
        let rows = stmt.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };
    if missing.is_empty() {
        return Ok(0);
    }

    // Seed the cache from embeddings that survived the chunk diff.
    let mut cache: HashMap<String, Vec<f32>> = HashMap::new();
    {
        let mut stmt = conn.prepare("SELECT content_hash, vector FROM code_embeddings")?;
        let rows = stmt.query_map([], |r| {
            Ok((r.get::<_, String>(0)?, r.get::<_, Vec<u8>>(1)?))
        })?;
        for row in rows {
            let (hash, blob) = row?;
            cache.insert(hash, bytes_to_vector(&blob));
        }
    }

    // Hash each chunk's embed_text; gather the unique cache-miss texts for
    // one batched model call. `pending` maps a hash to its slot in the batch
    // so duplicates within this pass embed exactly once.
    let mut hashes: Vec<String> = Vec::with_capacity(missing.len());
    let mut pending: HashMap<String, usize> = HashMap::new();
    let mut batch_texts: Vec<&str> = Vec::new();
    for (_, text) in &missing {
        let hash = sha256_hex(text.as_bytes());
        if !cache.contains_key(&hash) && !pending.contains_key(&hash) {
            pending.insert(hash.clone(), batch_texts.len());
            batch_texts.push(text.as_str());
        }
        hashes.push(hash);
    }

    if !batch_texts.is_empty() {
        let vectors = embed_batch(&batch_texts)?;
        for (hash, idx) in &pending {
            cache.insert(hash.clone(), vectors[*idx].clone());
        }
    }

    let tx = conn.transaction()?;
    {
        let mut insert = tx.prepare(
            "INSERT INTO code_embeddings(chunk_id, model_name, dimension, vector, content_hash)
             VALUES (?1, ?2, ?3, ?4, ?5)",
        )?;
        for ((chunk_id, _), hash) in missing.iter().zip(&hashes) {
            let vector = cache
                .get(hash)
                .expect("every missing chunk's vector is cached or freshly embedded");
            insert.execute(params![
                chunk_id,
                EMBEDDING_MODEL_NAME,
                EMBEDDING_DIMENSION as i64,
                vector_to_le_bytes(vector),
                hash,
            ])?;
        }
    }
    tx.commit()?;
    Ok(missing.len())
}

fn load_existing_files(tx: &Transaction<'_>) -> Result<HashMap<String, ExistingFile>> {
    let mut stmt = tx.prepare("SELECT id, path, mtime, size, content_hash FROM indexed_files")?;
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

/// Chunk `bytes` and insert the chunks for `file_id` via the caller's
/// hoisted statement. The `language` hint (from [`infer_language`]) picks
/// the AST grammar. Since task 69, only indexable source reaches here, so
/// the line-window fallback now covers just a grammar-known file that fails
/// to parse (not unknown extensions, which are filtered upstream). Returns
/// `true` when the file was non-UTF-8 (tracked, zero chunks) so callers can
/// count it.
fn insert_chunks(
    stmt: &mut Statement<'_>,
    file_id: i64,
    path: &str,
    language: Option<&str>,
    bytes: &[u8],
) -> Result<bool> {
    let Ok(text) = std::str::from_utf8(bytes) else {
        return Ok(true);
    };
    for c in &chunker::chunk_file(path, text, language) {
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

/// File mtime in milliseconds since the epoch, or `None` when the platform
/// can't report a modified time. Callers must treat `None` as "metadata
/// fast path unavailable" and fall back to the content hash, never as a
/// concrete `0` (which would make every same-size edit look unchanged).
fn mtime_millis(meta: &fs::Metadata) -> Option<i64> {
    meta.modified()
        .ok()
        .and_then(|t| t.duration_since(UNIX_EPOCH).ok())
        .map(|d| d.as_millis() as i64)
}

/// True when a tracked path should enter the code index (task 69): a
/// source language the grammar registry can chunk, and not a
/// vendored/minified bundle. This deliberately reverses the pre-task-69
/// "index every tracked file" behavior — docs, lockfiles, JSON/YAML/TOML,
/// and `*.min.*` artifacts are excluded so they cannot out-rank real
/// implementation files in `locate` (the dominant Recall error mode,
/// decision 114). The grammar registry is the single source of truth for
/// "indexable source", so a future language row is included automatically.
fn is_indexable_source(path: &str) -> bool {
    !is_vendored(path) && grammar::grammar_for(infer_language(path)).is_some()
}

/// A generated/minified bundle that has a real source extension but is not
/// hand-written code we want surfacing as a hit (e.g. `uplot.min.js`). The
/// `.min.` filename infix is the precise marker; matching it rather than a
/// `vendor/` directory name avoids excluding hand-written code that merely
/// lives under such a path.
fn is_vendored(path: &str) -> bool {
    path.rsplit('/').next().unwrap_or(path).contains(".min.")
}

/// Best-effort language hint from the file extension. Mirrors the set in
/// `ingest_git`; the grammar registry keys off this. Note the `toml`,
/// `markdown`, `json`, `yaml`, and `sql` arms have no grammar row, so
/// [`is_indexable_source`] excludes those files from the index; the hint
/// remains for parity with `ingest_git`.
fn infer_language(path: &str) -> Option<&'static str> {
    let extension = path.rsplit('.').next()?;
    match extension.to_ascii_lowercase().as_str() {
        "rs" => Some("rust"),
        "toml" => Some("toml"),
        "md" => Some("markdown"),
        "json" => Some("json"),
        "yaml" | "yml" => Some("yaml"),
        "ts" | "tsx" | "mts" | "cts" => Some("typescript"),
        "js" | "jsx" | "mjs" | "cjs" => Some("javascript"),
        "py" => Some("python"),
        "cs" => Some("csharp"),
        "java" => Some("java"),
        "go" => Some("go"),
        "sql" => Some("sql"),
        _ => None,
    }
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
    fn embeddings_and_fts_start_empty_on_fresh_bootstrap() {
        // A bootstrapped-but-never-refreshed index has no chunks, so the
        // embedding + FTS tables exist and are empty. Population happens in
        // refresh() (FTS via triggers, embeddings in hybrid mode).
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
    fn synchronous_pragma_is_normal_on_open() {
        // D5/Q35: WAL is paired with `synchronous = NORMAL` (decision 140)
        // rather than the SQLite default FULL, dropping one fsync per
        // commit. 1 is SQLite's integer encoding for NORMAL.
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("code_index.sqlite");
        let conn = open_code_index(&path).expect("open");

        let synchronous: i64 = conn
            .query_row("PRAGMA synchronous", [], |row| row.get(0))
            .expect("query synchronous pragma");
        assert_eq!(synchronous, 1, "expected synchronous = NORMAL (1)");
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

    #[test]
    fn indexable_source_accepts_grammar_languages_and_rejects_the_rest() {
        // Every grammar-known source language is indexable.
        for path in [
            "src/main.rs",
            "app/handler.ts",
            "ui/button.tsx",
            "lib/util.js",
            "server.mjs",
            "scripts/run.py",
            "cmd/main.go",
            "Service.cs",
            "Widget.java",
        ] {
            assert!(is_indexable_source(path), "should index: {path}");
        }
        // Non-source files have no grammar row and are excluded — these are
        // the locate pollutants task 69 targets.
        for path in [
            "README.md",
            "AGENTS.md",
            "Cargo.lock",
            "Cargo.toml",
            "data.json",
            "config.yaml",
            "deploy.yml",
            "migrations/0001_init.sql",
            "tests/code_locate_golden.json",
            "LICENSE",
        ] {
            assert!(!is_indexable_source(path), "should exclude: {path}");
        }
    }

    #[test]
    fn vendored_minified_bundles_are_excluded_despite_source_extension() {
        // A real .js extension has a grammar, but a minified bundle must
        // not surface as a code hit (task 69).
        assert!(is_vendored("src/dashboard/static/vendor/uplot.min.js"));
        assert!(is_vendored("uplot.min.css"));
        assert!(is_vendored("dist/app.min.mjs"));
        assert!(!is_indexable_source("src/dashboard/static/vendor/uplot.min.js"));
        // Hand-written code is not minified, even under a vendor-ish path.
        assert!(!is_vendored("src/vendor/adapter.js"));
        assert!(is_indexable_source("src/vendor/adapter.js"));
    }

    #[test]
    fn corrupt_version_value_triggers_drop_and_rebuild() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("code_index.sqlite");

        // Bootstrap, plant a row, then corrupt the version to a non-integer
        // so it no longer parses (finding L5).
        {
            let conn = open_code_index(&path).expect("open");
            conn.execute(
                "INSERT INTO indexed_files(path, mtime, size, content_hash)
                 VALUES ('src/main.rs', 1, 2, 'deadbeef')",
                [],
            )
            .expect("seed file row");
            conn.execute(
                "UPDATE index_meta SET value = 'not-a-number' WHERE key = 'schema_version'",
                [],
            )
            .expect("corrupt version");
        }

        let conn = open_code_index(&path).expect("reopen");
        assert_eq!(
            schema::stored_version(&conn).expect("version"),
            Some(CODE_INDEX_SCHEMA_VERSION)
        );
        let files: i64 = conn
            .query_row("SELECT COUNT(*) FROM indexed_files", [], |r| r.get(0))
            .expect("count files");
        assert_eq!(files, 0, "corrupt version should force a rebuild");
    }
}
