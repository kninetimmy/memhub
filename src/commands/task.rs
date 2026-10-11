use std::path::Path;

use crate::db;
use crate::models::Task;
use crate::{MemhubError, Result};

pub fn add(start: &Path, title: &str, notes: Option<&str>, actor: &str) -> Result<i64> {
    add_with_status(start, title, notes, None, "open", actor)
}

/// Insert a task. `summary` is the optional "what this answers"
/// paraphrase (issue #349); blank text stores none.
pub fn add_with_status(
    start: &Path,
    title: &str,
    notes: Option<&str>,
    summary: Option<&str>,
    status: &str,
    actor: &str,
) -> Result<i64> {
    if !matches!(status, "open" | "done" | "blocked") {
        return Err(MemhubError::InvalidInput(format!(
            "unsupported task status '{status}'"
        )));
    }

    let mut ctx = db::open_project(start)?;
    let mode = ctx.config.retrieval.mode;
    let tx = ctx.conn.transaction()?;

    let summary_value = crate::commands::decision::normalize_summary(summary);
    tx.execute(
        "INSERT INTO tasks(project_id, title, status, notes, summary)
         VALUES (1, ?1, ?2, ?3, ?4)",
        rusqlite::params![title, status, notes, summary_value],
    )?;
    let row_id = tx.last_insert_rowid();

    db::log_write(&tx, actor, "tasks", Some(row_id), "insert", "task add")?;

    let embed_text = crate::retrieval::task_embed_text(title, notes, summary_value.as_deref());
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

/// Set or clear an existing task's summary, then re-embed (issue #349).
/// Mirrors `decision::set_summary`: empty or whitespace-only input clears
/// it back to NULL. Leaves `updated_at` alone, as the summary changes how
/// the task is found, not the task.
pub fn set_summary(start: &Path, id: i64, summary: Option<&str>, actor: &str) -> Result<()> {
    let mut ctx = db::open_project(start)?;
    let mode = ctx.config.retrieval.mode;
    let tx = ctx.conn.transaction()?;

    let (title, notes): (String, Option<String>) = tx
        .query_row(
            "SELECT title, notes FROM tasks WHERE id = ?1",
            [id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => {
                MemhubError::InvalidInput(format!("task {id} does not exist"))
            }
            other => other.into(),
        })?;

    let summary_value = crate::commands::decision::normalize_summary(summary);
    tx.execute(
        "UPDATE tasks SET summary = ?1 WHERE id = ?2",
        rusqlite::params![summary_value, id],
    )?;

    db::log_write(&tx, actor, "tasks", Some(id), "update", "task set-summary")?;

    let embed_text =
        crate::retrieval::task_embed_text(&title, notes.as_deref(), summary_value.as_deref());
    crate::retrieval::eager_embed_in_tx(
        &tx,
        mode,
        crate::retrieval::SourceType::Task,
        id,
        &embed_text,
    )?;

    tx.commit()?;
    Ok(())
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

pub fn get(start: &Path, task_id: i64) -> Result<Task> {
    let ctx = db::open_project(start)?;
    ctx.conn
        .query_row(
            "SELECT id, title, status, notes, created_at, updated_at
             FROM tasks
             WHERE id = ?1",
            [task_id],
            |row| {
                Ok(Task {
                    id: row.get(0)?,
                    title: row.get(1)?,
                    status: row.get(2)?,
                    notes: row.get(3)?,
                    created_at: row.get(4)?,
                    updated_at: row.get(5)?,
                })
            },
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
    if !matches!(status.as_str(), "open" | "done" | "blocked") {
        return Err(MemhubError::InvalidInput(format!(
            "unsupported task status '{status}'"
        )));
    }

    let ctx = db::open_project(start)?;
    let mut stmt = ctx.conn.prepare(
        "SELECT id, title, status, notes, created_at, updated_at
         FROM tasks
         WHERE status = ?1
         ORDER BY updated_at DESC, id DESC
         LIMIT ?2",
    )?;
    let rows = stmt.query_map(rusqlite::params![status, limit as i64], |row| {
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

pub fn done(start: &Path, task_id: i64, actor: &str) -> Result<()> {
    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;

    let updated = tx.execute(
        "UPDATE tasks
         SET status = 'done', updated_at = CURRENT_TIMESTAMP
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
