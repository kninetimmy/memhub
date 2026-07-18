//! `memhub index` command surface (M8 PR5).
//!
//! `status` returns a counts/coverage snapshot of the embeddings table
//! against the durable source tables. `rebuild` wipes embeddings for the
//! active model and recomputes them from current source bodies, used
//! after a model upgrade or to clear a `stale_embeddings` warning from
//! `memhub recall`. Both operate locally; no network.

use std::path::Path;
use std::time::Instant;

use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;
use crate::config::RetrievalMode;
use crate::db;
use crate::retrieval::embeddings::{EMBEDDING_DIMENSION, EMBEDDING_MODEL_NAME, embed_batch};
use crate::retrieval::persist::{
    SourceType, decision_embed_text, doc_chunk_embed_text, fact_embed_text, note_embed_text,
    task_embed_text,
};
use crate::retrieval::util::{sha256_hex, vector_to_le_bytes};

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
    pub doc_chunks_total: i64,
    pub doc_chunks_embedded: i64,
    pub notes_total: i64,
    pub notes_embedded: i64,
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
    pub doc_chunks: usize,
    pub notes: usize,
    pub deleted: usize,
    pub elapsed_ms: u128,
}

pub fn status(start: &Path) -> Result<IndexStatusSummary> {
    let ctx = db::open_project(start)?;
    let conn = &ctx.conn;
    let rows = collect_source_rows(conn)?;

    let facts_total = rows.facts.len() as i64;
    let decisions_total = rows.decisions.len() as i64;
    let tasks_total = rows.tasks.len() as i64;
    let doc_chunks_total = rows.doc_chunks.len() as i64;
    let notes_total = rows.notes.len() as i64;

    let facts_embedded = count_current_fact_embeddings(conn, &rows.facts)?;
    let decisions_embedded = count_current_decision_embeddings(conn, &rows.decisions)?;
    let tasks_embedded = count_current_task_embeddings(conn, &rows.tasks)?;
    let doc_chunks_embedded = count_current_doc_chunk_embeddings(conn, &rows.doc_chunks)?;
    let notes_embedded = count_current_note_embeddings(conn, &rows.notes)?;

    let total_embeddings: i64 = conn.query_row(
        "SELECT COUNT(*) FROM embeddings WHERE model_name = ?1",
        params![EMBEDDING_MODEL_NAME],
        |row| row.get(0),
    )?;

    let source_rows = facts_total + decisions_total + tasks_total + doc_chunks_total + notes_total;
    let current_rows =
        facts_embedded + decisions_embedded + tasks_embedded + doc_chunks_embedded + notes_embedded;
    let missing_count = (source_rows - current_rows).max(0);
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
        doc_chunks_total,
        doc_chunks_embedded,
        notes_total,
        notes_embedded,
        total_embeddings,
        missing_count,
        stale_ratio,
    })
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
    let mut doc_chunks_vectors: Vec<(i64, Vec<f32>, String)> = Vec::new();
    let mut notes_vectors: Vec<(i64, Vec<f32>, String)> = Vec::new();

    if !rows.facts.is_empty() {
        let texts: Vec<String> = rows
            .facts
            .iter()
            .map(|(_, k, v)| fact_embed_text(k, v))
            .collect();
        let vectors = embed_batch(&texts)?;
        for ((id, _, _), (text, vector)) in rows.facts.iter().zip(texts.into_iter().zip(vectors)) {
            facts_vectors.push((*id, vector, text));
        }
    }
    if !rows.decisions.is_empty() {
        let texts: Vec<String> = rows
            .decisions
            .iter()
            .map(|(_, t, r, s)| decision_embed_text(t, r, s.as_deref()))
            .collect();
        let vectors = embed_batch(&texts)?;
        for ((id, _, _, _), (text, vector)) in
            rows.decisions.iter().zip(texts.into_iter().zip(vectors))
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
        for ((id, _, _), (text, vector)) in rows.tasks.iter().zip(texts.into_iter().zip(vectors)) {
            tasks_vectors.push((*id, vector, text));
        }
    }
    if !rows.doc_chunks.is_empty() {
        let texts: Vec<String> = rows
            .doc_chunks
            .iter()
            .map(|(_, h, b)| doc_chunk_embed_text(h, b))
            .collect();
        let vectors = embed_batch(&texts)?;
        for ((id, _, _), (text, vector)) in
            rows.doc_chunks.iter().zip(texts.into_iter().zip(vectors))
        {
            doc_chunks_vectors.push((*id, vector, text));
        }
    }
    if !rows.notes.is_empty() {
        let texts: Vec<String> = rows.notes.iter().map(|(_, t)| note_embed_text(t)).collect();
        let vectors = embed_batch(&texts)?;
        for ((id, _), (text, vector)) in rows.notes.iter().zip(texts.into_iter().zip(vectors)) {
            notes_vectors.push((*id, vector, text));
        }
    }

    // Single transaction: prune orphaned active-model rows, then UPSERT
    // vectors only when the source row still matches the snapshot that was
    // embedded. This avoids overwriting fresher eager embeddings from
    // concurrent writes.
    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;
    let deleted = delete_orphan_embeddings(&tx)?;

    let facts_written = upsert_batch(&tx, SourceType::Fact, &facts_vectors)?;
    let decisions_written = upsert_batch(&tx, SourceType::Decision, &decisions_vectors)?;
    let tasks_written = upsert_batch(&tx, SourceType::Task, &tasks_vectors)?;
    let doc_chunks_written = upsert_batch(&tx, SourceType::DocChunk, &doc_chunks_vectors)?;
    let notes_written = upsert_batch(&tx, SourceType::Note, &notes_vectors)?;

    db::log_write(
        &tx,
        actor,
        "embeddings",
        None,
        "rebuild",
        &format!(
            "index rebuild: model={} facts={} decisions={} tasks={} doc_chunks={} notes={}",
            EMBEDDING_MODEL_NAME,
            facts_written,
            decisions_written,
            tasks_written,
            doc_chunks_written,
            notes_written,
        ),
    )?;
    tx.commit()?;

    Ok(RebuildSummary {
        model: EMBEDDING_MODEL_NAME.to_string(),
        facts: facts_written,
        decisions: decisions_written,
        tasks: tasks_written,
        doc_chunks: doc_chunks_written,
        notes: notes_written,
        deleted,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

struct CollectedRows {
    facts: Vec<(i64, String, String)>,
    decisions: Vec<(i64, String, String, Option<String>)>,
    tasks: Vec<(i64, String, Option<String>)>,
    doc_chunks: Vec<(i64, String, String)>,
    notes: Vec<(i64, String)>,
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
    let mut stmt =
        conn.prepare("SELECT id, title, rationale, summary FROM decisions ORDER BY id")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, Option<String>>(3)?,
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

    let mut doc_chunks = Vec::new();
    let mut stmt = conn.prepare("SELECT id, heading_path, body FROM doc_chunks ORDER BY id")?;
    let rows = stmt.query_map([], |row| {
        Ok((
            row.get::<_, i64>(0)?,
            row.get::<_, String>(1)?,
            row.get::<_, String>(2)?,
        ))
    })?;
    for row in rows {
        doc_chunks.push(row?);
    }

    let mut notes = Vec::new();
    let mut stmt = conn.prepare("SELECT id, text FROM session_notes ORDER BY id")?;
    let rows = stmt.query_map([], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
    })?;
    for row in rows {
        notes.push(row?);
    }

    Ok(CollectedRows {
        facts,
        decisions,
        tasks,
        doc_chunks,
        notes,
    })
}

fn upsert_batch(
    tx: &rusqlite::Transaction<'_>,
    source_type: SourceType,
    rows: &[(i64, Vec<f32>, String)],
) -> Result<usize> {
    if rows.is_empty() {
        return Ok(0);
    }
    let mut stmt = tx.prepare(
        "INSERT INTO embeddings(
            project_id, source_type, source_id, model_name,
            dimension, vector, content_hash
         ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(source_type, source_id, model_name) DO UPDATE SET
             dimension = excluded.dimension,
             vector = excluded.vector,
             content_hash = excluded.content_hash,
             created_at = CURRENT_TIMESTAMP",
    )?;
    let st = source_type.as_str();
    let mut written = 0usize;
    for (id, vector, text) in rows {
        let hash = sha256_hex(text.as_bytes());
        if current_source_hash(tx, source_type, *id)?.as_deref() != Some(hash.as_str()) {
            continue;
        }
        let blob = vector_to_le_bytes(vector);
        stmt.execute(params![
            st,
            id,
            EMBEDDING_MODEL_NAME,
            EMBEDDING_DIMENSION as i64,
            blob,
            hash,
        ])?;
        written += 1;
    }
    Ok(written)
}

fn delete_orphan_embeddings(tx: &rusqlite::Transaction<'_>) -> Result<usize> {
    let mut deleted = 0usize;
    deleted += tx.execute(
        "DELETE FROM embeddings
         WHERE model_name = ?1
           AND source_type = 'fact'
           AND NOT EXISTS (SELECT 1 FROM facts WHERE facts.id = embeddings.source_id)",
        params![EMBEDDING_MODEL_NAME],
    )?;
    deleted += tx.execute(
        "DELETE FROM embeddings
         WHERE model_name = ?1
           AND source_type = 'decision'
           AND NOT EXISTS (SELECT 1 FROM decisions WHERE decisions.id = embeddings.source_id)",
        params![EMBEDDING_MODEL_NAME],
    )?;
    deleted += tx.execute(
        "DELETE FROM embeddings
         WHERE model_name = ?1
           AND source_type = 'task'
           AND NOT EXISTS (SELECT 1 FROM tasks WHERE tasks.id = embeddings.source_id)",
        params![EMBEDDING_MODEL_NAME],
    )?;
    deleted += tx.execute(
        "DELETE FROM embeddings
         WHERE model_name = ?1
           AND source_type = 'doc_chunk'
           AND NOT EXISTS (SELECT 1 FROM doc_chunks WHERE doc_chunks.id = embeddings.source_id)",
        params![EMBEDDING_MODEL_NAME],
    )?;
    deleted += tx.execute(
        "DELETE FROM embeddings
         WHERE model_name = ?1
           AND source_type = 'note'
           AND NOT EXISTS (SELECT 1 FROM session_notes WHERE session_notes.id = embeddings.source_id)",
        params![EMBEDDING_MODEL_NAME],
    )?;
    Ok(deleted)
}

fn count_current_fact_embeddings(conn: &Connection, rows: &[(i64, String, String)]) -> Result<i64> {
    let mut count = 0;
    for (id, key, value) in rows {
        let text = fact_embed_text(key, value);
        if embedding_matches(conn, SourceType::Fact, *id, &text)? {
            count += 1;
        }
    }
    Ok(count)
}

fn count_current_decision_embeddings(
    conn: &Connection,
    rows: &[(i64, String, String, Option<String>)],
) -> Result<i64> {
    let mut count = 0;
    for (id, title, rationale, summary) in rows {
        let text = decision_embed_text(title, rationale, summary.as_deref());
        if embedding_matches(conn, SourceType::Decision, *id, &text)? {
            count += 1;
        }
    }
    Ok(count)
}

fn count_current_task_embeddings(
    conn: &Connection,
    rows: &[(i64, String, Option<String>)],
) -> Result<i64> {
    let mut count = 0;
    for (id, title, notes) in rows {
        let text = task_embed_text(title, notes.as_deref());
        if embedding_matches(conn, SourceType::Task, *id, &text)? {
            count += 1;
        }
    }
    Ok(count)
}

fn count_current_doc_chunk_embeddings(
    conn: &Connection,
    rows: &[(i64, String, String)],
) -> Result<i64> {
    let mut count = 0;
    for (id, heading_path, body) in rows {
        let text = doc_chunk_embed_text(heading_path, body);
        if embedding_matches(conn, SourceType::DocChunk, *id, &text)? {
            count += 1;
        }
    }
    Ok(count)
}

fn count_current_note_embeddings(conn: &Connection, rows: &[(i64, String)]) -> Result<i64> {
    let mut count = 0;
    for (id, text) in rows {
        let text = note_embed_text(text);
        if embedding_matches(conn, SourceType::Note, *id, &text)? {
            count += 1;
        }
    }
    Ok(count)
}

fn embedding_matches(
    conn: &Connection,
    source_type: SourceType,
    source_id: i64,
    text: &str,
) -> Result<bool> {
    let expected = sha256_hex(text.as_bytes());
    let existing: Option<String> = conn
        .query_row(
            "SELECT content_hash FROM embeddings
             WHERE source_type = ?1
               AND source_id = ?2
               AND model_name = ?3
               AND dimension = ?4",
            params![
                source_type.as_str(),
                source_id,
                EMBEDDING_MODEL_NAME,
                EMBEDDING_DIMENSION as i64,
            ],
            |row| row.get(0),
        )
        .optional()?;
    Ok(existing.as_deref() == Some(expected.as_str()))
}

fn current_source_hash(
    tx: &rusqlite::Transaction<'_>,
    source_type: SourceType,
    source_id: i64,
) -> Result<Option<String>> {
    let text = match source_type {
        SourceType::Fact => tx
            .query_row(
                "SELECT key, value FROM facts WHERE id = ?1",
                params![source_id],
                |row| {
                    let key: String = row.get(0)?;
                    let value: String = row.get(1)?;
                    Ok(fact_embed_text(&key, &value))
                },
            )
            .optional()?,
        SourceType::Decision => tx
            .query_row(
                "SELECT title, rationale, summary FROM decisions WHERE id = ?1",
                params![source_id],
                |row| {
                    let title: String = row.get(0)?;
                    let rationale: String = row.get(1)?;
                    let summary: Option<String> = row.get(2)?;
                    Ok(decision_embed_text(&title, &rationale, summary.as_deref()))
                },
            )
            .optional()?,
        SourceType::Task => tx
            .query_row(
                "SELECT title, notes FROM tasks WHERE id = ?1",
                params![source_id],
                |row| {
                    let title: String = row.get(0)?;
                    let notes: Option<String> = row.get(1)?;
                    Ok(task_embed_text(&title, notes.as_deref()))
                },
            )
            .optional()?,
        SourceType::DocChunk => tx
            .query_row(
                "SELECT heading_path, body FROM doc_chunks WHERE id = ?1",
                params![source_id],
                |row| {
                    let heading_path: String = row.get(0)?;
                    let body: String = row.get(1)?;
                    Ok(doc_chunk_embed_text(&heading_path, &body))
                },
            )
            .optional()?,
        // Mirrors the DocChunk arm above: notes carry no title-analog
        // column, so the embed text is just the note body.
        SourceType::Note => tx
            .query_row(
                "SELECT text FROM session_notes WHERE id = ?1",
                params![source_id],
                |row| {
                    let text: String = row.get(0)?;
                    Ok(note_embed_text(&text))
                },
            )
            .optional()?,
    };
    Ok(text.map(|value| sha256_hex(value.as_bytes())))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{decision, fact, init, session_note, task};
    use crate::config::{ProjectConfig, RetrievalMode};
    use rusqlite::params;
    use tempfile::tempdir;

    fn switch_to_hybrid(repo_root: &std::path::Path) {
        let config_path = repo_root.join(".memhub").join("config.toml");
        let mut cfg = ProjectConfig::load(&config_path).expect("load config");
        cfg.retrieval.mode = RetrievalMode::Hybrid;
        cfg.save(&config_path).expect("save config");
    }

    #[test]
    fn status_reports_zero_coverage_under_fts_mode() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        fact::add(
            temp.path(),
            "build-command",
            "cargo build",
            "user",
            "cli:user",
        )
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
        fact::add(
            temp.path(),
            "build-command",
            "cargo build",
            "user",
            "cli:user",
        )
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

    /// Fix for the review on issue #98's PR: a note logged before hybrid
    /// mode was on (or before migration 0022 existed at all) has no
    /// embedding. `index rebuild` must be able to backfill it — the
    /// documented remedy for a `stale_embeddings` warning — not silently
    /// skip notes forever.
    #[test]
    fn rebuild_backfills_missing_note_embedding() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        // Logged under the default fts mode: no eager embedding is written.
        session_note::add(
            temp.path(),
            "Pre-hybrid scratch note with no embedding yet.",
            "claude-code",
            "claude-code:log_session_note",
        )
        .expect("session note");

        switch_to_hybrid(temp.path());

        let before = status(temp.path()).expect("status before rebuild");
        assert_eq!(before.notes_total, 1);
        assert_eq!(
            before.notes_embedded, 0,
            "note predates hybrid mode and must show as missing coverage"
        );

        let summary = rebuild(temp.path(), "cli:user").expect("rebuild");
        assert_eq!(summary.notes, 1, "rebuild must backfill the missing note");

        let after = status(temp.path()).expect("status after rebuild");
        assert_eq!(after.notes_embedded, 1);
        assert_eq!(
            after.missing_count, 0,
            "rebuild must clear the note from missing coverage"
        );
    }

    /// Companion to the backfill test: a note that already has a live
    /// eager-embedded vector (written by `session_note::add` in hybrid
    /// mode) must survive `rebuild`'s orphan-deletion pass —
    /// `delete_orphan_embeddings`'s `note` branch must key off
    /// `session_notes`, not delete every note embedding indiscriminately.
    #[test]
    fn rebuild_does_not_orphan_delete_live_note_embedding() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        switch_to_hybrid(temp.path());

        let note = session_note::add(
            temp.path(),
            "Hybrid-mode note, eager-embedded on write.",
            "claude-code",
            "claude-code:log_session_note",
        )
        .expect("session note");

        let count_note_embeddings = |temp_path: &std::path::Path| -> i64 {
            let ctx = db::open_project(temp_path).expect("open");
            ctx.conn
                .query_row(
                    "SELECT COUNT(*) FROM embeddings WHERE source_type = 'note' AND source_id = ?1",
                    params![note.id],
                    |r| r.get(0),
                )
                .expect("count note embeddings")
        };
        assert_eq!(
            count_note_embeddings(temp.path()),
            1,
            "eager-embedded on add"
        );

        let summary = rebuild(temp.path(), "cli:user").expect("rebuild");
        assert_eq!(
            summary.notes, 1,
            "rebuild re-confirms the still-current note embedding"
        );
        assert_eq!(
            count_note_embeddings(temp.path()),
            1,
            "live note embedding must survive delete_orphan_embeddings, not be dropped"
        );
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

    #[test]
    fn status_treats_content_hash_drift_as_missing_coverage() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        fact::add(temp.path(), "k", "value-one", "user", "cli:user").expect("seed");
        rebuild(temp.path(), "cli:user").expect("rebuild");

        {
            let ctx = db::open_project(temp.path()).expect("open");
            ctx.conn
                .execute(
                    "UPDATE facts SET value = 'value-two' WHERE key = 'k'",
                    params![],
                )
                .expect("drift source");
        }

        let summary = status(temp.path()).expect("status");
        assert_eq!(summary.facts_total, 1);
        assert_eq!(summary.facts_embedded, 0);
        assert_eq!(summary.missing_count, 1);
        assert!((summary.stale_ratio - 1.0).abs() < 1e-9);
    }

    #[test]
    fn rebuild_snapshot_upsert_does_not_overwrite_newer_eager_embedding() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let cfg_path = temp.path().join(".memhub/config.toml");
        let mut cfg = ProjectConfig::load(&cfg_path).expect("load config");
        cfg.retrieval.mode = RetrievalMode::Hybrid;
        cfg.save(&cfg_path).expect("save config");

        let fact_id = fact::add(temp.path(), "k", "value-one", "user", "cli:user")
            .expect("seed")
            .0;

        let snapshot_rows = {
            let ctx = db::open_project(temp.path()).expect("open");
            collect_source_rows(&ctx.conn).expect("collect")
        };
        let texts: Vec<String> = snapshot_rows
            .facts
            .iter()
            .map(|(_, key, value)| fact_embed_text(key, value))
            .collect();
        let vectors = embed_batch(&texts).expect("embed snapshot");
        let stale_vectors: Vec<(i64, Vec<f32>, String)> = snapshot_rows
            .facts
            .iter()
            .zip(texts.into_iter().zip(vectors))
            .map(|((id, _, _), (text, vector))| (*id, vector, text))
            .collect();

        fact::add(temp.path(), "k", "value-two", "user", "cli:user").expect("update");
        let fresh_hash: String = {
            let ctx = db::open_project(temp.path()).expect("open");
            ctx.conn
                .query_row(
                    "SELECT content_hash FROM embeddings
                     WHERE source_type = 'fact' AND source_id = ?1",
                    params![fact_id],
                    |r| r.get(0),
                )
                .expect("fresh hash")
        };

        let written = {
            let mut ctx = db::open_project(temp.path()).expect("open");
            let tx = ctx.conn.transaction().expect("tx");
            let written =
                upsert_batch(&tx, SourceType::Fact, &stale_vectors).expect("conditional upsert");
            tx.commit().expect("commit");
            written
        };

        let after_hash: String = {
            let ctx = db::open_project(temp.path()).expect("open");
            ctx.conn
                .query_row(
                    "SELECT content_hash FROM embeddings
                     WHERE source_type = 'fact' AND source_id = ?1",
                    params![fact_id],
                    |r| r.get(0),
                )
                .expect("after hash")
        };

        assert_eq!(written, 0);
        assert_eq!(after_hash, fresh_hash);
    }
}
