use std::path::Path;

use rusqlite::{Connection, params};

use crate::Result;
use crate::commands::fact;
use crate::db;
use crate::models::{
    CountByLabel, FACT_STALE_AFTER_DAYS, RecentFactKey, StatsSummary, TopCommandKind,
};

pub const TOP_N: usize = 5;
pub const DEFAULT_WINDOW_DAYS: i64 = 30;

#[derive(Debug, Clone, Copy)]
pub enum StatsWindow {
    Days(i64),
    All,
}

impl StatsWindow {
    pub fn label(&self) -> String {
        match self {
            StatsWindow::Days(d) => format!("last {d} days"),
            StatsWindow::All => "all time".to_string(),
        }
    }

    pub fn days(&self) -> Option<i64> {
        match self {
            StatsWindow::Days(d) => Some(*d),
            StatsWindow::All => None,
        }
    }
}

pub fn run(start: &Path, window: StatsWindow) -> Result<StatsSummary> {
    let ctx = db::open_project(start)?;
    let conn = &ctx.conn;

    let facts: i64 = conn.query_row("SELECT COUNT(*) FROM facts", [], |row| row.get(0))?;
    let stale_facts = fact::count_stale(start)?;
    let stale_ratio = if facts > 0 {
        Some(stale_facts as f64 / facts as f64)
    } else {
        None
    };
    let decisions: i64 = conn.query_row("SELECT COUNT(*) FROM decisions", [], |row| row.get(0))?;
    let tasks_total: i64 = conn.query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))?;
    let tasks_open: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tasks WHERE status = 'open'",
        [],
        |row| row.get(0),
    )?;
    let commands: i64 = conn.query_row("SELECT COUNT(*) FROM commands", [], |row| row.get(0))?;
    let commits: i64 = conn.query_row("SELECT COUNT(*) FROM commits", [], |row| row.get(0))?;
    let files: i64 = conn.query_row("SELECT COUNT(*) FROM files", [], |row| row.get(0))?;
    let chunks: i64 = conn.query_row("SELECT COUNT(*) FROM chunks", [], |row| row.get(0))?;
    let pending_writes_now: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pending_writes WHERE status = 'pending'",
        [],
        |row| row.get(0),
    )?;
    let writes_logged_total: i64 =
        conn.query_row("SELECT COUNT(*) FROM writes_log", [], |row| row.get(0))?;

    let writes_in_window = count_writes_in_window(conn, window)?;
    let writes_by_actor = top_writes_grouped(conn, window, "actor", TOP_N)?;
    let writes_by_table = top_writes_grouped(conn, window, "table_name", TOP_N)?;

    let pending_created_in_window = count_pending_created(conn, window)?;
    let pending_reviewed_in_window = count_pending_reviewed(conn, window)?;
    let review_rate = if pending_created_in_window > 0 {
        Some(pending_reviewed_in_window as f64 / pending_created_in_window as f64)
    } else {
        None
    };
    let pending_by_status = pending_status_counts(conn)?;

    let top_command_kinds = top_command_kinds(conn, TOP_N)?;
    let recent_facts = recent_facts(conn, TOP_N)?;

    Ok(StatsSummary {
        project_name: ctx.config.project_name,
        repo_root: ctx.paths.repo_root,
        window_label: window.label(),
        window_days: window.days(),
        facts,
        stale_facts,
        stale_ratio,
        decisions,
        tasks_total,
        tasks_open,
        commands,
        commits,
        files,
        chunks,
        pending_writes_now,
        writes_logged_total,
        writes_in_window,
        writes_by_actor,
        writes_by_table,
        pending_created_in_window,
        pending_reviewed_in_window,
        review_rate,
        pending_by_status,
        top_command_kinds,
        recent_facts,
    })
}

fn count_writes_in_window(conn: &Connection, window: StatsWindow) -> Result<i64> {
    Ok(match window {
        StatsWindow::All => {
            conn.query_row("SELECT COUNT(*) FROM writes_log", [], |row| row.get(0))?
        }
        StatsWindow::Days(d) => conn.query_row(
            &format!("SELECT COUNT(*) FROM writes_log WHERE at >= datetime('now', '-{d} days')"),
            [],
            |row| row.get(0),
        )?,
    })
}

fn top_writes_grouped(
    conn: &Connection,
    window: StatsWindow,
    column: &str,
    limit: usize,
) -> Result<Vec<CountByLabel>> {
    let where_clause = match window {
        StatsWindow::All => String::new(),
        StatsWindow::Days(d) => format!("WHERE at >= datetime('now', '-{d} days')"),
    };
    let sql = format!(
        "SELECT {column} AS label, COUNT(*) AS c
         FROM writes_log
         {where_clause}
         GROUP BY {column}
         ORDER BY c DESC, label ASC
         LIMIT ?1"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![limit as i64], |row| {
            Ok(CountByLabel {
                label: row.get::<_, String>(0)?,
                count: row.get::<_, i64>(1)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn count_pending_created(conn: &Connection, window: StatsWindow) -> Result<i64> {
    Ok(match window {
        StatsWindow::All => conn.query_row(
            "SELECT COUNT(*) FROM pending_writes",
            [],
            |row| row.get(0),
        )?,
        StatsWindow::Days(d) => conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM pending_writes WHERE created_at >= datetime('now', '-{d} days')"
            ),
            [],
            |row| row.get(0),
        )?,
    })
}

fn count_pending_reviewed(conn: &Connection, window: StatsWindow) -> Result<i64> {
    Ok(match window {
        StatsWindow::All => conn.query_row(
            "SELECT COUNT(*) FROM pending_writes WHERE status != 'pending'",
            [],
            |row| row.get(0),
        )?,
        StatsWindow::Days(d) => conn.query_row(
            &format!(
                "SELECT COUNT(*) FROM pending_writes
                 WHERE status != 'pending'
                   AND reviewed_at IS NOT NULL
                   AND reviewed_at >= datetime('now', '-{d} days')"
            ),
            [],
            |row| row.get(0),
        )?,
    })
}

fn pending_status_counts(conn: &Connection) -> Result<Vec<CountByLabel>> {
    let mut stmt = conn.prepare(
        "SELECT status, COUNT(*) AS c
         FROM pending_writes
         GROUP BY status
         ORDER BY status ASC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(CountByLabel {
                label: row.get::<_, String>(0)?,
                count: row.get::<_, i64>(1)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn top_command_kinds(conn: &Connection, limit: usize) -> Result<Vec<TopCommandKind>> {
    let mut stmt = conn.prepare(
        "SELECT kind, cmdline, success_count, fail_count, last_run_at
         FROM commands
         ORDER BY (success_count + fail_count) DESC, last_run_at DESC NULLS LAST
         LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![limit as i64], |row| {
            let success: i64 = row.get(2)?;
            let fail: i64 = row.get(3)?;
            let total = success + fail;
            let confidence = if total > 0 {
                Some(success as f64 / total as f64)
            } else {
                None
            };
            Ok(TopCommandKind {
                kind: row.get(0)?,
                cmdline: row.get(1)?,
                success_count: success,
                fail_count: fail,
                confidence,
                last_run_at: row.get(4)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn recent_facts(conn: &Connection, limit: usize) -> Result<Vec<RecentFactKey>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT key, verified_at,
                CASE
                    WHEN verified_at IS NULL THEN 1
                    WHEN julianday('now') - julianday(verified_at) > {FACT_STALE_AFTER_DAYS} THEN 1
                    ELSE 0
                END AS is_stale
         FROM facts
         ORDER BY verified_at DESC NULLS LAST, id DESC
         LIMIT ?1"
    ))?;
    let rows = stmt
        .query_map(params![limit as i64], |row| {
            let stale_flag: i64 = row.get(2)?;
            Ok(RecentFactKey {
                key: row.get(0)?,
                verified_at: row.get(1)?,
                is_stale: stale_flag != 0,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}
