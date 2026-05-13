//! Retrieval layer for memhub M8 (SQL+RAG hybrid recall).
//!
//! Owns the bundled embedding model. The hybrid scoring path, FTS5
//! integration, and `recall` surface are not part of this module yet —
//! PR1 ships only the embedding primitive.

pub mod embeddings;
pub mod persist;
pub mod recall;

pub use embeddings::{EMBEDDING_DIMENSION, EMBEDDING_MODEL_NAME, embed_batch, embed_one};
pub use persist::{
    SourceType, decision_embed_text, eager_embed_in_tx, fact_embed_text, task_embed_text,
};
pub use recall::{RecallHit, RecallOptions, RecallResponse, RecallWarning, recall};
