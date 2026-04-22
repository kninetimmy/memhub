use std::path::Path;

use rusqlite::params;

use crate::Result;
use crate::commands::search;
use crate::db;
use crate::models::Decision;
use crate::sync_md;

pub fn add(start: &Path, title: &str, rationale: &str) -> Result<i64> {
    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;

    tx.execute(
        "INSERT INTO decisions(project_id, title, rationale, status)
         VALUES (1, ?1, ?2, 'active')",
        params![title, rationale],
    )?;
    let row_id = tx.last_insert_rowid();
    search::sync_decision_chunks(&tx)?;

    db::log_write(
        &tx,
        "cli:user",
        "decisions",
        Some(row_id),
        "insert",
        "decision add",
    )?;

    tx.commit()?;
    sync_md::sync_if_enabled(start)?;
    Ok(row_id)
}

pub fn list(start: &Path) -> Result<Vec<Decision>> {
    let ctx = db::open_project(start)?;
    let mut stmt = ctx.conn.prepare(
        "SELECT id, title, rationale, status, decided_at
         FROM decisions
         ORDER BY decided_at DESC, id DESC",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(Decision {
            id: row.get(0)?,
            title: row.get(1)?,
            rationale: row.get(2)?,
            status: row.get(3)?,
            decided_at: row.get(4)?,
        })
    })?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn list_active_recent(start: &Path, limit: usize) -> Result<Vec<Decision>> {
    if limit == 0 {
        return Err(crate::MemhubError::InvalidInput(
            "decision list limit must be greater than zero".to_string(),
        ));
    }

    let ctx = db::open_project(start)?;
    let mut stmt = ctx.conn.prepare(
        "SELECT id, title, rationale, status, decided_at
         FROM decisions
         WHERE project_id = 1 AND status = 'active'
         ORDER BY decided_at DESC, id DESC
         LIMIT ?1",
    )?;

    let rows = stmt.query_map([limit as i64], |row| {
        Ok(Decision {
            id: row.get(0)?,
            title: row.get(1)?,
            rationale: row.get(2)?,
            status: row.get(3)?,
            decided_at: row.get(4)?,
        })
    })?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}
