use std::path::Path;

use rusqlite::{OptionalExtension, params};
use serde_json::Value;

use crate::commands::{decision, fact};
use crate::db;
use crate::models::{PendingWriteRecord, ReviewExpireSummary};
use crate::{MemhubError, Result};

pub const DEFAULT_LIST_LIMIT: usize = 25;
pub const DEFAULT_EXPIRY_DAYS: i64 = 30;

pub fn list(
    start: &Path,
    status_filter: Option<&str>,
    limit: usize,
) -> Result<Vec<PendingWriteRecord>> {
    if limit == 0 {
        return Err(MemhubError::InvalidInput(
            "review list limit must be greater than zero".to_string(),
        ));
    }
    if let Some(status) = status_filter {
        validate_status_filter(status)?;
    }

    let ctx = db::open_project(start)?;
    let conn = &ctx.conn;

    let rows: Vec<PendingWriteRecord> = match status_filter {
        Some(status) => {
            let mut stmt = conn.prepare(
                "SELECT id, kind, payload_json, rationale, status, actor, actor_raw,
                        provenance_json, created_at, reviewed_at
                 FROM pending_writes
                 WHERE project_id = 1 AND status = ?1
                 ORDER BY created_at DESC, id DESC
                 LIMIT ?2",
            )?;
            stmt.query_map(params![status, limit as i64], pending_row_to_record)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        }
        None => {
            let mut stmt = conn.prepare(
                "SELECT id, kind, payload_json, rationale, status, actor, actor_raw,
                        provenance_json, created_at, reviewed_at
                 FROM pending_writes
                 WHERE project_id = 1
                 ORDER BY created_at DESC, id DESC
                 LIMIT ?1",
            )?;
            stmt.query_map(params![limit as i64], pending_row_to_record)?
                .collect::<std::result::Result<Vec<_>, _>>()?
        }
    };

    Ok(rows)
}

pub fn show(start: &Path, id: i64) -> Result<PendingWriteRecord> {
    let ctx = db::open_project(start)?;
    let record = ctx
        .conn
        .query_row(
            "SELECT id, kind, payload_json, rationale, status, actor, actor_raw,
                    provenance_json, created_at, reviewed_at
             FROM pending_writes
             WHERE project_id = 1 AND id = ?1",
            params![id],
            pending_row_to_record,
        )
        .optional()?;

    record.ok_or_else(|| MemhubError::InvalidInput(format!("no pending write with id {id}")))
}

#[derive(Debug)]
pub struct AcceptOutcome {
    pub pending_id: i64,
    pub kind: String,
    pub durable_id: i64,
    pub durable_table: &'static str,
}

pub fn accept(start: &Path, id: i64, actor: &str) -> Result<AcceptOutcome> {
    let pending = load_pending(start, id)?;
    if pending.status != "pending" {
        return Err(MemhubError::InvalidInput(format!(
            "pending write {id} is already {}; only pending writes can be accepted",
            pending.status
        )));
    }

    let payload: Value = serde_json::from_str(&pending.payload_json).map_err(|err| {
        MemhubError::InvalidInput(format!(
            "pending write {id} has invalid payload_json: {err}"
        ))
    })?;

    let (durable_id, durable_table) = match pending.kind.as_str() {
        "fact" => {
            let key = payload
                .get("key")
                .and_then(Value::as_str)
                .ok_or_else(|| missing_payload_field(id, "key"))?;
            let value = payload
                .get("value")
                .and_then(Value::as_str)
                .ok_or_else(|| missing_payload_field(id, "value"))?;
            let (fact_id, _) = fact::add(start, key, value, "user", actor)?;
            (fact_id, "facts")
        }
        "decision" => {
            let title = payload
                .get("title")
                .and_then(Value::as_str)
                .ok_or_else(|| missing_payload_field(id, "title"))?;
            let decision_id = decision::add(start, title, &pending.rationale, actor)?;
            (decision_id, "decisions")
        }
        other => {
            return Err(MemhubError::InvalidInput(format!(
                "pending write {id} has unknown kind '{other}'"
            )));
        }
    };

    mark_status(
        start,
        id,
        "accepted",
        &format!("accept pending_write:{id}"),
        actor,
    )?;

    Ok(AcceptOutcome {
        pending_id: id,
        kind: pending.kind,
        durable_id,
        durable_table,
    })
}

pub fn reject(start: &Path, id: i64, reason: Option<&str>, actor: &str) -> Result<()> {
    let pending = load_pending(start, id)?;
    if pending.status != "pending" {
        return Err(MemhubError::InvalidInput(format!(
            "pending write {id} is already {}; only pending writes can be rejected",
            pending.status
        )));
    }

    let log_reason = match reason {
        Some(text) if !text.trim().is_empty() => {
            format!("reject pending_write:{id}: {}", text.trim())
        }
        _ => format!("reject pending_write:{id}"),
    };

    mark_status(start, id, "rejected", &log_reason, actor)?;
    Ok(())
}

pub fn expire(start: &Path, older_than_days: i64) -> Result<ReviewExpireSummary> {
    if older_than_days < 0 {
        return Err(MemhubError::InvalidInput(
            "older-than-days must not be negative".to_string(),
        ));
    }

    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;

    let cutoff_expr = format!("datetime('now', '-{older_than_days} days')");
    let update_sql = format!(
        "UPDATE pending_writes
         SET status = 'expired', reviewed_at = CURRENT_TIMESTAMP
         WHERE project_id = 1 AND status = 'pending' AND created_at < {cutoff_expr}"
    );
    let expired = tx.execute(&update_sql, [])?;

    if expired > 0 {
        db::log_write(
            &tx,
            "cli:user",
            "pending_writes",
            None,
            "update",
            &format!("expire {expired} pending writes older than {older_than_days} days"),
        )?;
    }

    tx.commit()?;

    Ok(ReviewExpireSummary {
        older_than_days,
        expired,
    })
}

fn load_pending(start: &Path, id: i64) -> Result<PendingWriteRecord> {
    show(start, id)
}

fn mark_status(start: &Path, id: i64, new_status: &str, reason: &str, actor: &str) -> Result<()> {
    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;

    let updated = tx.execute(
        "UPDATE pending_writes
         SET status = ?1, reviewed_at = CURRENT_TIMESTAMP
         WHERE project_id = 1 AND id = ?2 AND status = 'pending'",
        params![new_status, id],
    )?;

    if updated == 0 {
        return Err(MemhubError::InvalidInput(format!(
            "pending write {id} could not be updated; it may have been reviewed concurrently"
        )));
    }

    db::log_write(&tx, actor, "pending_writes", Some(id), "update", reason)?;

    tx.commit()?;
    Ok(())
}

fn pending_row_to_record(row: &rusqlite::Row<'_>) -> rusqlite::Result<PendingWriteRecord> {
    Ok(PendingWriteRecord {
        id: row.get(0)?,
        kind: row.get(1)?,
        payload_json: row.get(2)?,
        rationale: row.get(3)?,
        status: row.get(4)?,
        actor: row.get(5)?,
        actor_raw: row.get(6)?,
        provenance_json: row.get(7)?,
        created_at: row.get(8)?,
        reviewed_at: row.get(9)?,
    })
}

fn validate_status_filter(status: &str) -> Result<()> {
    match status {
        "pending" | "accepted" | "rejected" | "expired" => Ok(()),
        other => Err(MemhubError::InvalidInput(format!(
            "unknown pending write status '{other}'; expected one of pending|accepted|rejected|expired"
        ))),
    }
}

fn missing_payload_field(id: i64, field: &str) -> MemhubError {
    MemhubError::InvalidInput(format!(
        "pending write {id} payload is missing required field '{field}'"
    ))
}
