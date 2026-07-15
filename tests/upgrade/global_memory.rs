//! M9 machine-global memory end-to-end.
//!
//! All assertions live in ONE test so the `HOME` override (which
//! redirects `~/.memhub/global.sqlite` into a tempdir) stays in one
//! place. It takes `support::env_lock()` for the whole test — see
//! `upgrade/support.rs` (Wave 5 U4, issue #90) — to stay isolated from
//! sibling tests in this shared harness binary.

use memhub::commands::{doc, fact, global, init, pending_write, review};
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
            surface: None,
        },
    )
    .expect("recall")
}

fn has_scope(resp: &memhub::retrieval::RecallResponse, scope: &str) -> bool {
    resp.results.iter().any(|r| r.scope == scope)
}

#[test]
fn machine_global_memory_end_to_end() {
    // Held for the whole test (see module header and `upgrade/support.rs`).
    let _env_guard = crate::support::env_lock();

    let home = tempdir().expect("home tempdir");
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

    // --- global doc management: add / ls / show / rm round-trip ------
    let doc_file = repo_a.path().join("guide.md");
    std::fs::write(
        &doc_file,
        "# Guide\n\n## Section A\n\nalpha doc body\n\n## Section B\n\nbeta doc body\n",
    )
    .expect("write doc");
    let added =
        doc::add_global(repo_a.path(), &doc_file, None, "cli:user").expect("doc add_global");
    assert!(added.chunk_count >= 2);
    assert!(
        doc::list(repo_a.path()).expect("repo doc list").is_empty(),
        "a global doc must NOT be in the repo store"
    );
    let gdocs = doc::list_global(repo_a.path()).expect("list_global");
    assert_eq!(gdocs.len(), 1);
    assert_eq!(gdocs[0].id, added.doc_id);
    let (meta, chunks) = doc::show_global(repo_a.path(), &added.doc_id.to_string())
        .expect("show_global")
        .expect("present");
    assert_eq!(meta.id, added.doc_id);
    assert!(chunks.iter().any(|c| c.body.contains("alpha doc body")));
    // A repo that has not opted in cannot manage global docs (gate
    // mirrors `doc add --global`).
    assert!(
        doc::list_global(repo_c.path()).is_err(),
        "list_global must refuse when the repo has not opted in"
    );
    assert!(
        doc::remove_global(repo_c.path(), &added.doc_id.to_string(), "cli:user").is_err(),
        "remove_global must refuse when the repo has not opted in"
    );
    assert!(
        doc::remove_global(repo_a.path(), &added.doc_id.to_string(), "cli:user")
            .expect("remove_global")
    );
    assert!(
        doc::list_global(repo_a.path())
            .expect("list_global after rm")
            .is_empty()
    );
    assert!(
        !doc::remove_global(repo_a.path(), &added.doc_id.to_string(), "cli:user")
            .expect("remove_global again"),
        "a second remove of the same global doc finds nothing"
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
        None, // kind
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
    review::accept(repo_b.path(), pid, "cli:user", None, false).expect("accept");
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

    // --- replay-safe global accept (decision; no key upsert) ---------
    // Regression: a global decision proposal accepted once, where the
    // repo-side status flip is then lost (crash / repo-DB error in the
    // cross-DB commit window), must NOT insert a second global decision
    // when re-accepted. Facts are protected by key upsert; decisions
    // have no natural key, so this is the path that previously
    // duplicated and poisoned every repo.
    let dpid = pending_write::propose_decision_scoped(
        repo_b.path(),
        "global decision title",
        "global decision rationale",
        true, // global
        "codex",
        "openai-codex",
        "{\"source\":\"mcp\"}",
    )
    .expect("propose global decision");
    let dec_before = global::status(repo_b.path())
        .expect("status b dec0")
        .decision_count;
    review::accept(repo_b.path(), dpid, "cli:user", None, false).expect("accept global decision");
    let dec_after = global::status(repo_b.path())
        .expect("status b dec1")
        .decision_count;
    assert_eq!(
        dec_after,
        dec_before + 1,
        "accepting a global decision proposal adds exactly one global decision"
    );

    // Simulate the post-crash state: the global durable write + its
    // idempotency marker committed, but the repo-side status flip did
    // not — so the proposal is still `pending` in the repo DB.
    let repo_db = repo_b.path().join(".memhub").join("project.sqlite");
    let conn = rusqlite::Connection::open(&repo_db).expect("open repo db");
    conn.execute(
        "UPDATE pending_writes SET status = 'pending', reviewed_at = NULL WHERE id = ?1",
        rusqlite::params![dpid],
    )
    .expect("revert repo-side status to pending");
    drop(conn);

    // Re-accept. The marker is detected, so no second durable row is
    // written, and the repo-side flip now succeeds.
    review::accept(repo_b.path(), dpid, "cli:user", None, false).expect("replayed accept must succeed");
    assert_eq!(
        global::status(repo_b.path())
            .expect("status b dec2")
            .decision_count,
        dec_after,
        "replaying an interrupted global-decision accept must NOT duplicate the decision"
    );
    assert_eq!(
        review::show(repo_b.path(), dpid).expect("show dpid").status,
        "accepted",
        "the replayed accept must still flip the repo-side proposal to accepted"
    );

    // --- disable → recall is pre-M9 (no global merge) ----------------
    global::disable(repo_b.path()).expect("disable b");
    let d = fts_recall(repo_b.path(), "alpha-globalvalue");
    assert!(
        !has_scope(&d, "global"),
        "disabled repo must not merge the global corpus"
    );

    // --- `doc add --global`'s config flip is gated on the CALLING
    // REPO's own config state, never on the shared global store's
    // emptiness (issue #123) -------------------------------------------
    //
    // Regression: repo A's first `doc add --global` used to consume the
    // shared global store's only "documents table was empty" moment
    // (`was_first_doc`), so repo B's own later first global doc add saw
    // a non-empty store and silently never flipped repo B's own config
    // — no notice printed, and B's ingested doc never joined B's own
    // default recall. `repo_d` seeds the shared store first so it is
    // already non-empty by the time `repo_e` does ITS first global doc
    // add, reproducing the exact failure scenario.
    let repo_d = tempdir().expect("repo_d");
    let repo_e = tempdir().expect("repo_e");
    init::run(repo_d.path()).expect("init d");
    init::run(repo_e.path()).expect("init e");
    global::enable(repo_d.path()).expect("enable d");
    global::enable(repo_e.path()).expect("enable e");

    let seed_file = repo_d.path().join("seed.md");
    std::fs::write(&seed_file, "# Seed\n\n## S\n\nseed body\n").expect("write seed");
    let seeded =
        doc::add_global(repo_d.path(), &seed_file, None, "cli:user").expect("seed doc add_global");
    assert!(
        seeded.enabled_default_recall,
        "repo_d's own first global doc add must flip repo_d's own config"
    );

    let e_doc_file = repo_e.path().join("guide-e.md");
    std::fs::write(&e_doc_file, "# Guide E\n\n## Section\n\ne doc body\n").expect("write e doc");
    let e_added = doc::add_global(repo_e.path(), &e_doc_file, None, "cli:user")
        .expect("repo_e doc add_global");
    assert!(
        e_added.enabled_default_recall,
        "repo_e's own first global doc add must flip repo_e's config even though \
         the shared global store already held repo_d's seed doc"
    );
    let e_cfg_path = repo_e.path().join(".memhub").join("config.toml");
    let e_cfg = memhub::config::ProjectConfig::load(&e_cfg_path).expect("load e cfg");
    assert!(e_cfg.global.include_docs_in_default);

    // A repo whose config is already enabled: a further global doc add
    // is a no-op on the flag (no rewrite, no duplicate notice).
    let e_doc_file2 = repo_e.path().join("guide-e2.md");
    std::fs::write(
        &e_doc_file2,
        "# Guide E2\n\n## Section\n\ne doc body two\n",
    )
    .expect("write e doc 2");
    let e_added2 = doc::add_global(repo_e.path(), &e_doc_file2, None, "cli:user")
        .expect("repo_e second doc add_global");
    assert!(
        !e_added2.enabled_default_recall,
        "a second global doc add for a repo already enabled must be a no-op"
    );
}
