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
use sha2::{Digest, Sha256};

use crate::config::RetrievalMode;
use crate::retrieval::embeddings::{EMBEDDING_DIMENSION, EMBEDDING_MODEL_NAME, embed_one};
use crate::{MemhubError, Result};

/// Source row kind that an embedding refers back to.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceType {
    Fact,
    Decision,
    Task,
}

impl SourceType {
    pub fn as_str(self) -> &'static str {
        match self {
            SourceType::Fact => "fact",
            SourceType::Decision => "decision",
            SourceType::Task => "task",
        }
    }
}

/// Build the embed text for a fact. Embeds key + value so the model sees
/// the label and the body in one short sentence-like string.
pub fn fact_embed_text(key: &str, value: &str) -> String {
    format!("{key}: {value}")
}

/// Build the embed text for a decision. Title and rationale are separated
/// by a blank line so the tokenizer sees them as distinct segments.
pub fn decision_embed_text(title: &str, rationale: &str) -> String {
    format!("{title}\n\n{rationale}")
}

/// Build the embed text for a task. Notes are optional; when present they
/// follow the title separated by a blank line.
pub fn task_embed_text(title: &str, notes: Option<&str>) -> String {
    match notes {
        Some(n) if !n.is_empty() => format!("{title}\n\n{n}"),
        _ => title.to_string(),
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

    let content_hash = sha256_hex(embed_text);
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

fn sha256_hex(text: &str) -> String {
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

fn vector_to_le_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}
