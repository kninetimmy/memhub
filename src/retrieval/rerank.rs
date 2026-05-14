//! ms-marco-MiniLM-L-6-v2 cross-encoder re-ranker.
//!
//! The ONNX model and tokenizer files are bundled into the memhub binary
//! at build time by `build.rs`. The first call to [`rerank`] constructs
//! the fastembed [`TextRerank`] handle (~1–2 s ONNX session init); all
//! subsequent calls reuse it.
//!
//! Selected over BGE-reranker-v2-m3 in the cross-encoder bake-off
//! (decisions 68–70): +17.7pp Recall@1 over baseline, ~15× faster, no
//! keyword-query regressions. Bundled unconditionally; the `[retrieval]
//! use_reranker` config knob gates whether `recall::run` actually calls
//! into this module.

use std::sync::{Mutex, OnceLock};

use fastembed::{
    OnnxSource, RerankInitOptionsUserDefined, TextRerank, TokenizerFiles,
    UserDefinedRerankingModel,
};

use crate::{MemhubError, Result};

pub const RERANKER_MODEL_NAME: &str = "ms-marco-MiniLM-L-6-v2";

const MODEL_ONNX: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/ms-marco-MiniLM-L-6-v2/model.onnx"
));
const TOKENIZER_JSON: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/ms-marco-MiniLM-L-6-v2/tokenizer.json"
));
const CONFIG_JSON: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/ms-marco-MiniLM-L-6-v2/config.json"
));
const SPECIAL_TOKENS_JSON: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/ms-marco-MiniLM-L-6-v2/special_tokens_map.json"
));
const TOKENIZER_CONFIG_JSON: &[u8] = include_bytes!(concat!(
    env!("OUT_DIR"),
    "/ms-marco-MiniLM-L-6-v2/tokenizer_config.json"
));

static MODEL: OnceLock<Mutex<TextRerank>> = OnceLock::new();

fn shared() -> Result<&'static Mutex<TextRerank>> {
    if let Some(m) = MODEL.get() {
        return Ok(m);
    }
    let tokenizer_files = TokenizerFiles {
        tokenizer_file: TOKENIZER_JSON.to_vec(),
        config_file: CONFIG_JSON.to_vec(),
        special_tokens_map_file: SPECIAL_TOKENS_JSON.to_vec(),
        tokenizer_config_file: TOKENIZER_CONFIG_JSON.to_vec(),
    };
    let user_model = UserDefinedRerankingModel::new(
        OnnxSource::Memory(MODEL_ONNX.to_vec()),
        tokenizer_files,
    );
    let model = TextRerank::try_new_from_user_defined(
        user_model,
        RerankInitOptionsUserDefined::default(),
    )
    .map_err(|e| MemhubError::Rerank(format!("failed to load {RERANKER_MODEL_NAME}: {e}")))?;
    Ok(MODEL.get_or_init(|| Mutex::new(model)))
}

/// Score (query, doc) pairs with the bundled cross-encoder and return a
/// permutation of `0..docs.len()` sorted by descending relevance.
///
/// `docs[i]` is the document originally at input position `i`; the
/// returned ordering tells the caller how to reorder them. Empty input
/// returns an empty Vec without loading the model.
pub fn rerank(query: &str, docs: &[String]) -> Result<Vec<usize>> {
    if docs.is_empty() {
        return Ok(Vec::new());
    }
    let cell = shared()?;
    let mut model = cell
        .lock()
        .map_err(|e| MemhubError::Rerank(format!("rerank mutex poisoned: {e}")))?;
    let doc_refs: Vec<&str> = docs.iter().map(String::as_str).collect();
    let results = model
        .rerank(query, &doc_refs, false, None)
        .map_err(|e| MemhubError::Rerank(format!("rerank failed: {e}")))?;
    Ok(results.into_iter().map(|r| r.index).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rerank_empty_input_returns_empty_without_loading_model() {
        // Important: this must not trigger MODEL init. If it did, the test
        // would pay ~1–2 s of ONNX session setup. The early-return for
        // empty docs guards that.
        let out = rerank("anything", &[]).expect("empty rerank");
        assert!(out.is_empty());
    }

    #[test]
    fn rerank_returns_permutation_of_input_indices() {
        let docs = vec![
            "Rust is a systems programming language with strong memory safety".to_string(),
            "Cats are domesticated felines kept as household pets".to_string(),
            "The borrow checker enforces ownership rules at compile time".to_string(),
        ];
        let order = rerank("how does rust prevent memory bugs", &docs).expect("rerank");
        assert_eq!(order.len(), docs.len());
        let mut sorted = order.clone();
        sorted.sort();
        assert_eq!(sorted, vec![0, 1, 2], "must be a permutation");
        // The cat doc (index 1) is clearly unrelated; it should not rank #1.
        assert_ne!(order[0], 1, "off-topic doc should not be top-ranked");
    }
}
