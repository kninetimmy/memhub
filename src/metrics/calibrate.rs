//! One-time tokenizer calibration (task 63, decision 109).
//!
//! This module is **the only place in memhub that touches the network**,
//! and only when a user explicitly runs `memhub metrics calibrate`. It
//! measures the multiplier between the local cl100k_base estimate and
//! Anthropic's real tokenizer by counting one fixed, bundled corpus both
//! ways — locally with [`tokenizer::raw_token_count`] and remotely with
//! Anthropic's free `count_tokens` endpoint — and returns the ratio for
//! the caller to persist as `[metrics] calibration_factor`.
//!
//! Offline-first is preserved two ways. First, the corpus is bundled
//! ([`CALIBRATION_CORPUS`] via `include_str!`), so nothing from the
//! user's own project is ever transmitted — calibration measures a
//! property of the tokenizer, not of any repository. Second, the call is
//! never made implicitly: normal `memhub` operation, including every
//! recall, stays fully offline, and the stored factor is read from local
//! config thereafter. The command is deliberately CLI-only ops
//! housekeeping (like `gc` and `upgrade`), not an agent-triggerable MCP
//! surface — an agent must not be able to reach the network on its own.

use serde::Deserialize;

use crate::metrics::tokenizer;
use crate::{MemhubError, Result};

/// Fixed sample sent to `count_tokens`. Blends prose, identifiers,
/// paths, SQL, and code so the measured factor reflects the kind of
/// text memhub actually counts. See the file for the rationale.
pub const CALIBRATION_CORPUS: &str = include_str!("calibration_corpus.txt");

/// Default model for the `count_tokens` request. The Claude tokenizer is
/// shared across the Claude 3/4 families, so the exact model barely moves
/// the ratio; this is a stable, available default and is overridable.
pub const DEFAULT_CALIBRATION_MODEL: &str = "claude-sonnet-4-6";

const COUNT_TOKENS_URL: &str = "https://api.anthropic.com/v1/messages/count_tokens";
const ANTHROPIC_VERSION: &str = "2023-06-01";

/// Outcome of a calibration run.
#[derive(Debug, Clone)]
pub struct CalibrationResult {
    /// Local cl100k_base count of the corpus (uncalibrated).
    pub cl100k_tokens: usize,
    /// Anthropic `count_tokens` count of the same corpus.
    pub real_tokens: usize,
    /// `real_tokens / cl100k_tokens` — the multiplier to store.
    pub factor: f64,
    /// Model used for the remote count.
    pub model: String,
}

/// Pure ratio computation, split out so it is testable without the
/// network. Both inputs come from counting the same non-empty corpus, so
/// `cl100k` is always > 0 in practice; we still guard it.
pub fn compute_factor(real_tokens: usize, cl100k_tokens: usize) -> Result<f64> {
    if cl100k_tokens == 0 {
        return Err(MemhubError::InvalidInput(
            "calibration corpus produced zero cl100k tokens".to_string(),
        ));
    }
    Ok(real_tokens as f64 / cl100k_tokens as f64)
}

/// Run a calibration: count the bundled corpus locally and remotely,
/// then return the ratio. Makes one HTTPS request to Anthropic; every
/// failure (missing key, transport, non-2xx, malformed body) maps to a
/// clear `InvalidInput` so the CLI can surface it plainly.
pub fn calibrate(api_key: &str, model: &str) -> Result<CalibrationResult> {
    if api_key.trim().is_empty() {
        return Err(MemhubError::InvalidInput(
            "ANTHROPIC_API_KEY is empty".to_string(),
        ));
    }

    let cl100k_tokens = tokenizer::raw_token_count(CALIBRATION_CORPUS);
    let real_tokens = count_tokens_via_api(api_key, model)?;
    let factor = compute_factor(real_tokens, cl100k_tokens)?;

    Ok(CalibrationResult {
        cl100k_tokens,
        real_tokens,
        factor,
        model: model.to_string(),
    })
}

#[derive(Deserialize)]
struct CountTokensResponse {
    input_tokens: usize,
}

/// POST the corpus to Anthropic's `count_tokens` and return its
/// `input_tokens`. The response includes a small fixed message-framing
/// overhead (a handful of tokens for the user-message wrapper); against a
/// corpus of well over a thousand tokens this is a sub-1% inflation of
/// the factor, far inside the ±10% bias being corrected, and is left in
/// rather than guessed at.
fn count_tokens_via_api(api_key: &str, model: &str) -> Result<usize> {
    let body = serde_json::json!({
        "model": model,
        "messages": [{ "role": "user", "content": CALIBRATION_CORPUS }],
    });

    let response = ureq::post(COUNT_TOKENS_URL)
        .set("x-api-key", api_key)
        .set("anthropic-version", ANTHROPIC_VERSION)
        .set("content-type", "application/json")
        .send_json(body);

    let response = match response {
        Ok(resp) => resp,
        Err(ureq::Error::Status(code, resp)) => {
            // 4xx/5xx — surface the API's own error text when present.
            let detail = resp
                .into_string()
                .unwrap_or_else(|_| "<no response body>".to_string());
            return Err(MemhubError::InvalidInput(format!(
                "Anthropic count_tokens returned HTTP {code}: {}",
                detail.trim()
            )));
        }
        Err(ureq::Error::Transport(t)) => {
            return Err(MemhubError::InvalidInput(format!(
                "could not reach Anthropic count_tokens ({t}); \
                 calibration needs a one-time network connection"
            )));
        }
    };

    let parsed: CountTokensResponse = response.into_json().map_err(|err| {
        MemhubError::InvalidInput(format!("unexpected count_tokens response shape: {err}"))
    })?;

    if parsed.input_tokens == 0 {
        return Err(MemhubError::InvalidInput(
            "Anthropic count_tokens reported 0 input tokens".to_string(),
        ));
    }
    Ok(parsed.input_tokens)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corpus_is_substantial_enough_to_dwarf_framing_overhead() {
        // The message-framing overhead is a handful of tokens; the corpus
        // must be large enough that it is negligible. Guard a floor.
        let n = tokenizer::raw_token_count(CALIBRATION_CORPUS);
        assert!(n > 800, "calibration corpus only {n} cl100k tokens");
    }

    #[test]
    fn compute_factor_is_real_over_local() {
        let f = compute_factor(1100, 1000).expect("factor");
        assert!((f - 1.1).abs() < 1e-9);
    }

    #[test]
    fn compute_factor_rejects_zero_denominator() {
        assert!(compute_factor(1000, 0).is_err());
    }
}
