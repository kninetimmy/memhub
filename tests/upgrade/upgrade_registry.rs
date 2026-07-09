//! Machine-wide upgrade registry (decision 96 / task 49).
//!
//! Two guarantees are load-bearing and tested here:
//!
//! 1. The registry is self-maintaining, gated on the global store
//!    already existing, and debounced — it never creates the store and
//!    an immediate re-open does not rewrite the row.
//! 2. The **eval-regression guarantee**: a populated `known_projects`
//!    must not change recall output one bit while the repo has
//!    `[global] enabled = false`. Registry membership is not M9 opt-in.
//!
//! All assertions live in ONE test so the `HOME` override (which
//! redirects `~/.memhub/global.sqlite` into a tempdir) stays in one
//! place. It takes `support::env_lock()` for the whole test — see
//! `upgrade/support.rs` (Wave 5 U4, issue #90) — to stay isolated from
//! sibling tests in this shared harness binary.

use memhub::commands::{fact, global, init};
use memhub::config::RetrievalMode;
use memhub::db;
use memhub::retrieval::{RecallOptions, RecallResponse, recall};
use tempfile::tempdir;

fn fts_recall(path: &std::path::Path, query: &str) -> RecallResponse {
    recall(
        path,
        RecallOptions {
            query: query.to_string(),
            mode: Some(RetrievalMode::Fts),
            max_results: 10,
            source_types: vec![],
            include_stale: None,
            accepted_only: None,
            use_reranker: None,
            min_rerank_score: None,
            log_metrics: false,
            surface: None,
        },
    )
    .expect("recall")
}

/// Compare everything that recall promises to be stable. `elapsed_ms`
/// lives on the response and varies run to run, so it is deliberately
/// excluded — the guarantee is "byte-identical modulo wall-clock".
fn assert_recall_identical(a: &RecallResponse, b: &RecallResponse) {
    assert_eq!(
        format!("{:?}", a.results),
        format!("{:?}", b.results),
        "recall results changed when known_projects was populated \
         while [global] disabled — eval-regression guarantee violated"
    );
    assert_eq!(a.candidate_count, b.candidate_count);
    assert_eq!(a.returned_count, b.returned_count);
    assert_eq!(a.available_docs, b.available_docs);
    assert_eq!(a.mode, b.mode);
}

#[test]
fn registry_and_eval_regression_guarantee() {
    // Held for the whole test (see module header and `upgrade/support.rs`).
    let _env_guard = crate::support::env_lock();

    let home = tempdir().expect("home tempdir");
    // The TMP_OK seam lets the registry accept the tempdir repos this test
    // necessarily uses; production excludes OS-temp paths (covered by the
    // registry unit test).
    unsafe {
        std::env::set_var("HOME", home.path());
        std::env::remove_var("USERPROFILE");
        std::env::set_var("MEMHUB_REGISTRY_TMP_OK", "1");
    }

    let repo = tempdir().expect("repo");
    init::run(repo.path()).expect("init");
    fact::add(
        repo.path(),
        "registry-probe",
        "alpha-value",
        "user",
        "cli:user",
    )
    .expect("seed fact");

    // --- baseline recall with NO global store ------------------------
    assert!(
        !db::global_store_exists().expect("exists check"),
        "no global store should exist yet"
    );
    let baseline = fts_recall(repo.path(), "registry-probe");
    assert!(
        !baseline.results.is_empty(),
        "seed fact must be recallable so the comparison is meaningful"
    );

    // --- no global store => registry is inert, store NOT created -----
    assert!(
        db::registry::list_known().expect("list").is_empty(),
        "registry must be empty with no global store"
    );
    let _ = db::open_project(repo.path()).expect("open with no global store");
    assert!(
        !db::global_store_exists().expect("exists check"),
        "opening a project must NOT create the global store"
    );
    assert!(
        db::registry::list_known().expect("list").is_empty(),
        "still empty — record-on-open is gated on the store existing"
    );

    // --- create the store via enable, then opt the repo back OUT ----
    global::enable(repo.path()).expect("enable");
    global::disable(repo.path()).expect("disable");
    assert!(
        db::global_store_exists().expect("exists check"),
        "enable creates the store; disable keeps it on disk"
    );

    // Opening the project now self-registers it (store exists).
    let _ = db::open_project(repo.path()).expect("open with store present");
    let known = db::registry::list_known().expect("list");
    assert_eq!(known.len(), 1, "repo should self-register exactly once");
    assert_eq!(
        known[0].last_schema.as_deref(),
        Some(db::latest_schema_version()),
        "registry records the head schema the repo was brought to"
    );

    // --- debounce: an immediate re-open does not rewrite the row ----
    let seen_before = known[0].last_seen.clone();
    let _ = db::open_project(repo.path()).expect("re-open");
    let known2 = db::registry::list_known().expect("list2");
    assert_eq!(known2.len(), 1, "no duplicate row");
    assert_eq!(
        known2[0].last_seen, seen_before,
        "debounce: last_seen unchanged within the hour window"
    );

    // --- explicit register() adds an --also-style path --------------
    let other = tempdir().expect("other repo");
    let added =
        db::registry::register(other.path(), db::latest_schema_version()).expect("register");
    assert!(added, "register persists when the global store exists");
    assert_eq!(
        db::registry::list_known().expect("list3").len(),
        2,
        "explicit register adds a second known root"
    );

    // --- self-heal: a vanished repo is pruned, not left as junk -----
    let ghost = tempdir().expect("ghost repo");
    init::run(ghost.path()).expect("init ghost");
    let _ = db::open_project(ghost.path()).expect("open ghost");
    assert_eq!(
        db::registry::list_known().expect("list4").len(),
        3,
        "ghost repo registered (repo + other + ghost)"
    );
    let ghost_path = ghost.path().to_path_buf();
    drop(ghost); // tempdir deleted on disk; registry row now dead
    let dead = db::registry::dead_roots().expect("dead_roots");
    assert!(
        dead.iter().any(|p| p == &ghost_path
            || p.canonicalize().ok() == ghost_path.canonicalize().ok()
            || p.to_string_lossy()
                .contains(ghost_path.file_name().unwrap().to_str().unwrap())),
        "the deleted ghost repo must be detected as a dead root"
    );
    let removed = db::registry::prune_dead().expect("prune");
    assert!(removed >= 1, "prune removes at least the ghost row");
    assert!(
        !db::registry::list_known()
            .expect("list5")
            .iter()
            .any(|kp| kp
                .root_path
                .to_string_lossy()
                .contains(ghost_path.file_name().unwrap().to_str().unwrap())),
        "ghost row is gone after prune"
    );

    // --- THE GUARANTEE: populated registry + [global] disabled ------
    // Recall must be byte-identical to the pre-store baseline.
    let after = fts_recall(repo.path(), "registry-probe");
    assert_recall_identical(&baseline, &after);

    unsafe { std::env::remove_var("MEMHUB_REGISTRY_TMP_OK") };
}
