//! M9 machine-global memory end-to-end.
//!
//! All assertions live in ONE test so the `HOME` override (which
//! redirects `~/.memhub/global.sqlite` into a tempdir) cannot race
//! other tests in this binary. Other integration-test binaries run as
//! separate processes and are unaffected.

use memhub::commands::{fact, global, init, pending_write, review};
use memhub::config::RetrievalMode;
use memhub::retrieval::{RecallOptions, recall};
use tempfile::tempdir;

fn fts_recall(path: &std::path::Path, query: &str) -> memhub::retrieval::RecallResponse {
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
        },
    )
    .expect("recall")
}

fn has_scope(resp: &memhub::retrieval::RecallResponse, scope: &str) -> bool {
    resp.results.iter().any(|r| r.scope == scope)
}

#[test]
fn machine_global_memory_end_to_end() {
    let home = tempdir().expect("home tempdir");
    // SAFETY: single-test binary; no other thread reads HOME concurrently.
    unsafe {
        std::env::set_var("HOME", home.path());
        // macOS/Linux read HOME first, but clear USERPROFILE so the
        // resolver can't pick up a stale Windows-style var.
        std::env::remove_var("USERPROFILE");
    }

    let repo_a = tempdir().expect("repo_a");
    let repo_b = tempdir().expect("repo_b");
    let repo_c = tempdir().expect("repo_c");
    init::run(repo_a.path()).expect("init a");
    init::run(repo_b.path()).expect("init b");
    init::run(repo_c.path()).expect("init c");

    // --- enable-gate: a non-enabled repo refuses global writes -------
    assert!(
        fact::add_global(repo_c.path(), "x", "y", "user", "cli:user").is_err(),
        "global write must refuse when the repo has not opted in"
    );

    // --- enable creates the store ------------------------------------
    let en = global::enable(repo_a.path()).expect("enable a");
    assert!(en.store_created, "first enable creates the store");
    let st = global::status(repo_a.path()).expect("status a");
    assert!(st.enabled && st.exists);
    assert_eq!(st.fact_count, 0);

    // --- born-global fact lands in global, not the repo --------------
    let r = fact::add_global(
        repo_a.path(),
        "alpha-globalkey",
        "alpha-globalvalue",
        "user",
        "cli:user",
    )
    .expect("add_global");
    assert!(r.created);
    assert!(
        fact::list(repo_a.path()).expect("list a").is_empty(),
        "born-global fact must NOT be in the repo DB"
    );
    assert_eq!(
        global::status(repo_a.path()).expect("status a2").fact_count,
        1
    );

    // --- recall from a DIFFERENT repo merges global with scope -------
    global::enable(repo_b.path()).expect("enable b");
    fact::add(
        repo_b.path(),
        "beta-repokey",
        "beta-repovalue",
        "user",
        "cli:user",
    )
    .expect("repo-local fact");

    let g = fts_recall(repo_b.path(), "alpha-globalvalue");
    assert!(
        g.results.iter().any(|h| h.scope == "global"
            && (h.title.contains("alpha-globalkey") || h.body.contains("alpha-globalvalue"))),
        "global fact must surface in repo_b tagged scope=global; got {:?}",
        g.results
    );
    let l = fts_recall(repo_b.path(), "beta-repovalue");
    assert!(
        l.results.iter().any(|h| h.scope == "repo"),
        "repo-local fact must be tagged scope=repo"
    );

    // --- promote copies an existing repo fact into global -----------
    let (gamma_id, _) = fact::add(
        repo_b.path(),
        "gamma-key",
        "gamma-value",
        "user",
        "cli:user",
    )
    .expect("seed gamma");
    fact::promote(repo_b.path(), gamma_id, "cli:user").expect("promote");
    assert_eq!(
        global::status(repo_b.path()).expect("status b").fact_count,
        2,
        "promote adds a row to global (alpha + gamma)"
    );

    // --- MCP-style staged global proposal, durable only on accept ----
    let pid = pending_write::propose_fact_scoped(
        repo_b.path(),
        "delta-key",
        "delta-value",
        "machine-wide policy",
        true, // global
        "codex",
        "openai-codex",
        "{\"source\":\"mcp\"}",
    )
    .expect("propose global");
    // Not durable yet.
    assert_eq!(
        global::status(repo_b.path()).expect("status b2").fact_count,
        2
    );
    review::accept(repo_b.path(), pid, "cli:user").expect("accept");
    assert_eq!(
        global::status(repo_b.path()).expect("status b3").fact_count,
        3,
        "accepting a target:global proposal lands it in the global store"
    );
    assert!(
        !fact::list(repo_b.path())
            .expect("list b")
            .iter()
            .any(|f| f.key == "delta-key"),
        "an accepted global proposal must NOT land in the repo DB"
    );

    // --- disable → recall is pre-M9 (no global merge) ----------------
    global::disable(repo_b.path()).expect("disable b");
    let d = fts_recall(repo_b.path(), "alpha-globalvalue");
    assert!(
        !has_scope(&d, "global"),
        "disabled repo must not merge the global corpus"
    );
}
