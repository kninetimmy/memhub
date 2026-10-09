use std::path::Path;

use rusqlite::{OptionalExtension, Transaction, params};

use crate::MemhubError;
use crate::Result;
use crate::config::RetrievalMode;
use crate::db;
use crate::models::{NarrativeEntry, NarrativeKind};
use crate::retrieval::{SourceType, arch_section_embed_text, eager_embed_batch_in_tx};

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
    let mode = ctx.config.retrieval.mode;
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

    if matches!(kind, NarrativeKind::Arch) {
        rederive_arch_sections(&tx, mode)?;
    }

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

/// Replace every `arch_sections` row with the heading sections of the
/// latest `project_arch` body (issue #232). Sections are derived data:
/// only the current architecture is ever searchable. Old rows are deleted
/// explicitly so their FTS and embedding delete triggers fire
/// (recursive_triggers is OFF, so the `project_arch` FK cascade would
/// not). `mode` gates eager embedding exactly as for every other writer.
pub(crate) fn rederive_arch_sections(tx: &Transaction<'_>, mode: RetrievalMode) -> Result<()> {
    tx.execute("DELETE FROM arch_sections WHERE project_id = 1", [])?;
    let latest: Option<(i64, String)> = tx
        .query_row(
            "SELECT id, body FROM project_arch
             WHERE project_id = 1
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((arch_id, body)) = latest else {
        return Ok(());
    };

    let mut embed_rows: Vec<(i64, String)> = Vec::new();
    for (ord, (heading_path, section)) in crate::commands::doc::chunk_markdown(&body)
        .iter()
        .enumerate()
    {
        tx.execute(
            "INSERT INTO arch_sections(project_id, arch_id, ord, heading_path, body)
             VALUES (1, ?1, ?2, ?3, ?4)",
            params![arch_id, ord as i64, heading_path, section],
        )?;
        embed_rows.push((
            tx.last_insert_rowid(),
            arch_section_embed_text(heading_path, section),
        ));
    }
    eager_embed_batch_in_tx(tx, mode, SourceType::ArchSection, embed_rows)
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
