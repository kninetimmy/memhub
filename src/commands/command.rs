use std::path::Path;

use rusqlite::{OptionalExtension, params};

use crate::Result;
use crate::db;
use crate::errors::MemhubError;
use crate::models::CommandRecord;

pub fn list(start: &Path) -> Result<Vec<CommandRecord>> {
    let ctx = db::open_project(start)?;
    let mut stmt = ctx.conn.prepare(
        "SELECT id, kind, cmdline, last_exit_code, last_run_at, success_count, fail_count
         FROM commands
         ORDER BY COALESCE(last_run_at, '') DESC, id DESC",
    )?;

    let rows = stmt.query_map([], |row| {
        Ok(CommandRecord {
            id: row.get(0)?,
            kind: row.get(1)?,
            cmdline: row.get(2)?,
            last_exit_code: row.get(3)?,
            last_run_at: row.get(4)?,
            success_count: row.get(5)?,
            fail_count: row.get(6)?,
        })
    })?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

pub fn verify(start: &Path, kind: &str, cmdline: &str, exit_code: i64) -> Result<(i64, bool)> {
    let kind = kind.trim().to_ascii_lowercase();
    if kind.is_empty() {
        return Err(MemhubError::InvalidInput(
            "command kind cannot be empty".to_string(),
        ));
    }

    let cmdline = cmdline.trim();
    if cmdline.is_empty() {
        return Err(MemhubError::InvalidInput(
            "command line cannot be empty".to_string(),
        ));
    }

    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;
    let existing_id: Option<i64> = tx
        .query_row(
            "SELECT id
             FROM commands
             WHERE project_id = 1 AND kind = ?1 AND cmdline = ?2",
            params![kind.as_str(), cmdline],
            |row| row.get(0),
        )
        .optional()?;

    let succeeded = exit_code == 0;
    let (row_id, created) = if let Some(id) = existing_id {
        tx.execute(
            "UPDATE commands
             SET last_exit_code = ?1,
                 last_run_at = CURRENT_TIMESTAMP,
                 success_count = success_count + ?2,
                 fail_count = fail_count + ?3
             WHERE id = ?4",
            params![
                exit_code,
                if succeeded { 1 } else { 0 },
                if succeeded { 0 } else { 1 },
                id
            ],
        )?;
        (id, false)
    } else {
        tx.execute(
            "INSERT INTO commands(
                 project_id,
                 kind,
                 cmdline,
                 last_exit_code,
                 last_run_at,
                 success_count,
                 fail_count
            )
             VALUES (1, ?1, ?2, ?3, CURRENT_TIMESTAMP, ?4, ?5)",
            params![
                kind.as_str(),
                cmdline,
                exit_code,
                if succeeded { 1 } else { 0 },
                if succeeded { 0 } else { 1 }
            ],
        )?;
        (tx.last_insert_rowid(), true)
    };

    let action = if created { "insert" } else { "update" };
    let reason = format!("command verify ({kind})");
    db::log_write(&tx, "cli:user", "commands", Some(row_id), action, &reason)?;

    tx.commit()?;
    Ok((row_id, created))
}
