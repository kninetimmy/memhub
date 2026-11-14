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
use std::io::ErrorKind;
use std::path::{Component, Path, PathBuf};
use std::thread;
use std::time::{Duration, Instant, UNIX_EPOCH};

use rusqlite::{Connection, Statement, Transaction, TransactionBehavior, params};

use crate::config::{PathMatcher, ProjectConfig, RetrievalMode};
use crate::db::{self, MEMHUB_DIR};
use crate::fs_identity::file_info;
use crate::retrieval::embeddings::{EMBEDDING_DIMENSION, EMBEDDING_MODEL_NAME, embed_batch};
use crate::retrieval::util::{bytes_to_vector, sha256_hex, vector_to_le_bytes};
use crate::{MemhubError, Result};

/// Filename of the sibling code-index DB inside `.memhub/`.
pub const CODE_INDEX_DB_FILENAME: &str = "code_index.sqlite";

const CODE_INDEX_BUSY_TIMEOUT: Duration = Duration::from_secs(5);
const JOURNAL_MODE_RETRY_DELAY: Duration = Duration::from_millis(10);

/// Resolve the sibling DB path for a repo root: `<root>/.memhub/code_index.sqlite`.
pub fn code_index_db_path(repo_root: &Path) -> PathBuf {
    repo_root.join(MEMHUB_DIR).join(CODE_INDEX_DB_FILENAME)
}

const SQLITE_STORAGE_SUFFIXES: [&str; 4] = ["", "-wal", "-shm", "-journal"];

/// Resolve and validate the selected checkout's physical cache storage.
///
/// The metadata directory and every existing SQLite file must be direct
/// entries at their canonical expected paths. Symlink/junction targets and
/// multiply-linked files are refused so two checkouts cannot share one live
/// cache under different textual paths.
pub(crate) fn validate_code_index_storage(
    repo_root: &Path,
    create_metadata_dir: bool,
) -> Result<PathBuf> {
    let repo_root = fs::canonicalize(repo_root).map_err(|error| {
        cache_storage_error(format!(
            "cannot resolve code root '{}': {error}",
            repo_root.display()
        ))
    })?;
    let metadata_dir = repo_root.join(MEMHUB_DIR);
    match fs::symlink_metadata(&metadata_dir) {
        Ok(_) => {}
        Err(error) if error.kind() == ErrorKind::NotFound && create_metadata_dir => {
            match fs::create_dir(&metadata_dir) {
                Ok(()) => {}
                Err(error) if error.kind() == ErrorKind::AlreadyExists => {}
                Err(error) => {
                    return Err(cache_storage_error(format!(
                        "cannot create code cache metadata directory '{}': {error}",
                        metadata_dir.display()
                    )));
                }
            }
        }
        Err(error) if error.kind() == ErrorKind::NotFound => {
            return Ok(metadata_dir.join(CODE_INDEX_DB_FILENAME));
        }
        Err(error) => {
            return Err(cache_storage_error(format!(
                "cannot inspect code cache metadata directory '{}': {error}",
                metadata_dir.display()
            )));
        }
    }

    let metadata = fs::symlink_metadata(&metadata_dir).map_err(|error| {
        cache_storage_error(format!(
            "cannot inspect code cache metadata directory '{}': {error}",
            metadata_dir.display()
        ))
    })?;
    if metadata.file_type().is_symlink() || !metadata.is_dir() {
        return Err(cache_storage_error(format!(
            "code cache metadata directory '{}' is not a direct directory",
            metadata_dir.display()
        )));
    }
    let actual_dir = fs::canonicalize(&metadata_dir).map_err(|error| {
        cache_storage_error(format!(
            "cannot resolve code cache metadata directory '{}': {error}",
            metadata_dir.display()
        ))
    })?;
    if actual_dir != metadata_dir {
        return Err(cache_storage_error(format!(
            "code cache metadata directory '{}' resolves outside its selected checkout storage \
             to '{}'",
            metadata_dir.display(),
            actual_dir.display()
        )));
    }

    let db_path = metadata_dir.join(CODE_INDEX_DB_FILENAME);
    for suffix in SQLITE_STORAGE_SUFFIXES {
        validate_direct_cache_file(&sqlite_path(&db_path, suffix))?;
    }
    Ok(db_path)
}

fn validate_direct_cache_file(path: &Path) -> Result<()> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == ErrorKind::NotFound => return Ok(()),
        Err(error) => {
            return Err(cache_storage_error(format!(
                "cannot inspect code cache file '{}': {error}",
                path.display()
            )));
        }
    };
    if metadata.file_type().is_symlink() || !metadata.is_file() {
        return Err(cache_storage_error(format!(
            "code cache file '{}' is not a direct regular file",
            path.display()
        )));
    }
    let actual = fs::canonicalize(path).map_err(|error| {
        cache_storage_error(format!(
            "cannot resolve code cache file '{}': {error}",
            path.display()
        ))
    })?;
    if actual != path {
        return Err(cache_storage_error(format!(
            "code cache file '{}' resolves to '{}'",
            path.display(),
            actual.display()
        )));
    }
    let info = file_info(path).map_err(|error| {
        cache_storage_error(format!(
            "cannot verify code cache file '{}': {error}",
            path.display()
        ))
    })?;
    if info.links != 1 {
        return Err(cache_storage_error(format!(
            "code cache file '{}' has {} hard links; cache storage must be independent",
            path.display(),
            info.links
        )));
    }
    Ok(())
}

fn sqlite_path(db_path: &Path, suffix: &str) -> PathBuf {
    if suffix.is_empty() {
        return db_path.to_path_buf();
    }
    let mut path = db_path.as_os_str().to_os_string();
    path.push(suffix);
    PathBuf::from(path)
}

fn cache_storage_error(message: impl Into<String>) -> MemhubError {
    MemhubError::Binding(format!("code cache storage invalid: {}", message.into()))
}

/// A fixed checkout/config pairing for one code-index operation. Ordinary
/// CLI callers discover both from one project root; the MCP server supplies
/// the launch-bound checkout plus the primary project's config path.
#[derive(Debug, Clone)]
pub(crate) struct CodeIndexSource {
    repo_root: PathBuf,
    config_path: PathBuf,
}

impl CodeIndexSource {
    pub(crate) fn discover(start: &Path) -> Result<Self> {
        let paths = db::discover_paths(start)?;
        Ok(Self {
            repo_root: paths.repo_root,
            config_path: paths.config_path,
        })
    }

    pub(crate) fn bound(code_root: &Path, config_path: &Path) -> Result<Self> {
        Ok(Self {
            repo_root: fs::canonicalize(code_root)?,
            config_path: config_path.to_path_buf(),
        })
    }

    fn load_config(&self) -> Result<ProjectConfig> {
        if self.config_path.exists() {
            ProjectConfig::load(&self.config_path)
        } else {
            let repo_name = self
                .config_path
                .parent()
                .and_then(Path::parent)
                .and_then(Path::file_name)
                .and_then(|name| name.to_str())
                .unwrap_or("memhub");
            Ok(ProjectConfig::default_for_repo_name(repo_name))
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SourceFileIssue {
    InvalidRelativePath,
    Unreadable,
    Symlink,
    EscapesRoot,
    NotFile,
}

pub(crate) struct SourceFile {
    pub(crate) path: PathBuf,
    pub(crate) metadata: fs::Metadata,
}

/// Resolve one Git-reported relative path without allowing a final symlink
/// or an ancestor symlink/junction to escape `canonical_root`.
pub(crate) fn resolve_source_file(
    canonical_root: &Path,
    relative: &str,
) -> std::result::Result<SourceFile, SourceFileIssue> {
    let relative = Path::new(relative);
    if relative.as_os_str().is_empty()
        || relative
            .components()
            .any(|component| !matches!(component, Component::Normal(_)))
    {
        return Err(SourceFileIssue::InvalidRelativePath);
    }

    let lexical = canonical_root.join(relative);
    let link_metadata = fs::symlink_metadata(&lexical).map_err(|_| SourceFileIssue::Unreadable)?;
    if link_metadata.file_type().is_symlink() {
        return Err(SourceFileIssue::Symlink);
    }
    let canonical = fs::canonicalize(&lexical).map_err(|_| SourceFileIssue::Unreadable)?;
    if canonical == canonical_root || !canonical.starts_with(canonical_root) {
        return Err(SourceFileIssue::EscapesRoot);
    }
    let metadata = fs::metadata(&canonical).map_err(|_| SourceFileIssue::Unreadable)?;
    if !metadata.is_file() {
        return Err(SourceFileIssue::NotFile);
    }
    Ok(SourceFile {
        path: canonical,
        metadata,
    })
}

/// Open (creating + bootstrapping if necessary) a sibling code-index DB.
/// Production callers validate checkout-local storage before reaching this
/// low-level connection helper.
pub(crate) fn open_code_index(db_path: &Path) -> Result<Connection> {
    if let Some(parent) = db_path.parent() {
        fs::create_dir_all(parent)?;
    }
    let conn = Connection::open(db_path)?;
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA recursive_triggers = OFF;",
    )?;
    set_wal_journal_mode(&conn)?;
    conn.execute_batch("PRAGMA synchronous = NORMAL;")?;
    schema::bootstrap(&conn)?;
    Ok(conn)
}

fn set_wal_journal_mode(conn: &Connection) -> Result<()> {
    // Disable rusqlite's per-attempt default handler so repeated PRAGMAs
    // cannot each consume a full timeout beyond the outer deadline.
    conn.busy_timeout(Duration::ZERO)?;
    set_wal_journal_mode_with(conn, thread::sleep)?;
    conn.busy_timeout(CODE_INDEX_BUSY_TIMEOUT)?;
    Ok(())
}

fn set_wal_journal_mode_with<F>(conn: &Connection, mut wait: F) -> Result<()>
where
    F: FnMut(Duration),
{
    // A concurrent cold opener can make this mode transition return
    // SQLITE_BUSY without invoking SQLite's installed busy handler. Retry
    // only that operation, and only inside the same five-second budget used
    // by the connection's normal busy handler.
    let deadline = Instant::now() + CODE_INDEX_BUSY_TIMEOUT;
    loop {
        match conn.execute_batch("PRAGMA journal_mode = WAL;") {
            Ok(()) => return Ok(()),
            Err(error) if is_database_busy(&error) => {
                let Some(remaining) = deadline.checked_duration_since(Instant::now()) else {
                    return Err(error.into());
                };
                if remaining.is_zero() {
                    return Err(error.into());
                }
                wait(std::cmp::min(JOURNAL_MODE_RETRY_DELAY, remaining));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

fn is_database_busy(error: &rusqlite::Error) -> bool {
    matches!(
        error,
        rusqlite::Error::SqliteFailure(code, _)
            if code.code == rusqlite::ErrorCode::DatabaseBusy
    )
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
    let source = CodeIndexSource::discover(start)?;
    refresh_source(&source)
}

pub(crate) fn refresh_source(source: &CodeIndexSource) -> Result<RefreshSummary> {
    // Resolve the selected code root + config WITHOUT opening
    // project.sqlite. Ordinary callers discovered both together; a bound
    // MCP caller supplied its code root plus the primary config path. Both
    // skip open_project's heavy side effects (migrations, transcript scrape,
    // registry upsert), so the code index stays decoupled (finding M1).
    let repo_root = fs::canonicalize(&source.repo_root)?;
    let db_path = validate_code_index_storage(&repo_root, true)?;
    let config = source.load_config()?;
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

    let mut conn = open_code_index(&db_path)?;
    // Take the writer slot before reading the prior index state. A deferred
    // transaction could let two cold refreshes read the same empty snapshot,
    // then fail one writer while it upgrades after the other commits.
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

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
        // A final symlink, missing/unreadable file, or an ancestor
        // symlink/junction escaping the checkout is skipped before removing
        // any prior row from `existing`; cleanup then drops that stale row.
        let source_file = match resolve_source_file(&repo_root, path) {
            Ok(file) => file,
            Err(_) => {
                summary.skipped_files += 1;
                continue;
            }
        };
        let abs = source_file.path;
        let meta = source_file.metadata;
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
    let db_path = validate_code_index_storage(&repo_root, false)?;
    let config = if paths.config_path.exists() {
        ProjectConfig::load(&paths.config_path)?
    } else {
        let repo_name = repo_root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("memhub");
        ProjectConfig::default_for_repo_name(repo_name)
    };
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
    let db_path = validate_code_index_storage(&paths.repo_root, false)?;
    let removed = db_path.exists();
    for suffix in ["", "-wal", "-shm"] {
        let candidate = sqlite_path(&db_path, suffix);
        if candidate.exists() {
            // A missing WAL/SHM is fine; only surface a real removal error.
            fs::remove_file(&candidate)?;
        }
    }
    Ok(RemoveOutcome { db_path, removed })
}

/// Embed every `code_chunks` row that has no current-model `code_embeddings`
/// row and persist the vectors. Reuses the shared BGE model ([`embed_batch`]).
///
/// An in-run cache keyed by the `embed_text` hash means a chunk whose text
/// matches one already embedded — duplicated code, or an unchanged sibling
/// whose vector survived this refresh — reuses that vector instead of
/// re-running the model. Cache misses are gathered into a single batched
/// inference. Returns the number of chunks given an embedding row.
fn embed_missing(conn: &mut Connection) -> Result<usize> {
    embed_missing_with(conn, |texts| embed_batch(texts))
}

fn embed_missing_with<F>(conn: &mut Connection, embed: F) -> Result<usize>
where
    F: FnOnce(&[&str]) -> Result<Vec<Vec<f32>>>,
{
    // Vectors from another model or dimension are neither searchable (locate
    // scans only the current model) nor reusable, so drop them: their chunks
    // then count as missing below, and the cache is seeded from current-model
    // rows only.
    // The probe is a read, so a warm refresh with nothing to drop takes no
    // write lock.
    let has_foreign: bool = conn.query_row(
        "SELECT EXISTS(SELECT 1 FROM code_embeddings WHERE model_name <> ?1 OR dimension <> ?2)",
        params![EMBEDDING_MODEL_NAME, EMBEDDING_DIMENSION as i64],
        |r| r.get(0),
    )?;
    if has_foreign {
        conn.execute(
            "DELETE FROM code_embeddings WHERE model_name <> ?1 OR dimension <> ?2",
            params![EMBEDDING_MODEL_NAME, EMBEDDING_DIMENSION as i64],
        )?;
    }
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
            if blob.len() == EMBEDDING_DIMENSION * 4 {
                cache.insert(hash, bytes_to_vector(&blob));
            }
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
        let vectors = embed(&batch_texts)?;
        if vectors.len() != batch_texts.len() {
            return Err(crate::MemhubError::Embedding(format!(
                "expected {} code embedding vector(s), got {}",
                batch_texts.len(),
                vectors.len()
            )));
        }
        if let Some(vector) = vectors
            .iter()
            .find(|vector| vector.len() != EMBEDDING_DIMENSION)
        {
            return Err(crate::MemhubError::Embedding(format!(
                "expected {EMBEDDING_DIMENSION}-dim code embedding, got {}",
                vector.len()
            )));
        }
        for (hash, idx) in &pending {
            cache.insert(hash.clone(), vectors[*idx].clone());
        }
    }

    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut inserted = 0;
    {
        let mut insert = tx.prepare(
            "INSERT INTO code_embeddings(chunk_id, model_name, dimension, vector, content_hash)
             SELECT ?1, ?2, ?3, ?4, ?5
             FROM code_chunks
             WHERE id = ?1 AND embed_text = ?6
             ON CONFLICT(chunk_id) DO NOTHING",
        )?;
        for ((chunk_id, text), hash) in missing.iter().zip(&hashes) {
            let vector = cache
                .get(hash)
                .expect("every missing chunk's vector is cached or freshly embedded");
            inserted += insert.execute(params![
                chunk_id,
                EMBEDDING_MODEL_NAME,
                EMBEDDING_DIMENSION as i64,
                vector_to_le_bytes(vector),
                hash,
                text,
            ])?;
        }
    }
    tx.commit()?;
    Ok(inserted)
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
/// hand-written code we want surfacing as a hit (e.g. `jquery.min.js`). The
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
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Arc, Barrier, mpsc};
    use std::thread;
    use std::time::Duration;
    use tempfile::tempdir;

    fn table_exists(conn: &Connection, name: &str) -> bool {
        conn.query_row(
            "SELECT 1 FROM sqlite_master WHERE type IN ('table','view') AND name = ?1",
            params![name],
            |row| row.get::<_, i64>(0),
        )
        .is_ok()
    }

    fn seed_chunks(path: &Path, chunks: &[(i64, &str)]) {
        let conn = open_code_index(path).expect("open");
        conn.execute(
            "INSERT INTO indexed_files(id, path, mtime, size, content_hash, language)
             VALUES (1, 'src/lib.rs', 0, 0, 'file', 'rust')",
            [],
        )
        .expect("seed file");
        for (id, text) in chunks {
            conn.execute(
                "INSERT INTO code_chunks(
                    id, file_id, start_line, end_line, symbol, kind, content_hash, embed_text
                 ) VALUES (?1, 1, ?1, ?1, NULL, 'function', ?2, ?3)",
                params![id, sha256_hex(text.as_bytes()), text],
            )
            .expect("seed chunk");
        }
    }

    fn test_vectors(texts: &[&str]) -> Result<Vec<Vec<f32>>> {
        Ok(texts
            .iter()
            .map(|text| {
                let mut vector = vec![0.0; EMBEDDING_DIMENSION];
                vector[0] = text.bytes().map(f32::from).sum();
                vector
            })
            .collect())
    }

    fn count_rows(path: &Path, table: &str) -> i64 {
        let conn = Connection::open(path).expect("open");
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .expect("count")
    }

    #[test]
    fn overlapping_embedding_writers_converge_without_duplicate_rows() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("code_index.sqlite");
        seed_chunks(
            &path,
            &[(1, "alpha text"), (2, "beta text"), (3, "gamma text")],
        );

        // Both writers must finish their missing-row reads before either is
        // allowed to persist. This reproduces the old duplicate-key window
        // without a timing-dependent sleep.
        let ready = Arc::new(Barrier::new(2));
        let mut writers = Vec::new();
        for _ in 0..2 {
            let path = path.clone();
            let ready = Arc::clone(&ready);
            writers.push(thread::spawn(move || {
                let mut conn = open_code_index(&path).expect("open writer");
                embed_missing_with(&mut conn, move |texts| {
                    ready.wait();
                    test_vectors(texts)
                })
            }));
        }

        let inserted: usize = writers
            .into_iter()
            .map(|writer| writer.join().expect("writer thread").expect("embed writer"))
            .sum();
        assert_eq!(inserted, 3, "only one row per chunk should be inserted");
        assert_eq!(count_rows(&path, "code_embeddings"), 3);

        let conn = Connection::open(&path).expect("open");
        let valid: i64 = conn
            .query_row(
                "SELECT COUNT(*)
                 FROM code_embeddings e
                 JOIN code_chunks c ON c.id = e.chunk_id
                 WHERE e.model_name = ?1
                   AND e.dimension = ?2
                   AND LENGTH(e.vector) = ?3",
                params![
                    EMBEDDING_MODEL_NAME,
                    EMBEDDING_DIMENSION as i64,
                    (EMBEDDING_DIMENSION * 4) as i64
                ],
                |row| row.get(0),
            )
            .expect("count valid");
        assert_eq!(valid, 3);
    }

    #[test]
    fn delayed_embeddings_skip_changed_reused_and_deleted_chunks() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("code_index.sqlite");
        seed_chunks(&path, &[(1, "old reused text"), (2, "old deleted text")]);

        let (selected_tx, selected_rx) = mpsc::channel();
        let (resume_tx, resume_rx) = mpsc::channel();
        let writer_path = path.clone();
        let writer = thread::spawn(move || {
            let mut conn = open_code_index(&writer_path).expect("open writer");
            embed_missing_with(&mut conn, move |texts| {
                selected_tx.send(texts.len()).expect("signal selection");
                resume_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("resume writer");
                test_vectors(texts)
            })
        });

        assert_eq!(
            selected_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("writer selected chunks"),
            2
        );

        // Replace id 1 with different text and remove id 2 while the old
        // texts are being "embedded". Explicit ids make row reuse certain.
        {
            let mut conn = open_code_index(&path).expect("open mutator");
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .expect("begin mutation");
            tx.execute("DELETE FROM code_chunks WHERE id IN (1, 2)", [])
                .expect("delete old chunks");
            tx.execute(
                "INSERT INTO code_chunks(
                    id, file_id, start_line, end_line, symbol, kind, content_hash, embed_text
                 ) VALUES (1, 1, 1, 1, NULL, 'function', 'new', 'new current text')",
                [],
            )
            .expect("reuse chunk id");
            tx.commit().expect("commit mutation");
        }
        resume_tx.send(()).expect("resume writer");

        assert_eq!(
            writer
                .join()
                .expect("writer thread")
                .expect("delayed embed"),
            0,
            "old work must not attach to changed or deleted rows"
        );
        assert_eq!(count_rows(&path, "code_embeddings"), 0);

        let mut conn = open_code_index(&path).expect("open retry");
        assert_eq!(
            embed_missing_with(&mut conn, test_vectors).expect("retry embed"),
            1
        );
        let (text, hash, vector, model, dimension): (String, String, Vec<u8>, String, i64) = conn
            .query_row(
                "SELECT c.embed_text, e.content_hash, e.vector, e.model_name, e.dimension
                 FROM code_chunks c
                 JOIN code_embeddings e ON e.chunk_id = c.id
                 WHERE c.id = 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .expect("current embedding");
        assert_eq!(text, "new current text");
        assert_eq!(hash, sha256_hex(text.as_bytes()));
        assert_eq!(
            vector,
            vector_to_le_bytes(&test_vectors(&[&text]).expect("vector")[0])
        );
        assert_eq!(model, EMBEDDING_MODEL_NAME);
        assert_eq!(dimension, EMBEDDING_DIMENSION as i64);
    }

    #[test]
    fn embedding_and_writer_failures_leave_committed_index_recoverable() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("code_index.sqlite");
        seed_chunks(
            &path,
            &[
                (1, "already embedded"),
                (2, "pending two"),
                (3, "pending three"),
            ],
        );
        {
            let conn = Connection::open(&path).expect("open");
            let text = "already embedded";
            conn.execute(
                "INSERT INTO code_embeddings(
                    chunk_id, model_name, dimension, vector, content_hash
                 ) VALUES (1, ?1, ?2, ?3, ?4)",
                params![
                    EMBEDDING_MODEL_NAME,
                    EMBEDDING_DIMENSION as i64,
                    vector_to_le_bytes(&test_vectors(&[text]).expect("vector")[0]),
                    sha256_hex(text.as_bytes())
                ],
            )
            .expect("seed embedding");
        }

        let mut conn = open_code_index(&path).expect("open");
        let err = embed_missing_with(&mut conn, |_| {
            Err(crate::MemhubError::Embedding(
                "injected inference failure".into(),
            ))
        })
        .expect_err("embedding must fail");
        assert!(err.to_string().contains("injected inference failure"));
        assert_eq!(count_rows(&path, "code_chunks"), 3);
        assert_eq!(
            count_rows(&path, "code_embeddings"),
            1,
            "already committed vector must survive inference failure"
        );

        conn.execute_batch(
            "CREATE TRIGGER fail_embedding_write
             BEFORE INSERT ON code_embeddings
             WHEN new.chunk_id = 3
             BEGIN SELECT RAISE(ABORT, 'injected writer failure'); END;",
        )
        .expect("install failure");
        embed_missing_with(&mut conn, test_vectors).expect_err("writer must fail");
        assert_eq!(
            count_rows(&path, "code_embeddings"),
            1,
            "the failed embedding transaction must roll back every pending row"
        );

        conn.execute_batch("DROP TRIGGER fail_embedding_write")
            .expect("remove failure");
        assert_eq!(
            embed_missing_with(&mut conn, test_vectors).expect("retry"),
            2
        );
        assert_eq!(count_rows(&path, "code_embeddings"), 3);
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
    fn journal_mode_busy_skips_sqlite_handler_but_narrow_retry_recovers() {
        let temp = tempdir().expect("tempdir");
        let path = temp.path().join("code_index.sqlite");
        let holder_path = path.clone();
        let (locked_tx, locked_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let (committed_tx, committed_rx) = mpsc::channel();

        let holder = thread::spawn(move || {
            let mut conn = Connection::open(holder_path).expect("open holder");
            conn.execute_batch("CREATE TABLE held_write(value INTEGER);")
                .expect("create holder table");
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .expect("begin held write");
            tx.execute("INSERT INTO held_write(value) VALUES (1)", [])
                .expect("write held row");
            locked_tx.send(()).expect("signal held write");
            release_rx
                .recv_timeout(Duration::from_secs(10))
                .expect("release held write");
            tx.commit().expect("commit held write");
            committed_tx.send(()).expect("signal commit");
        });
        locked_rx
            .recv_timeout(Duration::from_secs(10))
            .expect("writer acquired lock");

        // This is the cold-open failure from CI: changing a rollback-journal
        // database to WAL while another connection writes returns BUSY
        // without consulting even an installed busy handler.
        let busy_calls = Arc::new(AtomicUsize::new(0));
        let observed_calls = Arc::clone(&busy_calls);
        let mut probe = Connection::open(&path).expect("open probe");
        probe
            .busy_handler(Some(move |_| {
                observed_calls.fetch_add(1, Ordering::SeqCst);
                false
            }))
            .expect("install probe busy handler");
        let error = probe
            .execute_batch("PRAGMA journal_mode = WAL;")
            .expect_err("journal transition should be busy");
        assert!(is_database_busy(&error), "unexpected error: {error}");
        assert_eq!(
            busy_calls.load(Ordering::SeqCst),
            0,
            "SQLite bypassed the handler for this journal-mode transition"
        );
        drop(probe);

        let conn = Connection::open(&path).expect("open retry connection");
        conn.busy_timeout(Duration::ZERO)
            .expect("disable per-attempt timeout");
        let mut released = false;
        set_wal_journal_mode_with(&conn, |_| {
            if !released {
                release_tx.send(()).expect("release holder");
                committed_rx
                    .recv_timeout(Duration::from_secs(10))
                    .expect("holder committed");
                released = true;
            }
        })
        .expect("retry journal transition");
        conn.busy_timeout(CODE_INDEX_BUSY_TIMEOUT)
            .expect("restore busy timeout");
        assert!(released, "the first journal transition should be busy");
        let mode: String = conn
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .expect("query journal mode");
        assert_eq!(mode, "wal");

        schema::bootstrap(&conn).expect("bootstrap after journal transition");
        assert_eq!(
            schema::stored_version(&conn).expect("schema version"),
            Some(CODE_INDEX_SCHEMA_VERSION)
        );
        holder.join().expect("join holder");
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
        assert!(!is_indexable_source(
            "src/dashboard/static/vendor/uplot.min.js"
        ));
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
