//! Shared text formatter for the token-accounting panel (decision 76).
//!
//! `render_period_block` is the single atom used by every surface:
//! - MCP `memhub.metrics` renders two blocks (7d + 30d) inside `rendered_panel`
//! - `src/render/mod.rs` (#33) renders the 7d block into the PROJECT.md
//!   Token Accounting section
//!
//! All period-block text is produced here. No second formatter anywhere.

/// Aggregated token-accounting totals for a time window.
#[derive(Debug, Default, Clone)]
pub struct PeriodTotals {
    pub recalls: i64,
    pub bundle_tokens: i64,
    pub ledger_tokens: i64,
    pub sessions: i64,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub cache_read_tokens: i64,
    pub cache_creation_tokens: i64,
    /// Mean of each in-window session's own churn ratio
    /// (`cache_creation / (cache_read + cache_creation)`), weighting every
    /// session equally so one large session cannot dominate the figure.
    /// `None` when no session in the window had any cache activity. Computed
    /// per-session in SQL (not derivable from the summed fields above), so
    /// it is carried as a field rather than a method.
    pub mean_session_churn_pct: Option<f64>,
    /// Number of distinct sessions in the window that had ≥1 non-empty
    /// recall — the sessions the empirical counterfactual is charged
    /// against. Multiplies `empirical_baseline` to form the empirical
    /// denominator (task 64).
    pub recall_sessions: i64,
    /// Median first-turn startup cost (`baseline_input_tokens`) across the
    /// window's NO-recall sessions (`recall_calls = 0`). This is the
    /// measured counterfactual baseline: what a session that didn't use
    /// recall actually cost to start. `None` when no no-recall session in
    /// the window has a recorded baseline yet (task 64, decision 109).
    pub empirical_baseline: Option<i64>,
}

impl PeriodTotals {
    /// Ratio of bundle tokens to the ASSUMED full-ledger baseline as a
    /// percentage [0, ∞). This is the legacy counterfactual: we guess the
    /// agent would otherwise have loaded the whole `PROJECT_LEDGER.md`.
    /// Shown alongside `empirical_offset_pct` so the gap between the
    /// assumption and the measured cost is visible. None when
    /// ledger_tokens == 0 (nothing to compare against).
    pub fn context_offset_pct(&self) -> Option<f64> {
        if self.ledger_tokens == 0 {
            return None;
        }
        Some(self.bundle_tokens as f64 / self.ledger_tokens as f64 * 100.0)
    }

    /// Ratio of bundle tokens to the MEASURED counterfactual baseline as a
    /// percentage [0, ∞) — the empirical headline (task 64). The
    /// denominator is `recall_sessions × empirical_baseline`: what the
    /// recall-using sessions would have cost at startup had they instead
    /// cold-loaded like a session that used no recall. `None` when there is
    /// no measured baseline yet or no recall-using session to charge it
    /// against.
    pub fn empirical_offset_pct(&self) -> Option<f64> {
        let baseline = self.empirical_baseline?;
        let denom = self.recall_sessions.checked_mul(baseline)?;
        if denom <= 0 {
            return None;
        }
        Some(self.bundle_tokens as f64 / denom as f64 * 100.0)
    }

    /// Window-level cache churn as a percentage: the share of all cache
    /// tokens that were *creation* (rebuilt prefix) rather than *read*
    /// (reused prefix). At a 1M-token window the real recurring cost is
    /// cumulative per-turn `cache_read`, so a high creation share is the
    /// honest "we kept rebuilding the cache" signal. Summed across the
    /// window, so it is dominated by the largest sessions — read alongside
    /// `mean_session_churn_pct` for the per-session view.
    /// `None` when there was no cache activity at all.
    pub fn churn_pct(&self) -> Option<f64> {
        let denom = self.cache_read_tokens + self.cache_creation_tokens;
        if denom <= 0 {
            return None;
        }
        Some(self.cache_creation_tokens as f64 / denom as f64 * 100.0)
    }

    pub fn is_empty(&self) -> bool {
        self.recalls == 0 && self.sessions == 0
    }
}

/// One session row for the recent-sessions table.
#[derive(Debug)]
pub struct SessionSummary {
    pub session_id: String,
    pub agent: String,
    pub started_at: String,
    pub ended_at: Option<String>,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub recall_calls: i64,
}

/// Render a period block for `label` (e.g. "Last 7 days").
///
/// Shape:
/// ```text
/// Recalls:        N
/// Sessions:       N
/// Real tokens:    in=N  out=N  cache_read=N  cache_creation=N
/// Cache churn:    N% (cache_creation share) · N% per-session mean
/// Context offset: N% of measured no-recall startup (~N tok/session)
///                 N% of assumed full-ledger baseline
/// ```
/// The cache-churn line is omitted when there was no cache activity; the
/// `· N% per-session mean` tail is dropped when no session carried cache
/// data. The context-offset block prefers the measured no-recall baseline
/// (task 64) and shows the assumed full-ledger baseline beneath it so the
/// gap is visible; each of the two offset lines is omitted when its baseline
/// is unavailable, and the whole block is omitted when neither is.
pub fn render_period_block(label: &str, t: &PeriodTotals) -> String {
    let mut lines = vec![
        format!("### {label}"),
        format!("Recalls:        {}", fmt_n(t.recalls)),
        format!("Sessions:       {}", fmt_n(t.sessions)),
        format!(
            "Real tokens:    in={}  out={}  cache_read={}  cache_creation={}",
            fmt_n(t.input_tokens),
            fmt_n(t.output_tokens),
            fmt_n(t.cache_read_tokens),
            fmt_n(t.cache_creation_tokens),
        ),
    ];
    if let Some(churn) = t.churn_pct() {
        let mut line = format!("Cache churn:    {churn:.0}% (cache_creation share)");
        if let Some(mean) = t.mean_session_churn_pct {
            line.push_str(&format!(" · {mean:.0}% per-session mean"));
        }
        lines.push(line);
    }
    // Context offset prefers the measured no-recall baseline (task 64); the
    // assumed full-ledger baseline follows on an aligned continuation line so
    // the gap between assumption and measurement is visible. The label
    // (16 cols) is printed only on whichever line comes first.
    let empirical = t.empirical_offset_pct();
    let assumed = t.context_offset_pct();
    let mut printed_label = false;
    if let (Some(pct), Some(baseline)) = (empirical, t.empirical_baseline) {
        lines.push(format!(
            "Context offset: {pct:.0}% of measured no-recall startup (~{} tok/session)",
            fmt_n(baseline)
        ));
        printed_label = true;
    }
    if let Some(pct) = assumed {
        let prefix = if printed_label {
            "                "
        } else {
            "Context offset: "
        };
        lines.push(format!("{prefix}{pct:.0}% of assumed full-ledger baseline"));
    }
    lines.join("\n")
}

/// Render the full layered panel for the `/metrics` skill:
/// 7d block, 30d block, and a recent-sessions table (≤10 rows).
///
/// Branch: enabled + ≥1 row of data.
pub fn render_panel(
    totals_7d: &PeriodTotals,
    totals_30d: &PeriodTotals,
    sessions: &[SessionSummary],
) -> String {
    let mut parts = Vec::new();
    parts.push(render_period_block("Last 7 days", totals_7d));
    parts.push(render_period_block("Last 30 days", totals_30d));

    if !sessions.is_empty() {
        let mut table = String::new();
        table.push_str("### Recent sessions (newest first)\n");
        table.push_str(&format!(
            "{:<12}  {:<12}  {:<14}  {:>12}  {:>12}  {:>7}\n",
            "session", "agent", "started (UTC)", "in", "out", "recalls"
        ));
        table.push_str(&format!(
            "{:-<12}  {:-<12}  {:-<14}  {:->12}  {:->12}  {:->7}\n",
            "", "", "", "", "", ""
        ));
        for s in sessions {
            let id_prefix = safe_prefix(&s.session_id, 8);
            let agent = truncate(&s.agent, 12);
            let started = fmt_ts(&s.started_at);
            table.push_str(&format!(
                "{:<12}  {:<12}  {:<14}  {:>12}  {:>12}  {:>7}\n",
                id_prefix,
                agent,
                started,
                fmt_n(s.input_tokens),
                fmt_n(s.output_tokens),
                fmt_n(s.recall_calls),
            ));
        }
        parts.push(table.trim_end().to_string());
    }

    parts.join("\n\n")
}

/// Panel text when metrics is enabled but zero rows have been captured yet.
pub fn render_panel_no_data() -> &'static str {
    "Metrics enabled — no recall or session data captured yet."
}

fn fmt_n(n: i64) -> String {
    if n == 0 {
        return "0".to_string();
    }
    let negative = n < 0;
    let mut digits = n.unsigned_abs().to_string();
    let mut result = String::new();
    let len = digits.len();
    for (i, ch) in digits.drain(..).enumerate() {
        if i > 0 && (len - i).is_multiple_of(3) {
            result.push(',');
        }
        result.push(ch);
    }
    if negative {
        result.insert(0, '-');
    }
    result
}

/// Format an ISO-8601 timestamp as `MM-DD HH:MM (UTC)`.
/// Handles both `T`-separated (`2026-05-15T13:35:25.609Z`) and
/// space-separated (`2026-05-15 13:35:25`) forms.
/// Returns a best-effort slice on malformed input.
fn fmt_ts(ts: &str) -> String {
    let s = ts.trim();
    if s.len() >= 16 {
        let sep = s.as_bytes().get(10).copied();
        if sep == Some(b'T') || sep == Some(b' ') {
            let month_day = &s[5..10];
            let hhmm = &s[11..16];
            return format!("{month_day} {hhmm}");
        }
    }
    s[..s.len().min(16)].to_string()
}

fn safe_prefix(s: &str, n: usize) -> String {
    s.chars().take(n).collect()
}

fn truncate(s: &str, max: usize) -> String {
    let c: String = s.chars().take(max).collect();
    if s.chars().count() > max {
        // replace last char with '…'
        let mut t: Vec<char> = c.chars().collect();
        if let Some(last) = t.last_mut() {
            *last = '…';
        }
        t.into_iter().collect()
    } else {
        c
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_n_adds_commas() {
        assert_eq!(fmt_n(0), "0");
        assert_eq!(fmt_n(999), "999");
        assert_eq!(fmt_n(1_000), "1,000");
        assert_eq!(fmt_n(1_234_567), "1,234,567");
        assert_eq!(fmt_n(-42_000), "-42,000");
    }

    #[test]
    fn fmt_ts_handles_t_separator() {
        assert_eq!(fmt_ts("2026-05-15T13:35:25.609Z"), "05-15 13:35");
    }

    #[test]
    fn fmt_ts_handles_space_separator() {
        assert_eq!(fmt_ts("2026-05-15 13:35:25"), "05-15 13:35");
    }

    #[test]
    fn render_period_block_omits_offset_when_no_ledger_data() {
        let t = PeriodTotals {
            recalls: 3,
            sessions: 1,
            input_tokens: 100,
            ..Default::default()
        };
        let block = render_period_block("Last 7 days", &t);
        assert!(block.contains("Recalls:        3"));
        assert!(!block.contains("Context offset"));
    }

    #[test]
    fn render_period_block_includes_assumed_offset_when_ledger_nonzero() {
        let t = PeriodTotals {
            recalls: 5,
            bundle_tokens: 500,
            ledger_tokens: 1000,
            sessions: 2,
            ..Default::default()
        };
        let block = render_period_block("Last 7 days", &t);
        // No empirical baseline → the assumed line carries the label.
        assert!(block.contains("Context offset: 50% of assumed full-ledger baseline"));
        assert!(!block.contains("measured no-recall startup"));
    }

    #[test]
    fn empirical_offset_pct_uses_recall_sessions_times_baseline() {
        let t = PeriodTotals {
            bundle_tokens: 5_000,
            recall_sessions: 4,
            empirical_baseline: Some(25_000), // denom = 100_000
            ..Default::default()
        };
        assert_eq!(t.empirical_offset_pct(), Some(5.0));
    }

    #[test]
    fn empirical_offset_pct_is_none_without_baseline_or_sessions() {
        let no_baseline = PeriodTotals {
            bundle_tokens: 5_000,
            recall_sessions: 4,
            empirical_baseline: None,
            ..Default::default()
        };
        assert_eq!(no_baseline.empirical_offset_pct(), None);

        let no_sessions = PeriodTotals {
            bundle_tokens: 5_000,
            recall_sessions: 0,
            empirical_baseline: Some(25_000),
            ..Default::default()
        };
        assert_eq!(no_sessions.empirical_offset_pct(), None);
    }

    #[test]
    fn render_period_block_prefers_measured_baseline_and_shows_assumed_alongside() {
        let t = PeriodTotals {
            recalls: 5,
            bundle_tokens: 5_000,
            ledger_tokens: 50_000, // assumed: 10%
            sessions: 6,
            recall_sessions: 4,
            empirical_baseline: Some(25_000), // measured: 5_000 / 100_000 = 5%
            ..Default::default()
        };
        let block = render_period_block("Last 7 days", &t);
        assert!(
            block.contains("Context offset: 5% of measured no-recall startup (~25,000 tok/session)")
        );
        assert!(block.contains("                10% of assumed full-ledger baseline"));
    }

    #[test]
    fn render_period_block_omits_both_offsets_when_no_baseline_at_all() {
        let t = PeriodTotals {
            recalls: 3,
            bundle_tokens: 400,
            sessions: 1,
            ..Default::default()
        };
        let block = render_period_block("Last 7 days", &t);
        assert!(!block.contains("Context offset"));
    }

    #[test]
    fn churn_pct_is_creation_share_of_cache_tokens() {
        let t = PeriodTotals {
            cache_read_tokens: 900,
            cache_creation_tokens: 100,
            ..Default::default()
        };
        assert_eq!(t.churn_pct(), Some(10.0));
    }

    #[test]
    fn churn_pct_is_none_without_cache_activity() {
        let t = PeriodTotals {
            input_tokens: 500,
            ..Default::default()
        };
        assert_eq!(t.churn_pct(), None);
    }

    #[test]
    fn render_period_block_includes_churn_with_mean() {
        let t = PeriodTotals {
            sessions: 2,
            cache_read_tokens: 880,
            cache_creation_tokens: 120,
            mean_session_churn_pct: Some(18.0),
            ..Default::default()
        };
        let block = render_period_block("Last 7 days", &t);
        assert!(block.contains("Cache churn:    12% (cache_creation share) · 18% per-session mean"));
    }

    #[test]
    fn render_period_block_churn_drops_mean_tail_when_absent() {
        let t = PeriodTotals {
            sessions: 1,
            cache_read_tokens: 900,
            cache_creation_tokens: 100,
            mean_session_churn_pct: None,
            ..Default::default()
        };
        let block = render_period_block("Last 7 days", &t);
        assert!(block.contains("Cache churn:    10% (cache_creation share)"));
        assert!(!block.contains("per-session mean"));
    }

    #[test]
    fn render_period_block_omits_churn_without_cache_activity() {
        let t = PeriodTotals {
            sessions: 1,
            input_tokens: 100,
            ..Default::default()
        };
        let block = render_period_block("Last 7 days", &t);
        assert!(!block.contains("Cache churn"));
    }

    #[test]
    fn render_panel_no_data_is_stable_string() {
        assert_eq!(
            render_panel_no_data(),
            "Metrics enabled — no recall or session data captured yet."
        );
    }
}
