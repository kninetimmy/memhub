//! Integration coverage for the M11 code-index refresh staleness engine
//! (task #66 review fixes). Drives a real git repo so `git ls-files` and
//! the lazy (mtime,size)+hash diff exercise the same path production does.

use std::fs;
use std::path::Path;
use std::process::Command;

use memhub::code_index::{self, code_index_db_path};
use memhub::commands::init;

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
