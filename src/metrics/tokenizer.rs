//! cl100k_base token counting for the metrics subsystem (decision 74).
//!
//! Backed by `tiktoken-rs`, which embeds the cl100k_base BPE table
//! into the binary at compile time — no runtime network call, no
//! filesystem dependency. The first call lazily builds the singleton
//! `CoreBPE` (~10 ms on a modern laptop); subsequent calls reuse it.
//!
//! **This is an estimate, not ground truth.** Anthropic's real
//! tokenizer is not public; cl100k_base is OpenAI's GPT-3.5/GPT-4
//! tokenizer and drifts roughly ±10% from Anthropic's counts on
//! typical English prose. We use it symmetrically across both sides
//! of every comparison (e.g. "bundle tokens vs full-ledger tokens"),
//! so the ratio stays sound even though the absolute number is
//! rounded. Do not display these numbers as if they were authoritative
//! per-request token counts.

use std::sync::OnceLock;

use tiktoken_rs::{CoreBPE, cl100k_base};

static BPE: OnceLock<CoreBPE> = OnceLock::new();

fn bpe() -> &'static CoreBPE {
    BPE.get_or_init(|| {
        cl100k_base().expect("cl100k_base BPE is bundled at compile time and must initialize")
    })
}

/// Estimate the cl100k_base token count for `s`.
///
/// Uses `encode_ordinary` (no special-token handling) — appropriate
/// because the metrics subsystem counts user-visible text, not chat
/// transcripts with special tokens already injected by the agent.
pub fn tokens_of(s: &str) -> usize {
    bpe().encode_ordinary(s).len()
}
