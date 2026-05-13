//! Eval harness tests for M8 PR6.
//!
//! Drives `commands::eval::run_retrieval` against a seeded in-memory
//! `.memhub/` project. Verifies Recall@K math, safety-failure
//! reporting, kind=empty success, and `--golden` path overrides.

use std::path::PathBuf;

use memhub::commands::eval::{
    DEFAULT_GOLDEN_PATH, EvalOptions, GoldenKind, run_retrieval,
};
use memhub::commands::{decision, fact, init, task};
use tempfile::tempdir;

fn seed_demo_project(root: &std::path::Path) {
    init::run(root).expect("init");
    fact::add(root, "build-command", "cargo build", "user", "cli:user").expect("fact build");
    fact::add(root, "test-command", "cargo test", "user", "cli:user").expect("fact test");
    decision::add(
        root,
        "memhub recall is read-only and never writes to writes_log",
        "Recall fetches FTS hits and never inserts into writes_log; codified to prevent observability creep.",
        "user+agent:claude-code",
        "cli:user",
    )
    .expect("decision recall readonly");
    decision::add(
        root,
        "FTS5 virtual tables attached to source tables",
        "Contentless FTS5 over facts.value, decisions.title+rationale, tasks.title+notes.",
        "user+agent:claude-code",
        "cli:user",
    )
    .expect("decision fts5");
    decision::add(
        root,
        "Eval metric: Recall@3 via tests/retrieval_golden.json",
        "Single-number test: across golden queries, what fraction had the expected row in top 3?",
        "user+agent:claude-code",
        "cli:user",
    )
    .expect("decision recall@3");
    task::add(
        root,
        "PR6: eval harness — golden queries + /eval-recall skill",
        Some("tests/retrieval_golden.json with 12 seeded queries. memhub eval retrieval command computes Recall@3."),
        "cli:user",
    )
    .expect("task pr6");
}

fn write_golden(dir: &std::path::Path, json: &str) -> PathBuf {
    let path = dir.join("golden.json");
    std::fs::write(&path, json).expect("write golden");
    path
}

#[test]
fn run_retrieval_returns_full_recall_on_seeded_db() {
    let temp = tempdir().expect("tempdir");
    seed_demo_project(temp.path());
    let golden = write_golden(
        temp.path(),
        r#"{
            "version": 1,
            "queries": [
                {
                    "id": "d-readonly",
                    "query": "recall read-only writes_log",
                    "kind": "match",
                    "source_type": "decision",
                    "title_contains": ["recall", "read-only"],
                    "body_contains": ["writes_log"]
                },
                {
                    "id": "f-build",
                    "query": "build command cargo",
                    "kind": "match",
                    "source_type": "fact",
                    "title_contains": ["build"],
                    "body_contains": ["cargo build"]
                },
                {
                    "id": "t-pr6",
                    "query": "PR6 eval harness golden",
                    "kind": "match",
                    "source_type": "task",
                    "title_contains": ["PR6", "eval"]
                },
                {
                    "id": "neg",
                    "query": "zxqv-totally-nonsense-token",
                    "kind": "empty"
                }
            ]
        }"#,
    );

    let summary = run_retrieval(
        temp.path(),
        EvalOptions {
            golden_path: golden,
            k: 3,
            mode: None,
        },
    )
    .expect("eval");

    assert_eq!(summary.total_queries, 4);
    assert_eq!(summary.match_queries, 3);
    assert_eq!(summary.empty_queries, 1);
    assert_eq!(summary.match_passes, 3);
    assert_eq!(summary.empty_passes, 1);
    assert_eq!(summary.safety_failures, 0);
    assert!(
        (summary.recall_at_k - 1.0).abs() < 1e-9,
        "expected perfect recall, got {}",
        summary.recall_at_k,
    );
}

#[test]
fn empty_query_with_matching_results_is_safety_failure() {
    let temp = tempdir().expect("tempdir");
    seed_demo_project(temp.path());
    // The "empty" probe uses a real keyword that exists in the seed,
    // so recall WILL return something — that must register as a
    // safety failure, not a silent pass.
    let golden = write_golden(
        temp.path(),
        r#"{
            "version": 1,
            "queries": [
                {
                    "id": "false-empty",
                    "query": "cargo",
                    "kind": "empty"
                }
            ]
        }"#,
    );

    let summary = run_retrieval(
        temp.path(),
        EvalOptions {
            golden_path: golden,
            k: 3,
            mode: None,
        },
    )
    .expect("eval");

    assert_eq!(summary.match_queries, 0);
    assert_eq!(summary.empty_queries, 1);
    assert_eq!(summary.empty_passes, 0);
    assert_eq!(summary.safety_failures, 1);
    let outcome = &summary.outcomes[0];
    assert!(!outcome.passed);
    assert!(outcome.kind == GoldenKind::Empty);
    let reason = outcome.failure_reason.clone().expect("reason");
    assert!(reason.contains("expected empty"), "{reason}");
}

#[test]
fn unmatched_query_drives_recall_below_one() {
    let temp = tempdir().expect("tempdir");
    seed_demo_project(temp.path());
    let golden = write_golden(
        temp.path(),
        r#"{
            "version": 1,
            "queries": [
                {
                    "id": "ok",
                    "query": "build command cargo",
                    "kind": "match",
                    "source_type": "fact",
                    "title_contains": ["build"]
                },
                {
                    "id": "wrong-source-type",
                    "query": "build command cargo",
                    "kind": "match",
                    "source_type": "decision",
                    "title_contains": ["build"]
                }
            ]
        }"#,
    );

    let summary = run_retrieval(
        temp.path(),
        EvalOptions {
            golden_path: golden,
            k: 3,
            mode: None,
        },
    )
    .expect("eval");

    assert_eq!(summary.match_passes, 1);
    assert_eq!(summary.match_queries, 2);
    assert!((summary.recall_at_k - 0.5).abs() < 1e-9);
    let failed = summary
        .outcomes
        .iter()
        .find(|o| o.id == "wrong-source-type")
        .expect("failed outcome");
    assert!(!failed.passed);
    let reason = failed.failure_reason.clone().expect("reason");
    assert!(reason.contains("no top-3"), "{reason}");
}

#[test]
fn missing_golden_path_returns_invalid_input_error() {
    let temp = tempdir().expect("tempdir");
    seed_demo_project(temp.path());
    let bogus = temp.path().join("does-not-exist.json");
    let err = run_retrieval(
        temp.path(),
        EvalOptions {
            golden_path: bogus,
            k: 3,
            mode: None,
        },
    )
    .expect_err("missing golden");
    let msg = format!("{err}");
    assert!(msg.contains("golden file not found"), "{msg}");
}

#[test]
fn k_must_be_positive() {
    let temp = tempdir().expect("tempdir");
    seed_demo_project(temp.path());
    let err = run_retrieval(
        temp.path(),
        EvalOptions {
            golden_path: temp.path().join("anything.json"),
            k: 0,
            mode: None,
        },
    )
    .expect_err("k=0");
    let msg = format!("{err}");
    assert!(msg.contains("--k"), "{msg}");
}

#[test]
fn shipped_golden_file_parses_cleanly() {
    // Guards against accidental schema drift in the checked-in starter set.
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    let path = repo_root.join(DEFAULT_GOLDEN_PATH);
    let bytes = std::fs::read(&path)
        .unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    let parsed: memhub::commands::eval::GoldenFile = serde_json::from_slice(&bytes)
        .unwrap_or_else(|e| panic!("parse {}: {e}", path.display()));
    assert_eq!(parsed.version, 1);
    assert_eq!(
        parsed.queries.len(),
        12,
        "starter golden set must keep its 12-query baseline per addendum §9; got {}",
        parsed.queries.len(),
    );
    let empties = parsed
        .queries
        .iter()
        .filter(|q| q.kind == GoldenKind::Empty)
        .count();
    assert!(
        empties >= 1,
        "starter golden set must include at least one safety (empty) probe",
    );
}
