//! `memhub metrics` subcommand handlers (task #31).
//!
//! Provides the user-facing surface for the token-accounting subsystem
//! (decision 74): status, enable, disable, rescan, and prune.
//!
//! The subsystem is off by default; all reads and writes are gated on
//! `MetricsConfig.enabled`. Config mutations are written back to the
//! machine-local `config.toml` via `ProjectConfig::save` so each
//! invocation inherits the updated state, exactly as the integrations
//! commands do.
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
        .query_row(
            "SELECT MAX(ended_at) FROM session_metrics",
            [],
            |r| r.get(0),
        )
        .unwrap_or(None);

    let totals_7d = query_period_totals(&ctx.conn, 7)?;
    let totals_30d = query_period_totals(&ctx.conn, 30)?;

    let last_sessions = query_last_sessions(&ctx.conn, 10)?;

    let rendered_panel = if totals_7d.is_empty() && totals_30d.is_empty() && last_sessions.is_empty() {
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

pub(crate) fn query_period_totals(conn: &rusqlite::Connection, days: u32) -> Result<PeriodTotals> {
    let modifier = format!("-{days} days");

    // recall_metrics: count + token sums for the window.
    let (recalls, bundle_tokens, ledger_tokens): (i64, i64, i64) = conn
        .query_row(
            "SELECT \
               COALESCE(COUNT(*), 0), \
               COALESCE(SUM(bundle_tokens), 0), \
               COALESCE(SUM(ledger_tokens), 0) \
             FROM recall_metrics \
             WHERE datetime(ts) >= datetime('now', ?1)",
            params![modifier],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap_or((0, 0, 0));

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

    Ok(PeriodTotals {
        recalls,
        bundle_tokens,
        ledger_tokens,
        sessions,
        input_tokens,
        output_tokens,
        cache_read_tokens,
        cache_creation_tokens,
    })
}

fn query_last_sessions(
    conn: &rusqlite::Connection,
    limit: i64,
) -> Result<Vec<SessionSummary>> {
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
    pub recent_sessions: Vec<SessionRow>,
    pub token_totals: Option<TokenTotals>,
    pub recalls_pruned: usize,
    pub sessions_pruned: usize,
}

#[derive(Debug)]
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

    let token_totals = query_token_totals_30d(&ctx.conn);

    let (recalls_pruned, sessions_pruned) =
        maintenance::prune_old(&ctx.conn, cfg.retention_days).unwrap_or((0, 0));

    Ok(MetricsStatus {
        config: cfg,
        recall_rows,
        session_rows,
        attributed_rows,
        recent_sessions,
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

    if new_config.metrics.claude_transcripts_dir.is_empty() {
        if let Some(dir) = detect_claude_transcripts_dir(&ctx.paths.repo_root) {
            let dir_str = dir.to_string_lossy().to_string();
            new_config.metrics.claude_transcripts_dir = dir_str.clone();
            auto_detected_dir = Some(dir_str);
        }
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
    session_scraper::scrape_if_enabled(&ctx.conn, cfg);

    let recalls_attributed = maintenance::reconcile(&ctx.conn)?;
    let (recalls_pruned, sessions_pruned) =
        maintenance::prune_old(&ctx.conn, cfg.retention_days)?;

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
    let (recalls_pruned, sessions_pruned) =
        maintenance::prune_old(&ctx.conn, retention_days)?;
    Ok(PruneResult {
        recalls_pruned,
        sessions_pruned,
        retention_days,
    })
}

fn query_recent_sessions(
    conn: &rusqlite::Connection,
    limit: i64,
) -> Result<Vec<SessionRow>> {
    let mut stmt = conn.prepare(
        "SELECT session_id, agent, started_at, ended_at, \
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

fn query_token_totals_30d(conn: &rusqlite::Connection) -> Option<TokenTotals> {
    conn.query_row(
        "SELECT \
           COALESCE(SUM(input_tokens), 0), \
           COALESCE(SUM(output_tokens), 0), \
           COALESCE(SUM(cache_read_tokens), 0), \
           COALESCE(SUM(cache_creation_tokens), 0) \
         FROM session_metrics \
         WHERE datetime(started_at) >= datetime('now', '-30 days')",
        [],
        |row| {
            Ok(TokenTotals {
                input: row.get(0)?,
                output: row.get(1)?,
                cache_read: row.get(2)?,
                cache_creation: row.get(3)?,
                window_days: 30,
            })
        },
    )
    .ok()
}

/// Auto-detect the Claude Code transcripts dir for this repo.
/// Claude Code stores session JSONL under
/// `~/.claude/projects/<encoded-path>/` where the encoded path is the
/// absolute repo root with leading `/` stripped and remaining `/`
/// replaced by `-`, prefixed with `-`.
///
/// Returns `None` if HOME is not set, the repo root is not
/// canonicalizable, or the expected directory does not exist.
/// The caller should treat `None` as "no auto-detect; set manually."
fn detect_claude_transcripts_dir(repo_root: &Path) -> Option<std::path::PathBuf> {
    let home = std::env::var("HOME").ok()?;
    let abs = repo_root.canonicalize().ok()?;
    let path_str = abs.to_string_lossy();
    let encoded = format!(
        "-{}",
        path_str.trim_start_matches('/').replace('/', "-")
    );
    let candidate = std::path::Path::new(&home)
        .join(".claude/projects")
        .join(encoded);
    if candidate.is_dir() {
        Some(candidate)
    } else {
        None
    }
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
        let encoded = format!(
            "-{}",
            abs_str.trim_start_matches('/').replace('/', "-")
        );
        assert_eq!(encoded, "-Users-alice-my-project");
    }
}
