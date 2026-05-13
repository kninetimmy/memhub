use std::path::Path;

use rusqlite::{OptionalExtension, Transaction, params};

use crate::Result;
use crate::db;
use crate::models::{FACT_STALE_AFTER_DAYS, Fact};
use crate::sync_md;

pub fn add(start: &Path, key: &str, value: &str, source: &str, actor: &str) -> Result<(i64, bool)> {
    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;
    let outcome = add_in_tx(&tx, key, value, source, actor)?;
    tx.commit()?;
    sync_md::sync_if_enabled(start)?;
    Ok(outcome)
}

pub fn add_in_tx(
    tx: &Transaction<'_>,
    key: &str,
    value: &str,
    source: &str,
    actor: &str,
) -> Result<(i64, bool)> {
    crate::commands::validate_source(source)?;

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
        tx,
        actor,
        "facts",
        Some(row_id),
        if created { "insert" } else { "update" },
        "fact add",
    )?;

    Ok((row_id, created))
}

pub fn list(start: &Path) -> Result<Vec<Fact>> {
    let ctx = db::open_project(start)?;
    let mut stmt = ctx.conn.prepare(
        "SELECT id, key, value, confidence, source, verified_at, created_at,
                CASE
                    WHEN verified_at IS NULL THEN 1
                    WHEN (julianday('now') - julianday(verified_at)) > ?1 THEN 1
                    ELSE 0
                END AS is_stale
         FROM facts
         ORDER BY key ASC",
    )?;

    let rows = stmt.query_map(params![FACT_STALE_AFTER_DAYS], |row| {
        let is_stale_int: i64 = row.get(7)?;
        Ok(Fact {
            id: row.get(0)?,
            key: row.get(1)?,
            value: row.get(2)?,
            confidence: row.get(3)?,
            source: row.get(4)?,
            verified_at: row.get(5)?,
            created_at: row.get(6)?,
            is_stale: is_stale_int != 0,
        })
    })?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn count_stale(start: &Path) -> Result<i64> {
    let ctx = db::open_project(start)?;
    let count: i64 = ctx.conn.query_row(
        "SELECT COUNT(*)
         FROM facts
         WHERE verified_at IS NULL
            OR (julianday('now') - julianday(verified_at)) > ?1",
        params![FACT_STALE_AFTER_DAYS],
        |row| row.get(0),
    )?;
    Ok(count)
}
