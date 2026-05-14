use std::path::Path;

use rusqlite::{Transaction, params};

use crate::Result;
use crate::commands::search;
use crate::db;
use crate::models::Decision;
use crate::sync_md;

pub fn add(start: &Path, title: &str, rationale: &str, source: &str, actor: &str) -> Result<i64> {
    add_with_decided_at(start, title, rationale, None, None, source, actor)
}

pub fn add_with_decided_at(
    start: &Path,
    title: &str,
    rationale: &str,
    decided_at: Option<&str>,
    summary: Option<&str>,
    source: &str,
    actor: &str,
) -> Result<i64> {
    let mut ctx = db::open_project(start)?;
    let mode = ctx.config.retrieval.mode;
    let tx = ctx.conn.transaction()?;
    let row_id = add_with_decided_at_in_tx(
        &tx, title, rationale, decided_at, summary, source, actor, mode,
    )?;
    tx.commit()?;
    sync_md::sync_if_enabled(start)?;
    Ok(row_id)
}

pub fn add_with_decided_at_in_tx(
    tx: &Transaction<'_>,
    title: &str,
    rationale: &str,
    decided_at: Option<&str>,
    summary: Option<&str>,
    source: &str,
    actor: &str,
    mode: crate::config::RetrievalMode,
) -> Result<i64> {
    crate::commands::validate_source(source)?;
    let summary_value = normalize_summary(summary);

    match decided_at {
        Some(when) => {
            tx.execute(
                "INSERT INTO decisions(project_id, title, rationale, status, decided_at, source, summary)
                 VALUES (1, ?1, ?2, 'active', ?3, ?4, ?5)",
                params![title, rationale, when, source, summary_value],
            )?;
        }
        None => {
            tx.execute(
                "INSERT INTO decisions(project_id, title, rationale, status, source, summary)
                 VALUES (1, ?1, ?2, 'active', ?3, ?4)",
                params![title, rationale, source, summary_value],
            )?;
        }
    }
    let row_id = tx.last_insert_rowid();
    search::sync_decision_chunks(tx)?;

    db::log_write(
        tx,
        actor,
        "decisions",
        Some(row_id),
        "insert",
        "decision add",
    )?;

    let embed_text =
        crate::retrieval::decision_embed_text(title, rationale, summary_value.as_deref());
    crate::retrieval::eager_embed_in_tx(
        tx,
        mode,
        crate::retrieval::SourceType::Decision,
        row_id,
        &embed_text,
    )?;

    Ok(row_id)
}

/// Update an existing decision's summary, then re-embed.
///
/// Backfill path for jargon-titled decisions added before the summary
/// column existed (migration 0011 / decision 72). Empty or whitespace-
/// only input is normalized to NULL so the embed text falls back to the
/// unaugmented format.
pub fn set_summary(start: &Path, id: i64, summary: Option<&str>, actor: &str) -> Result<()> {
    let mut ctx = db::open_project(start)?;
    let mode = ctx.config.retrieval.mode;
    let tx = ctx.conn.transaction()?;

    let (title, rationale): (String, String) = tx
        .query_row(
            "SELECT title, rationale FROM decisions WHERE id = ?1",
            params![id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => crate::MemhubError::InvalidInput(format!(
                "no decision with id {id}"
            )),
            other => crate::MemhubError::from(other),
        })?;

    let summary_value = normalize_summary(summary);
    tx.execute(
        "UPDATE decisions SET summary = ?1 WHERE id = ?2",
        params![summary_value, id],
    )?;

    db::log_write(
        &tx,
        actor,
        "decisions",
        Some(id),
        "update",
        "decision set-summary",
    )?;

    let embed_text =
        crate::retrieval::decision_embed_text(&title, &rationale, summary_value.as_deref());
    crate::retrieval::eager_embed_in_tx(
        &tx,
        mode,
        crate::retrieval::SourceType::Decision,
        id,
        &embed_text,
    )?;

    tx.commit()?;
    sync_md::sync_if_enabled(start)?;
    Ok(())
}

fn normalize_summary(summary: Option<&str>) -> Option<String> {
    match summary {
        Some(s) if !s.trim().is_empty() => Some(s.to_string()),
        _ => None,
    }
}

pub fn list(start: &Path) -> Result<Vec<Decision>> {
    let ctx = db::open_project(start)?;
    let mut stmt = ctx.conn.prepare(
        "SELECT id, title, rationale, status, decided_at, source, summary
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
            source: row.get(5)?,
            summary: row.get(6)?,
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
        "SELECT id, title, rationale, status, decided_at, source, summary
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
            source: row.get(5)?,
            summary: row.get(6)?,
        })
    })?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}
