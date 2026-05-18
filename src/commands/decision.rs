use std::path::Path;

use rusqlite::{OptionalExtension, Transaction, params};

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

#[allow(clippy::too_many_arguments)]
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
            rusqlite::Error::QueryReturnedNoRows => {
                crate::MemhubError::InvalidInput(format!("no decision with id {id}"))
            }
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

#[derive(Debug)]
pub struct GlobalDecisionOutcome {
    pub id: i64,
    pub store_created: bool,
    /// True when a global decision with the same title already
    /// existed. Decisions have no natural key, so a re-promote
    /// duplicates; the CLI surfaces this as a warning.
    pub title_collision: bool,
}

/// Born-global decision write (M9). Requires `memhub global enable`
/// in this repo. Embeds using the *repo's* retrieval mode.
pub fn add_global(
    start: &Path,
    title: &str,
    rationale: &str,
    summary: Option<&str>,
    source: &str,
    actor: &str,
) -> Result<GlobalDecisionOutcome> {
    let mut gw = crate::commands::global::begin_write(start)?;
    let tx = gw.ctx.conn.transaction()?;
    let row_id =
        add_with_decided_at_in_tx(&tx, title, rationale, None, summary, source, actor, gw.mode)?;
    tx.commit()?;
    Ok(GlobalDecisionOutcome {
        id: row_id,
        store_created: gw.store_created,
        title_collision: false,
    })
}

/// Copy an existing repo decision into the machine-global store
/// (copy, not move). Decisions have no natural key; re-promoting the
/// same decision duplicates. `title_collision` reports whether a
/// global decision with this title already existed so the caller can
/// warn.
pub fn promote(start: &Path, id: i64, actor: &str) -> Result<GlobalDecisionOutcome> {
    let repo = db::open_project(start)?;
    crate::commands::global::ensure_enabled(&repo.config)?;
    let mode = repo.config.retrieval.mode;

    let (title, rationale, summary, source): (String, String, Option<String>, String) = repo
        .conn
        .query_row(
            "SELECT title, rationale, summary, source
             FROM decisions WHERE id = ?1 AND project_id = 1",
            params![id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
        )
        .map_err(|err| match err {
            rusqlite::Error::QueryReturnedNoRows => {
                crate::MemhubError::InvalidInput(format!("no decision with id {id}"))
            }
            other => crate::MemhubError::from(other),
        })?;

    let repo_root = repo.paths.repo_root.display().to_string();
    let store_created = !db::global_store_exists()?;

    let mut g = db::open_global()?;
    let tx = g.conn.transaction()?;

    let title_collision: bool = tx
        .query_row(
            "SELECT 1 FROM decisions WHERE project_id = 1 AND title = ?1 LIMIT 1",
            params![title],
            |_| Ok(()),
        )
        .optional()?
        .is_some();

    let row_id = add_with_decided_at_in_tx(
        &tx,
        &title,
        &rationale,
        None,
        summary.as_deref(),
        &source,
        actor,
        mode,
    )?;
    db::log_write(
        &tx,
        actor,
        "decisions",
        Some(row_id),
        "promote",
        &format!("promote from {repo_root}"),
    )?;
    tx.commit()?;

    Ok(GlobalDecisionOutcome {
        id: row_id,
        store_created,
        title_collision,
    })
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
