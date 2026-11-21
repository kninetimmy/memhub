use std::path::Path;

use rusqlite::Row;

use crate::db;
use crate::models::Task;
use crate::{MemhubError, Result};

pub fn add(start: &Path, title: &str, notes: Option<&str>, actor: &str) -> Result<i64> {
    add_with_status(start, title, notes, "open", actor)
}

pub fn add_with_status(
    start: &Path,
    title: &str,
    notes: Option<&str>,
    status: &str,
    actor: &str,
) -> Result<i64> {
    if status == "dropped" {
        return Err(MemhubError::InvalidInput(
            "add the task before using `memhub task drop` to record its reason and prior state"
                .into(),
        ));
    }
    if !matches!(status, "open" | "done" | "blocked") {
        return Err(MemhubError::InvalidInput(format!(
            "unsupported task status '{status}'"
        )));
    }

    let mut ctx = db::open_project(start)?;
    let mode = ctx.config.retrieval.mode;
    let tx = ctx.conn.transaction()?;

    tx.execute(
        "INSERT INTO tasks(project_id, title, status, notes)
         VALUES (1, ?1, ?2, ?3)",
        rusqlite::params![title, status, notes],
    )?;
    let row_id = tx.last_insert_rowid();

    db::log_write(&tx, actor, "tasks", Some(row_id), "insert", "task add")?;

    let embed_text = crate::retrieval::task_embed_text(title, notes);
    crate::retrieval::eager_embed_in_tx(
        &tx,
        mode,
        crate::retrieval::SourceType::Task,
        row_id,
        &embed_text,
    )?;

    tx.commit()?;
    Ok(row_id)
}

pub fn list(start: &Path, status_filter: Option<&str>) -> Result<Vec<Task>> {
    let ctx = db::open_project(start)?;

    let (sql, params): (&str, Vec<&str>) = match status_filter {
        Some(status) if status != "all" => (
            "SELECT id, title, status, notes, created_at, updated_at, dropped_reason
             FROM tasks
             WHERE status = ?1
             ORDER BY updated_at DESC, id DESC",
            vec![status],
        ),
        _ => (
            "SELECT id, title, status, notes, created_at, updated_at, dropped_reason
             FROM tasks
             ORDER BY updated_at DESC, id DESC",
            Vec::new(),
        ),
    };

    let mut stmt = ctx.conn.prepare(sql)?;
    let rows = stmt.query_map(rusqlite::params_from_iter(params), read_task)?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn get(start: &Path, task_id: i64) -> Result<Task> {
    let ctx = db::open_project(start)?;
    ctx.conn
        .query_row(
            "SELECT id, title, status, notes, created_at, updated_at, dropped_reason
             FROM tasks
             WHERE id = ?1",
            [task_id],
            read_task,
        )
        .map_err(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => {
                MemhubError::InvalidInput(format!("task {task_id} does not exist"))
            }
            other => other.into(),
        })
}

pub fn list_by_status(start: &Path, status: &str, limit: usize) -> Result<Vec<Task>> {
    if limit == 0 {
        return Err(MemhubError::InvalidInput(
            "task list limit must be greater than zero".to_string(),
        ));
    }

    let status = status.trim().to_ascii_lowercase();
    if !matches!(status.as_str(), "open" | "done" | "blocked" | "dropped") {
        return Err(MemhubError::InvalidInput(format!(
            "unsupported task status '{status}'"
        )));
    }

    let ctx = db::open_project(start)?;
    let mut stmt = ctx.conn.prepare(
        "SELECT id, title, status, notes, created_at, updated_at, dropped_reason
         FROM tasks
         WHERE status = ?1
         ORDER BY updated_at DESC, id DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![status, limit as i64], read_task)?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub(crate) fn read_task(row: &Row<'_>) -> rusqlite::Result<Task> {
    Ok(Task {
        id: row.get(0)?,
        title: row.get(1)?,
        status: row.get(2)?,
        notes: row.get(3)?,
        created_at: row.get(4)?,
        updated_at: row.get(5)?,
        dropped_reason: row.get(6)?,
    })
}

pub fn done(start: &Path, task_id: i64, actor: &str) -> Result<()> {
    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;

    let updated = tx.execute(
        "UPDATE tasks
         SET status = 'done', updated_at = CURRENT_TIMESTAMP, dropped_reason = NULL,
             pre_drop_status = NULL, pre_drop_updated_at = NULL,
             pre_keep_updated_at = NULL, kept_at = NULL
         WHERE id = ?1 AND status != 'done'",
        [task_id],
    )?;

    if updated == 0 {
        let exists = tx.query_row("SELECT 1 FROM tasks WHERE id = ?1", [task_id], |_| Ok(()));
        return Err(match exists {
            Ok(()) => MemhubError::InvalidInput(format!("task {task_id} is already done")),
            Err(rusqlite::Error::QueryReturnedNoRows) => {
                MemhubError::InvalidInput(format!("task {task_id} does not exist"))
            }
            Err(err) => err.into(),
        });
    }

    db::log_write(&tx, actor, "tasks", Some(task_id), "update", "task done")?;

    tx.commit()?;
    Ok(())
}
