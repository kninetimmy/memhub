//! Component A of the token-accounting subsystem (decision 74,
//! task #28).
//!
//! Logs one row to `recall_metrics` per `memhub recall` call when the
//! user has opted in via `metrics.enabled && metrics.recall_proxy`.
//! No-op otherwise. Local arithmetic only — no external file reads
//! outside the rendered ledger — so this component cannot break
//! across Claude Code updates.

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::{Mutex, OnceLock};
use std::time::{Instant, SystemTime};

use rusqlite::{Connection, params};
use sha2::{Digest, Sha256};

use crate::Result;
use crate::config::MetricsConfig;
use crate::metrics::tokenizer::tokens_of;
use crate::retrieval::RecallResponse;

/// Process-wide ledger token cache. CLI one-shots build it once per
/// invocation; the MCP server amortizes the read across calls.
struct LedgerCacheEntry {
    path: PathBuf,
    mtime: SystemTime,
    tokens: usize,
    cached_at: Instant,
}

static LEDGER_CACHE: OnceLock<Mutex<Option<LedgerCacheEntry>>> = OnceLock::new();

const LEDGER_CACHE_TTL_SECS: u64 = 60;
const LEDGER_FILENAME: &str = "PROJECT_LEDGER.md";

/// Log one `recall_metrics` row for the call that just produced
/// `response`. Honors the `metrics.enabled` master switch and the
/// `metrics.recall_proxy` sub-switch; when either is off this is a
/// zero-cost no-op (one bool check + return). Errors are not
/// propagated to the caller — losing a metrics row must never fail
/// an otherwise-successful recall.
pub fn log_recall(
    conn: &Connection,
    cfg: &MetricsConfig,
    project_root: &Path,
    render_output_dir: &str,
    query: &str,
    response: &RecallResponse,
) {
    if !cfg.enabled || !cfg.recall_proxy {
        return;
    }
    if let Err(err) = try_log_recall(conn, project_root, render_output_dir, query, response) {
        log::warn!("recall_metrics insert failed: {err}");
    }
}

fn try_log_recall(
    conn: &Connection,
    project_root: &Path,
    render_output_dir: &str,
    query: &str,
    response: &RecallResponse,
) -> Result<()> {
    let query_hash = sha256_hex(query);
    let bundle_tokens = bundle_token_estimate(response);
    let ledger_tokens = ledger_token_estimate(project_root, render_output_dir);
    let rerank_used = if response.matcher == "recall:hybrid+rerank" {
        1
    } else {
        0
    };
    let result_count = response.results.len() as i64;

    conn.execute(
        "INSERT INTO recall_metrics \
            (ts, session_id, query_hash, bundle_tokens, ledger_tokens, rerank_used, result_count) \
         VALUES (CURRENT_TIMESTAMP, NULL, ?1, ?2, ?3, ?4, ?5)",
        params![
            query_hash,
            bundle_tokens as i64,
            ledger_tokens as i64,
            rerank_used,
            result_count,
        ],
    )?;
    Ok(())
}

fn bundle_token_estimate(response: &RecallResponse) -> usize {
    // Sum tokens over what the agent actually reads from each hit:
    // the title and body. Per-hit metadata (score, source, ts) is
    // small constant overhead that doesn't move the ratio against
    // ledger_tokens; counting only title+body keeps the proxy stable
    // even if RecallHit grows new metadata fields later.
    response
        .results
        .iter()
        .map(|hit| tokens_of(&format!("{}\n{}", hit.title, hit.body)))
        .sum()
}

fn ledger_token_estimate(project_root: &Path, render_output_dir: &str) -> usize {
    let path = project_root.join(render_output_dir).join(LEDGER_FILENAME);
    let mtime = match fs::metadata(&path).and_then(|m| m.modified()) {
        Ok(m) => m,
        Err(_) => return 0,
    };

    let cache = LEDGER_CACHE.get_or_init(|| Mutex::new(None));
    if let Ok(guard) = cache.lock()
        && let Some(entry) = guard.as_ref()
        && entry.path == path
        && entry.mtime == mtime
        && entry.cached_at.elapsed().as_secs() < LEDGER_CACHE_TTL_SECS
    {
        return entry.tokens;
    }

    let body = match fs::read_to_string(&path) {
        Ok(s) => s,
        Err(_) => return 0,
    };
    let tokens = tokens_of(&body);

    if let Ok(mut guard) = cache.lock() {
        *guard = Some(LedgerCacheEntry {
            path,
            mtime,
            tokens,
            cached_at: Instant::now(),
        });
    }
    tokens
}

fn sha256_hex(s: &str) -> String {
    let mut hasher = Sha256::new();
    hasher.update(s.as_bytes());
    let digest = hasher.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sha256_is_deterministic_and_hex() {
        let a = sha256_hex("memhub recall");
        let b = sha256_hex("memhub recall");
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
