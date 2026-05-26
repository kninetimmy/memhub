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
//!
//! ## Calibration (task 63, decision 109)
//!
//! The ±10% bias above is a fixed multiplier, not noise, so it can be
//! corrected once. `memhub metrics calibrate` measures the
//! cl100k→Anthropic ratio against the real `count_tokens` API (the
//! single network touch in all of memhub, explicitly user-invoked —
//! see `metrics::calibrate`) and stores it as `[metrics]
//! calibration_factor`. The metrics entry points then call
//! [`set_calibration_factor`] so [`tokens_of`] scales every count by it.
//! Default `1.0` is an exact passthrough, so an uncalibrated install —
//! and every unit test that calls `tokens_of` directly — is
//! byte-identical to a pre-calibration build. Use [`raw_token_count`]
//! when you need the unscaled cl100k count (the calibrator does, to
//! compute the ratio without applying it to itself).

use std::sync::OnceLock;
use std::sync::atomic::{AtomicU64, Ordering};

use tiktoken_rs::{CoreBPE, cl100k_base};

static BPE: OnceLock<CoreBPE> = OnceLock::new();

/// Calibration multiplier as raw f64 bits. `0` is a sentinel for
/// "unset" and reads back as `1.0`, so the static needs no lazy init
/// and the uncalibrated path is a plain atomic load.
static CALIBRATION_BITS: AtomicU64 = AtomicU64::new(0);

fn bpe() -> &'static CoreBPE {
    BPE.get_or_init(|| {
        cl100k_base().expect("cl100k_base BPE is bundled at compile time and must initialize")
    })
}

/// Set the process-wide calibration multiplier applied by [`tokens_of`].
/// Called from the metrics entry points with `[metrics]
/// calibration_factor`. Non-finite or non-positive values are rejected
/// (reset to the `1.0` passthrough) so a corrupt config can never zero
/// out or NaN every token count.
pub fn set_calibration_factor(factor: f64) {
    let safe = if factor.is_finite() && factor > 0.0 {
        factor
    } else {
        1.0
    };
    CALIBRATION_BITS.store(safe.to_bits(), Ordering::Relaxed);
}

/// Current calibration multiplier; `1.0` when unset.
pub fn calibration_factor() -> f64 {
    let bits = CALIBRATION_BITS.load(Ordering::Relaxed);
    if bits == 0 { 1.0 } else { f64::from_bits(bits) }
}

/// Raw cl100k_base token count for `s`, with no calibration applied.
///
/// Uses `encode_ordinary` (no special-token handling) — appropriate
/// because the metrics subsystem counts user-visible text, not chat
/// transcripts with special tokens already injected by the agent.
pub fn raw_token_count(s: &str) -> usize {
    bpe().encode_ordinary(s).len()
}

/// Estimate the Anthropic-equivalent token count for `s`: the raw
/// cl100k count scaled by the calibration multiplier and rounded. When
/// uncalibrated (`factor == 1.0`) this is exactly [`raw_token_count`].
pub fn tokens_of(s: &str) -> usize {
    let raw = raw_token_count(s);
    let factor = calibration_factor();
    if factor == 1.0 {
        return raw;
    }
    (raw as f64 * factor).round() as usize
}

#[cfg(test)]
mod tests {
    use super::*;

    // The calibration multiplier is process-global state; these tests
    // mutate it, so they must not run concurrently with assumptions
    // about its value. Each test sets the factor it needs and resets to
    // 1.0 at the end. They share one #[test] to keep ordering explicit.
    #[test]
    fn calibration_scales_and_rounds_then_resets() {
        let sample = "the quick brown fox jumps over the lazy dog";
        let raw = raw_token_count(sample);
        assert!(raw > 0);

        // Uncalibrated passthrough.
        set_calibration_factor(1.0);
        assert_eq!(tokens_of(sample), raw);

        // A clean 2x scales and stays integral.
        set_calibration_factor(2.0);
        assert_eq!(calibration_factor(), 2.0);
        assert_eq!(tokens_of(sample), raw * 2);

        // A fractional factor rounds to nearest.
        set_calibration_factor(1.1);
        assert_eq!(tokens_of(sample), (raw as f64 * 1.1).round() as usize);

        // raw_token_count never reflects the factor.
        assert_eq!(raw_token_count(sample), raw);

        // Garbage factors are rejected back to 1.0.
        set_calibration_factor(0.0);
        assert_eq!(calibration_factor(), 1.0);
        set_calibration_factor(-3.0);
        assert_eq!(calibration_factor(), 1.0);
        set_calibration_factor(f64::NAN);
        assert_eq!(calibration_factor(), 1.0);
        set_calibration_factor(f64::INFINITY);
        assert_eq!(calibration_factor(), 1.0);

        // Leave the global as the default for any later test.
        set_calibration_factor(1.0);
        assert_eq!(tokens_of(sample), raw);
    }
}
