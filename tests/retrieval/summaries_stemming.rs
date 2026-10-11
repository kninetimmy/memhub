//! Issue #349: optional summaries on facts and tasks, summary columns in
//! full-text search, and porter stemming on the six recall FTS tables
//! (migration 0026).

use memhub::commands::{decision, doc, fact, init, narrative, session_note, task};
use memhub::config::RetrievalMode;
use memhub::db;
use memhub::models::NarrativeKind;
use memhub::retrieval::{RecallOptions, SourceType, recall};
use tempfile::tempdir;

/// Ids of the `source_type` rows an fts-mode recall of `query` returns,
/// scoped to that one source type.
fn fts_ids(root: &std::path::Path, query: &str, source_type: SourceType) -> Vec<i64> {
    recall(
        root,
        RecallOptions {
            query: query.to_string(),
            mode: Some(RetrievalMode::Fts),
            max_results: 10,
            source_types: vec![source_type],
            include_stale: None,
            accepted_only: None,
            use_reranker: None,
            min_rerank_score: None,
            log_metrics: false,
            variants: Vec::new(),
            surface: None,
            explain: false,
        },
    )
    .expect("recall")
    .results
    .iter()
    .filter(|h| h.source_type == source_type.as_str())
    .map(|h| h.source_id)
    .collect()
}

fn stored_summary(root: &std::path::Path, table: &str, id: i64) -> Option<String> {
    let ctx = db::open_project(root).expect("open project");
    ctx.conn
        .query_row(
            &format!("SELECT summary FROM {table} WHERE id = ?1"),
            [id],
            |r| r.get(0),
        )
        .expect("read summary")
}

#[test]
fn fts_recall_finds_rows_by_words_only_in_their_summary() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    let (fact_id, _) = fact::add_with_kind(
        temp.path(),
        "deploy-host",
        "prod-edge-42",
        None,
        Some("which kookaburra server do we ship to"),
        "user",
        "cli:user",
    )
    .expect("fact");
    let decision_id = decision::add_with_decided_at(
        temp.path(),
        "Pin tokio",
        "Avoid the IO-driver regression.",
        None,
        Some("why kookaburra stays on an old runtime"),
        "user",
        "cli:user",
    )
    .expect("decision");
    let task_id = task::add_with_status(
        temp.path(),
        "Backport hotfix",
        None,
        Some("kookaburra release patch"),
        "open",
        "cli:user",
    )
    .expect("task");

    assert_eq!(
        fts_ids(temp.path(), "kookaburra", SourceType::Fact),
        vec![fact_id]
    );
    assert_eq!(
        fts_ids(temp.path(), "kookaburra", SourceType::Decision),
        vec![decision_id]
    );
    assert_eq!(
        fts_ids(temp.path(), "kookaburra", SourceType::Task),
        vec![task_id]
    );
}

#[test]
fn set_summary_sets_and_an_empty_text_clears_it() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    let (fact_id, _) = fact::add(temp.path(), "k", "v", "user", "cli:user").expect("fact");
    let task_id = task::add(temp.path(), "t", None, "cli:user").expect("task");

    fact::set_summary(temp.path(), fact_id, Some("platypus fact"), "cli:user").expect("set");
    task::set_summary(temp.path(), task_id, Some("platypus task"), "cli:user").expect("set");
    assert_eq!(
        stored_summary(temp.path(), "facts", fact_id).as_deref(),
        Some("platypus fact")
    );
    assert_eq!(
        stored_summary(temp.path(), "tasks", task_id).as_deref(),
        Some("platypus task")
    );
    assert_eq!(
        fts_ids(temp.path(), "platypus", SourceType::Fact),
        vec![fact_id]
    );
    assert_eq!(
        fts_ids(temp.path(), "platypus", SourceType::Task),
        vec![task_id]
    );

    // The CLI maps an empty or whitespace-only text to `None`; a blank
    // `Some` is normalized the same way.
    fact::set_summary(temp.path(), fact_id, None, "cli:user").expect("clear");
    task::set_summary(temp.path(), task_id, Some("   "), "cli:user").expect("clear");
    assert_eq!(stored_summary(temp.path(), "facts", fact_id), None);
    assert_eq!(stored_summary(temp.path(), "tasks", task_id), None);
    assert!(fts_ids(temp.path(), "platypus", SourceType::Fact).is_empty());
    assert!(fts_ids(temp.path(), "platypus", SourceType::Task).is_empty());

    assert!(fact::set_summary(temp.path(), 999, Some("x"), "cli:user").is_err());
    assert!(task::set_summary(temp.path(), 999, Some("x"), "cli:user").is_err());
}

#[test]
fn re_adding_a_fact_keeps_its_summary_unless_one_is_given() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    let (id, _) = fact::add_with_kind(
        temp.path(),
        "k",
        "v1",
        None,
        Some("first summary"),
        "user",
        "cli:user",
    )
    .expect("add");

    let (same_id, created) = fact::add(temp.path(), "k", "v2", "user", "cli:user").expect("re-add");
    assert_eq!((same_id, created), (id, false));
    assert_eq!(
        stored_summary(temp.path(), "facts", id).as_deref(),
        Some("first summary"),
        "re-adding without --summary keeps the summary"
    );

    fact::add_with_kind(
        temp.path(),
        "k",
        "v3",
        None,
        Some("second summary"),
        "user",
        "cli:user",
    )
    .expect("re-add with summary");
    assert_eq!(
        stored_summary(temp.path(), "facts", id).as_deref(),
        Some("second summary"),
        "re-adding with --summary replaces it"
    );
}

#[test]
fn fts_recall_matches_other_inflections_in_every_recall_source_type() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    let (fact_id, _) = fact::add(
        temp.path(),
        "schema-step",
        "the migration runs on open",
        "user",
        "cli:user",
    )
    .expect("fact");
    let decision_id = decision::add(
        temp.path(),
        "Ship one migration",
        "Fewer steps for users.",
        "user",
        "cli:user",
    )
    .expect("decision");
    let task_id = task::add(temp.path(), "Plan the migration", None, "cli:user").expect("task");
    let doc_file = temp.path().join("guide.md");
    std::fs::write(&doc_file, "# Guide\n\n## Upgrade\n\nA migration guide.\n").expect("doc");
    doc::add(temp.path(), &doc_file, None, "cli:user").expect("doc add");
    let note =
        session_note::add(temp.path(), "paused mid migration", "user", "cli:user").expect("note");
    narrative::set(
        temp.path(),
        NarrativeKind::Arch,
        "# Storage\n\nEach migration is ordered.",
        "user",
        "cli:user",
    )
    .expect("arch");

    for query in ["migrating", "migrations"] {
        assert_eq!(fts_ids(temp.path(), query, SourceType::Fact), vec![fact_id]);
        assert_eq!(
            fts_ids(temp.path(), query, SourceType::Decision),
            vec![decision_id]
        );
        assert_eq!(fts_ids(temp.path(), query, SourceType::Task), vec![task_id]);
        assert_eq!(fts_ids(temp.path(), query, SourceType::DocChunk).len(), 1);
        assert_eq!(fts_ids(temp.path(), query, SourceType::Note), vec![note.id]);
        assert_eq!(
            fts_ids(temp.path(), query, SourceType::ArchSection).len(),
            1
        );
    }
}
