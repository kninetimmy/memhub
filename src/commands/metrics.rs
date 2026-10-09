//! `memhub metrics` subcommand handlers (task #31).
//!
//! Provides the user-facing surface for the token-accounting subsystem
//! (decision 74): status, enable, disable, rescan, and prune.
//!
//! The subsystem is off by default; all reads and writes are gated on
//! `MetricsConfig.enabled`. Config mutations are written back to the
//! machine-local `config.toml` via `ProjectConfig::save` so each
//! invocation inherits the updated state.
//!
//! `db::open_project` already runs `scrape_if_enabled` and
//! `maintenance::run_if_enabled` opportunistically on every `memhub`
//! invocation (task #30), so by the time any `metrics` handler runs the
//! DB is current. The explicit maintenance calls here (`rescan`,
//! `prune`) are idempotent but surface their return values to the user,
//! which the background path does not.

use std::path::Path;

use rusqlite::params;

use crate::config::MetricsConfig;
use crate::db;
use crate::metrics::formatter::{self, PeriodTotals, SessionSummary};
use crate::metrics::maintenance;
use crate::metrics::session_scraper;
use crate::transcript_files::{detect_claude_transcripts_dir, detect_codex_sessions_dir};
use crate::{MemhubError, Result};

const METRICS_ACTOR: &str = "cli:user";

/// All data the `memhub.metrics` MCP tool needs. Built by `query_tool_data`.
#[derive(Debug)]
pub struct MetricsToolData {
    pub enabled: bool,
    pub recall_proxy: bool,
    pub session_accounting: bool,
    pub claude_transcripts_dir: Option<String>,
    /// MAX(ended_at) from session_metrics — the last timestamp the scraper advanced.
    pub last_scrape_ts: Option<String>,
    pub totals_7d: PeriodTotals,
    pub totals_30d: PeriodTotals,
    /// Up to 10 most recent sessions, newest first.
    pub last_sessions: Vec<SessionSummary>,
    /// Pre-rendered text panel for the /metrics skill.
    pub rendered_panel: String,
}

/// Query all token-accounting data for the MCP tool. Returns immediately
/// when `enabled = false` with only the flag set; no DB aggregation runs
/// so no stale rows from a prior-enabled stretch can leak through.
pub fn query_tool_data(start: &Path) -> Result<MetricsToolData> {
    let ctx = db::open_project(start)?;
    let cfg = &ctx.config.metrics;

    if !cfg.enabled {
        return Ok(MetricsToolData {
            enabled: false,
            recall_proxy: cfg.recall_proxy,
            session_accounting: cfg.session_accounting,
            claude_transcripts_dir: None,
            last_scrape_ts: None,
            totals_7d: PeriodTotals::default(),
            totals_30d: PeriodTotals::default(),
            last_sessions: Vec::new(),
            rendered_panel: String::new(),
        });
    }

    let transcripts_dir = if cfg.claude_transcripts_dir.is_empty() {
        None
    } else {
        Some(cfg.claude_transcripts_dir.clone())
    };

    // MAX(ended_at) across all scraped sessions — most recent scrape advance.
    let last_scrape_ts: Option<String> = ctx
        .conn
        .query_row("SELECT MAX(ended_at) FROM session_metrics", [], |r| {
            r.get(0)
        })
        .unwrap_or(None);

    let totals_7d = query_period_totals(&ctx.conn, 7)?;
    let totals_30d = query_period_totals(&ctx.conn, 30)?;

    let last_sessions = query_last_sessions(&ctx.conn, 10)?;

    let rendered_panel =
        if totals_7d.is_empty() && totals_30d.is_empty() && last_sessions.is_empty() {
            formatter::render_panel_no_data().to_string()
        } else {
            formatter::render_panel(&totals_7d, &totals_30d, &last_sessions)
        };

    Ok(MetricsToolData {
        enabled: true,
        recall_proxy: cfg.recall_proxy,
        session_accounting: cfg.session_accounting,
        claude_transcripts_dir: transcripts_dir,
        last_scrape_ts,
        totals_7d,
        totals_30d,
        last_sessions,
        rendered_panel,
    })
}

/// Median of a pre-sorted ascending slice. Even-length sets average the two
/// middle values (integer division — a token estimate, not exact arithmetic).
/// `None` on an empty slice.
fn median_i64(sorted: &[i64]) -> Option<i64> {
    let n = sorted.len();
    if n == 0 {
        return None;
    }
    if n % 2 == 1 {
        Some(sorted[n / 2])
    } else {
        Some((sorted[n / 2 - 1] + sorted[n / 2]) / 2)
    }
}

pub(crate) fn query_period_totals(conn: &rusqlite::Connection, days: u32) -> Result<PeriodTotals> {
    let modifier = format!("-{days} days");

    // Total recall count (all calls, including empty-bundle ones).
    let recalls: i64 = conn
        .query_row(
            "SELECT COALESCE(COUNT(*), 0) FROM recall_metrics \
             WHERE datetime(ts) >= datetime('now', ?1)",
            params![modifier],
            |r| r.get(0),
        )
        .unwrap_or(0);

    // Sum of non-empty bundle_tokens. Empty recalls (bundle_tokens == 0)
    // are not savings events and are excluded.
    let bundle_tokens: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(CASE WHEN bundle_tokens > 0 THEN bundle_tokens ELSE 0 END), 0) \
             FROM recall_metrics WHERE datetime(ts) >= datetime('now', ?1)",
            params![modifier],
            |r| r.get(0),
        )
        .unwrap_or(0);

    // Session-scoped ledger baseline: charge one ledger load (the minimum
    // observed across all recalls in that session, as a proxy for session
    // start) per session that had at least one non-empty recall. Sessions
    // where every recall returned an empty bundle contribute 0.
    let ledger_tokens: i64 = conn
        .query_row(
            "SELECT COALESCE(SUM(min_ledger), 0) FROM ( \
                 SELECT MIN(ledger_tokens) AS min_ledger \
                 FROM recall_metrics \
                 WHERE datetime(ts) >= datetime('now', ?1) \
                   AND session_id IS NOT NULL \
                 GROUP BY session_id \
                 HAVING SUM(CASE WHEN bundle_tokens > 0 THEN 1 ELSE 0 END) > 0 \
             )",
            params![modifier],
            |r| r.get(0),
        )
        .unwrap_or(0);

    // session_metrics: count + token sums for sessions starting in the window.
    let (sessions, input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens): (
        i64,
        i64,
        i64,
        i64,
        i64,
    ) = conn
        .query_row(
            "SELECT \
               COALESCE(COUNT(*), 0), \
               COALESCE(SUM(input_tokens), 0), \
               COALESCE(SUM(output_tokens), 0), \
               COALESCE(SUM(cache_read_tokens), 0), \
               COALESCE(SUM(cache_creation_tokens), 0) \
             FROM session_metrics \
             WHERE datetime(started_at) >= datetime('now', ?1)",
            params![modifier],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
        )
        .unwrap_or((0, 0, 0, 0, 0));

    // Per-session mean churn: each in-window session's own
    // cache_creation/(cache_read+cache_creation), averaged with equal
    // weight so one large session can't dominate. Sessions with no cache
    // activity are excluded from the denominator; AVG over zero qualifying
    // rows yields SQL NULL → None.
    let mean_session_churn_pct: Option<f64> = conn
        .query_row(
            "SELECT AVG( \
                 cache_creation_tokens * 1.0 \
                 / (cache_read_tokens + cache_creation_tokens) \
             ) * 100.0 \
             FROM session_metrics \
             WHERE datetime(started_at) >= datetime('now', ?1) \
               AND (cache_read_tokens + cache_creation_tokens) > 0",
            params![modifier],
            |r| r.get(0),
        )
        .unwrap_or(None);

    // Distinct sessions in the window that had ≥1 non-empty recall — the
    // sessions the empirical counterfactual is charged against (task 64).
    // Same grouping/HAVING as the ledger baseline above, counted.
    let recall_sessions: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM ( \
                 SELECT session_id FROM recall_metrics \
                 WHERE datetime(ts) >= datetime('now', ?1) \
                   AND session_id IS NOT NULL \
                 GROUP BY session_id \
                 HAVING SUM(CASE WHEN bundle_tokens > 0 THEN 1 ELSE 0 END) > 0 \
             )",
            params![modifier],
            |r| r.get(0),
        )
        .unwrap_or(0);

    // Empirical counterfactual baseline (task 64): the MEDIAN first-turn
    // startup cost across the window's NO-recall sessions. SQLite has no
    // median aggregate, so pull the (small) set of values and compute it in
    // Rust. recall_calls is owned by the reconciler; a session that never
    // recalled stays at 0.
    let baselines: Vec<i64> = {
        let mut stmt = conn.prepare(
            "SELECT baseline_input_tokens FROM session_metrics \
             WHERE datetime(started_at) >= datetime('now', ?1) \
               AND recall_calls = 0 \
               AND baseline_input_tokens IS NOT NULL \
             ORDER BY baseline_input_tokens",
        )?;
        stmt.query_map(params![modifier], |r| r.get(0))?
            .filter_map(|x| x.ok())
            .collect()
    };
    let empirical_baseline = median_i64(&baselines);

    Ok(PeriodTotals {
        recalls,
        bundle_tokens,
        ledger_tokens,
        sessions,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_creation_tokens,
        mean_session_churn_pct,
        recall_sessions,
        empirical_baseline,
    })
}

fn query_last_sessions(conn: &rusqlite::Connection, limit: i64) -> Result<Vec<SessionSummary>> {
    let mut stmt = conn.prepare(
        "SELECT \
           session_id, \
           agent, \
           datetime(started_at, 'localtime') AS started_local, \
           datetime(ended_at, 'localtime') AS ended_local, \
           input_tokens, \
           output_tokens, \
           recall_calls \
         FROM session_metrics \
         ORDER BY started_at DESC \
         LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit], |row| {
            Ok(SessionSummary {
                session_id: row.get(0)?,
                agent: row.get(1)?,
                started_at: row.get::<_, Option<String>>(2)?.unwrap_or_default(),
                ended_at: row.get(3)?,
                input_tokens: row.get(4)?,
                output_tokens: row.get(5)?,
                recall_calls: row.get(6)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

#[derive(Debug)]
pub struct MetricsStatus {
    pub config: MetricsConfig,
    pub recall_rows: i64,
    pub session_rows: i64,
    pub attributed_rows: i64,
    pub current_session: Option<SessionRow>,
    pub recent_sessions: Vec<SessionRow>,
    pub token_totals_7d: Option<TokenTotals>,
    pub token_totals: Option<TokenTotals>,
    pub recalls_pruned: usize,
    pub sessions_pruned: usize,
}

#[derive(Debug, Clone)]
pub struct SessionRow {
    pub session_id: String,
    pub agent: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    pub recall_calls: i64,
}

#[derive(Debug)]
pub struct TokenTotals {
    pub input: i64,
    pub output: i64,
    pub cache_read: i64,
    pub cache_creation: i64,
    pub window_days: u32,
}

#[derive(Debug)]
pub struct EnableResult {
    pub already_enabled: bool,
    pub auto_detected_dir: Option<String>,
    pub config: MetricsConfig,
}

#[derive(Debug)]
pub struct RescanResult {
    pub recalls_attributed: usize,
    pub recalls_pruned: usize,
    pub sessions_pruned: usize,
    pub recall_rows: i64,
    pub session_rows: i64,
    pub attributed_rows: i64,
}

#[derive(Debug)]
pub struct PruneResult {
    pub recalls_pruned: usize,
    pub sessions_pruned: usize,
    pub retention_days: u32,
}

pub fn status(start: &Path) -> Result<MetricsStatus> {
    let ctx = db::open_project(start)?;
    let cfg = ctx.config.metrics.clone();

    let recall_rows: i64 = ctx
        .conn
        .query_row("SELECT COUNT(*) FROM recall_metrics", [], |r| r.get(0))
        .unwrap_or(0);

    let attributed_rows: i64 = ctx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM recall_metrics WHERE session_id IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);

    let session_rows: i64 = ctx
        .conn
        .query_row("SELECT COUNT(*) FROM session_metrics", [], |r| r.get(0))
        .unwrap_or(0);

    let recent_sessions = query_recent_sessions(&ctx.conn, 5)?;
    let current_session = recent_sessions.first().cloned();

    let token_totals_7d = query_token_totals_nd(&ctx.conn, 7);
    let token_totals = query_token_totals_nd(&ctx.conn, 30);

    let (recalls_pruned, sessions_pruned) =
        maintenance::prune_old(&ctx.conn, cfg.retention_days).unwrap_or((0, 0));

    Ok(MetricsStatus {
        config: cfg,
        recall_rows,
        session_rows,
        attributed_rows,
        current_session,
        recent_sessions,
        token_totals_7d,
        token_totals,
        recalls_pruned,
        sessions_pruned,
    })
}

pub fn enable(start: &Path) -> Result<EnableResult> {
    let ctx = db::open_project(start)?;
    let already_enabled = ctx.config.metrics.enabled;

    let mut new_config = ctx.config.clone();
    let mut auto_detected_dir: Option<String> = None;

    if new_config.metrics.claude_transcripts_dir.is_empty()
        && let Some(dir) = detect_claude_transcripts_dir(&ctx.paths.repo_root)
    {
        let dir_str = dir.to_string_lossy().to_string();
        new_config.metrics.claude_transcripts_dir = dir_str.clone();
        auto_detected_dir = Some(dir_str);
    }

    if new_config.metrics.codex_transcripts_dir.is_empty()
        && let Some(dir) = detect_codex_sessions_dir()
    {
        new_config.metrics.codex_transcripts_dir = dir.to_string_lossy().to_string();
    }

    new_config.metrics.enabled = true;
    new_config.save(&ctx.paths.config_path)?;

    db::log_write(
        &ctx.conn,
        METRICS_ACTOR,
        "config",
        None,
        "update",
        "metrics enable",
    )?;

    Ok(EnableResult {
        already_enabled,
        auto_detected_dir,
        config: new_config.metrics,
    })
}

pub fn disable(start: &Path) -> Result<()> {
    let ctx = db::open_project(start)?;
    let mut new_config = ctx.config.clone();
    new_config.metrics.enabled = false;
    new_config.save(&ctx.paths.config_path)?;

    db::log_write(
        &ctx.conn,
        METRICS_ACTOR,
        "config",
        None,
        "update",
        "metrics disable",
    )?;
    Ok(())
}

/// What `memhub metrics calibrate` reports.
#[derive(Debug, Clone)]
pub struct CalibrateResult {
    pub cl100k_tokens: usize,
    pub real_tokens: usize,
    pub previous_factor: f64,
    pub factor: f64,
    pub model: String,
}

/// Run a one-time tokenizer calibration and persist the multiplier to
/// `[metrics] calibration_factor`. Reads `ANTHROPIC_API_KEY` from the
/// environment — this is the only memhub command that touches the
/// network, and only when explicitly invoked. Metrics need not be
/// enabled to calibrate (the stored factor is inert until metrics run),
/// but a project must exist so the factor lands in the repo's local
/// config. The factor is *not* applied retroactively: rows written
/// before calibration keep their earlier scaling. Calibration corrects a
/// fixed ±10% bias, so re-running it occasionally is the intended way to
/// refresh after a binary/tokenizer change.
pub fn calibrate(start: &Path, model: Option<String>) -> Result<CalibrateResult> {
    let api_key = std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
        MemhubError::InvalidInput(
            "ANTHROPIC_API_KEY is not set; calibration needs it for the one-time \
             count_tokens call. Export it and re-run `memhub metrics calibrate`."
                .to_string(),
        )
    })?;

    let model =
        model.unwrap_or_else(|| crate::metrics::calibrate::DEFAULT_CALIBRATION_MODEL.to_string());

    let ctx = db::open_project(start)?;
    let previous_factor = ctx.config.metrics.calibration_factor;

    // The single network call lives behind this — fails loudly with a
    // clear message and leaves config untouched on any error.
    let result = crate::metrics::calibrate::calibrate(&api_key, &model)?;

    let mut new_config = ctx.config.clone();
    new_config.metrics.calibration_factor = result.factor;
    new_config.save(&ctx.paths.config_path)?;

    // Apply immediately so any tokens_of later in this process is calibrated.
    crate::metrics::tokenizer::set_calibration_factor(result.factor);

    db::log_write(
        &ctx.conn,
        METRICS_ACTOR,
        "config",
        None,
        "update",
        &format!(
            "metrics calibrate: factor {:.4} (cl100k={} real={} model={})",
            result.factor, result.cl100k_tokens, result.real_tokens, result.model
        ),
    )?;

    Ok(CalibrateResult {
        cl100k_tokens: result.cl100k_tokens,
        real_tokens: result.real_tokens,
        previous_factor,
        factor: result.factor,
        model: result.model,
    })
}

pub fn rescan(start: &Path) -> Result<RescanResult> {
    let ctx = db::open_project(start)?;
    let cfg = &ctx.config.metrics;

    if !cfg.enabled {
        return Err(MemhubError::InvalidInput(
            "metrics not enabled; run `memhub metrics enable` first".to_string(),
        ));
    }
    if !cfg.session_accounting {
        return Err(MemhubError::InvalidInput(
            "session_accounting is disabled in config; \
             set [metrics] session_accounting = true to use rescan"
                .to_string(),
        ));
    }

    // open_project already scraped this pass; a second call is a no-op
    // (offsets consumed) but documents intent and picks up any new files
    // that appeared between open and here.
    session_scraper::scrape_if_enabled(&ctx.conn, cfg, &ctx.paths.repo_root);

    let recalls_attributed = maintenance::reconcile(&ctx.conn)?;
    let (recalls_pruned, sessions_pruned) = maintenance::prune_old(&ctx.conn, cfg.retention_days)?;

    let recall_rows: i64 = ctx
        .conn
        .query_row("SELECT COUNT(*) FROM recall_metrics", [], |r| r.get(0))
        .unwrap_or(0);
    let attributed_rows: i64 = ctx
        .conn
        .query_row(
            "SELECT COUNT(*) FROM recall_metrics WHERE session_id IS NOT NULL",
            [],
            |r| r.get(0),
        )
        .unwrap_or(0);
    let session_rows: i64 = ctx
        .conn
        .query_row("SELECT COUNT(*) FROM session_metrics", [], |r| r.get(0))
        .unwrap_or(0);

    Ok(RescanResult {
        recalls_attributed,
        recalls_pruned,
        sessions_pruned,
        recall_rows,
        session_rows,
        attributed_rows,
    })
}

pub fn prune(start: &Path) -> Result<PruneResult> {
    let ctx = db::open_project(start)?;
    let retention_days = ctx.config.metrics.retention_days;
    let (recalls_pruned, sessions_pruned) = maintenance::prune_old(&ctx.conn, retention_days)?;
    Ok(PruneResult {
        recalls_pruned,
        sessions_pruned,
        retention_days,
    })
}

fn query_recent_sessions(conn: &rusqlite::Connection, limit: i64) -> Result<Vec<SessionRow>> {
    let mut stmt = conn.prepare(
        "SELECT session_id, agent, \
         datetime(started_at, 'localtime') AS started_local, \
         datetime(ended_at, 'localtime') AS ended_local, \
         input_tokens, output_tokens, cache_read_tokens, cache_creation_tokens, recall_calls \
         FROM session_metrics \
         ORDER BY started_at DESC \
         LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit], |row| {
            Ok(SessionRow {
                session_id: row.get(0)?,
                agent: row.get(1)?,
                started_at: row.get(2)?,
                ended_at: row.get(3)?,
                input_tokens: row.get(4)?,
                output_tokens: row.get(5)?,
                cache_read_tokens: row.get(6)?,
                cache_creation_tokens: row.get(7)?,
                recall_calls: row.get(8)?,
            })
        })?
        .filter_map(|r| r.ok())
        .collect();
    Ok(rows)
}

fn query_token_totals_nd(conn: &rusqlite::Connection, days: u32) -> Option<TokenTotals> {
    let modifier = format!("-{days} days");
    conn.query_row(
        "SELECT \
           COALESCE(SUM(input_tokens), 0), \
           COALESCE(SUM(output_tokens), 0), \
           COALESCE(SUM(cache_read_tokens), 0), \
           COALESCE(SUM(cache_creation_tokens), 0) \
         FROM session_metrics \
         WHERE datetime(started_at) >= datetime('now', ?1)",
        params![modifier],
        |row| {
            Ok(TokenTotals {
                input: row.get(0)?,
                output: row.get(1)?,
                cache_read: row.get(2)?,
                cache_creation: row.get(3)?,
                window_days: days,
            })
        },
    )
    .ok()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::init;
    use tempfile::tempdir;

    fn init_project(dir: &std::path::Path) {
        init::run(dir).expect("init");
    }

    #[test]
    fn status_works_when_disabled() {
        let temp = tempdir().expect("tempdir");
        init_project(temp.path());
        let s = status(temp.path()).expect("status");
        assert!(!s.config.enabled);
        assert_eq!(s.recall_rows, 0);
        assert_eq!(s.session_rows, 0);
    }

    #[test]
    fn enable_sets_enabled_flag_in_config() {
        let temp = tempdir().expect("tempdir");
        init_project(temp.path());
        let r = enable(temp.path()).expect("enable");
        assert!(!r.already_enabled);
        assert!(r.config.enabled);

        // Config file persisted across a fresh open_project.
        let ctx = db::open_project(temp.path()).expect("open");
        assert!(ctx.config.metrics.enabled);
    }

    #[test]
    fn enable_is_idempotent() {
        let temp = tempdir().expect("tempdir");
        init_project(temp.path());
        enable(temp.path()).expect("first enable");
        let r = enable(temp.path()).expect("second enable");
        assert!(r.already_enabled);
        assert!(r.config.enabled);
    }

    #[test]
    fn disable_clears_enabled_flag() {
        let temp = tempdir().expect("tempdir");
        init_project(temp.path());
        enable(temp.path()).expect("enable");
        disable(temp.path()).expect("disable");

        let ctx = db::open_project(temp.path()).expect("open");
        assert!(!ctx.config.metrics.enabled);
    }

    #[test]
    fn prune_returns_zero_when_no_data() {
        let temp = tempdir().expect("tempdir");
        init_project(temp.path());
        let r = prune(temp.path()).expect("prune");
        assert_eq!(r.recalls_pruned, 0);
        assert_eq!(r.sessions_pruned, 0);
        assert_eq!(r.retention_days, 90);
    }

    #[test]
    fn rescan_requires_metrics_to_be_enabled() {
        let temp = tempdir().expect("tempdir");
        init_project(temp.path());
        let err = rescan(temp.path()).expect_err("should fail when disabled");
        let msg = err.to_string();
        assert!(msg.contains("not enabled"), "unexpected error: {msg}");
    }

    #[test]
    fn rescan_returns_zero_counts_on_empty_db() {
        let temp = tempdir().expect("tempdir");
        init_project(temp.path());
        enable(temp.path()).expect("enable");
        let r = rescan(temp.path()).expect("rescan");
        assert_eq!(r.recall_rows, 0);
        assert_eq!(r.session_rows, 0);
        assert_eq!(r.recalls_attributed, 0);
        assert_eq!(r.recalls_pruned, 0);
        assert_eq!(r.sessions_pruned, 0);
    }

    #[test]
    fn enable_logs_a_writes_log_entry() {
        let temp = tempdir().expect("tempdir");
        init_project(temp.path());
        enable(temp.path()).expect("enable");

        let ctx = db::open_project(temp.path()).expect("open");
        let count: i64 = ctx
            .conn
            .query_row(
                "SELECT COUNT(*) FROM writes_log WHERE actor = 'cli:user' AND action = 'update'",
                [],
                |r| r.get(0),
            )
            .expect("query");
        assert!(count >= 1, "expected at least one writes_log entry");
    }

    #[test]
    fn detect_transcripts_dir_encodes_path_correctly() {
        // White-box: verify the path encoding logic by checking a known
        // repo root. We only verify the encoded string shape, not the
        // filesystem probe, so this is pure.
        let root = std::path::Path::new("/Users/alice/my-project");
        // Simulate what detect_claude_transcripts_dir does, without
        // needing HOME or a real directory.
        let abs_str = root.to_string_lossy();
        let encoded = format!("-{}", abs_str.trim_start_matches('/').replace('/', "-"));
        assert_eq!(encoded, "-Users-alice-my-project");
    }

    #[test]
    fn period_totals_cache_churn_window_vs_per_session_mean() {
        let temp = tempdir().expect("tempdir");
        init_project(temp.path());
        enable(temp.path()).expect("enable");
        {
            let ctx = db::open_project(temp.path()).expect("open");
            // Session A: low churn (10%). Session B: high churn (50%).
            // Window aggregate is token-weighted, so A's larger volume pulls
            // it toward 10%; the per-session mean weights both equally → 30%.
            ctx.conn
                .execute(
                    "INSERT INTO session_metrics \
                     (session_id, agent, started_at, ended_at, \
                      input_tokens, output_tokens, \
                      cache_read_tokens, cache_creation_tokens) VALUES \
                     ('a', 'claude-code', datetime('now','-1 hour'), \
                      datetime('now'), 10, 10, 900, 100), \
                     ('b', 'claude-code', datetime('now','-2 hours'), \
                      datetime('now','-1 hour'), 10, 10, 200, 200)",
                    [],
                )
                .expect("sessions");
        }

        let ctx = db::open_project(temp.path()).expect("reopen");
        let t = query_period_totals(&ctx.conn, 7).expect("totals");

        // Window churn = 300 creation / 1400 total ≈ 21.4%.
        let window = t.churn_pct().expect("window churn");
        assert!(
            (window - 300.0 / 1400.0 * 100.0).abs() < 1e-9,
            "window churn = {window}"
        );

        // Per-session mean = (10% + 50%) / 2 = 30%, equal-weighted.
        let mean = t.mean_session_churn_pct.expect("per-session mean");
        assert!((mean - 30.0).abs() < 1e-9, "per-session mean = {mean}");
    }

    #[test]
    fn median_i64_handles_odd_even_and_empty() {
        assert_eq!(median_i64(&[]), None);
        assert_eq!(median_i64(&[7]), Some(7));
        assert_eq!(median_i64(&[10, 20, 30]), Some(20));
        assert_eq!(median_i64(&[10, 20, 30, 40]), Some(25));
    }

    #[test]
    fn period_totals_empirical_baseline_is_median_of_no_recall_sessions() {
        let temp = tempdir().expect("tempdir");
        init_project(temp.path());
        enable(temp.path()).expect("enable");
        {
            let ctx = db::open_project(temp.path()).expect("open");
            // Three no-recall sessions (recall_calls = 0) with baselines
            // 10k / 20k / 30k → median 20k. A recall-using session carries a
            // wildly different baseline and recall_calls > 0; it must be
            // excluded from the median (task 64).
            ctx.conn
                .execute(
                    "INSERT INTO session_metrics \
                     (session_id, agent, started_at, recall_calls, baseline_input_tokens) VALUES \
                     ('n1','claude-code',datetime('now','-1 hour'),0,10000), \
                     ('n2','claude-code',datetime('now','-2 hours'),0,20000), \
                     ('n3','claude-code',datetime('now','-3 hours'),0,30000), \
                     ('r1','claude-code',datetime('now','-4 hours'),2,999999)",
                    [],
                )
                .expect("sessions");
            // The recall-using session has two non-empty recalls, both
            // attributed to it → recall_sessions = 1, bundle_tokens = 2000.
            ctx.conn
                .execute(
                    "INSERT INTO recall_metrics \
                     (ts, session_id, query_hash, bundle_tokens, ledger_tokens, \
                      rerank_used, result_count) VALUES \
                     (datetime('now','-4 hours'),'r1','h1',1000,50000,1,3), \
                     (datetime('now','-4 hours'),'r1','h2',1000,50000,1,3)",
                    [],
                )
                .expect("recalls");
        }

        let ctx = db::open_project(temp.path()).expect("reopen");
        let t = query_period_totals(&ctx.conn, 7).expect("totals");

        assert_eq!(t.empirical_baseline, Some(20_000), "median of 10k/20k/30k");
        assert_eq!(t.recall_sessions, 1, "one session had a non-empty recall");
        assert_eq!(t.bundle_tokens, 2_000);
        // Empirical offset = 2000 / (1 * 20000) = 10%.
        assert_eq!(t.empirical_offset_pct(), Some(10.0));
    }

    #[test]
    fn period_totals_churn_none_without_cache_activity() {
        let temp = tempdir().expect("tempdir");
        init_project(temp.path());
        enable(temp.path()).expect("enable");
        {
            let ctx = db::open_project(temp.path()).expect("open");
            ctx.conn
                .execute(
                    "INSERT INTO session_metrics \
                     (session_id, agent, started_at, ended_at, \
                      input_tokens, output_tokens) VALUES \
                     ('a', 'claude-code', datetime('now','-1 hour'), \
                      datetime('now'), 100, 200)",
                    [],
                )
                .expect("session");
        }
        let ctx = db::open_project(temp.path()).expect("reopen");
        let t = query_period_totals(&ctx.conn, 7).expect("totals");
        assert_eq!(t.churn_pct(), None);
        assert_eq!(t.mean_session_churn_pct, None);
    }
}
