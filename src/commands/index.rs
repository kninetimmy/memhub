//! `memhub index` command surface (M8 PR5).
//!
//! `status` returns a counts/coverage snapshot of the embeddings table
//! against the durable source tables. `rebuild` wipes embeddings for the
//! active model and recomputes them from current source bodies, used
//! after a model upgrade or to clear a `stale_embeddings` warning from
//! `memhub recall`. Both operate locally; no network.

use std::path::Path;
use std::time::Instant;

use rusqlite::{Connection, params};

use crate::Result;
use crate::config::RetrievalMode;
use crate::db;
use crate::retrieval::embeddings::{EMBEDDING_DIMENSION, EMBEDDING_MODEL_NAME, embed_batch};
use crate::retrieval::persist::{
    SourceType, decision_embed_text, fact_embed_text, task_embed_text,
};

#[derive(Debug)]
pub struct IndexStatusSummary {
    pub model: String,
    pub mode: RetrievalMode,
    pub facts_total: i64,
    pub facts_embedded: i64,
    pub decisions_total: i64,
    pub decisions_embedded: i64,
    pub tasks_total: i64,
    pub tasks_embedded: i64,
    pub total_embeddings: i64,
    pub missing_count: i64,
    pub stale_ratio: f64,
}

#[derive(Debug)]
pub struct RebuildSummary {
    pub model: String,
    pub facts: usize,
    pub decisions: usize,
    pub tasks: usize,
    pub deleted: usize,
    pub elapsed_ms: u128,
}

pub fn status(start: &Path) -> Result<IndexStatusSummary> {
    let ctx = db::open_project(start)?;
    let conn = &ctx.conn;

    let facts_total = count(conn, "SELECT COUNT(*) FROM facts")?;
    let decisions_total = count(conn, "SELECT COUNT(*) FROM decisions")?;
    let tasks_total = count(conn, "SELECT COUNT(*) FROM tasks")?;

    let facts_embedded = count_embedded(conn, "fact")?;
    let decisions_embedded = count_embedded(conn, "decision")?;
    let tasks_embedded = count_embedded(conn, "task")?;

    let total_embeddings: i64 = conn.query_row(
        "SELECT COUNT(*) FROM embeddings WHERE model_name = ?1",
        params![EMBEDDING_MODEL_NAME],
        |row| row.get(0),
    )?;

    let source_rows = facts_total + decisions_total + tasks_total;
    let embedded_rows = facts_embedded + decisions_embedded + tasks_embedded;
    let missing_count = (source_rows - embedded_rows).max(0);
    let stale_ratio = if source_rows == 0 {
        0.0
    } else {
        missing_count as f64 / source_rows as f64
    };

    Ok(IndexStatusSummary {
        model: EMBEDDING_MODEL_NAME.to_string(),
        mode: ctx.config.retrieval.mode,
        facts_total,
        facts_embedded,
        decisions_total,
        decisions_embedded,
        tasks_total,
        tasks_embedded,
        total_embeddings,
        missing_count,
        stale_ratio,
    })
}

fn count(conn: &Connection, sql: &str) -> Result<i64> {
    let n: i64 = conn.query_row(sql, params![], |row| row.get(0))?;
    Ok(n)
}

fn count_embedded(conn: &Connection, source_type: &str) -> Result<i64> {
    let n: i64 = conn.query_row(
        "SELECT COUNT(*) FROM embeddings WHERE source_type = ?1 AND model_name = ?2",
        params![source_type, EMBEDDING_MODEL_NAME],
        |row| row.get(0),
    )?;
    Ok(n)
}

pub fn rebuild(start: &Path, actor: &str) -> Result<RebuildSummary> {
    let started = Instant::now();

    // Collect all source rows up front so we don't hold a transaction
    // open while the model warms up (~150 ms on first call).
    let rows = {
        let ctx = db::open_project(start)?;
        collect_source_rows(&ctx.conn)?
    };

    // Embed in one batch per source type to amortize model overhead.
    let mut facts_vectors: Vec<(i64, Vec<f32>, String)> = Vec::new();
    let mut decisions_vectors: Vec<(i64, Vec<f32>, String)> = Vec::new();
    let mut tasks_vectors: Vec<(i64, Vec<f32>, String)> = Vec::new();

    if !rows.facts.is_empty() {
        let texts: Vec<String> = rows
            .facts
            .iter()
            .map(|(_, k, v)| fact_embed_text(k, v))
            .collect();
        let vectors = embed_batch(&texts)?;
        for ((id, _, _), (text, vector)) in
            rows.facts.iter().zip(texts.into_iter().zip(vectors.into_iter()))
        {
            facts_vectors.push((*id, vector, text));
        }
    }
    if !rows.decisions.is_empty() {
        let texts: Vec<String> = rows
            .decisions
            .iter()
            .map(|(_, t, r)| decision_embed_text(t, r))
            .collect();
        let vectors = embed_batch(&texts)?;
        for ((id, _, _), (text, vector)) in rows
            .decisions
            .iter()
            .zip(texts.into_iter().zip(vectors.into_iter()))
        {
            decisions_vectors.push((*id, vector, text));
        }
    }
    if !rows.tasks.is_empty() {
        let texts: Vec<String> = rows
            .tasks
            .iter()
            .map(|(_, t, n)| task_embed_text(t, n.as_deref()))
            .collect();
        let vectors = embed_batch(&texts)?;
        for ((id, _, _), (text, vector)) in rows
            .tasks
            .iter()
            .zip(texts.into_iter().zip(vectors.into_iter()))
        {
            tasks_vectors.push((*id, vector, text));
        }
    }

    // Single transaction: clear active-model rows, then UPSERT fresh.
    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;
    let deleted: usize = tx.execute(
        "DELETE FROM embeddings WHERE model_name = ?1",
        params![EMBEDDING_MODEL_NAME],
    )?;

    upsert_batch(&tx, SourceType::Fact, &facts_vectors)?;
    upsert_batch(&tx, SourceType::Decision, &decisions_vectors)?;
    upsert_batch(&tx, SourceType::Task, &tasks_vectors)?;

    db::log_write(
        &tx,
        actor,
        "embeddings",
        None,
        "rebuild",
        &format!(
            "index rebuild: model={} facts={} decisions={} tasks={}",
            EMBEDDING_MODEL_NAME,
            facts_vectors.len(),
            decisions_vectors.len(),
            tasks_vectors.len(),
        ),
    )?;
    tx.commit()?;

    Ok(RebuildSummary {
        model: EMBEDDING_MODEL_NAME.to_string(),
        facts: facts_vectors.len(),
        decisions: decisions_vectors.len(),
        tasks: tasks_vectors.len(),
        deleted,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

struct CollectedRows {
    facts: Vec<(i64, String, String)>,
    decisions: Vec<(i64, String, String)>,
    tasks: Vec<(i64, String, Option<String>)>,
}

fn collect_source_rows(conn: &Connection) -> Result<CollectedRows> {
    let mut facts = Vec::new();
    let mut stmt = conn.prepare("SELECT id, key, value FROM facts ORDER BY id")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        facts.push(row?);
    }

    let mut decisions = Vec::new();
    let mut stmt = conn.prepare("SELECT id, title, rationale FROM decisions ORDER BY id")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        decisions.push(row?);
    }

    let mut tasks = Vec::new();
    let mut stmt = conn.prepare("SELECT id, title, notes FROM tasks ORDER BY id")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, Option<String>>(2)?,
        ))
    })?;
    for row in rows {
        tasks.push(row?);
    }

    Ok(CollectedRows {
        facts,
        decisions,
        tasks,
    })
}

fn upsert_batch(
    tx: &rusqlite::Transaction<'_>,
    source_type: SourceType,
    rows: &[(i64, Vec<f32>, String)],
) -> Result<()> {
    if rows.is_empty() {
        return Ok(());
    }
    let mut stmt = tx.prepare(
        "INSERT INTO embeddings(
            project_id, source_type, source_id, model_name,
            dimension, vector, content_hash
         ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    let st = source_type.as_str();
    for (id, vector, text) in rows {
        let blob = vector_to_le_bytes(vector);
        let hash = sha256_hex(text);
        stmt.execute(params![
            st,
            id,
            EMBEDDING_MODEL_NAME,
            EMBEDDING_DIMENSION as i64,
            blob,
            hash,
        ])?;
    }
    Ok(())
}

fn vector_to_le_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{decision, fact, init, task};
    use rusqlite::params;
    use tempfile::tempdir;

    #[test]
    fn status_reports_zero_coverage_under_fts_mode() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        fact::add(temp.path(), "build-command", "cargo build", "user", "cli:user")
            .expect("fact");
        decision::add(
            temp.path(),
            "Stage agent writes",
            "Require human review for durable rows.",
            "user",
            "cli:user",
        )
        .expect("decision");
        task::add(temp.path(), "Ship M8", Some("PR5 in flight"), "cli:user").expect("task");

        let summary = status(temp.path()).expect("status");
        assert_eq!(summary.facts_total, 1);
        assert_eq!(summary.decisions_total, 1);
        assert_eq!(summary.tasks_total, 1);
        assert_eq!(summary.facts_embedded, 0);
        assert_eq!(summary.decisions_embedded, 0);
        assert_eq!(summary.tasks_embedded, 0);
        assert_eq!(summary.missing_count, 3);
        assert!((summary.stale_ratio - 1.0).abs() < 1e-9);
    }

    #[test]
    fn rebuild_populates_embeddings_for_existing_rows() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        fact::add(temp.path(), "build-command", "cargo build", "user", "cli:user")
            .expect("fact");
        decision::add(
            temp.path(),
            "Adopt hybrid recall",
            "Blend FTS and vector scoring.",
            "user",
            "cli:user",
        )
        .expect("decision");
        task::add(temp.path(), "Ship recall", None, "cli:user").expect("task");

        let summary = rebuild(temp.path(), "cli:user").expect("rebuild");
        assert_eq!(summary.facts, 1);
        assert_eq!(summary.decisions, 1);
        assert_eq!(summary.tasks, 1);

        let after = status(temp.path()).expect("status");
        assert_eq!(after.facts_embedded, 1);
        assert_eq!(after.decisions_embedded, 1);
        assert_eq!(after.tasks_embedded, 1);
        assert_eq!(after.missing_count, 0);
    }

    #[test]
    fn rebuild_logs_one_writes_log_entry() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        fact::add(temp.path(), "k", "v", "user", "cli:user").expect("seed");

        rebuild(temp.path(), "claude-code:reindex").expect("rebuild");

        let ctx = db::open_project(temp.path()).expect("open");
        let (actor, reason): (String, String) = ctx
            .conn
            .query_row(
                "SELECT actor, reason FROM writes_log
                 WHERE table_name = 'embeddings'
                 ORDER BY id DESC LIMIT 1",
                params![],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("writes_log row");
        assert_eq!(actor, "claude-code:reindex");
        assert!(reason.contains("rebuild"));
    }

    #[test]
    fn rebuild_replaces_stale_embeddings_after_source_drift() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        fact::add(temp.path(), "k", "value-one", "user", "cli:user").expect("seed");
        rebuild(temp.path(), "cli:user").expect("first");

        let before_hash: String = {
            let ctx = db::open_project(temp.path()).expect("open");
            ctx.conn
                .query_row(
                    "SELECT content_hash FROM embeddings WHERE source_type = 'fact'",
                    params![],
                    |r| r.get(0),
                )
                .expect("hash")
        };

        // Mutate the row out-of-band (simulating model upgrade or direct edit)
        // so that the embedding hash no longer matches current text.
        {
            let ctx = db::open_project(temp.path()).expect("open");
            ctx.conn
                .execute(
                    "UPDATE facts SET value = 'value-two' WHERE key = 'k'",
                    params![],
                )
                .expect("mutate");
        }

        rebuild(temp.path(), "cli:user").expect("second");
        let after_hash: String = {
            let ctx = db::open_project(temp.path()).expect("open");
            ctx.conn
                .query_row(
                    "SELECT content_hash FROM embeddings WHERE source_type = 'fact'",
                    params![],
                    |r| r.get(0),
                )
                .expect("hash")
        };
        assert_ne!(before_hash, after_hash);
    }
}
