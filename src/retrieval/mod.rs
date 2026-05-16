//! Retrieval layer for memhub M8 (SQL+RAG hybrid recall).
//!
//! Owns the bundled embedding model and (as of task #21) the bundled
//! cross-encoder re-ranker. The hybrid scoring path lives in `recall`;
//! `rerank` is the optional post-blend reordering step gated by
//! `[retrieval] use_reranker` in project config.

pub mod embeddings;
pub mod persist;
pub mod recall;
pub mod rerank;

pub use embeddings::{EMBEDDING_DIMENSION, EMBEDDING_MODEL_NAME, embed_batch, embed_one};
pub use persist::{
    SourceType, decision_embed_text, doc_chunk_embed_text, eager_embed_in_tx, fact_embed_text,
    task_embed_text,
};
pub use recall::{RecallHit, RecallOptions, RecallResponse, RecallWarning, recall};
pub use rerank::RERANKER_MODEL_NAME;
