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

use crate::Result;
use crate::config::MetricsConfig;
use crate::metrics::tokenizer::{set_calibration_factor, tokens_of};
use crate::retrieval::{RecallResponse, RecallSurface};
use crate::retrieval::util::sha256_hex;

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
    surface: Option<RecallSurface>,
) {
    if !cfg.enabled || !cfg.recall_proxy {
        return;
    }
    // Apply the machine's stored tokenizer calibration (task 63) so the
    // bundle/ledger counts written below approximate Anthropic's real
    // tokenizer. Default 1.0 is an exact passthrough, so an uncalibrated
    // install logs identical numbers to a pre-calibration build.
    set_calibration_factor(cfg.calibration_factor);
    if let Err(err) = try_log_recall(
        conn,
        project_root,
        render_output_dir,
        query,
        response,
        surface,
    ) {
        log::warn!("recall_metrics insert failed: {err}");
    }
}

fn try_log_recall(
    conn: &Connection,
    project_root: &Path,
    render_output_dir: &str,
    query: &str,
    response: &RecallResponse,
    surface: Option<RecallSurface>,
) -> Result<()> {
    let query_hash = sha256_hex(query.as_bytes());
    let bundle_tokens = bundle_token_estimate(response);
    let ledger_tokens = ledger_token_estimate(project_root, render_output_dir);
    let rerank_used = if response.matcher == "recall:hybrid+rerank" {
        1
    } else {
        0
    };
    let result_count = response.results.len() as i64;
    // NULL for internal callers (eval sweeps, dashboard inspector, upgrade
    // smoke check) — none of them reach here anyway since they all pass
    // `log_metrics: false`. 'cli' / 'mcp' for the two agent-facing entry
    // points (issue #70 / Wave 4 gate Q17).
    let surface_str = surface.map(RecallSurface::as_str);

    conn.execute(
        "INSERT INTO recall_metrics \
            (ts, session_id, query_hash, bundle_tokens, ledger_tokens, rerank_used, \
             result_count, surface) \
         VALUES (CURRENT_TIMESTAMP, NULL, ?1, ?2, ?3, ?4, ?5, ?6)",
        params![
            query_hash,
            bundle_tokens as i64,
            ledger_tokens as i64,
            rerank_used,
            result_count,
            surface_str,
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

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ProjectConfig, RetrievalMode};

    /// A minimal, result-free `RecallResponse` — `log_recall` only reads
    /// `results` (for `bundle_token_estimate`) and `matcher`, so an empty
    /// bundle is enough to exercise the INSERT itself without dragging in
    /// a full recall fixture.
    fn empty_response(query: &str) -> RecallResponse {
        RecallResponse {
            query: query.to_string(),
            mode: RetrievalMode::Fts,
            results: Vec::new(),
            candidate_count: 0,
            returned_count: 0,
            warnings: Vec::new(),
            matcher: "recall:fts".to_string(),
            elapsed_ms: 0,
            available_docs: 0,
        }
    }

    /// Issue #70 / Wave 4 gate Q17: `log_recall`'s `surface` argument must
    /// land verbatim as the `recall_metrics.surface` column — `'cli'` /
    /// `'mcp'` — so a follow-up can split latency by calling surface.
    #[test]
    fn log_recall_writes_the_calling_surface() {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        let cfg_path = temp.path().join(".memhub/config.toml");
        let mut cfg = ProjectConfig::load(&cfg_path).expect("load");
        cfg.metrics.enabled = true;
        cfg.metrics.recall_proxy = true;
        cfg.save(&cfg_path).expect("save");

        let ctx = crate::db::open_project(temp.path()).expect("open");

        log_recall(
            &ctx.conn,
            &ctx.config.metrics,
            &ctx.paths.repo_root,
            &ctx.config.render.output_dir,
            "cli query",
            &empty_response("cli query"),
            Some(RecallSurface::Cli),
        );
        log_recall(
            &ctx.conn,
            &ctx.config.metrics,
            &ctx.paths.repo_root,
            &ctx.config.render.output_dir,
            "mcp query",
            &empty_response("mcp query"),
            Some(RecallSurface::Mcp),
        );

        let mut stmt = ctx
            .conn
            .prepare("SELECT surface FROM recall_metrics ORDER BY id")
            .expect("prepare");
        let rows: Vec<Option<String>> = stmt
            .query_map([], |r| r.get(0))
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect");

        assert_eq!(
            rows,
            vec![Some("cli".to_string()), Some("mcp".to_string())],
            "surface column must record each call's actual calling surface"
        );
    }

    #[test]
    fn sha256_is_deterministic_and_hex() {
        let a = sha256_hex("memhub recall".as_bytes());
        let b = sha256_hex("memhub recall".as_bytes());
        assert_eq!(a, b);
        assert_eq!(a.len(), 64);
        assert!(a.chars().all(|c| c.is_ascii_hexdigit()));
    }
}
