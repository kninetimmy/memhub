use std::path::Path;

use rusqlite::params;
use serde_json::json;

use crate::MemhubError;
use crate::Result;
use crate::db;

pub fn propose_fact(
    start: &Path,
    key: &str,
    value: &str,
    rationale: &str,
    actor: &str,
    actor_raw: &str,
) -> Result<i64> {
    ensure_non_empty("fact key", key)?;
    ensure_non_empty("fact value", value)?;
    ensure_non_empty("fact rationale", rationale)?;
    let payload = json!({
        "key": key,
        "value": value,
    });

    insert_pending_write(
        start,
        "fact",
        &payload.to_string(),
        rationale,
        actor,
        actor_raw,
        "mcp propose_fact",
    )
}

pub fn propose_decision(
    start: &Path,
    title: &str,
    rationale: &str,
    actor: &str,
    actor_raw: &str,
) -> Result<i64> {
    ensure_non_empty("decision title", title)?;
    ensure_non_empty("decision rationale", rationale)?;
    let payload = json!({
        "title": title,
    });

    insert_pending_write(
        start,
        "decision",
        &payload.to_string(),
        rationale,
        actor,
        actor_raw,
        "mcp propose_decision",
    )
}

fn insert_pending_write(
    start: &Path,
    kind: &str,
    payload_json: &str,
    rationale: &str,
    actor: &str,
    actor_raw: &str,
    reason: &str,
) -> Result<i64> {
    ensure_non_empty("pending write kind", kind)?;
    ensure_non_empty("pending write actor", actor)?;
    ensure_non_empty("pending write actor_raw", actor_raw)?;
    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;

    tx.execute(
        "INSERT INTO pending_writes(
            project_id,
            kind,
            payload_json,
            rationale,
            actor,
            actor_raw
         ) VALUES (1, ?1, ?2, ?3, ?4, ?5)",
        params![kind, payload_json, rationale, actor, actor_raw],
    )?;
    let row_id = tx.last_insert_rowid();

    db::log_write(&tx, actor, "pending_writes", Some(row_id), "insert", reason)?;

    tx.commit()?;
    Ok(row_id)
}

fn ensure_non_empty(field_name: &str, value: &str) -> Result<()> {
    if value.trim().is_empty() {
        return Err(MemhubError::InvalidInput(format!(
            "{field_name} must not be empty"
        )));
    }

    Ok(())
}
