//! Schema tests for migration 0009 (M8 PR2).
//!
//! Exercises the new contentless FTS5 tables, their sync triggers, and the
//! shape / constraints of the embeddings table. PR3 will add the writer
//! path that populates embeddings; here we drive it via raw SQL.

use memhub::commands::{decision, fact, init, task};
use memhub::db;
use rusqlite::params;
use tempfile::tempdir;

fn fts_hit(conn: &rusqlite::Connection, table: &str, query: &str) -> bool {
    // FTS5 treats bare hyphens as the NOT operator and bare colons as
    // column scopes. Wrapping in double quotes turns the input into a
    // single phrase token that matches the literal characters.
    let sql = format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE {table} MATCH ?1)");
    let phrase = format!("\"{query}\"");
    conn.query_row(&sql, params![phrase], |r| r.get(0))
        .expect("fts match query")
}

#[test]
fn fts_and_embeddings_tables_exist_after_migration() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    let ctx = db::open_project(temp.path()).expect("open project");

    for name in ["facts_fts", "decisions_fts", "tasks_fts", "embeddings"] {
        let count: i64 = ctx
            .conn
            .query_row(
                "SELECT COUNT(*) FROM sqlite_master WHERE name = ?1",
                params![name],
                |r| r.get(0),
            )
            .expect("query sqlite_master");
        assert_eq!(count, 1, "{name} should exist after migration");
    }
}

#[test]
fn inserting_fact_populates_facts_fts_via_trigger() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    fact::add(
        temp.path(),
        "build-command",
        "cargo nextest run --all-features",
        "user",
        "cli:user",
    )
    .expect("add fact");

    let ctx = db::open_project(temp.path()).expect("open project");
    assert!(fts_hit(&ctx.conn, "facts_fts", "nextest"));
    assert!(fts_hit(&ctx.conn, "facts_fts", "build-command"));
}

#[test]
fn updating_fact_swaps_old_value_for_new_in_fts() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    fact::add(
        temp.path(),
        "build-command",
        "cargo nextest run",
        "user",
        "cli:user",
    )
    .expect("add fact v1");

    // Re-add with the same key -> writer performs an UPSERT (UPDATE path).
    fact::add(
        temp.path(),
        "build-command",
        "cargo flamegraph build",
        "user",
        "cli:user",
    )
    .expect("add fact v2");

    let ctx = db::open_project(temp.path()).expect("open project");
    assert!(
        !fts_hit(&ctx.conn, "facts_fts", "nextest"),
        "old value should be evicted from fts after update"
    );
    assert!(
        fts_hit(&ctx.conn, "facts_fts", "flamegraph"),
        "new value should be present in fts after update"
    );
}

#[test]
fn deleting_fact_removes_it_from_fts() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    fact::add(
        temp.path(),
        "telemetry-token",
        "doppleganger-sentinel-alpha",
        "user",
        "cli:user",
    )
    .expect("add fact");

    let ctx = db::open_project(temp.path()).expect("open project");
    assert!(fts_hit(&ctx.conn, "facts_fts", "doppleganger"));
    ctx.conn
        .execute(
            "DELETE FROM facts WHERE key = ?1",
            params!["telemetry-token"],
        )
        .expect("delete fact");
    assert!(
        !fts_hit(&ctx.conn, "facts_fts", "doppleganger"),
        "deleted fact must not match"
    );
}

#[test]
fn inserting_decision_populates_decisions_fts() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    decision::add(
        temp.path(),
        "Adopt parquet for cold storage",
        "Row-group filtering matches our analytical access pattern.",
        "user",
        "cli:user",
    )
    .expect("add decision");

    let ctx = db::open_project(temp.path()).expect("open project");
    assert!(fts_hit(&ctx.conn, "decisions_fts", "parquet"));
    assert!(fts_hit(&ctx.conn, "decisions_fts", "row-group"));
}

#[test]
fn inserting_task_populates_tasks_fts_with_notes() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    task::add(
        temp.path(),
        "Migrate ingestion to streaming",
        Some("Bottleneck is the legacy batch reducer in shard-7"),
        "cli:user",
    )
    .expect("add task");

    let ctx = db::open_project(temp.path()).expect("open project");
    assert!(fts_hit(&ctx.conn, "tasks_fts", "streaming"));
    assert!(fts_hit(&ctx.conn, "tasks_fts", "shard-7"));
}

#[test]
fn fts_rebuild_repopulates_after_index_wipe() {
    // Simulates the backfill path: existing source rows, FTS index cleared,
    // 'rebuild' command repopulates from the source tables.
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    fact::add(
        temp.path(),
        "deploy-host",
        "prod-edge-42.us-west",
        "user",
        "cli:user",
    )
    .expect("add fact");
    decision::add(
        temp.path(),
        "Pin tokio to 1.48",
        "Avoid the 1.49 IO-driver regression hitting our hotspots.",
        "user",
        "cli:user",
    )
    .expect("add decision");
    task::add(
        temp.path(),
        "Backport hotfix",
        Some("Cherry-pick 0xa3f1 into release/2025.04"),
        "cli:user",
    )
    .expect("add task");

    let ctx = db::open_project(temp.path()).expect("open project");
    for (table, term) in [
        ("facts_fts", "prod-edge-42"),
        ("decisions_fts", "regression"),
        ("tasks_fts", "cherry-pick"),
    ] {
        ctx.conn
            .execute_batch(&format!("INSERT INTO {table}({table}) VALUES('delete-all');"))
            .expect("clear fts");
        assert!(
            !fts_hit(&ctx.conn, table, term),
            "{table} should be empty after delete-all"
        );
        ctx.conn
            .execute_batch(&format!("INSERT INTO {table}({table}) VALUES('rebuild');"))
            .expect("rebuild fts");
        assert!(
            fts_hit(&ctx.conn, table, term),
            "{table} should contain '{term}' after rebuild"
        );
    }
}

#[test]
fn embeddings_blob_round_trips() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    fact::add(
        temp.path(),
        "deploy-host",
        "prod-edge-42",
        "user",
        "cli:user",
    )
    .expect("add fact");

    let ctx = db::open_project(temp.path()).expect("open project");
    let fact_id: i64 = ctx
        .conn
        .query_row(
            "SELECT id FROM facts WHERE key = ?1",
            params!["deploy-host"],
            |r| r.get(0),
        )
        .expect("fact id");

    // Fake 384-dim vector as little-endian f32 bytes.
    let vector: Vec<f32> = (0..384).map(|i| (i as f32) * 0.0001).collect();
    let mut blob = Vec::with_capacity(vector.len() * 4);
    for v in &vector {
        blob.extend_from_slice(&v.to_le_bytes());
    }
    let blob_for_insert = blob.clone();

    ctx.conn
        .execute(
            "INSERT INTO embeddings(project_id, source_type, source_id, model_name, dimension, vector, content_hash)
             VALUES (1, 'fact', ?1, 'bge-small-en-v1.5', 384, ?2, 'abc123')",
            params![fact_id, blob_for_insert],
        )
        .expect("insert embedding");

    let (dim, fetched_blob, hash): (i64, Vec<u8>, String) = ctx
        .conn
        .query_row(
            "SELECT dimension, vector, content_hash FROM embeddings
             WHERE source_type = 'fact' AND source_id = ?1 AND model_name = 'bge-small-en-v1.5'",
            params![fact_id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .expect("fetch embedding");

    assert_eq!(dim, 384);
    assert_eq!(hash, "abc123");
    assert_eq!(fetched_blob.len(), blob.len());
    assert_eq!(fetched_blob, blob);
}

#[test]
fn embeddings_unique_constraint_blocks_duplicate() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    fact::add(temp.path(), "k", "v", "user", "cli:user").expect("add fact");

    let ctx = db::open_project(temp.path()).expect("open project");
    let fact_id: i64 = ctx
        .conn
        .query_row("SELECT id FROM facts WHERE key = 'k'", [], |r| r.get(0))
        .expect("fact id");

    let blob = vec![0u8; 4];
    ctx.conn
        .execute(
            "INSERT INTO embeddings(project_id, source_type, source_id, model_name, dimension, vector, content_hash)
             VALUES (1, 'fact', ?1, 'bge-small-en-v1.5', 1, ?2, 'h')",
            params![fact_id, blob],
        )
        .expect("first insert");

    let err = ctx
        .conn
        .execute(
            "INSERT INTO embeddings(project_id, source_type, source_id, model_name, dimension, vector, content_hash)
             VALUES (1, 'fact', ?1, 'bge-small-en-v1.5', 1, ?2, 'h')",
            params![fact_id, blob],
        )
        .expect_err("second insert must violate UNIQUE");

    let message = err.to_string();
    assert!(
        message.contains("UNIQUE") || message.contains("constraint"),
        "expected UNIQUE constraint violation, got: {message}"
    );
}

#[test]
fn embeddings_source_type_check_rejects_unknown_kinds() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    let ctx = db::open_project(temp.path()).expect("open project");

    let err = ctx
        .conn
        .execute(
            "INSERT INTO embeddings(project_id, source_type, source_id, model_name, dimension, vector, content_hash)
             VALUES (1, 'session_note', 99, 'bge-small-en-v1.5', 1, X'00', 'h')",
            [],
        )
        .expect_err("session_note is not a valid source_type");
    let message = err.to_string();
    assert!(
        message.contains("CHECK") || message.contains("constraint"),
        "expected CHECK constraint violation, got: {message}"
    );
}

#[test]
fn embeddings_source_type_check_accepts_doc_chunk_after_0014() {
    // Migration 0014 widened the CHECK to include 'doc_chunk'. The
    // table-rebuild must preserve the constraint shape (still rejecting
    // junk) while admitting the new kind.
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    let ctx = db::open_project(temp.path()).expect("open project");

    ctx.conn
        .execute(
            "INSERT INTO embeddings(project_id, source_type, source_id, model_name, dimension, vector, content_hash)
             VALUES (1, 'doc_chunk', 1, 'bge-small-en-v1.5', 1, X'00', 'h')",
            [],
        )
        .expect("doc_chunk is a valid source_type after migration 0014");
}
