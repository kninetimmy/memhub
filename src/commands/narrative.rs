use std::path::Path;

use rusqlite::{OptionalExtension, params};

use crate::MemhubError;
use crate::Result;
use crate::db;
use crate::models::{NarrativeEntry, NarrativeKind};

pub const MAX_BODY_LEN: usize = 65_536;
pub const DEFAULT_HISTORY_LIMIT: usize = 25;

pub fn set(
    start: &Path,
    kind: NarrativeKind,
    body: &str,
    actor: &str,
    actor_raw: &str,
) -> Result<NarrativeEntry> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(MemhubError::InvalidInput(format!(
            "{} body must not be empty",
            kind.as_str()
        )));
    }
    if trimmed.chars().count() > MAX_BODY_LEN {
        return Err(MemhubError::InvalidInput(format!(
            "{} body must be {MAX_BODY_LEN} characters or fewer",
            kind.as_str()
        )));
    }
    if actor.trim().is_empty() {
        return Err(MemhubError::InvalidInput(format!(
            "{} actor must not be empty",
            kind.as_str()
        )));
    }

    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;

    let insert_sql = format!(
        "INSERT INTO {}(project_id, body, actor, actor_raw)
         VALUES (1, ?1, ?2, ?3)",
        kind.table()
    );
    tx.execute(&insert_sql, params![trimmed, actor, actor_raw])?;
    let row_id = tx.last_insert_rowid();

    db::log_write(
        &tx,
        actor,
        kind.table(),
        Some(row_id),
        "insert",
        &format!("{} set", kind.as_str()),
    )?;

    let select_sql = format!(
        "SELECT id, body, actor, actor_raw, created_at
         FROM {} WHERE id = ?1",
        kind.table()
    );
    let entry = tx.query_row(&select_sql, params![row_id], row_to_entry)?;

    tx.commit()?;
    Ok(entry)
}

pub fn show(start: &Path, kind: NarrativeKind) -> Result<Option<NarrativeEntry>> {
    let ctx = db::open_project(start)?;
    let sql = format!(
        "SELECT id, body, actor, actor_raw, created_at
         FROM {}
         WHERE project_id = 1
         ORDER BY created_at DESC, id DESC
         LIMIT 1",
        kind.table()
    );
    let entry = ctx.conn.query_row(&sql, [], row_to_entry).optional()?;
    Ok(entry)
}

pub fn history(start: &Path, kind: NarrativeKind, limit: usize) -> Result<Vec<NarrativeEntry>> {
    if limit == 0 {
        return Err(MemhubError::InvalidInput(format!(
            "{} history limit must be greater than zero",
            kind.as_str()
        )));
    }
    let ctx = db::open_project(start)?;
    let sql = format!(
        "SELECT id, body, actor, actor_raw, created_at
         FROM {}
         WHERE project_id = 1
         ORDER BY created_at DESC, id DESC
         LIMIT ?1",
        kind.table()
    );
    let mut stmt = ctx.conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![limit as i64], row_to_entry)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<NarrativeEntry> {
    Ok(NarrativeEntry {
        id: row.get(0)?,
        body: row.get(1)?,
        actor: row.get(2)?,
        actor_raw: row.get(3)?,
        created_at: row.get(4)?,
    })
}
