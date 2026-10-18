//! Issue #392: the `missing_embeddings` recall warning counts the machine-
//! global store too when the repo is opted in and the store is searched, and
//! leads its fix text with `/reindex`.
//!
//! Every memhub call is a spawned child with `HOME` and `USERPROFILE` at a
//! tempdir (same pattern as `global_read_no_migrate.rs`).

use std::path::Path;
use std::process::Command;

use rusqlite::Connection;
use serde_json::Value;
use tempfile::{TempDir, tempdir};

fn memhub(cwd: &Path, home: &Path, args: &[&str]) -> String {
    let out = Command::new(env!("CARGO_BIN_EXE_memhub"))
        .args(args)
        .current_dir(cwd)
        .env("HOME", home)
        .env("USERPROFILE", home)
        .env("MEMHUB_LOG", "off")
        .output()
        .expect("spawn memhub");
    assert!(
        out.status.success(),
        "{args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).into_owned()
}

/// A hybrid repo (every row embedded) and a global store holding one fact
/// whose embedding was deleted. `opt_in` controls `global enable`.
fn fixture(opt_in: bool) -> (TempDir, TempDir) {
    let (home, repo) = (tempdir().expect("home"), tempdir().expect("repo"));
    let run = |args: &[&str]| memhub(repo.path(), home.path(), args);
    run(&["init"]);
    // The global store only exists once an opted-in repo has written to it.
    run(&["global", "enable"]);
    run(&["fact", "add", "repo widget", "widget lives in the repo"]);
    run(&[
        "fact",
        "add",
        "--global",
        "global widget",
        "widget lives in the global store",
    ]);
    Connection::open(home.path().join(".memhub").join("global.sqlite"))
        .expect("open global")
        .execute("DELETE FROM embeddings", [])
        .expect("drop global embeddings");
    if !opt_in {
        run(&["global", "disable"]);
    }
    (home, repo)
}

fn missing(home: &Path, repo: &Path) -> Option<Value> {
    let body: Value = serde_json::from_str(
        memhub(
            repo,
            home,
            &[
                "recall",
                "widget",
                "--mode",
                "hybrid",
                "--no-rerank",
                "--json",
            ],
        )
        .trim(),
    )
    .expect("recall json");
    body["warnings"]
        .as_array()
        .expect("warnings")
        .iter()
        .find(|w| w["kind"] == "missing_embeddings")
        .cloned()
}

fn repo_total(repo: &Path) -> u64 {
    let conn = Connection::open(repo.join(".memhub").join("project.sqlite")).expect("repo db");
    [
        "facts",
        "decisions",
        "tasks",
        "doc_chunks",
        "session_notes",
        "arch_sections",
    ]
    .iter()
    .map(|t| {
        conn.query_row(&format!("SELECT COUNT(*) FROM {t}"), [], |r| {
            r.get::<_, u64>(0)
        })
        .expect("count")
    })
    .sum()
}

#[test]
fn opted_in_repo_counts_the_global_store() {
    let (home, repo) = fixture(true);
    let w = missing(home.path(), repo.path()).expect("missing_embeddings warning");
    let total = repo_total(repo.path()) + 1;
    assert_eq!(w["stale_count"], 1, "{w}");
    assert_eq!(w["total_count"], total, "{w}");
    let reason = w["reason"].as_str().expect("reason");
    assert!(reason.contains("global store"), "{reason}");
    let fix = w["fix"].as_str().expect("fix");
    assert!(fix.starts_with("Run /reindex"), "{fix}");
    assert!(fix.contains("memhub index rebuild"), "{fix}");
}

#[test]
fn repo_not_opted_in_counts_its_own_store_only() {
    let (home, repo) = fixture(false);
    assert!(missing(home.path(), repo.path()).is_none());
}

#[test]
fn skipped_global_store_is_not_counted() {
    let (home, repo) = fixture(true);
    let conn = Connection::open(home.path().join(".memhub").join("global.sqlite")).expect("db");
    conn.execute(
        "DELETE FROM schema_migrations WHERE version = (SELECT MAX(version) FROM schema_migrations)",
        [],
    )
    .expect("make store older than the binary");
    drop(conn);
    assert!(missing(home.path(), repo.path()).is_none());
}
