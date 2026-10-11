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
    OnnxSource, RerankInitOptionsUserDefined, TextRerank, TokenizerFiles, UserDefinedRerankingModel,
};

use crate::{MemhubError, Result};

pub const RERANKER_MODEL_NAME: &str = "ms-marco-MiniLM-L-6-v2";

/// Passages scored per ONNX session run. fastembed pads a batch to its
/// longest passage, so one run over a large pool needs memory proportional
/// to pool size x longest passage (a 50-row pool in one batch used 4.1 GB);
/// small batches keep it bounded.
pub const RERANK_BATCH_SIZE: usize = 8;

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
    let user_model =
        UserDefinedRerankingModel::new(OnnxSource::Memory(MODEL_ONNX.to_vec()), tokenizer_files);
    let model =
        TextRerank::try_new_from_user_defined(user_model, RerankInitOptionsUserDefined::default())
            .map_err(|e| {
                MemhubError::Rerank(format!("failed to load {RERANKER_MODEL_NAME}: {e}"))
            })?;
    Ok(MODEL.get_or_init(|| Mutex::new(model)))
}

/// Score (query, doc) pairs with the bundled cross-encoder and return
/// `(input_index, score)` pairs sorted by descending relevance.
///
/// `docs[i]` is the document originally at input position `i`. The
/// returned `input_index` tells the caller how to reorder them; the
/// `score` is the cross-encoder relevance logit (positive = relevant,
/// negative = irrelevant — see decision 70 follow-up on rerank-score-
/// based nonsense rejection). Empty input returns an empty Vec without
/// loading the model.
pub fn rerank(query: &str, docs: &[String]) -> Result<Vec<(usize, f32)>> {
    rerank_in_batches(query, docs, RERANK_BATCH_SIZE)
}

/// [`rerank`] with an explicit batch size (at least 1).
fn rerank_in_batches(query: &str, docs: &[String], batch_size: usize) -> Result<Vec<(usize, f32)>> {
    if docs.is_empty() {
        return Ok(Vec::new());
    }
    let cell = shared()?;
    let mut model = cell
        .lock()
        .map_err(|e| MemhubError::Rerank(format!("rerank mutex poisoned: {e}")))?;
    let doc_refs: Vec<&str> = docs.iter().map(String::as_str).collect();
    let results = model
        .rerank(query, &doc_refs, false, Some(batch_size.max(1)))
        .map_err(|e| MemhubError::Rerank(format!("rerank failed: {e}")))?;
    Ok(results.into_iter().map(|r| (r.index, r.score)).collect())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Largest per-row score difference tolerated between batch layouts.
    const QUANTIZATION_DRIFT: f32 = 0.5;

    #[test]
    fn rerank_empty_input_returns_empty_without_loading_model() {
        // Important: this must not trigger MODEL init. If it did, the test
        // would pay ~1–2 s of ONNX session setup. The early-return for
        // empty docs guards that.
        let out = rerank("anything", &[]).expect("empty rerank");
        assert!(out.is_empty());
    }

    #[test]
    fn batched_scores_match_a_single_pass_over_the_whole_pool() {
        let docs: Vec<String> = (0..RERANK_BATCH_SIZE * 2 + 3)
            .map(|i| match i % 3 {
                0 => "Rust is a systems programming language with strong memory safety".to_string(),
                1 => format!(
                    "Cats are domesticated felines kept as household pets. {}",
                    "They sleep for most of the day and hunt at dusk. ".repeat(i % 4 + 1)
                ),
                _ => format!("The borrow checker enforces ownership rules at compile time {i}"),
            })
            .collect();
        assert!(docs.len() > RERANK_BATCH_SIZE);
        let query = "how does rust prevent memory bugs";
        let by_index = |scored: Vec<(usize, f32)>| {
            let mut scores = vec![f32::NAN; docs.len()];
            for (i, s) in scored {
                scores[i] = s;
            }
            scores
        };
        let batched = by_index(rerank(query, &docs).expect("batched rerank"));
        let single = by_index(rerank_in_batches(query, &docs, docs.len()).expect("single pass"));
        // Not bit-equal: the bundled model is int8 dynamic-quantized, so each
        // run's activation scale depends on the batch it sees (measured drift
        // up to ~0.25 logits on padded long passages; unrelated passages sit
        // several logits apart, so a mis-indexed batch still fails).
        for (i, (b, s)) in batched.iter().zip(&single).enumerate() {
            assert!(
                (b - s).abs() < QUANTIZATION_DRIFT,
                "doc {i}: batched {b} vs single pass {s}"
            );
        }
    }

    #[test]
    fn rerank_returns_permutation_of_input_indices() {
        let docs = vec![
            "Rust is a systems programming language with strong memory safety".to_string(),
            "Cats are domesticated felines kept as household pets".to_string(),
            "The borrow checker enforces ownership rules at compile time".to_string(),
        ];
        let scored = rerank("how does rust prevent memory bugs", &docs).expect("rerank");
        assert_eq!(scored.len(), docs.len());
        let mut indices: Vec<usize> = scored.iter().map(|(i, _)| *i).collect();
        indices.sort();
        assert_eq!(indices, vec![0, 1, 2], "must be a permutation");
        // The cat doc (index 1) is clearly unrelated; it should not rank #1.
        assert_ne!(scored[0].0, 1, "off-topic doc should not be top-ranked");
        // Scores must be sorted descending — the top is the highest-relevance.
        for w in scored.windows(2) {
            assert!(w[0].1 >= w[1].1, "rerank scores must be descending");
        }
    }
}
