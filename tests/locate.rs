//! Integration coverage for the M11 PR3 locate query path (task #63).
//! Drives a real git repo so the lazy refresh + FTS/vector fusion run the
//! same path production does.

use std::fs;
use std::path::Path;
use std::process::Command;

use memhub::code_index::locate::{LocateOptions, locate};
use memhub::code_index::{self, code_index_db_path};
use memhub::commands::init;
use memhub::config::{ProjectConfig, RetrievalMode};

fn git(repo: &Path, args: &[&str]) {
    let status = Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(args)
        .status()
        .expect("spawn git");
    assert!(status.success(), "git {args:?} failed");
}

fn set_hybrid(root: &Path) {
    let config_path = root.join(".memhub").join("config.toml");
    let mut config = ProjectConfig::load(&config_path).expect("load config");
    config.retrieval.mode = RetrievalMode::Hybrid;
    config.save(&config_path).expect("save config");
}

/// A repo with git + memhub initialized and `files` staged.
fn repo_with_files(files: &[(&str, &str)]) -> tempfile::TempDir {
    let temp = tempfile::tempdir().expect("tempdir");
    let root = temp.path();
    git(root, &["init"]);
    // A committed HEAD so current_head resolves for the staleness check.
    for (rel, body) in files {
        let abs = root.join(rel);
        if let Some(parent) = abs.parent() {
            fs::create_dir_all(parent).expect("mkdir");
        }
        fs::write(&abs, body).expect("write file");
    }
    init::run(root).expect("memhub init");
    git(root, &["add", "-A"]);
    git(
        root,
        &[
            "-c",
            "user.email=t@t",
            "-c",
            "user.name=t",
            "commit",
            "-m",
            "init",
        ],
    );
    temp
}

#[test]
fn locate_finds_symbol_by_name_fts() {
    let temp = repo_with_files(&[
        (
            "src/parser.rs",
            "pub fn parse_manifest() -> bool { true }\n",
        ),
        ("src/render.rs", "pub fn draw_widget() {}\n"),
    ]);
    let root = temp.path();

    let response = locate(
        root,
        LocateOptions {
            query: "parse manifest".to_string(),
            limit: 5,
            use_reranker: false,
            no_refresh: false,
        },
    )
    .expect("locate");

    assert_eq!(response.mode, RetrievalMode::Fts, "default config is fts");
    assert!(!response.results.is_empty(), "should find the symbol");
    let top = &response.results[0];
    assert_eq!(top.path, "src/parser.rs");
    assert_eq!(top.symbol.as_deref(), Some("parse_manifest"));
    assert_eq!(top.kind, "function");
    assert!(top.snippet.contains("parse_manifest"), "snippet has body");
    assert!(top.start_line >= 1);
}

#[test]
fn locate_auto_refreshes_a_never_indexed_repo() {
    let temp = repo_with_files(&[("src/a.rs", "pub fn alpha() {}\n")]);
    let root = temp.path();

    // No prior `code index` call: the sibling DB does not exist yet.
    assert!(!code_index_db_path(root).exists());

    let response = locate(
        root,
        LocateOptions {
            query: "alpha".to_string(),
            limit: 5,
            use_reranker: false,
            no_refresh: false,
        },
    )
    .expect("locate");

    // The lazy refresh built the index on the fly.
    assert!(code_index_db_path(root).exists());
    assert!(response.chunks_total >= 1);
    assert!(response.results.iter().any(|h| h.path == "src/a.rs"));
}

#[test]
fn locate_picks_up_an_edit_via_lazy_refresh() {
    let temp = repo_with_files(&[("src/a.rs", "pub fn original_name() {}\n")]);
    let root = temp.path();

    // Warm the index, then edit on disk without re-indexing manually.
    code_index::refresh(root).expect("warm");
    fs::write(root.join("src/a.rs"), "pub fn renamed_symbol() {}\n").expect("rewrite");

    let response = locate(
        root,
        LocateOptions {
            query: "renamed symbol".to_string(),
            limit: 5,
            use_reranker: false,
            no_refresh: false,
        },
    )
    .expect("locate");

    assert!(
        response
            .results
            .iter()
            .any(|h| h.symbol.as_deref() == Some("renamed_symbol")),
        "lazy refresh should surface the edited symbol: {:?}",
        response.results
    );
    let old = locate(
        root,
        LocateOptions {
            query: "original_name".to_string(),
            limit: 5,
            use_reranker: false,
            no_refresh: false,
        },
    )
    .expect("locate old");
    assert!(
        old.results
            .iter()
            .all(|h| h.symbol.as_deref() != Some("original_name")),
        "stale symbol must be gone"
    );
}

/// Issue #67: `--no-refresh` is the mirror image of the test above — same
/// warm-then-edit setup, but with the flag set the edit must NOT surface
/// and the stale symbol must still be served. `files_total`/`chunks_total`/
/// `head` must come straight off the index (matching the prior warm
/// refresh), not a freshly recomputed count.
#[test]
fn locate_no_refresh_skips_the_freshness_pass() {
    let temp = repo_with_files(&[("src/a.rs", "pub fn original_name() {}\n")]);
    let root = temp.path();

    // Warm the index, then edit on disk without re-indexing manually.
    let warm = code_index::refresh(root).expect("warm");
    fs::write(root.join("src/a.rs"), "pub fn renamed_symbol() {}\n").expect("rewrite");

    let response = locate(
        root,
        LocateOptions {
            query: "renamed symbol".to_string(),
            limit: 5,
            use_reranker: false,
            no_refresh: true,
        },
    )
    .expect("locate");
    assert!(
        response
            .results
            .iter()
            .all(|h| h.symbol.as_deref() != Some("renamed_symbol")),
        "no-refresh must not pick up the unindexed edit: {:?}",
        response.results
    );

    let stale = locate(
        root,
        LocateOptions {
            query: "original_name".to_string(),
            limit: 5,
            use_reranker: false,
            no_refresh: true,
        },
    )
    .expect("locate stale");
    assert!(
        stale
            .results
            .iter()
            .any(|h| h.symbol.as_deref() == Some("original_name")),
        "no-refresh should still serve the last-indexed (stale) symbol: {:?}",
        stale.results
    );
    assert_eq!(stale.files_total, warm.files_total);
    assert_eq!(stale.chunks_total, warm.chunks_total);
    assert_eq!(stale.head, warm.head);
}

/// `--no-refresh` never *populates* the sibling DB — it skips `refresh`
/// entirely, so on a never-indexed repo the query pool stays empty. (The
/// DB file itself still gets bootstrapped, same as any `open_code_index`
/// call — that side effect is unconditional and predates this flag; see
/// `status_reports_counts_without_creating_the_index` for the command that
/// actually guarantees no side effects.) An empty result here, not an
/// error and not the auto-build behavior `locate_auto_refreshes_a_never_
/// indexed_repo` covers for the default (refreshing) path.
#[test]
fn locate_no_refresh_on_a_never_indexed_repo_returns_no_matches() {
    let temp = repo_with_files(&[("src/a.rs", "pub fn alpha() {}\n")]);
    let root = temp.path();
    assert!(!code_index_db_path(root).exists());

    let response = locate(
        root,
        LocateOptions {
            query: "alpha".to_string(),
            limit: 5,
            use_reranker: false,
            no_refresh: true,
        },
    )
    .expect("locate");

    assert!(response.results.is_empty());
    assert_eq!(response.files_total, 0);
    assert_eq!(response.chunks_total, 0);
    assert_eq!(response.head, None);
}

#[test]
fn locate_hybrid_blends_vector_and_returns_scores() {
    let temp = repo_with_files(&[
        ("src/a.rs", "pub fn compute_checksum() -> u64 { 0 }\n"),
        ("src/b.rs", "pub fn unrelated_helper() {}\n"),
    ]);
    let root = temp.path();
    set_hybrid(root);

    let response = locate(
        root,
        LocateOptions {
            query: "compute checksum".to_string(),
            limit: 5,
            use_reranker: false,
            no_refresh: false,
        },
    )
    .expect("locate");

    assert_eq!(response.mode, RetrievalMode::Hybrid);
    assert!(!response.results.is_empty());
    let top = &response.results[0];
    assert_eq!(top.symbol.as_deref(), Some("compute_checksum"));
    // Hybrid: the vector side contributed a non-zero similarity.
    assert!(top.vector_score > 0.0, "vector score should be populated");
}

#[test]
fn locate_empty_query_returns_no_matches() {
    let temp = repo_with_files(&[("src/a.rs", "pub fn alpha() {}\n")]);
    let root = temp.path();

    let response = locate(
        root,
        LocateOptions {
            query: "   ".to_string(),
            limit: 5,
            use_reranker: false,
            no_refresh: false,
        },
    )
    .expect("locate");
    assert!(response.results.is_empty());
    assert_eq!(response.candidate_count, 0);
}

#[test]
fn status_reports_counts_without_creating_the_index() {
    let temp = repo_with_files(&[("src/a.rs", "pub fn alpha() {}\n")]);
    let root = temp.path();

    // Before any index op, status reports not-built and does NOT create it.
    let before = code_index::status(root).expect("status");
    assert!(!before.exists);
    assert_eq!(before.files_total, 0);
    assert!(
        !code_index_db_path(root).exists(),
        "status must not create DB"
    );

    code_index::refresh(root).expect("refresh");
    let after = code_index::status(root).expect("status");
    assert!(after.exists);
    assert!(after.files_total >= 1);
    assert!(after.chunks_total >= 1);
    assert!(after.schema_version.is_some());
    // Freshly indexed against HEAD: not stale.
    assert!(!after.head_stale());
}

#[test]
fn remove_index_wipes_the_disposable_db() {
    let temp = repo_with_files(&[("src/a.rs", "pub fn alpha() {}\n")]);
    let root = temp.path();
    code_index::refresh(root).expect("refresh");
    assert!(code_index_db_path(root).exists());

    let outcome = code_index::remove_index(root).expect("remove");
    assert!(outcome.removed);
    assert!(!code_index_db_path(root).exists());

    // Removing again is a clean no-op.
    let again = code_index::remove_index(root).expect("remove again");
    assert!(!again.removed);
}
