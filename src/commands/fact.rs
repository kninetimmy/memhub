use std::path::Path;

use rusqlite::{OptionalExtension, params};

use crate::Result;
use crate::db;
use crate::models::Fact;

pub fn add(start: &Path, key: &str, value: &str, source: &str) -> Result<(i64, bool)> {
    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;

    let existing_id: Option<i64> = tx
        .query_row(
            "SELECT id FROM facts WHERE project_id = 1 AND key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()?;

    let (row_id, created) = if let Some(id) = existing_id {
        tx.execute(
            "UPDATE facts
             SET value = ?1, source = ?2, confidence = 1.0, verified_at = CURRENT_TIMESTAMP
             WHERE id = ?3",
            params![value, source, id],
        )?;
        (id, false)
    } else {
        tx.execute(
            "INSERT INTO facts(project_id, key, value, confidence, source, verified_at)
             VALUES (1, ?1, ?2, 1.0, ?3, CURRENT_TIMESTAMP)",
            params![key, value, source],
        )?;
        (tx.last_insert_rowid(), true)
    };

    db::log_write(
        &tx,
        "cli:user",
        "facts",
        Some(row_id),
        if created { "insert" } else { "update" },
        "fact add",
    )?;

    tx.commit()?;
    Ok((row_id, created))
}

pub fn list(start: &Path) -> Result<Vec<Fact>> {
    let ctx = db::open_project(start)?;
    let mut stmt = ctx.conn.prepare(
        "SELECT id, key, value, confidence, source, verified_at, created_at
         FROM facts
         ORDER BY key ASC",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(Fact {
            id: row.get(0)?,
            key: row.get(1)?,
            value: row.get(2)?,
            confidence: row.get(3)?,
            source: row.get(4)?,
            verified_at: row.get(5)?,
            created_at: row.get(6)?,
        })
    })?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}
