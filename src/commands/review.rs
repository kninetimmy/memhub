use std::path::Path;

use rusqlite::{OptionalExtension, Transaction, TransactionBehavior, params};
use serde_json::Value;

use crate::commands::{decision, fact};
use crate::db;
use crate::models::{PendingWriteRecord, ReviewExpireSummary};
use crate::sync_md;
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
    let mut ctx = db::open_project(start)?;
    let mode = ctx.config.retrieval.mode;
    // Immediate behavior acquires the write lock at BEGIN so concurrent acceptors
    // serialize at the lock instead of both racing past the status check.
    let tx = ctx
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;

    let pending = read_pending_in_tx(&tx, id)?;
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

    let derived_source = derive_source_from_pending_actor(&pending.actor);

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
            let (fact_id, _) =
                fact::add_in_tx(&tx, key, value, &derived_source, actor, mode)?;
            (fact_id, "facts")
        }
        "decision" => {
            let title = payload
                .get("title")
                .and_then(Value::as_str)
                .ok_or_else(|| missing_payload_field(id, "title"))?;
            let decision_id = decision::add_with_decided_at_in_tx(
                &tx,
                title,
                &pending.rationale,
                None,
                None,
                &derived_source,
                actor,
                mode,
            )?;
            (decision_id, "decisions")
        }
        other => {
            return Err(MemhubError::InvalidInput(format!(
                "pending write {id} has unknown kind '{other}'"
            )));
        }
    };

    let updated = tx.execute(
        "UPDATE pending_writes
         SET status = 'accepted', reviewed_at = CURRENT_TIMESTAMP
         WHERE project_id = 1 AND id = ?1 AND status = 'pending'",
        params![id],
    )?;
    if updated == 0 {
        return Err(MemhubError::InvalidInput(format!(
            "pending write {id} could not be updated; it may have been reviewed concurrently"
        )));
    }

    db::log_write(
        &tx,
        actor,
        "pending_writes",
        Some(id),
        "update",
        &format!("accept pending_write:{id}"),
    )?;

    tx.commit()?;
    sync_md::sync_if_enabled(start)?;

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

fn read_pending_in_tx(tx: &Transaction<'_>, id: i64) -> Result<PendingWriteRecord> {
    let record = tx
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

/// Compose the durable `source` value for a pending write being accepted.
///
/// `pending_writes.actor` holds the normalized client identity (e.g. `codex`,
/// `claude-code`) captured at MCP `initialize` time, or a free-form CLI actor
/// string. When an agent's claim is accepted by an operator, both signals
/// matter — the operator endorsed it, and the agent surfaced it. See
/// `docs/reference/memhub-prd-source-vocabulary-addendum.md` §2.
fn derive_source_from_pending_actor(actor: &str) -> String {
    match actor {
        "" | "user" | "unknown" => "user".to_string(),
        agent => format!("user+agent:{agent}"),
    }
}

#[cfg(test)]
mod tests {
    use super::derive_source_from_pending_actor;

    #[test]
    fn derive_source_handles_user_and_unknown() {
        assert_eq!(derive_source_from_pending_actor("user"), "user");
        assert_eq!(derive_source_from_pending_actor("unknown"), "user");
        assert_eq!(derive_source_from_pending_actor(""), "user");
    }

    #[test]
    fn derive_source_composes_agent_identities() {
        assert_eq!(
            derive_source_from_pending_actor("codex"),
            "user+agent:codex"
        );
        assert_eq!(
            derive_source_from_pending_actor("claude-code"),
            "user+agent:claude-code"
        );
    }
}
