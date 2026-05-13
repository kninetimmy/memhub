//! BGE-small-en-v1.5 embedding wrapper.
//!
//! The ONNX model and tokenizer files are bundled into the memhub binary at
//! build time by `build.rs`. The first call to [`embed_one`] or
//! [`embed_batch`] constructs the fastembed [`TextEmbedding`] handle; all
//! subsequent calls reuse it.

use std::sync::{Mutex, OnceLock};

use fastembed::{
    InitOptionsUserDefined, Pooling, TextEmbedding, TokenizerFiles, UserDefinedEmbeddingModel,
};

use crate::{MemhubError, Result};

pub const EMBEDDING_MODEL_NAME: &str = "bge-small-en-v1.5";
pub const EMBEDDING_DIMENSION: usize = 384;

const MODEL_ONNX: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/bge-small-en-v1.5/model.onnx"));
const TOKENIZER_JSON: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/bge-small-en-v1.5/tokenizer.json"
));
const CONFIG_JSON: &[u8] =
    include_bytes!(concat!(env!("OUT_DIR"), "/bge-small-en-v1.5/config.json"));
const SPECIAL_TOKENS_JSON: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/bge-small-en-v1.5/special_tokens_map.json"
));
const TOKENIZER_CONFIG_JSON: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/bge-small-en-v1.5/tokenizer_config.json"
));

static MODEL: OnceLock<Mutex<TextEmbedding>> = OnceLock::new();

fn shared() -> Result<&'static Mutex<TextEmbedding>> {
    if let Some(m) = MODEL.get() {
        return Ok(m);
    }
    let tokenizer_files = TokenizerFiles {
        tokenizer_file: TOKENIZER_JSON.to_vec(),
        config_file: CONFIG_JSON.to_vec(),
        special_tokens_map_file: SPECIAL_TOKENS_JSON.to_vec(),
        tokenizer_config_file: TOKENIZER_CONFIG_JSON.to_vec(),
    };
    let user_model = UserDefinedEmbeddingModel::new(MODEL_ONNX.to_vec(), tokenizer_files)
        .with_pooling(Pooling::Cls);
    let model = TextEmbedding::try_new_from_user_defined(user_model, InitOptionsUserDefined::new())
        .map_err(|e| MemhubError::Embedding(format!("failed to load {EMBEDDING_MODEL_NAME}: {e}")))?;
    // If another thread won the race, drop ours and return the winner.
    Ok(MODEL.get_or_init(|| Mutex::new(model)))
}

/// Embed a single text into a 384-dim vector.
pub fn embed_one(text: &str) -> Result<Vec<f32>> {
    let mut out = embed_batch(&[text])?;
    out.pop()
        .ok_or_else(|| MemhubError::Embedding("empty embedding output".to_string()))
}

/// Embed a batch of texts. Each output is a 384-dim vector in the same order.
pub fn embed_batch<S: AsRef<str> + Send + Sync>(texts: &[S]) -> Result<Vec<Vec<f32>>> {
    if texts.is_empty() {
        return Ok(Vec::new());
    }
    let cell = shared()?;
    let mut model = cell
        .lock()
        .map_err(|e| MemhubError::Embedding(format!("model mutex poisoned: {e}")))?;
    let documents: Vec<&str> = texts.iter().map(AsRef::as_ref).collect();
    model
        .embed(documents, None)
        .map_err(|e| MemhubError::Embedding(format!("embed failed: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smoke_embed_dimension_and_finite() {
        let v = embed_one("memhub local-first project memory").expect("embed_one");
        assert_eq!(v.len(), EMBEDDING_DIMENSION, "expected 384-dim vector");
        assert!(
            v.iter().all(|f| f.is_finite()),
            "embedding contained non-finite values"
        );
        let norm: f32 = v.iter().map(|f| f * f).sum::<f32>().sqrt();
        // BGE outputs are L2-normalized by fastembed; allow some float slack.
        assert!(
            (0.5..=1.5).contains(&norm),
            "embedding norm {norm} is unexpectedly far from 1.0"
        );
    }

    #[test]
    fn smoke_embed_batch_preserves_order_and_length() {
        let texts = vec!["alpha", "beta", "gamma"];
        let vecs = embed_batch(&texts).expect("embed_batch");
        assert_eq!(vecs.len(), 3);
        assert!(vecs.iter().all(|v| v.len() == EMBEDDING_DIMENSION));
        // alpha and beta should not be identical vectors.
        assert!(vecs[0] != vecs[1]);
    }
}
