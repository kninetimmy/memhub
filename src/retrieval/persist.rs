//! Eager-embed write path for retrieval (M8 PR3).
//!
//! Fact/decision/task writers call into this module inside the same DB
//! transaction as the source-row write. Per addendum §5 the flow is:
//!   1. Compute a stable content_hash over the embed text.
//!   2. Look up the existing embedding row for (source_type, source_id,
//!      active model).
//!   3. If the hash matches, no-op (the embedding is already current).
//!   4. Otherwise run the embedding model and UPSERT the row inside the
//!      same transaction.
//!
//! When [`RetrievalMode::Fts`] is configured the whole path is a no-op:
//! the embedding model never loads and the embeddings table stays empty.
//! Switching to [`RetrievalMode::Hybrid`] on an existing repo will need
//! a one-shot backfill (`memhub index rebuild` — not yet shipped) before
//! the embeddings table is fully populated.

use rusqlite::{OptionalExtension, Transaction, params};

use crate::config::RetrievalMode;
use crate::retrieval::embeddings::{
    EMBEDDING_DIMENSION, EMBEDDING_MODEL_NAME, embed_batch, embed_one,
};
use crate::retrieval::util::{sha256_hex, vector_to_le_bytes};
use crate::{MemhubError, Result};

/// Source row kind that an embedding refers back to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceType {
    Fact,
    Decision,
    Task,
    DocChunk,
}

impl SourceType {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceType::Fact => "fact",
            SourceType::Decision => "decision",
            SourceType::Task => "task",
            SourceType::DocChunk => "doc_chunk",
        }
    }
}

/// Build the embed text for a fact. Embeds key + value so the model sees
/// the label and the body in one short sentence-like string.
pub fn fact_embed_text(key: &str, value: &str) -> String {
    format!("{key}: {value}")
}

/// Build the embed text for a decision. When `summary` is present and
/// non-empty, it is prepended so the bi-encoder picks up the natural-
/// language framing in addition to the (often jargon-heavy) title. Title
/// and rationale are separated by a blank line so the tokenizer sees
/// them as distinct segments. See decision 72 / task #23.
pub fn decision_embed_text(title: &str, rationale: &str, summary: Option<&str>) -> String {
    match summary {
        Some(s) if !s.trim().is_empty() => format!("{s}\n\n{title}\n\n{rationale}"),
        _ => format!("{title}\n\n{rationale}"),
    }
}

/// Build the embed text for a task. Notes are optional; when present they
/// follow the title separated by a blank line.
pub fn task_embed_text(title: &str, notes: Option<&str>) -> String {
    match notes {
        Some(n) if !n.is_empty() => format!("{title}\n\n{n}"),
        _ => title.to_string(),
    }
}

/// Build the embed text for a doc chunk. The heading-path breadcrumb
/// (e.g. `Components > Buttons`) acts as the title-analog so the
/// bi-encoder and cross-encoder both see the section context, not just
/// the prose body. Mirrors the `title\n\nbody` shape used for tasks.
pub fn doc_chunk_embed_text(heading_path: &str, body: &str) -> String {
    if heading_path.trim().is_empty() {
        body.to_string()
    } else {
        format!("{heading_path}\n\n{body}")
    }
}

/// Eager-embed entry point. No-op when mode != Hybrid.
pub fn eager_embed_in_tx(
    tx: &Transaction<'_>,
    mode: RetrievalMode,
    source_type: SourceType,
    source_id: i64,
    embed_text: &str,
) -> Result<()> {
    if mode != RetrievalMode::Hybrid {
        return Ok(());
    }

    let content_hash = sha256_hex(embed_text.as_bytes());
    let source_type_str = source_type.as_str();

    let existing_hash: Option<String> = tx
        .query_row(
            "SELECT content_hash FROM embeddings
             WHERE source_type = ?1 AND source_id = ?2 AND model_name = ?3",
            params![source_type_str, source_id, EMBEDDING_MODEL_NAME],
            |row| row.get(0),
        )
        .optional()?;

    if existing_hash.as_deref() == Some(content_hash.as_str()) {
        return Ok(());
    }

    let vector = embed_one(embed_text)?;
    if vector.len() != EMBEDDING_DIMENSION {
        return Err(MemhubError::Embedding(format!(
            "expected {EMBEDDING_DIMENSION}-dim vector, got {}",
            vector.len()
        )));
    }
    let blob = vector_to_le_bytes(&vector);

    tx.execute(
        "DELETE FROM embeddings
         WHERE source_type = ?1 AND source_id = ?2 AND model_name = ?3",
        params![source_type_str, source_id, EMBEDDING_MODEL_NAME],
    )?;
    tx.execute(
        "INSERT INTO embeddings(
            project_id, source_type, source_id, model_name,
            dimension, vector, content_hash
        ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            source_type_str,
            source_id,
            EMBEDDING_MODEL_NAME,
            EMBEDDING_DIMENSION as i64,
            blob,
            content_hash,
        ],
    )?;

    Ok(())
}

/// Batched counterpart to [`eager_embed_in_tx`] for callers that insert many
/// rows of the same `source_type` in one transaction (currently doc-chunk
/// ingest, see `commands::doc::insert_chunks`). Same semantics row for
/// row — mode-gated no-op, unchanged-content-hash rows skipped, DELETE then
/// INSERT the vector — but every row still needing an embedding is sent to
/// the model in a single [`embed_batch`] call instead of one model call per
/// row, which is where the per-chunk cost actually lives (batching amortizes
/// ONNX inference overhead; the SQLite hash-check stays per-row because it's
/// cheap and lets an unchanged row skip embedding entirely).
pub fn eager_embed_batch_in_tx(
    tx: &Transaction<'_>,
    mode: RetrievalMode,
    source_type: SourceType,
    rows: Vec<(i64, String)>,
) -> Result<()> {
    if mode != RetrievalMode::Hybrid || rows.is_empty() {
        return Ok(());
    }

    let source_type_str = source_type.as_str();

    // Same skip-if-unchanged check as the single-row path, run up front so
    // only rows that actually need a new vector go into the batch call.
    let mut pending: Vec<(i64, String, String)> = Vec::with_capacity(rows.len());
    for (source_id, embed_text) in rows {
        let content_hash = sha256_hex(embed_text.as_bytes());
        let existing_hash: Option<String> = tx
            .query_row(
                "SELECT content_hash FROM embeddings
                 WHERE source_type = ?1 AND source_id = ?2 AND model_name = ?3",
                params![source_type_str, source_id, EMBEDDING_MODEL_NAME],
                |row| row.get(0),
            )
            .optional()?;
        if existing_hash.as_deref() == Some(content_hash.as_str()) {
            continue;
        }
        pending.push((source_id, embed_text, content_hash));
    }

    if pending.is_empty() {
        return Ok(());
    }

    let texts: Vec<&str> = pending.iter().map(|(_, t, _)| t.as_str()).collect();
    let vectors = embed_batch(&texts)?;

    for ((source_id, _, content_hash), vector) in pending.iter().zip(vectors) {
        if vector.len() != EMBEDDING_DIMENSION {
            return Err(MemhubError::Embedding(format!(
                "expected {EMBEDDING_DIMENSION}-dim vector, got {}",
                vector.len()
            )));
        }
        let blob = vector_to_le_bytes(&vector);

        tx.execute(
            "DELETE FROM embeddings
             WHERE source_type = ?1 AND source_id = ?2 AND model_name = ?3",
            params![source_type_str, source_id, EMBEDDING_MODEL_NAME],
        )?;
        tx.execute(
            "INSERT INTO embeddings(
                project_id, source_type, source_id, model_name,
                dimension, vector, content_hash
            ) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                source_type_str,
                source_id,
                EMBEDDING_MODEL_NAME,
                EMBEDDING_DIMENSION as i64,
                blob,
                content_hash,
            ],
        )?;
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_embed_text_without_summary_matches_legacy_format() {
        let out = decision_embed_text("Title", "Rationale body", None);
        assert_eq!(out, "Title\n\nRationale body");
    }

    #[test]
    fn decision_embed_text_with_summary_prepends_it() {
        let out = decision_embed_text(
            "content_hash drift detection per embedding",
            "Store a hash of source body alongside each vector.",
            Some("How does memhub know when an embedding has gone stale?"),
        );
        assert_eq!(
            out,
            "How does memhub know when an embedding has gone stale?\n\n\
             content_hash drift detection per embedding\n\n\
             Store a hash of source body alongside each vector."
        );
    }

    #[test]
    fn decision_embed_text_treats_empty_or_whitespace_summary_as_absent() {
        // Empty and whitespace-only summaries must not pollute the embed
        // text with a leading blank, which would change content_hash and
        // force a spurious re-embed across every machine.
        assert_eq!(decision_embed_text("T", "R", Some("")), "T\n\nR");
        assert_eq!(decision_embed_text("T", "R", Some("   \n  ")), "T\n\nR");
    }

    // eager_embed_batch_in_tx has no real FK on embeddings.source_id (see
    // migration 0014's comment — the key is the polymorphic (source_type,
    // source_id) pair), so these unit tests exercise it directly against
    // synthetic source_ids rather than needing real doc_chunks rows.

    #[test]
    fn eager_embed_batch_in_tx_is_noop_outside_hybrid_mode() {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        let mut ctx = crate::db::open_project(temp.path()).expect("open");
        let tx = ctx.conn.transaction().expect("tx");

        eager_embed_batch_in_tx(
            &tx,
            RetrievalMode::Fts,
            SourceType::DocChunk,
            vec![(1, "some text".to_string())],
        )
        .expect("noop");

        let count: i64 = tx
            .query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 0, "Fts mode must never write an embedding row");
    }

    #[test]
    fn eager_embed_batch_in_tx_skips_unchanged_and_reembeds_changed() {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        let mut ctx = crate::db::open_project(temp.path()).expect("open");
        let tx = ctx.conn.transaction().expect("tx");

        eager_embed_batch_in_tx(
            &tx,
            RetrievalMode::Hybrid,
            SourceType::DocChunk,
            vec![(1, "alpha text".to_string()), (2, "beta text".to_string())],
        )
        .expect("first batch");
        let count: i64 = tx
            .query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))
            .expect("count after first batch");
        assert_eq!(count, 2);
        let hash1: String = tx
            .query_row(
                "SELECT content_hash FROM embeddings
                 WHERE source_type = 'doc_chunk' AND source_id = 1",
                [],
                |r| r.get(0),
            )
            .expect("hash1");

        // Row 1 unchanged, row 2 changed, plus a brand new row 3 in the
        // same batch call — must skip 1, re-embed 2, insert 3.
        eager_embed_batch_in_tx(
            &tx,
            RetrievalMode::Hybrid,
            SourceType::DocChunk,
            vec![
                (1, "alpha text".to_string()),
                (2, "beta text CHANGED".to_string()),
                (3, "gamma text".to_string()),
            ],
        )
        .expect("second batch");

        let count: i64 = tx
            .query_row("SELECT COUNT(*) FROM embeddings", [], |r| r.get(0))
            .expect("count after second batch");
        assert_eq!(
            count, 3,
            "row 1 skip + row 2 update + row 3 insert => 3 total rows"
        );

        let hash1_again: String = tx
            .query_row(
                "SELECT content_hash FROM embeddings
                 WHERE source_type = 'doc_chunk' AND source_id = 1",
                [],
                |r| r.get(0),
            )
            .expect("hash1 again");
        assert_eq!(
            hash1, hash1_again,
            "unchanged row must be skipped, not re-embedded"
        );

        let hash2: String = tx
            .query_row(
                "SELECT content_hash FROM embeddings
                 WHERE source_type = 'doc_chunk' AND source_id = 2",
                [],
                |r| r.get(0),
            )
            .expect("hash2");
        assert_eq!(hash2, sha256_hex("beta text CHANGED".as_bytes()));
    }
}
