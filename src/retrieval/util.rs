//! Shared low-level helpers for the retrieval fusion paths.
//!
//! [`crate::retrieval::recall`] (project recall) and
//! [`crate::code_index::locate`] (the code locator) each implement the
//! same FTS5-match-building, min-max-normalization, and cosine-similarity
//! logic over their own tables; the embedding write paths
//! ([`crate::retrieval::persist`], `commands::index`, `commands::doc`,
//! `commands::review`, `metrics::recall_proxy`) and the code index
//! ([`crate::code_index`]) each hand-rolled their own content-hash and/or
//! vector<->bytes codec. Every copy was byte-identical (same algorithm,
//! same output for the same input) at the time of consolidation (issue
//! #69 / R9) — this module is a pure relocation, not a behavior change.

use sha2::{Digest, Sha256};

/// Hex-encode the SHA-256 digest of `bytes` (lowercase, no separators).
/// Used as a stable content-hash to detect whether a source row's embed
/// text — or, for the code index, a file's raw on-disk bytes — has
/// changed since the last embed/ingest. Takes `&[u8]` rather than `&str`
/// so the code-index caller can hash raw (not-necessarily-UTF-8) file
/// bytes directly; `&str` callers pass `.as_bytes()`.
pub(crate) fn sha256_hex(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

/// Whitespace-split `query` into punctuation-trimmed, non-empty tokens.
fn query_tokens(query: &str) -> impl Iterator<Item = &str> {
    query
        .split_whitespace()
        .map(|token| token.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | '.' | ':' | ';')))
        .filter(|t| !t.is_empty())
}

fn quote_fts_token(token: &str) -> String {
    format!("\"{}\"", token.replace('"', "\"\""))
}

/// Tokenize a free-text query into a quoted FTS5 `AND` of terms. Returns
/// `None` when the query has no usable tokens (so the caller skips FTS).
pub(crate) fn build_fts_match(query: &str) -> Option<String> {
    let tokens: Vec<String> = query_tokens(query).map(quote_fts_token).collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" AND "))
    }
}

/// English function words dropped from the `OR` expression: as a bag of
/// alternatives they would match nearly every row and drown the content
/// words in bm25.
const FTS_STOPWORDS: &[&str] = &[
    "a", "an", "and", "any", "are", "aren't", "as", "at", "be", "between", "but", "by", "can",
    "do", "does", "doesn't", "for", "from", "how", "i", "if", "in", "into", "is", "it", "its",
    "me", "my", "of", "on", "or", "our", "should", "so", "that", "the", "their", "then", "there",
    "this", "to", "was", "we", "what", "when", "where", "which", "who", "why", "will", "with",
    "would", "you", "your",
];

/// Like [`build_fts_match`] but an `OR` of the query's content terms (stopwords
/// removed), for the recall fallback when no single row holds every token.
/// Kept separate so the code locator keeps requiring every token.
pub(crate) fn build_fts_match_any(query: &str) -> Option<String> {
    let tokens: Vec<String> = query_tokens(query)
        .filter(|t| !FTS_STOPWORDS.contains(&t.to_lowercase().as_str()))
        .map(quote_fts_token)
        .collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" OR "))
    }
}

/// Min-max normalize an FTS bm25 score into `[0, 1]` against the
/// candidate pool's `[min, max]` range. A degenerate range (single hit,
/// or a tie across the whole pool) is treated as full strength (`1.0`)
/// rather than dividing by zero.
pub(crate) fn normalize_fts(value: f64, min: f64, max: f64) -> f64 {
    if !value.is_finite() || !min.is_finite() || !max.is_finite() {
        return 0.0;
    }
    if (max - min).abs() < f64::EPSILON {
        // Single FTS hit (or ties): treat as full strength.
        return 1.0;
    }
    ((value - min) / (max - min)).clamp(0.0, 1.0)
}

/// Cosine similarity between two equal-length embedding vectors. Returns
/// `0.0` for length mismatches or (near-)zero-norm vectors rather than
/// dividing by zero / producing NaN.
pub(crate) fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut dot: f64 = 0.0;
    let mut na: f64 = 0.0;
    let mut nb: f64 = 0.0;
    for (x, y) in a.iter().zip(b.iter()) {
        let xf = *x as f64;
        let yf = *y as f64;
        dot += xf * yf;
        na += xf * xf;
        nb += yf * yf;
    }
    if na <= f64::EPSILON || nb <= f64::EPSILON {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

/// Pack a vector as little-endian f32 bytes for storage in the
/// `embeddings` / `code_embeddings` BLOB column.
pub(crate) fn vector_to_le_bytes(v: &[f32]) -> Vec<u8> {
    let mut out = Vec::with_capacity(v.len() * 4);
    for f in v {
        out.extend_from_slice(&f.to_le_bytes());
    }
    out
}

/// Unpack little-endian f32 bytes back into a vector. A trailing partial
/// chunk (corrupt blob) is ignored rather than panicking.
pub(crate) fn bytes_to_vector(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn build_fts_match_any_ors_content_terms_and_drops_stopwords() {
        assert_eq!(
            build_fts_match_any("how does the parser, handle \"quotes\""),
            Some("\"parser\" OR \"handle\" OR \"quotes\"".to_string()),
        );
        assert_eq!(build_fts_match_any("what is the"), None);
        assert_eq!(build_fts_match_any("   "), None);
    }
}
