use std::fs;
use std::path::Path;
use std::process::Command;
use std::thread;
use std::time::Duration;

use memhub::commands::{decision, ingest_git, init, search, status};
use memhub::db;
use memhub::models::SearchResult;
use rusqlite::{Connection, params};
use tempfile::tempdir;

#[test]
fn ingest_git_populates_history_and_exact_file_search() {
    let temp = tempdir().expect("tempdir");
    init_git_repo(temp.path());
    init::run(temp.path()).expect("init succeeds");

    write_file(
        temp.path(),
        "src/lib.rs",
        "pub fn version() -> &'static str { \"v1\" }\n",
    );
    git(temp.path(), ["add", "src/lib.rs"]);
    git(temp.path(), ["commit", "-m", "add library"]);

    thread::sleep(Duration::from_secs(1));
    write_file(
        temp.path(),
        "src/lib.rs",
        "pub fn version() -> &'static str { \"v2\" }\n",
    );
    git(temp.path(), ["add", "src/lib.rs"]);
    git(temp.path(), ["commit", "-m", "update library"]);

    let summary = ingest_git::run(temp.path(), None).expect("git ingest");
    let project = status::run(temp.path()).expect("status");
    let search_response = search::run(temp.path(), "src/lib.rs", 10).expect("search");

    assert_eq!(summary.commits_seen, 2);
    assert_eq!(summary.unique_files_seen, 1);
    assert_eq!(summary.commit_file_links_seen, 2);
    assert_eq!(project.commits, 2);
    assert_eq!(project.files, 1);
    assert_eq!(search_response.matcher, "exact:file-history");
    assert_eq!(search_response.results.len(), 2);

    match &search_response.results[0] {
        SearchResult::FileHistory(hit) => {
            assert_eq!(hit.path, "src/lib.rs");
            assert_eq!(hit.change_type, "M");
            assert_eq!(hit.message, "update library");
        }
        other => panic!("unexpected search result: {other:?}"),
    }

    match &search_response.results[1] {
        SearchResult::FileHistory(hit) => {
            assert_eq!(hit.change_type, "A");
            assert_eq!(hit.message, "add library");
        }
        other => panic!("unexpected search result: {other:?}"),
    }
}

#[test]
fn search_indexes_decision_rationales_via_fts() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init succeeds");

    decision::add(
        temp.path(),
        "Use bundled rusqlite mode",
        "Avoid SQLite setup friction on Windows and keep onboarding local-first.",
    )
    .expect("decision add");

    let response =
        search::run(temp.path(), "decisions about Windows onboarding", 10).expect("search");
    let project = status::run(temp.path()).expect("status");

    assert_eq!(response.matcher, "fts:decision");
    assert_eq!(project.chunks, 1);
    assert_eq!(response.results.len(), 1);

    match &response.results[0] {
        SearchResult::Decision(hit) => {
            assert_eq!(hit.title, "Use bundled rusqlite mode");
            assert!(hit.rationale.contains("Windows"));
        }
        other => panic!("unexpected search result: {other:?}"),
    }
}

#[test]
fn milestone_two_queries_avoid_full_scans() {
    let temp = tempdir().expect("tempdir");
    init_git_repo(temp.path());
    init::run(temp.path()).expect("init succeeds");

    write_file(
        temp.path(),
        "src/lib.rs",
        "pub fn version() -> &'static str { \"v1\" }\n",
    );
    git(temp.path(), ["add", "src/lib.rs"]);
    git(temp.path(), ["commit", "-m", "add library"]);
    ingest_git::run(temp.path(), None).expect("git ingest");

    decision::add(
        temp.path(),
        "Use SQLite FTS5",
        "Built-in full-text search keeps queries indexed and local.",
    )
    .expect("decision add");

    let ctx = db::open_project(temp.path()).expect("open project");

    let file_plan = explain_query_plan(
        &ctx.conn,
        "SELECT
             f.path,
             c.sha,
             c.author,
             c.committed_at,
             c.message,
             cf.change_type
         FROM files f
         JOIN commit_files cf ON cf.file_id = f.id
         JOIN commits c ON c.sha = cf.commit_sha
         WHERE f.project_id = 1 AND f.path = ?1
         ORDER BY c.committed_at DESC
         LIMIT ?2",
        params!["src/lib.rs", 10_i64],
    );

    let decision_plan = explain_query_plan(
        &ctx.conn,
        "SELECT
             d.id,
             d.title,
             d.rationale,
             d.decided_at,
             bm25(chunk_fts) AS score
         FROM chunk_fts
         JOIN chunks ch ON ch.id = chunk_fts.rowid
         JOIN decisions d
             ON d.id = CAST(ch.source_id AS INTEGER)
            AND d.project_id = ch.project_id
         WHERE chunk_fts MATCH ?1
           AND ch.source_type = 'decision'
         ORDER BY score ASC, d.decided_at DESC
         LIMIT ?2",
        params!["\"indexed\"", 10_i64],
    );

    assert_no_unbounded_scans(&file_plan, &[]);
    assert_no_unbounded_scans(&decision_plan, &["SCAN chunk_fts VIRTUAL TABLE INDEX"]);
}

fn init_git_repo(repo_root: &Path) {
    git(repo_root, ["init"]);
    git(repo_root, ["config", "user.name", "Memhub Test"]);
    git(repo_root, ["config", "user.email", "memhub@example.com"]);
}

fn write_file(repo_root: &Path, relative_path: &str, contents: &str) {
    let path = repo_root.join(relative_path);
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent).expect("create parent dirs");
    }
    fs::write(path, contents).expect("write file");
}

fn git<const N: usize>(repo_root: &Path, args: [&str; N]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo_root)
        .output()
        .expect("run git");

    assert!(
        output.status.success(),
        "git {:?} failed: {}",
        args,
        String::from_utf8_lossy(&output.stderr)
    );
}

fn explain_query_plan<P>(conn: &Connection, sql: &str, params: P) -> Vec<String>
where
    P: rusqlite::Params,
{
    let explain = format!("EXPLAIN QUERY PLAN {sql}");
    let mut stmt = conn.prepare(&explain).expect("prepare explain");
    let rows = stmt
        .query_map(params, |row| row.get::<_, String>(3))
        .expect("query plan");

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .expect("collect query plan")
}

fn assert_no_unbounded_scans(plan: &[String], allowed_prefixes: &[&str]) {
    for detail in plan {
        if detail.contains("SCAN ")
            && !allowed_prefixes
                .iter()
                .any(|allowed| detail.starts_with(allowed))
        {
            panic!("unexpected full scan in query plan: {detail} ({plan:?})");
        }
    }
}
