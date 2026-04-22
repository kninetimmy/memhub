use std::path::Path;

use crate::db;
use crate::models::Task;
use crate::{MemhubError, Result};

pub fn add(start: &Path, title: &str, notes: Option<&str>) -> Result<i64> {
    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;

    tx.execute(
        "INSERT INTO tasks(project_id, title, status, notes)
         VALUES (1, ?1, 'open', ?2)",
        rusqlite::params![title, notes],
    )?;
    let row_id = tx.last_insert_rowid();

    db::log_write(&tx, "cli:user", "tasks", Some(row_id), "insert", "task add")?;

    tx.commit()?;
    Ok(row_id)
}

pub fn list(start: &Path, status_filter: Option<&str>) -> Result<Vec<Task>> {
    let ctx = db::open_project(start)?;

    let (sql, params): (&str, Vec<&str>) = match status_filter {
        Some(status) if status != "all" => (
            "SELECT id, title, status, notes, created_at, updated_at
             FROM tasks
             WHERE status = ?1
             ORDER BY updated_at DESC, id DESC",
            vec![status],
        ),
        _ => (
            "SELECT id, title, status, notes, created_at, updated_at
             FROM tasks
             ORDER BY updated_at DESC, id DESC",
            Vec::new(),
        ),
    };

    let mut stmt = ctx.conn.prepare(sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), |row| {
        Ok(Task {
            id: row.get(0)?,
            title: row.get(1)?,
            status: row.get(2)?,
            notes: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
        })
    })?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn done(start: &Path, task_id: i64) -> Result<()> {
    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;

    let updated = tx.execute(
        "UPDATE tasks
         SET status = 'done', updated_at = CURRENT_TIMESTAMP
         WHERE id = ?1",
        [task_id],
    )?;

    if updated == 0 {
        return Err(MemhubError::InvalidInput(format!(
            "task {task_id} does not exist"
        )));
    }

    db::log_write(
        &tx,
        "cli:user",
        "tasks",
        Some(task_id),
        "update",
        "task done",
    )?;

    tx.commit()?;
    Ok(())
}
