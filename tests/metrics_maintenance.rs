//! Integration coverage for the token-accounting maintenance pass
//! (decision 74, task #30): the recall→session reconciler and the
//! retention pruner.
//!
//! Each test bootstraps a real temp project, crafts `recall_metrics` /
//! `session_metrics` rows with deliberately mixed timestamp formats
//! (SQLite `CURRENT_TIMESTAMP` space form vs Claude Code's ISO-`T`-`Z`
//! form), then exercises the public `maintenance` API directly. One
//! test drives it through the gated `db::open_project` path to prove
//! the master switch wiring.

use std::path::Path;

use memhub::commands::init;
use memhub::config::ProjectConfig;
use memhub::db;
use memhub::metrics::maintenance;
use rusqlite::{Connection, params};
use tempfile::tempdir;

/// Insert a `session_metrics` row. `ended_at = None` models an open
/// (still-running) session.
fn insert_session(
    conn: &Connection,
    session_id: &str,
    agent: &str,
    started_at: &str,
    ended_at: Option<&str>,
) {
    conn.execute(
        "INSERT INTO session_metrics \
            (session_id, agent, started_at, ended_at) \
         VALUES (?1, ?2, ?3, ?4)",
        params![session_id, agent, started_at, ended_at],
    )
    .expect("insert session_metrics");
}

/// Insert a `recall_metrics` row with an explicit `ts` and a NULL
/// `session_id` (the state the recall path always writes).
fn insert_recall(conn: &Connection, ts: &str) {
    conn.execute(
        "INSERT INTO recall_metrics \
            (ts, session_id, query_hash, bundle_tokens, ledger_tokens, \
             rerank_used, result_count) \
         VALUES (?1, NULL, 'hash', 10, 1000, 1, 3)",
        params![ts],
    )
    .expect("insert recall_metrics");
}

fn session_id_of_recall(conn: &Connection, ts: &str) -> Option<String> {
    conn.query_row(
        "SELECT session_id FROM recall_metrics WHERE ts = ?1",
        params![ts],
        |r| r.get(0),
    )
    .expect("query recall row")
}

fn recall_calls(conn: &Connection, session_id: &str) -> i64 {
    conn.query_row(
        "SELECT recall_calls FROM session_metrics WHERE session_id = ?1",
        params![session_id],
        |r| r.get(0),
    )
    .expect("query recall_calls")
}

fn count(conn: &Connection, table: &str) -> i64 {
    conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
        .expect("count")
}

fn open(repo: &Path) -> db::ProjectContext {
    db::open_project(repo).expect("open_project")
}

/// The core correctness case: the recall `ts` is in SQLite
/// `CURRENT_TIMESTAMP` form (space separator) while the session bounds
/// are in Claude Code's ISO `T`/`Z` form. A naive string compare would
/// never match these; the `datetime()` normalization must.
#[test]
fn reconciles_across_mixed_timestamp_formats() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    let ctx = open(temp.path());

    insert_session(
        &ctx.conn,
        "sess-1",
        "claude-code",
        "2026-05-15T09:00:00.000Z",
        Some("2026-05-15T09:10:00.000Z"),
    );
    insert_recall(&ctx.conn, "2026-05-15 09:05:00");

    let n = maintenance::reconcile(&ctx.conn).expect("reconcile");

    assert_eq!(n, 1, "the one in-window recall is attributed");
    assert_eq!(
        session_id_of_recall(&ctx.conn, "2026-05-15 09:05:00").as_deref(),
        Some("sess-1")
    );
    assert_eq!(recall_calls(&ctx.conn, "sess-1"), 1);
}

/// A recall outside every session window stays NULL and is not counted.
#[test]
fn recall_outside_any_window_is_left_unattributed() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    let ctx = open(temp.path());

    insert_session(
        &ctx.conn,
        "sess-1",
        "claude-code",
        "2026-05-15T09:00:00.000Z",
        Some("2026-05-15T09:10:00.000Z"),
    );
    insert_recall(&ctx.conn, "2026-05-15 12:00:00"); // hours later

    let n = maintenance::reconcile(&ctx.conn).expect("reconcile");

    assert_eq!(n, 0);
    assert_eq!(session_id_of_recall(&ctx.conn, "2026-05-15 12:00:00"), None);
    assert_eq!(
        recall_calls(&ctx.conn, "sess-1"),
        0,
        "untouched session keeps its default 0"
    );
}

/// An open session (NULL `ended_at`) extends its window to "now", so a
/// recall after `started_at` still attributes.
#[test]
fn open_session_window_extends_to_now() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    let ctx = open(temp.path());

    insert_session(
        &ctx.conn,
        "sess-open",
        "claude-code",
        "2020-01-01T00:00:00.000Z",
        None,
    );
    // CURRENT_TIMESTAMP-form "now-ish" value, comfortably <= now.
    ctx.conn
        .execute(
            "INSERT INTO recall_metrics \
                (ts, session_id, query_hash, bundle_tokens, ledger_tokens, \
                 rerank_used, result_count) \
             VALUES (datetime('now', '-1 minute'), NULL, 'h', 1, 2, 0, 1)",
            [],
        )
        .expect("insert recall");

    let n = maintenance::reconcile(&ctx.conn).expect("reconcile");
    assert_eq!(n, 1);
    assert_eq!(recall_calls(&ctx.conn, "sess-open"), 1);
}

/// Overlapping windows: the recall lands in the overlap and must go to
/// the most-recently-started containing session (documented lossy
/// behavior).
#[test]
fn overlapping_windows_assign_to_latest_started_session() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    let ctx = open(temp.path());

    insert_session(
        &ctx.conn,
        "sess-early",
        "claude-code",
        "2026-05-15T09:00:00.000Z",
        Some("2026-05-15T09:20:00.000Z"),
    );
    insert_session(
        &ctx.conn,
        "sess-late",
        "codex",
        "2026-05-15T09:10:00.000Z",
        Some("2026-05-15T09:30:00.000Z"),
    );
    insert_recall(&ctx.conn, "2026-05-15 09:15:00"); // in both windows

    maintenance::reconcile(&ctx.conn).expect("reconcile");

    assert_eq!(
        session_id_of_recall(&ctx.conn, "2026-05-15 09:15:00").as_deref(),
        Some("sess-late"),
        "later started_at wins the overlap"
    );
    assert_eq!(recall_calls(&ctx.conn, "sess-late"), 1);
    assert_eq!(recall_calls(&ctx.conn, "sess-early"), 0);
}

/// Reconcile is idempotent: a second pass neither re-attributes an
/// already-assigned row nor double-counts `recall_calls`.
#[test]
fn reconcile_is_idempotent() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    let ctx = open(temp.path());

    insert_session(
        &ctx.conn,
        "sess-1",
        "claude-code",
        "2026-05-15T09:00:00.000Z",
        Some("2026-05-15T09:59:00.000Z"),
    );
    insert_recall(&ctx.conn, "2026-05-15 09:05:00");
    insert_recall(&ctx.conn, "2026-05-15 09:06:00");
    insert_recall(&ctx.conn, "2026-05-15 09:07:00");

    let first = maintenance::reconcile(&ctx.conn).expect("reconcile 1");
    let second = maintenance::reconcile(&ctx.conn).expect("reconcile 2");

    assert_eq!(first, 3, "all three attributed on the first pass");
    assert_eq!(second, 0, "nothing left to attribute on the second");
    assert_eq!(
        recall_calls(&ctx.conn, "sess-1"),
        3,
        "recount, not a doubling += "
    );
}

/// The pruner drops rows past the horizon, keeps recent ones, and never
/// prunes an open (NULL `ended_at`) session however old it started.
#[test]
fn prune_respects_retention_and_open_sessions() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    let ctx = open(temp.path());

    insert_recall(&ctx.conn, "2020-01-01 00:00:00"); // ancient
    ctx.conn
        .execute(
            "INSERT INTO recall_metrics \
                (ts, session_id, query_hash, bundle_tokens, ledger_tokens, \
                 rerank_used, result_count) \
             VALUES (datetime('now'), NULL, 'h', 1, 2, 0, 1)",
            [],
        )
        .expect("recent recall");

    insert_session(
        &ctx.conn,
        "old-closed",
        "claude-code",
        "2020-01-01T00:00:00.000Z",
        Some("2020-01-01T01:00:00.000Z"),
    );
    insert_session(
        &ctx.conn,
        "old-open",
        "claude-code",
        "2020-01-01T00:00:00.000Z",
        None,
    );

    let (recalls, sessions) = maintenance::prune_old(&ctx.conn, 90).expect("prune");

    assert_eq!(recalls, 1, "only the 2020 recall is past 90 days");
    assert_eq!(sessions, 1, "only the closed 2020 session is pruned");
    assert_eq!(count(&ctx.conn, "recall_metrics"), 1, "recent recall kept");
    assert!(
        ctx.conn
            .query_row(
                "SELECT 1 FROM session_metrics WHERE session_id = 'old-open'",
                [],
                |_| Ok(())
            )
            .is_ok(),
        "open session is never pruned by age"
    );
}

/// `retention_days = 0` means keep forever — a strict no-op.
#[test]
fn retention_zero_is_a_no_op() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");
    let ctx = open(temp.path());

    insert_recall(&ctx.conn, "2000-01-01 00:00:00");
    let (recalls, sessions) = maintenance::prune_old(&ctx.conn, 0).expect("prune");

    assert_eq!((recalls, sessions), (0, 0));
    assert_eq!(count(&ctx.conn, "recall_metrics"), 1);
}

/// The master switch gates the wired `open_project` path: with metrics
/// disabled (the default) the reconciler never runs even though an
/// in-window recall exists; flipping it on makes the next open attribute
/// the row.
#[test]
fn open_project_runs_maintenance_only_when_enabled() {
    let temp = tempdir().expect("tempdir");
    init::run(temp.path()).expect("init");

    {
        let ctx = open(temp.path());
        insert_session(
            &ctx.conn,
            "sess-1",
            "claude-code",
            "2026-05-15T09:00:00.000Z",
            Some("2026-05-15T09:59:00.000Z"),
        );
        insert_recall(&ctx.conn, "2026-05-15 09:05:00");
    }

    // Default config: metrics.enabled = false. open_project runs the
    // gated pass, which must early-return.
    {
        let ctx = open(temp.path());
        assert_eq!(
            session_id_of_recall(&ctx.conn, "2026-05-15 09:05:00"),
            None,
            "disabled: recall stays unattributed"
        );
    }

    // Flip the master switch on disk, reopen.
    let config_path = temp.path().join(".memhub").join("config.toml");
    let mut cfg = ProjectConfig::load(&config_path).expect("load config");
    cfg.metrics.enabled = true;
    cfg.save(&config_path).expect("save config");

    let ctx = open(temp.path());
    assert_eq!(
        session_id_of_recall(&ctx.conn, "2026-05-15 09:05:00").as_deref(),
        Some("sess-1"),
        "enabled: open_project attributed the recall"
    );
    assert_eq!(recall_calls(&ctx.conn, "sess-1"), 1);
}
