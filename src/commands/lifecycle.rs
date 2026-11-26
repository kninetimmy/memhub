//! CLI-only, transactional changes to the circulation of stored memory.

use std::path::Path;

use rusqlite::{OptionalExtension, params};

use crate::{MemhubError, Result, db};

fn check_reason(reason: Option<&str>) -> Result<()> {
    if reason.is_some_and(|reason| reason.trim().is_empty()) {
        return Err(MemhubError::InvalidInput("--reason cannot be empty".into()));
    }
    Ok(())
}

/// A reason retires the fact; no reason restores it without re-verifying it.
pub fn set_fact_retirement(
    start: &Path,
    ident: &str,
    reason: Option<&str>,
    actor: &str,
) -> Result<i64> {
    check_reason(reason)?;
    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;
    let (id, key) = super::fact::resolve_fact(&tx, ident)?
        .ok_or_else(|| MemhubError::InvalidInput(format!("no fact matched '{ident}'")))?;
    let updated = tx.execute(
        "UPDATE facts
         SET retired_at = CASE WHEN ?2 IS NOT NULL THEN CURRENT_TIMESTAMP END,
             retired_reason = ?2
         WHERE id = ?1 AND (retired_at IS NULL) = (?2 IS NOT NULL)",
        params![id, reason],
    )?;
    if updated == 0 {
        let state = if reason.is_some() {
            "already retired"
        } else {
            "not retired"
        };
        return Err(MemhubError::InvalidInput(format!(
            "fact '{key}' is {state}"
        )));
    }
    let (action, detail) = match reason {
        Some(reason) => ("retire", reason),
        None => ("unretire", "fact unretire"),
    };
    db::log_write(&tx, actor, "facts", Some(id), action, detail)?;
    tx.commit()?;
    Ok(id)
}

/// Archive or restore the parent document without rewriting any chunks.
pub fn set_doc_archive(
    start: &Path,
    ident: &str,
    reason: Option<&str>,
    actor: &str,
) -> Result<i64> {
    check_reason(reason)?;
    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;
    let id = super::doc::resolve_doc_id(&tx, ident)?
        .ok_or_else(|| MemhubError::InvalidInput(format!("no document matched '{ident}'")))?;
    set_doc_archive_in_tx(&tx, id, reason, actor)?;
    tx.commit()?;
    Ok(id)
}

pub(super) fn set_doc_archive_in_tx(
    tx: &rusqlite::Transaction<'_>,
    id: i64,
    reason: Option<&str>,
    actor: &str,
) -> Result<()> {
    let updated = tx.execute(
        "UPDATE documents
         SET archived_at = CASE WHEN ?2 IS NOT NULL THEN CURRENT_TIMESTAMP END,
             archived_reason = ?2
         WHERE id = ?1 AND (archived_at IS NULL) = (?2 IS NOT NULL)",
        params![id, reason],
    )?;
    if updated == 0 {
        let state = if reason.is_some() {
            "already archived"
        } else {
            "not archived"
        };
        return Err(MemhubError::InvalidInput(format!(
            "document #{id} is {state}"
        )));
    }
    let (action, detail) = match reason {
        Some(reason) => ("archive", reason),
        None => ("unarchive", "doc unarchive"),
    };
    db::log_write(tx, actor, "documents", Some(id), action, detail)
}

/// A reason drops a live task; no reason refreshes its update time and saves it for undo.
pub fn drop_or_keep_task(start: &Path, id: i64, reason: Option<&str>, actor: &str) -> Result<()> {
    check_reason(reason)?;
    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;
    drop_or_keep_task_in_tx(&tx, id, reason, actor)?;
    tx.commit()?;
    Ok(())
}

pub(super) fn drop_or_keep_task_in_tx(
    tx: &rusqlite::Transaction<'_>,
    id: i64,
    reason: Option<&str>,
    actor: &str,
) -> Result<()> {
    let (sql, action, detail) = match reason {
        Some(reason) => (
            "UPDATE tasks SET status = 'dropped', dropped_reason = ?2,
                              pre_drop_status = status, pre_drop_updated_at = updated_at,
                              updated_at = CURRENT_TIMESTAMP
             WHERE id = ?1 AND status IN ('open', 'blocked')",
            "drop",
            reason,
        ),
        None => (
            "UPDATE tasks SET pre_keep_updated_at = updated_at, kept_at = CURRENT_TIMESTAMP,
                              updated_at = CURRENT_TIMESTAMP
             WHERE id = ?1 AND status IN ('open', 'blocked')",
            "keep",
            "task keep",
        ),
    };
    let updated = match reason {
        Some(reason) => tx.execute(sql, params![id, reason])?,
        None => tx.execute(sql, [id])?,
    };
    if updated == 0 {
        let status: Option<String> = tx
            .query_row("SELECT status FROM tasks WHERE id = ?1", [id], |r| r.get(0))
            .optional()?;
        return Err(MemhubError::InvalidInput(match status {
            Some(status) => {
                format!("cannot {action} task {id}: it is {status}, not open or blocked")
            }
            None => format!("task {id} does not exist"),
        }));
    }
    db::log_write(tx, actor, "tasks", Some(id), action, detail)
}

/// Restore the task exactly as it was when dropped, including its age.
pub fn undrop_task(start: &Path, id: i64, actor: &str) -> Result<()> {
    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;
    let updated = tx.execute(
        "UPDATE tasks SET status = pre_drop_status, updated_at = pre_drop_updated_at,
                          dropped_reason = NULL, pre_drop_status = NULL, pre_drop_updated_at = NULL
         WHERE id = ?1 AND status = 'dropped'
           AND pre_drop_status IS NOT NULL AND pre_drop_updated_at IS NOT NULL",
        [id],
    )?;
    if updated == 0 {
        return Err(MemhubError::InvalidInput(format!(
            "task {id} has no recorded drop to undo"
        )));
    }
    db::log_write(&tx, actor, "tasks", Some(id), "undrop", "task undrop")?;
    tx.commit()?;
    Ok(())
}

/// Undo the latest keep, but never replace the timestamp of a later change.
pub fn unkeep_task(start: &Path, id: i64, actor: &str) -> Result<()> {
    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;
    let updated = tx.execute(
        "UPDATE tasks SET updated_at = pre_keep_updated_at,
                          pre_keep_updated_at = NULL, kept_at = NULL
         WHERE id = ?1 AND status IN ('open', 'blocked')
           AND pre_keep_updated_at IS NOT NULL AND updated_at = kept_at",
        [id],
    )?;
    if updated == 0 {
        let has_saved_time = tx
            .query_row(
                "SELECT pre_keep_updated_at IS NOT NULL FROM tasks WHERE id = ?1",
                [id],
                |r| r.get::<_, bool>(0),
            )
            .optional()?;
        return Err(MemhubError::InvalidInput(match has_saved_time {
            Some(true) => format!("cannot unkeep task {id}: it changed since it was kept"),
            Some(false) => format!("task {id} has no keep to undo"),
            None => format!("task {id} does not exist"),
        }));
    }
    db::log_write(&tx, actor, "tasks", Some(id), "unkeep", "task unkeep")?;
    tx.commit()?;
    Ok(())
}
