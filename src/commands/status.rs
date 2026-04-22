use std::path::Path;

use crate::Result;
use crate::db;
use crate::models::StatusSummary;

pub fn run(start: &Path) -> Result<StatusSummary> {
    let ctx = db::open_project(start)?;
    let conn = &ctx.conn;

    let schema_version: String = conn.query_row(
        "SELECT schema_version FROM projects WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    let facts: i64 = conn.query_row("SELECT COUNT(*) FROM facts", [], |row| row.get(0))?;
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
    let pending_writes: i64 = conn.query_row(
        "SELECT COUNT(*) FROM pending_writes WHERE status = 'pending'",
        [],
        |row| row.get(0),
    )?;
    let writes_logged: i64 =
        conn.query_row("SELECT COUNT(*) FROM writes_log", [], |row| row.get(0))?;

    let project_name = ctx.config.project_name;
    let repo_root = ctx.paths.repo_root;
    let db_path = ctx.paths.db_path;
    let config_path = ctx.paths.config_path;

    Ok(StatusSummary {
        project_name,
        repo_root,
        db_path,
        config_path,
        schema_version,
        facts,
        decisions,
        tasks_open,
        tasks_total,
        commands,
        commits,
        files,
        chunks,
        pending_writes,
        writes_logged,
    })
}
