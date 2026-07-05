//! Post-scrape maintenance for the token-accounting subsystem
//! (decision 74, task #30): the recall→session reconciler and the
//! retention pruner.
//!
//! Both run opportunistically and never on a schedule: `run_if_enabled`
//! is called once from `db::open_project`, right after the Component B
//! scrape has refreshed the session windows, so on every `memhub`
//! invocation the reconciler sees the freshest `session_metrics` bounds.
//! `prune_old` is also re-exported so the `memhub metrics status` path
//! (task #31) can run the pruner at its own tail per decision 74.
//!
//! Like the scraper, this is gated and defensive: a no-op early return
//! on a non-opted-in install, and any SQL failure is logged and
//! swallowed — losing a maintenance pass must never fail an otherwise
//! successful host command.
//!
//! ## Reconciler — attribution is intentionally lossy
//!
//! `recall_metrics` rows are written with `session_id = NULL` (the
//! recall path has no session identity). We attribute them after the
//! fact by timestamp window: a recall is assigned to the session whose
//! `[started_at, COALESCE(ended_at, started_at + N h)]` interval contains
//! the recall `ts`. An open session (`ended_at IS NULL`) is capped at
//! `started_at + OPEN_SESSION_MAX_HOURS` rather than reaching to `now`, so
//! a sync-adopted zombie from another machine can't swallow local recalls
//! (F3). When two sessions' windows overlap (concurrent Claude Code +
//! Codex, or two Claude Code windows on the same machine) a recall in
//! the overlap is assigned to the **most recently started** containing
//! session and the other loses it. This is a deliberate, documented
//! loss — recall attribution is an advisory rollup, not an accounting
//! ledger, and a per-row "ambiguous" state would complicate every
//! consumer for no real benefit. Per task #30 this stays a code
//! comment, never a runtime warning.
//!
//! ## Timestamp normalization
//!
//! `recall_metrics.ts` is written by SQLite `CURRENT_TIMESTAMP`
//! (`YYYY-MM-DD HH:MM:SS`, space separator). `session_metrics`
//! start/end come from Claude Code's JSONL `timestamp`
//! (`YYYY-MM-DDTHH:MM:SS.sssZ`, `T` separator) or, as a fallback, from
//! `CURRENT_TIMESTAMP`. A raw string compare between the two formats is
//! wrong: a space (0x20) always sorts before `T` (0x54), so a
//! same-instant recall would compare as strictly *before* a `T`-form
//! session bound and never match. Every comparison therefore wraps both
//! sides in SQLite `datetime(...)`, which parses both forms (and the
//! trailing `Z` / fractional seconds) into one canonical
//! `YYYY-MM-DD HH:MM:SS`. `datetime()` yields NULL on an unparseable
//! value, so a malformed timestamp simply fails to match / fails to be
//! pruned — the conservative direction.

use rusqlite::{Connection, params};

use crate::Result;
use crate::config::MetricsConfig;
use crate::db::log_write;

const RECONCILER_ACTOR: &str = "metrics:reconciler";
const PRUNER_ACTOR: &str = "metrics:pruner";

/// How long a still-open (`ended_at IS NULL`) session's attribution
/// window may extend past its `started_at`. A whole-DB sync adopt (M10)
/// can import another machine's open session row; `COALESCE(ended_at,
/// 'now')` would give it a window reaching to *this* machine's clock and
/// swallow every local recall (F3 — a Mac zombie captured all 20 Windows
/// recalls). Capping at `started_at + N h` keeps a real live session's
/// own recalls attributable while a days-old zombie's window has long
/// since closed. A local session is re-stamped with a fresh `ended_at`
/// on every scrape, so this only ever bites imported/abandoned rows.
const OPEN_SESSION_MAX_HOURS: i64 = 12;

/// Opportunistic entry point, gated by the `metrics.enabled` master
/// switch only — both maintenance jobs touch `recall_metrics`, which
/// the recall-proxy component populates independently of the
/// `session_accounting` sub-switch, so this must not hide behind it.
/// Off by default, so this is a zero-cost early return on a
/// non-opted-in install. Errors are logged, never propagated.
pub fn run_if_enabled(conn: &Connection, cfg: &MetricsConfig) {
    if !cfg.enabled {
        return;
    }

    match reconcile(conn) {
        Ok(n) if n > 0 => {
            let _ = log_write(
                conn,
                RECONCILER_ACTOR,
                "recall_metrics",
                None,
                "reconcile",
                &format!("attributed {n} recall row(s) to a session by timestamp window"),
            );
        }
        Ok(_) => {}
        Err(err) => log::warn!("metrics: recall→session reconcile failed: {err}"),
    }

    match prune_old(conn, cfg.retention_days) {
        Ok((0, 0)) => {}
        Ok((recalls, sessions)) => {
            let _ = log_write(
                conn,
                PRUNER_ACTOR,
                "recall_metrics",
                None,
                "prune",
                &format!(
                    "retention {} days: deleted {recalls} recall_metrics + \
                     {sessions} session_metrics row(s)",
                    cfg.retention_days
                ),
            );
        }
        Err(err) => log::warn!("metrics: retention prune failed: {err}"),
    }
}

/// Fill `recall_metrics.session_id` for rows still NULL by matching the
/// recall `ts` into a session window, then recompute
/// `session_metrics.recall_calls` as the count of rows now attributed
/// to each session. Returns the number of recall rows newly attributed
/// this pass.
///
/// Both statements are idempotent: the fill only ever touches
/// `session_id IS NULL` rows (an already-attributed recall is never
/// reassigned), and `recall_calls` is a full recount rather than a
/// delta `+=`, so it stays correct even after the pruner later deletes
/// some recall rows. A session with zero attributed recalls is left
/// untouched (we never thrash every session row back to 0 on every
/// invocation); the consequence is that if a session's last recall row
/// is pruned away, its `recall_calls` keeps the last non-zero count —
/// an acceptable staleness for an advisory rollup.
pub fn reconcile(conn: &Connection) -> Result<usize> {
    // "Most recently started containing session wins" — see the
    // lossy-attribution note in the module docs. An open session's window
    // is capped at `started_at + OPEN_SESSION_MAX_HOURS` (not 'now') so an
    // imported zombie can't swallow this machine's recalls (F3).
    let window_cap = format!("+{OPEN_SESSION_MAX_HOURS} hours");
    let attributed = conn.execute(
        "UPDATE recall_metrics \
         SET session_id = ( \
             SELECT s.session_id FROM session_metrics s \
             WHERE datetime(recall_metrics.ts) >= datetime(s.started_at) \
               AND datetime(recall_metrics.ts) <= \
                   datetime(COALESCE(s.ended_at, datetime(s.started_at, ?1))) \
             ORDER BY s.started_at DESC \
             LIMIT 1 \
         ) \
         WHERE session_id IS NULL \
           AND EXISTS ( \
             SELECT 1 FROM session_metrics s \
             WHERE datetime(recall_metrics.ts) >= datetime(s.started_at) \
               AND datetime(recall_metrics.ts) <= \
                   datetime(COALESCE(s.ended_at, datetime(s.started_at, ?1))) \
           )",
        params![window_cap],
    )?;

    conn.execute(
        "UPDATE session_metrics \
         SET recall_calls = ( \
             SELECT COUNT(*) FROM recall_metrics r \
             WHERE r.session_id = session_metrics.session_id \
         ) \
         WHERE EXISTS ( \
             SELECT 1 FROM recall_metrics r \
             WHERE r.session_id = session_metrics.session_id \
         )",
        [],
    )?;

    Ok(attributed)
}

/// Delete metrics rows older than `retention_days`. `retention_days == 0`
/// means "keep forever" and is a no-op. `recall_metrics` is pruned by
/// `ts`; `session_metrics` only by a non-NULL `ended_at` (an open
/// session with no end is never pruned, however old its `started_at`).
/// Returns `(recall_rows_deleted, session_rows_deleted)`.
pub fn prune_old(conn: &Connection, retention_days: u32) -> Result<(usize, usize)> {
    if retention_days == 0 {
        return Ok((0, 0));
    }

    // Built in Rust rather than concatenated in SQL: `datetime('now',
    // '-N days')` is SQLite's UTC-now-minus-N-days, the same clock the
    // recall/scraper rows are stamped against.
    let modifier = format!("-{retention_days} days");

    let recalls = conn.execute(
        "DELETE FROM recall_metrics \
         WHERE datetime(ts) < datetime('now', ?1)",
        params![modifier],
    )?;

    let sessions = conn.execute(
        "DELETE FROM session_metrics \
         WHERE ended_at IS NOT NULL \
           AND datetime(ended_at) < datetime('now', ?1)",
        params![modifier],
    )?;

    // Per-turn rows (migration 0013) live exactly as long as their
    // parent session: prune by the session whose row was just removed,
    // not by the turn's own ts. This keeps the invariant simple (no
    // orphaned turn history, ever) and also sweeps any turn rows left
    // behind by an edge-case where a session_metrics row vanished
    // without going through this pruner. Not separately reported — it
    // is a derived consequence of the session prune above.
    conn.execute(
        "DELETE FROM session_turn_metrics \
         WHERE session_id NOT IN (SELECT session_id FROM session_metrics)",
        [],
    )?;

    Ok((recalls, sessions))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pruning_an_old_session_also_drops_its_turn_history() {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        let ctx = crate::db::open_project(temp.path()).expect("open");
        let conn = &ctx.conn;

        // One stale session (ended 100 days ago) with two turn rows,
        // and one fresh session with one turn row.
        conn.execute(
            "INSERT INTO session_metrics \
                (session_id, agent, started_at, ended_at) \
             VALUES \
                ('old', 'claude-code', datetime('now','-101 days'), \
                 datetime('now','-100 days')), \
                ('new', 'claude-code', datetime('now','-1 hour'), \
                 datetime('now'))",
            [],
        )
        .expect("seed sessions");
        conn.execute(
            "INSERT INTO session_turn_metrics (session_id, ts, input_tokens) \
             VALUES ('old', datetime('now','-100 days'), 1), \
                    ('old', datetime('now','-100 days'), 2), \
                    ('new', datetime('now'), 3)",
            [],
        )
        .expect("seed turns");

        let (_, sessions) = prune_old(conn, 90).expect("prune");
        assert_eq!(sessions, 1, "the stale session is pruned");

        let remaining: i64 = conn
            .query_row("SELECT COUNT(*) FROM session_turn_metrics", [], |r| {
                r.get(0)
            })
            .expect("count");
        assert_eq!(
            remaining, 1,
            "old session's 2 turn rows go with it; fresh one stays"
        );
        let orphans: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_turn_metrics \
                 WHERE session_id = 'old'",
                [],
                |r| r.get(0),
            )
            .expect("count orphans");
        assert_eq!(orphans, 0);
    }

    #[test]
    fn reconciler_ignores_a_stale_open_session_zombie() {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        let ctx = crate::db::open_project(temp.path()).expect("open");
        let conn = &ctx.conn;

        // A zombie: open (ended_at NULL), started 30 days ago — the shape a
        // whole-DB sync adopt imports from another machine. Its capped
        // window (started_at + 12h) closed weeks ago, so a recall *now*
        // must NOT be attributed to it (the F3 bug attributed all of them).
        conn.execute(
            "INSERT INTO session_metrics (session_id, agent, started_at, ended_at) \
             VALUES ('mac-zombie', 'claude-code', datetime('now','-30 days'), NULL)",
            [],
        )
        .expect("seed zombie");
        conn.execute(
            "INSERT INTO recall_metrics \
                (ts, session_id, query_hash, bundle_tokens, ledger_tokens, \
                 rerank_used, result_count) \
             VALUES (datetime('now'), NULL, 'q', 10, 100, 0, 3)",
            [],
        )
        .expect("seed recall");

        reconcile(conn).expect("reconcile");

        let attributed: Option<String> = conn
            .query_row("SELECT session_id FROM recall_metrics", [], |r| r.get(0))
            .expect("read");
        assert_eq!(
            attributed, None,
            "a days-old open zombie must not capture the recall"
        );
    }

    #[test]
    fn reconciler_attributes_to_a_live_open_session() {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        let ctx = crate::db::open_project(temp.path()).expect("open");
        let conn = &ctx.conn;

        // A genuinely live session: open, started an hour ago. Its recall
        // is inside the capped window and must attribute normally.
        conn.execute(
            "INSERT INTO session_metrics (session_id, agent, started_at, ended_at) \
             VALUES ('live', 'claude-code', datetime('now','-1 hour'), NULL)",
            [],
        )
        .expect("seed live");
        conn.execute(
            "INSERT INTO recall_metrics \
                (ts, session_id, query_hash, bundle_tokens, ledger_tokens, \
                 rerank_used, result_count) \
             VALUES (datetime('now'), NULL, 'q', 10, 100, 0, 3)",
            [],
        )
        .expect("seed recall");

        let n = reconcile(conn).expect("reconcile");
        assert_eq!(n, 1, "the live session's recall attributes");
        let attributed: Option<String> = conn
            .query_row("SELECT session_id FROM recall_metrics", [], |r| r.get(0))
            .expect("read");
        assert_eq!(attributed.as_deref(), Some("live"));
    }

    #[test]
    fn retention_zero_keeps_everything_including_turns() {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        let ctx = crate::db::open_project(temp.path()).expect("open");
        let conn = &ctx.conn;
        conn.execute(
            "INSERT INTO session_metrics (session_id, agent, started_at, ended_at) \
             VALUES ('s', 'claude-code', datetime('now','-999 days'), \
                     datetime('now','-999 days'))",
            [],
        )
        .expect("seed");
        conn.execute(
            "INSERT INTO session_turn_metrics (session_id, ts, input_tokens) \
             VALUES ('s', datetime('now','-999 days'), 5)",
            [],
        )
        .expect("seed turn");

        let (r, s) = prune_old(conn, 0).expect("prune");
        assert_eq!((r, s), (0, 0), "retention 0 = keep forever");
        let turns: i64 = conn
            .query_row("SELECT COUNT(*) FROM session_turn_metrics", [], |r| {
                r.get(0)
            })
            .expect("count");
        assert_eq!(turns, 1);
    }
}
