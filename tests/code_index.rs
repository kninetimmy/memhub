//! Integration coverage for the M11 code-index refresh staleness engine
//! (task #66 review fixes). Drives a real git repo so `git ls-files` and
//! the lazy (mtime,size)+hash diff exercise the same path production does.

use std::fs;
use std::path::Path;
use std::process::Command;

use memhub::code_index::{self, code_index_db_path};
use memhub::commands::init;
use memhub::config::{ProjectConfig, RetrievalMode};

/// Run a git subcommand in `repo`, failing the test on a non-zero status.
fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .expect("spawn git");
    assert!(status.success(), "git {args:?} failed");
}

/// Number of chunks indexed for a repo-relative path (forward-slashed).
fn chunks_for(db: &Path, rel: &str) -> i64 {
    let conn = rusqlite::Connection::open(db).expect("open code_index");
    conn.query_row(
        "SELECT COUNT(*) FROM code_chunks c
         JOIN indexed_files f ON f.id = c.file_id
         WHERE f.path = ?1",
        rusqlite::params![rel],
        |r| r.get(0),
    )
    .expect("count chunks")
}

fn file_indexed(db: &Path, rel: &str) -> bool {
    let conn = rusqlite::Connection::open(db).expect("open code_index");
    let n: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM indexed_files WHERE path = ?1",
            rusqlite::params![rel],
            |r| r.get(0),
        )
        .expect("count file");
    n > 0
}

fn count(db: &Path, sql: &str) -> i64 {
    let conn = rusqlite::Connection::open(db).expect("open code_index");
    conn.query_row(sql, [], |r| r.get(0)).expect("count query")
}

/// Switch the repo's retrieval mode to hybrid so a refresh embeds chunks.
fn set_hybrid(root: &Path) {
    let config_path = root.join(".memhub").join("config.toml");
    let mut config = ProjectConfig::load(&config_path).expect("load config");
    config.retrieval.mode = RetrievalMode::Hybrid;
    config.save(&config_path).expect("save config");
}

/// A repo with git initialized, memhub initialized, and `files` staged.
fn repo_with_files(files: &[(&str, &str)]) -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    git(root, &["init"]);
    for (rel, body) in files {
        let abs = root.join(rel);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(&abs, body).expect("write file");
    }
    init::run(root).expect("memhub init");
    git(root, &["add", "-A"]);
    temp
}

#[test]
fn first_refresh_indexes_tracked_files_and_chunks() {
    let temp = repo_with_files(&[("src/a.rs", "fn a() {}\n"), ("src/b.rs", "fn b() {}\n")]);
    let root = temp.path();

    let summary = code_index::refresh(root).expect("refresh");
    let db = code_index_db_path(root);

    assert!(summary.new_files >= 2, "both source files should be new");
    assert_eq!(summary.changed_files, 0);
    assert_eq!(summary.skipped_files, 0);
    // Clean pass: everything examined was indexed.
    assert_eq!(summary.new_files, summary.files_total);
    assert!(chunks_for(&db, "src/a.rs") >= 1);
    assert!(chunks_for(&db, "src/b.rs") >= 1);
}

#[test]
fn unchanged_files_take_the_fast_path_on_reindex() {
    let temp = repo_with_files(&[("src/a.rs", "fn a() {}\n")]);
    let root = temp.path();

    let first = code_index::refresh(root).expect("first refresh");
    let second = code_index::refresh(root).expect("second refresh");

    assert_eq!(second.new_files, 0);
    assert_eq!(second.changed_files, 0);
    assert_eq!(second.unchanged_files, first.files_total);
}

#[test]
fn edited_file_is_rechunked() {
    let temp = repo_with_files(&[("src/a.rs", "fn a() {}\n")]);
    let root = temp.path();
    code_index::refresh(root).expect("first refresh");

    // Rewrite with different content (and necessarily a new size).
    fs::write(root.join("src/a.rs"), "fn a() {}\nfn c() {}\n").expect("rewrite");
    let summary = code_index::refresh(root).expect("second refresh");
    assert_eq!(summary.changed_files, 1);
    assert!(chunks_for(&code_index_db_path(root), "src/a.rs") >= 1);
}

/// Finding P2 (HIGH): a file deleted from disk but whose deletion is NOT
/// staged is still listed by `git ls-files`. Its stale chunks must be
/// dropped, not retained.
#[test]
fn deleted_but_unstaged_file_drops_its_chunks() {
    let temp = repo_with_files(&[
        ("src/a.rs", "fn a() {}\n"),
        ("src/keep.rs", "fn keep() {}\n"),
    ]);
    let root = temp.path();
    let db = code_index_db_path(root);

    code_index::refresh(root).expect("first refresh");
    assert!(file_indexed(&db, "src/a.rs"));

    // Remove from the worktree only — do NOT `git rm`, so ls-files still
    // lists it as tracked.
    fs::remove_file(root.join("src/a.rs")).expect("delete file");

    let summary = code_index::refresh(root).expect("refresh after delete");
    assert!(
        summary.deleted_files >= 1,
        "the absent file's row should drop"
    );
    assert!(summary.skipped_files >= 1, "absent file is counted skipped");
    assert!(!file_indexed(&db, "src/a.rs"), "stale row must be gone");
    assert_eq!(chunks_for(&db, "src/a.rs"), 0, "stale chunks must be gone");
    // The untouched sibling stays indexed.
    assert!(file_indexed(&db, "src/keep.rs"));
}

/// Finding H1 (HIGH): an unreadable file must not abort the whole refresh,
/// and when the file already had a row that row must survive (don't wipe a
/// good entry over a transient read failure).
#[cfg(unix)]
#[test]
fn unreadable_file_is_skipped_and_prior_row_survives() {
    use std::os::unix::fs::PermissionsExt;

    let temp = repo_with_files(&[("src/a.rs", "fn a() {}\n"), ("src/ok.rs", "fn ok() {}\n")]);
    let root = temp.path();
    let db = code_index_db_path(root);
    code_index::refresh(root).expect("first refresh");

    // Change content (so the fast path is skipped) then make it unreadable.
    let a = root.join("src/a.rs");
    fs::write(&a, "fn a() {}\nfn changed() {}\n").expect("rewrite");
    fs::set_permissions(&a, fs::Permissions::from_mode(0o000)).expect("chmod 000");

    let summary = code_index::refresh(root).expect("refresh must not abort");
    assert!(
        summary.skipped_files >= 1,
        "unreadable file counted skipped"
    );
    // Prior row + chunks for the unreadable file are preserved.
    assert!(
        file_indexed(&db, "src/a.rs"),
        "prior row must survive a read blip"
    );
    assert!(chunks_for(&db, "src/a.rs") >= 1);
    // A readable sibling is still indexed in the same pass.
    assert!(file_indexed(&db, "src/ok.rs"));

    // Restore perms so the tempdir cleans up.
    fs::set_permissions(&a, fs::Permissions::from_mode(0o644)).expect("restore perms");
}

/// PR2: a Rust file is chunked by symbol (tree-sitter), not line windows —
/// each top-level item becomes a kind-tagged, symbol-named chunk.
#[test]
fn rust_file_is_chunked_by_symbol() {
    let temp = repo_with_files(&[(
        "src/widget.rs",
        "/// Build a widget.\npub fn build_widget() -> u32 { 42 }\nstruct Widget { id: u32 }\n",
    )]);
    let root = temp.path();
    code_index::refresh(root).expect("refresh");

    let conn = rusqlite::Connection::open(code_index_db_path(root)).expect("open");
    let mut stmt = conn
        .prepare("SELECT kind, symbol FROM code_chunks ORDER BY symbol")
        .expect("prepare");
    let rows: Vec<(String, Option<String>)> = stmt
        .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();

    assert!(
        rows.contains(&("function".into(), Some("build_widget".into()))),
        "{rows:?}"
    );
    assert!(
        rows.contains(&("struct".into(), Some("Widget".into()))),
        "{rows:?}"
    );
}

/// PR2: chunk writes keep the contentless code_chunks_fts in step (via the
/// schema triggers), and a symbol name is keyword-searchable. This holds in
/// fts mode — FTS does not depend on embedding.
#[test]
fn refresh_populates_fts_and_finds_symbol() {
    let temp = repo_with_files(&[("src/a.rs", "pub fn parse_manifest() -> bool { true }\n")]);
    let root = temp.path();
    code_index::refresh(root).expect("refresh");
    let db = code_index_db_path(root);

    // Every chunk has a matching FTS row.
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM code_chunks_fts"),
        count(&db, "SELECT COUNT(*) FROM code_chunks"),
        "FTS row count must track chunk count"
    );

    let conn = rusqlite::Connection::open(&db).expect("open");
    let hits: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM code_chunks_fts WHERE code_chunks_fts MATCH ?1",
            rusqlite::params!["parse_manifest"],
            |r| r.get(0),
        )
        .expect("fts match");
    assert!(hits >= 1, "symbol name should be FTS-searchable");
}

/// PR2: a re-chunked file's stale FTS rows are removed (the delete trigger
/// fires on the cascade), so the FTS index never drifts past the chunks.
#[test]
fn fts_rows_track_rechunk_and_delete() {
    let temp = repo_with_files(&[("src/a.rs", "fn old_name() {}\n")]);
    let root = temp.path();
    let db = code_index_db_path(root);
    code_index::refresh(root).expect("first refresh");

    fs::write(root.join("src/a.rs"), "fn new_name() {}\n").expect("rewrite");
    code_index::refresh(root).expect("second refresh");

    let conn = rusqlite::Connection::open(&db).expect("open");
    let old_hits: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM code_chunks_fts WHERE code_chunks_fts MATCH ?1",
            rusqlite::params!["old_name"],
            |r| r.get(0),
        )
        .expect("fts match old");
    assert_eq!(old_hits, 0, "stale FTS row for old_name must be gone");
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM code_chunks_fts"),
        count(&db, "SELECT COUNT(*) FROM code_chunks"),
    );
}

/// PR2: in hybrid mode a refresh embeds every chunk into code_embeddings;
/// a no-op second refresh embeds nothing more. In fts mode the table stays
/// empty.
#[test]
fn hybrid_mode_embeds_every_chunk() {
    let temp = repo_with_files(&[("src/a.rs", "pub fn alpha() {}\npub fn beta() {}\n")]);
    let root = temp.path();
    let db = code_index_db_path(root);

    // Default (fts) mode: no embeddings.
    code_index::refresh(root).expect("fts refresh");
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM code_embeddings"),
        0,
        "fts mode must not populate embeddings"
    );

    // Hybrid mode: every chunk gets a vector.
    set_hybrid(root);
    let summary = code_index::refresh(root).expect("hybrid refresh");
    let chunks = count(&db, "SELECT COUNT(*) FROM code_chunks");
    assert!(chunks >= 2, "two functions => at least two chunks");
    assert_eq!(
        count(&db, "SELECT COUNT(*) FROM code_embeddings"),
        chunks,
        "every chunk should have an embedding"
    );
    assert_eq!(summary.embedded_chunks as i64, chunks);

    // Stored vectors are the expected dimension (384 f32 => 1536 bytes).
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM code_embeddings WHERE LENGTH(vector) = 1536"
        ),
        chunks,
    );

    // A second refresh with no changes re-embeds nothing.
    let again = code_index::refresh(root).expect("third refresh");
    assert_eq!(again.embedded_chunks, 0, "unchanged tree re-embeds nothing");
}

/// Finding M3: a tracked symlink must be skipped, never followed/indexed
/// (it could read outside the repo).
#[cfg(unix)]
#[test]
fn tracked_symlink_is_skipped_not_indexed() {
    let temp = repo_with_files(&[("src/a.rs", "fn a() {}\n")]);
    let root = temp.path();
    let db = code_index_db_path(root);

    std::os::unix::fs::symlink("/etc/hosts", root.join("link.rs")).expect("symlink");
    git(root, &["add", "-A"]);

    let summary = code_index::refresh(root).expect("refresh");
    assert!(summary.skipped_files >= 1, "symlink counted skipped");
    assert!(!file_indexed(&db, "link.rs"), "symlink must not be indexed");
    assert!(file_indexed(&db, "src/a.rs"));
}
