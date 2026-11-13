//! Integration coverage for the M11 code-index refresh staleness engine
//! (task #66 review fixes). Drives a real git repo so `git ls-files` and
//! the lazy (mtime,size)+hash diff exercise the same path production does.

use std::fs;
use std::path::Path;
use std::process::{Command, Output, Stdio};

use memhub::code_index::{self, code_index_db_path};
use memhub::commands::init;
use memhub::config::{ProjectConfig, RetrievalMode};
use memhub::retrieval::embeddings::{EMBEDDING_DIMENSION, EMBEDDING_MODEL_NAME};
use sha2::{Digest, Sha256};

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

fn memhub_command(root: &Path, home: &Path, args: &[&str]) -> Command {
    let mut command = Command::new(env!("CARGO_BIN_EXE_memhub"));
    command
        .args(args)
        .current_dir(root)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("MEMHUB_LOG", "off");
    command
}

fn assert_success(output: Output, label: &str) -> Output {
    assert!(
        output.status.success(),
        "{label} failed ({}):\nstdout: {}\nstderr: {}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
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
    // Indexing tests pin fts (the default before issue #382); the hybrid
    // ones switch with `set_hybrid`.
    let config_path = root.join(".memhub").join("config.toml");
    let mut config = ProjectConfig::load(&config_path).expect("load config");
    config.retrieval.mode = RetrievalMode::Fts;
    config.save(&config_path).expect("save config");
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

    // Pinned fts mode: no embeddings.
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

    // Stored vectors are the bundled model's, at its dimension (f32 => 4 bytes).
    assert_eq!(
        count(
            &db,
            &format!(
                "SELECT COUNT(*) FROM code_embeddings WHERE model_name = '{EMBEDDING_MODEL_NAME}' AND dimension = {EMBEDDING_DIMENSION} AND LENGTH(vector) = {}",
                EMBEDDING_DIMENSION * 4
            )
        ),
        chunks,
    );

    // A second refresh with no changes re-embeds nothing.
    let again = code_index::refresh(root).expect("third refresh");
    assert_eq!(again.embedded_chunks, 0, "unchanged tree re-embeds nothing");
}

#[test]
fn concurrent_cli_refreshes_create_and_backfill_one_shared_index() {
    let repo = tempfile::tempdir().expect("repo");
    let home = tempfile::tempdir().expect("home");
    let root = repo.path();
    git(root, &["init"]);

    let mut source = String::new();
    for index in 0..120 {
        source.push_str(&format!(
            "pub fn shared_lookup_{index:03}() -> usize {{ {index} }}\n"
        ));
    }
    fs::create_dir_all(root.join("src")).expect("mkdir");
    fs::write(root.join("src/lib.rs"), source).expect("write source");
    assert_success(
        memhub_command(root, home.path(), &["init"])
            .output()
            .expect("run init"),
        "init",
    );
    let config_path = root.join(".memhub").join("config.toml");
    let mut config = ProjectConfig::load(&config_path).expect("load config");
    config.retrieval.mode = RetrievalMode::Fts;
    config.save(&config_path).expect("save fts config");
    git(root, &["add", "-A"]);

    let project_db = root.join(".memhub").join("project.sqlite");
    let project_before = fs::read(&project_db).expect("read project db");

    // Start together against no code DB. The immediate refresh transaction
    // makes the second process read the first process's committed index
    // instead of trying to insert the same file rows from a stale snapshot.
    let cold_one = memhub_command(root, home.path(), &["code", "index", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cold writer one");
    let cold_two = memhub_command(root, home.path(), &["code", "index", "--json"])
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn cold writer two");
    assert_success(
        cold_one.wait_with_output().expect("wait cold one"),
        "cold one",
    );
    assert_success(
        cold_two.wait_with_output().expect("wait cold two"),
        "cold two",
    );

    let db = code_index_db_path(root);
    let chunks = count(&db, "SELECT COUNT(*) FROM code_chunks");
    assert!(
        chunks >= 120,
        "fixture should produce one chunk per function"
    );
    assert_eq!(count(&db, "SELECT COUNT(*) FROM code_chunks_fts"), chunks);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM code_embeddings"), 0);

    // Reproduce issue #435: leave the shared chunks/FTS index in place,
    // switch to hybrid, then make two CLI locate processes backfill the same
    // missing vectors concurrently.
    let mut config = ProjectConfig::load(&config_path).expect("load config");
    config.retrieval.mode = RetrievalMode::Hybrid;
    config.save(&config_path).expect("save hybrid config");
    let locate_one = memhub_command(
        root,
        home.path(),
        &["locate", "shared lookup", "--limit", "5", "--json"],
    )
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("spawn locate one");
    let locate_two = memhub_command(
        root,
        home.path(),
        &["locate", "shared lookup", "--limit", "5", "--json"],
    )
    .stdout(Stdio::piped())
    .stderr(Stdio::piped())
    .spawn()
    .expect("spawn locate two");
    let first = assert_success(
        locate_one.wait_with_output().expect("wait locate one"),
        "locate one",
    );
    let second = assert_success(
        locate_two.wait_with_output().expect("wait locate two"),
        "locate two",
    );
    for output in [first, second] {
        let payload: serde_json::Value =
            serde_json::from_slice(&output.stdout).expect("locate JSON");
        assert!(
            payload["returned_count"]
                .as_u64()
                .is_some_and(|count| count > 0),
            "locate should return shared_lookup hits: {payload}"
        );
    }

    let conn = rusqlite::Connection::open(&db).expect("open code index");
    let embeddings: i64 = conn
        .query_row("SELECT COUNT(*) FROM code_embeddings", [], |row| row.get(0))
        .expect("count embeddings");
    assert_eq!(embeddings, chunks, "exactly one embedding per chunk");
    let mut stmt = conn
        .prepare(
            "SELECT c.embed_text, e.content_hash, e.model_name, e.dimension, LENGTH(e.vector)
             FROM code_chunks c
             JOIN code_embeddings e ON e.chunk_id = c.id",
        )
        .expect("prepare embeddings");
    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, i64>(4)?,
            ))
        })
        .expect("query embeddings");
    for row in rows {
        let (text, hash, model, dimension, bytes) = row.expect("embedding row");
        assert_eq!(hash, format!("{:x}", Sha256::digest(text.as_bytes())));
        assert_eq!(model, EMBEDDING_MODEL_NAME);
        assert_eq!(dimension, EMBEDDING_DIMENSION as i64);
        assert_eq!(bytes, (EMBEDDING_DIMENSION * 4) as i64);
    }

    assert_eq!(
        fs::read(&project_db).expect("reread project db"),
        project_before,
        "code-cache refresh must not write durable project memory"
    );
}

/// A warm refresh with no vectors from another model must not delete any
/// code embedding row: a trigger aborts every DELETE on the table.
#[test]
fn warm_refresh_deletes_no_embedding_rows_without_foreign_vectors() {
    let temp = repo_with_files(&[(
        "src/a.rs",
        "pub fn alpha() {}
pub fn beta() {}
",
    )]);
    let root = temp.path();
    let db = code_index_db_path(root);
    set_hybrid(root);
    code_index::refresh(root).expect("first refresh");
    let rows = count(&db, "SELECT COUNT(*) FROM code_embeddings");
    assert!(rows >= 2);

    rusqlite::Connection::open(&db)
        .expect("open code_index")
        .execute_batch(
            "CREATE TRIGGER no_embedding_deletes BEFORE DELETE ON code_embeddings
             BEGIN SELECT RAISE(ABORT, 'code_embeddings delete attempted'); END;",
        )
        .expect("install trigger");

    let again = code_index::refresh(root).expect("warm refresh must not delete");
    assert_eq!(again.embedded_chunks, 0);
    assert_eq!(count(&db, "SELECT COUNT(*) FROM code_embeddings"), rows);
}

/// A chunk whose only vector comes from another model or dimension is
/// re-embedded with the bundled model on the next refresh, the foreign
/// vector is removed, and it is never reused for the new row.
#[test]
fn refresh_replaces_vectors_from_another_model() {
    let temp = repo_with_files(&[(
        "src/a.rs",
        "pub fn alpha() {}
pub fn beta() {}
",
    )]);
    let root = temp.path();
    let db = code_index_db_path(root);
    set_hybrid(root);
    code_index::refresh(root).expect("first refresh");
    let chunks = count(&db, "SELECT COUNT(*) FROM code_chunks");
    assert!(chunks >= 2);

    // Re-tag every vector as another model at another dimension.
    rusqlite::Connection::open(&db)
        .expect("open code_index")
        .execute(
            "UPDATE code_embeddings SET model_name = 'other-model', dimension = 8, vector = zeroblob(32)",
            [],
        )
        .expect("retag vectors");

    let summary = code_index::refresh(root).expect("refresh after model change");
    assert_eq!(summary.embedded_chunks as i64, chunks);
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM code_embeddings WHERE model_name <> 'other-model'"
        ),
        chunks,
        "every chunk now has a bundled-model vector"
    );
    assert_eq!(
        count(
            &db,
            "SELECT COUNT(*) FROM code_embeddings WHERE model_name = 'other-model' OR dimension <> LENGTH(vector) / 4"
        ),
        0,
        "no foreign or wrong-length vector may survive or be reused"
    );
    assert_eq!(
        count(
            &db,
            &format!(
                "SELECT COUNT(*) FROM code_embeddings WHERE model_name = '{EMBEDDING_MODEL_NAME}' AND dimension = {EMBEDDING_DIMENSION}"
            )
        ),
        chunks
    );
}

/// M11 review M1: nested enum/delegate inside a class must be emitted as
/// `ClassName::MemberName`, not just `MemberName`.
#[test]
fn csharp_nested_enum_gets_qualified_symbol() {
    let src = "class Outer { enum Status { Active, Inactive } }\n";
    let temp = repo_with_files(&[("src/Widget.cs", src)]);
    let root = temp.path();
    code_index::refresh(root).expect("refresh");

    let conn = rusqlite::Connection::open(code_index_db_path(root)).expect("open");
    let mut stmt = conn
        .prepare("SELECT symbol FROM code_chunks WHERE kind = 'enum'")
        .expect("prepare");
    let symbols: Vec<Option<String>> = stmt
        .query_map([], |r| r.get(0))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();

    assert!(
        symbols.contains(&Some("Outer::Status".into())),
        "nested enum should be qualified as Outer::Status, got: {symbols:?}"
    );
}

#[test]
fn java_nested_enum_gets_qualified_symbol() {
    let src = "class Outer { enum Status { ACTIVE, INACTIVE } }\n";
    let temp = repo_with_files(&[("src/Widget.java", src)]);
    let root = temp.path();
    code_index::refresh(root).expect("refresh");

    let conn = rusqlite::Connection::open(code_index_db_path(root)).expect("open");
    let mut stmt = conn
        .prepare("SELECT symbol FROM code_chunks WHERE kind = 'enum'")
        .expect("prepare");
    let symbols: Vec<Option<String>> = stmt
        .query_map([], |r| r.get(0))
        .expect("query")
        .map(|r| r.expect("row"))
        .collect();

    assert!(
        symbols.contains(&Some("Outer::Status".into())),
        "nested enum should be qualified as Outer::Status, got: {symbols:?}"
    );
}

/// Task 69: the index is scoped to grammar-known source files. Tracked
/// non-source files (docs, lockfiles, JSON) and vendored/minified bundles
/// must be excluded so they can't out-rank real code in `locate`.
#[test]
fn non_source_files_are_excluded_from_index() {
    let temp = repo_with_files(&[
        ("src/a.rs", "fn a() {}\n"),
        ("README.md", "# Project\n\nlots of prose here\n"),
        ("Cargo.lock", "# autogenerated\n[[package]]\nname = \"x\"\n"),
        ("data.json", "{\"k\": \"v\"}\n"),
        (
            "src/dashboard/static/vendor/uplot.min.js",
            "!function(){var a=1;return a}();\n",
        ),
    ]);
    let root = temp.path();
    let db = code_index_db_path(root);

    let summary = code_index::refresh(root).expect("refresh");

    // Only the real source file is indexed.
    assert!(file_indexed(&db, "src/a.rs"));
    assert!(chunks_for(&db, "src/a.rs") >= 1);
    for excluded in [
        "README.md",
        "Cargo.lock",
        "data.json",
        "src/dashboard/static/vendor/uplot.min.js",
    ] {
        assert!(!file_indexed(&db, excluded), "{excluded} must be excluded");
    }
    assert_eq!(summary.files_total, 1, "only src/a.rs is indexable");
    assert!(
        summary.excluded_files >= 4,
        "the four non-source files are counted excluded, got {}",
        summary.excluded_files
    );
}

/// Task 69: a file that was indexed before the source-scoping (e.g. a
/// previously line-windowed doc) is auto-pruned on the next refresh — it
/// drops out of the tracked set and the cleanup pass removes its row.
#[test]
fn previously_indexed_non_source_is_pruned_on_reindex() {
    let temp = repo_with_files(&[("src/a.rs", "fn a() {}\n"), ("NOTES.md", "# notes\n")]);
    let root = temp.path();
    let db = code_index_db_path(root);

    // A first refresh bootstraps the schema (NOTES.md is already excluded).
    // Plant a stale doc row directly to simulate a pre-task-69 index that
    // line-windowed the doc, then confirm the next refresh drops it.
    code_index::refresh(root).expect("first refresh");
    {
        let conn = rusqlite::Connection::open(&db).expect("open");
        conn.execute(
            "INSERT INTO indexed_files(path, mtime, size, content_hash, language)
             VALUES ('NOTES.md', 1, 2, 'deadbeef', 'markdown')",
            [],
        )
        .expect("plant stale doc row");
    }
    assert!(file_indexed(&db, "NOTES.md"), "stale row planted");

    let summary = code_index::refresh(root).expect("second refresh");
    assert!(
        !file_indexed(&db, "NOTES.md"),
        "stale doc row must be pruned"
    );
    assert!(summary.deleted_files >= 1, "the doc row is dropped");
    assert!(file_indexed(&db, "src/a.rs"), "real source stays indexed");
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

/// A regular tracked path whose ancestor is replaced by a directory
/// symlink/junction must not let the walker read outside the checkout.
#[cfg(any(unix, windows))]
#[test]
fn tracked_path_beneath_directory_link_cannot_escape_checkout() {
    let temp = repo_with_files(&[("src/escape.rs", "fn original_inside() {}\n")]);
    let root = temp.path();
    let db = code_index_db_path(root);
    let outside = tempfile::tempdir().expect("outside");
    fs::write(
        outside.path().join("escape.rs"),
        "fn outside_secret_marker() {}\n",
    )
    .unwrap();
    fs::remove_dir_all(root.join("src")).unwrap();

    #[cfg(unix)]
    std::os::unix::fs::symlink(outside.path(), root.join("src")).expect("directory symlink");

    #[cfg(windows)]
    {
        let output = Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(root.join("src"))
            .arg(outside.path())
            .output()
            .expect("create junction");
        assert!(
            output.status.success(),
            "mklink failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let summary = code_index::refresh(root).expect("refresh");
    assert_eq!(summary.skipped_files, 1);
    assert!(
        !file_indexed(&db, "src/escape.rs"),
        "escaped bytes must never enter the index"
    );
}
