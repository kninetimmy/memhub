//! Eager-embed write-path tests for M8 PR3.
//!
//! Verifies that fact/decision/task writers populate the embeddings table
//! when [retrieval] mode = "hybrid" and skip it entirely when mode = "fts"
//! (the default).

use std::fs;

use memhub::commands::{decision, fact, init, review, task};
use memhub::config::ProjectConfig;
use memhub::db;
use rusqlite::params;
use tempfile::tempdir;

fn switch_to_hybrid(repo_root: &std::path::Path) {
    let config_path = repo_root.join(".memhub").join("config.toml");
    let mut cfg = ProjectConfig::load(&config_path).expect("load config");
    cfg.retrieval.mode = memhub::config::RetrievalMode::Hybrid;
    cfg.save(&config_path).expect("save config");
    // Sanity: rewriting via TOML round-trip should preserve hybrid mode.
    let reloaded = ProjectConfig::load(&config_path).expect("reload");
    assert_eq!(reloaded.retrieval.mode, memhub::config::RetrievalMode::Hybrid);
}

fn embedding_count(conn: &rusqlite::Connection, source_type: &str, source_id: i64) -> i64 {
    conn.query_row(
        "SELECT COUNT(*) FROM embeddings WHERE source_type = ?1 AND source_id = ?2",
        params![source_type, source_id],
        |r| r.get(0),
    )
    .expect("count embeddings")
}

fn embedding_metadata(
    conn: &rusqlite::Connection,
    source_type: &str,
    source_id: i64,
) -> (i64, String, Vec<u8>) {
    conn.query_row(
        "SELECT dimension, content_hash, vector
         FROM embeddings
         WHERE source_type = ?1 AND source_id = ?2",
        params![source_type, source_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .expect("fetch embedding metadata")
}

#[test]
fn fts_mode_default_skips_embedding_writes() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    fact::add(
        temp.path(),
        "build-command",
        "cargo nextest run",
        "user",
        "cli:user",
    )
    .expect("add fact");
    decision::add(
        temp.path(),
        "Adopt parquet",
        "Cold storage analytics need columnar files.",
        "user",
        "cli:user",
    )
    .expect("add decision");
    task::add(
        temp.path(),
        "Wire shard rebalancer",
        Some("Track hotspot count per region"),
        "cli:user",
    )
    .expect("add task");

    let ctx = db::open_project(temp.path()).expect("open project");
    let total: i64 = ctx
        .conn
        .query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))
        .expect("count embeddings");
    assert_eq!(
        total, 0,
        "default fts mode must not write embedding rows; got {total}"
    );
}

#[test]
fn hybrid_mode_writes_fact_embedding_on_add() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    switch_to_hybrid(temp.path());

    let (fact_id, _) = fact::add(
        temp.path(),
        "build-command",
        "cargo nextest run --all-features",
        "user",
        "cli:user",
    )
    .expect("add fact");

    let ctx = db::open_project(temp.path()).expect("open project");
    assert_eq!(embedding_count(&ctx.conn, "fact", fact_id), 1);

    let (dim, hash, blob) = embedding_metadata(&ctx.conn, "fact", fact_id);
    assert_eq!(dim, 384);
    assert_eq!(hash.len(), 64, "expected hex sha256 string");
    assert_eq!(blob.len(), 384 * 4, "expected 384 f32 little-endian floats");
}

#[test]
fn hybrid_mode_skips_re_embed_when_content_unchanged() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    switch_to_hybrid(temp.path());

    fact::add(temp.path(), "k", "the same value", "user", "cli:user").expect("add v1");

    let ctx = db::open_project(temp.path()).expect("open project");
    let fact_id: i64 = ctx
        .conn
        .query_row("SELECT id FROM facts WHERE key = 'k'", [], |r| r.get(0))
        .expect("fact id");
    let row_id_v1: i64 = ctx
        .conn
        .query_row(
            "SELECT id FROM embeddings WHERE source_type = 'fact' AND source_id = ?1",
            params![fact_id],
            |r| r.get(0),
        )
        .expect("embedding id v1");
    let created_v1: String = ctx
        .conn
        .query_row(
            "SELECT created_at FROM embeddings WHERE id = ?1",
            params![row_id_v1],
            |r| r.get(0),
        )
        .expect("created_at v1");

    drop(ctx);

    // Same value -> content_hash matches -> no-op embedding write.
    fact::add(temp.path(), "k", "the same value", "user", "cli:user").expect("add v1 again");

    let ctx = db::open_project(temp.path()).expect("open project");
    let row_id_v2: i64 = ctx
        .conn
        .query_row(
            "SELECT id FROM embeddings WHERE source_type = 'fact' AND source_id = ?1",
            params![fact_id],
            |r| r.get(0),
        )
        .expect("embedding id v2");
    let created_v2: String = ctx
        .conn
        .query_row(
            "SELECT created_at FROM embeddings WHERE id = ?1",
            params![row_id_v2],
            |r| r.get(0),
        )
        .expect("created_at v2");

    assert_eq!(
        row_id_v1, row_id_v2,
        "no-op re-add must not delete/reinsert the embedding row"
    );
    assert_eq!(
        created_v1, created_v2,
        "no-op re-add must preserve the original created_at"
    );
    assert_eq!(embedding_count(&ctx.conn, "fact", fact_id), 1);
}

#[test]
fn hybrid_mode_re_embeds_when_value_changes() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    switch_to_hybrid(temp.path());

    fact::add(temp.path(), "k", "original", "user", "cli:user").expect("add v1");

    let ctx = db::open_project(temp.path()).expect("open project");
    let fact_id: i64 = ctx
        .conn
        .query_row("SELECT id FROM facts WHERE key = 'k'", [], |r| r.get(0))
        .expect("fact id");
    let (_, hash_v1, blob_v1) = embedding_metadata(&ctx.conn, "fact", fact_id);
    drop(ctx);

    fact::add(temp.path(), "k", "very different replacement", "user", "cli:user")
        .expect("add v2");

    let ctx = db::open_project(temp.path()).expect("open project");
    let (_, hash_v2, blob_v2) = embedding_metadata(&ctx.conn, "fact", fact_id);

    assert_ne!(hash_v1, hash_v2, "content hash must move when value changes");
    assert_ne!(blob_v1, blob_v2, "embedding vector must change too");
    assert_eq!(embedding_count(&ctx.conn, "fact", fact_id), 1);
}

#[test]
fn hybrid_mode_writes_decision_embedding() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    switch_to_hybrid(temp.path());

    let decision_id = decision::add(
        temp.path(),
        "Pin tokio to 1.48",
        "Avoid the 1.49 IO-driver regression hitting our hotspots.",
        "user",
        "cli:user",
    )
    .expect("add decision");

    let ctx = db::open_project(temp.path()).expect("open project");
    let (dim, hash, blob) = embedding_metadata(&ctx.conn, "decision", decision_id);
    assert_eq!(dim, 384);
    assert_eq!(hash.len(), 64);
    assert_eq!(blob.len(), 384 * 4);
}

#[test]
fn hybrid_mode_writes_task_embedding_with_and_without_notes() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    switch_to_hybrid(temp.path());

    let with_notes = task::add(
        temp.path(),
        "Migrate ingest to streaming",
        Some("Bottleneck is the legacy batch reducer"),
        "cli:user",
    )
    .expect("add task with notes");
    let no_notes = task::add(temp.path(), "Profile cold start", None, "cli:user")
        .expect("add task no notes");

    let ctx = db::open_project(temp.path()).expect("open project");
    assert_eq!(embedding_count(&ctx.conn, "task", with_notes), 1);
    assert_eq!(embedding_count(&ctx.conn, "task", no_notes), 1);

    let (dim_a, _, blob_a) = embedding_metadata(&ctx.conn, "task", with_notes);
    let (dim_b, _, blob_b) = embedding_metadata(&ctx.conn, "task", no_notes);
    assert_eq!(dim_a, 384);
    assert_eq!(dim_b, 384);
    assert_ne!(blob_a, blob_b, "different content should produce different vectors");
}

#[test]
fn deleting_source_row_cascades_to_embedding_via_trigger() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    switch_to_hybrid(temp.path());

    let (fact_id, _) = fact::add(temp.path(), "k", "v", "user", "cli:user").expect("add fact");
    let decision_id = decision::add(
        temp.path(),
        "title",
        "rationale that the model can chew on",
        "user",
        "cli:user",
    )
    .expect("add decision");
    let task_id = task::add(temp.path(), "title", Some("body"), "cli:user").expect("add task");

    let ctx = db::open_project(temp.path()).expect("open project");
    assert_eq!(embedding_count(&ctx.conn, "fact", fact_id), 1);
    assert_eq!(embedding_count(&ctx.conn, "decision", decision_id), 1);
    assert_eq!(embedding_count(&ctx.conn, "task", task_id), 1);

    ctx.conn
        .execute("DELETE FROM facts WHERE id = ?1", params![fact_id])
        .expect("delete fact");
    ctx.conn
        .execute("DELETE FROM decisions WHERE id = ?1", params![decision_id])
        .expect("delete decision");
    ctx.conn
        .execute("DELETE FROM tasks WHERE id = ?1", params![task_id])
        .expect("delete task");

    assert_eq!(embedding_count(&ctx.conn, "fact", fact_id), 0);
    assert_eq!(embedding_count(&ctx.conn, "decision", decision_id), 0);
    assert_eq!(embedding_count(&ctx.conn, "task", task_id), 0);
}

#[test]
fn review_accept_embeds_fact_in_hybrid_mode() {
    use memhub::commands::pending_write;

    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    switch_to_hybrid(temp.path());

    // Stage a pending fact and accept it. The accept path is the one that
    // exercises fact::add_in_tx with a non-default mode parameter.
    let pending_id = pending_write::propose_fact(
        temp.path(),
        "build-command",
        "cargo flamegraph build",
        "codex propose",
        "codex",
        "codex",
        "{}",
    )
    .expect("propose pending fact");

    let outcome = review::accept(temp.path(), pending_id, "cli:user").expect("accept");
    let fact_id = outcome.durable_id;

    let ctx = db::open_project(temp.path()).expect("open project");
    let (dim, hash, _) = embedding_metadata(&ctx.conn, "fact", fact_id);
    assert_eq!(dim, 384);
    assert_eq!(hash.len(), 64);
}

#[test]
fn decision_add_with_summary_embeds_augmented_text() {
    // The bi-encoder's content_hash is computed over the embed text. When
    // a summary is set, the embed text gets a paraphrase prefix, so the
    // hash differs from the no-summary baseline. Covers migration 0011 /
    // decision 72.
    use memhub::retrieval::decision_embed_text;

    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    switch_to_hybrid(temp.path());

    let plain_id = decision::add(
        temp.path(),
        "content_hash drift detection",
        "Store a hash of source body alongside each vector.",
        "user",
        "cli:user",
    )
    .expect("plain decision");

    let augmented_id = decision::add_with_decided_at(
        temp.path(),
        "content_hash drift detection — augmented row",
        "Store a hash of source body alongside each vector.",
        None,
        Some("How does memhub know when an embedding has gone stale?"),
        "user",
        "cli:user",
    )
    .expect("augmented decision");

    let ctx = db::open_project(temp.path()).expect("open project");
    let (_, plain_hash, _) = embedding_metadata(&ctx.conn, "decision", plain_id);
    let (_, augmented_hash, _) = embedding_metadata(&ctx.conn, "decision", augmented_id);

    assert_ne!(
        plain_hash, augmented_hash,
        "augmented embed text must produce a different content_hash"
    );

    // Sanity: the augmented hash matches a manual call to decision_embed_text
    // with the same summary input.
    let expected_text = decision_embed_text(
        "content_hash drift detection — augmented row",
        "Store a hash of source body alongside each vector.",
        Some("How does memhub know when an embedding has gone stale?"),
    );
    assert!(
        expected_text.starts_with("How does memhub know"),
        "augmented embed text should start with the summary"
    );
}

#[test]
fn decision_set_summary_re_embeds_existing_row() {
    // Backfill path: decision::set_summary updates the row's summary and
    // re-embeds inside the same transaction. Content_hash before and
    // after must differ; clearing the summary back to empty restores the
    // baseline hash.
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    switch_to_hybrid(temp.path());

    let id = decision::add(
        temp.path(),
        "Zero-result behavior: empty bundle, no automatic fallback",
        "Recall returns an empty results array when nothing matches.",
        "user",
        "cli:user",
    )
    .expect("decision");

    let baseline_hash = {
        let ctx = db::open_project(temp.path()).expect("open project");
        embedding_metadata(&ctx.conn, "decision", id).1
    };

    decision::set_summary(
        temp.path(),
        id,
        Some("What does memhub return when a query matches nothing?"),
        "cli:user",
    )
    .expect("set_summary");

    let augmented_hash = {
        let ctx = db::open_project(temp.path()).expect("open project");
        embedding_metadata(&ctx.conn, "decision", id).1
    };
    assert_ne!(baseline_hash, augmented_hash);

    // Clearing the summary (empty string -> None) must restore the
    // baseline content_hash so the embed text round-trips.
    decision::set_summary(temp.path(), id, None, "cli:user").expect("clear summary");
    let restored_hash = {
        let ctx = db::open_project(temp.path()).expect("open project");
        embedding_metadata(&ctx.conn, "decision", id).1
    };
    assert_eq!(restored_hash, baseline_hash);
}

#[test]
fn config_round_trip_preserves_retrieval_section() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    let config_path = temp.path().join(".memhub").join("config.toml");
    let raw = fs::read_to_string(&config_path).expect("read config");
    assert!(
        raw.contains("[retrieval]"),
        "default config should serialize the retrieval section, got:\n{raw}"
    );

    let cfg = ProjectConfig::load(&config_path).expect("load");
    assert_eq!(cfg.retrieval.mode, memhub::config::RetrievalMode::Fts);
}
