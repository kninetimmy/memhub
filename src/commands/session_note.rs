use std::path::Path;

use rusqlite::params;

use crate::MemhubError;
use crate::Result;
use crate::db;
use crate::models::SessionNote;

pub const DEFAULT_LIST_LIMIT: usize = 25;
pub const MAX_TEXT_LEN: usize = 4096;

pub fn add(start: &Path, text: &str, actor: &str, actor_raw: &str) -> Result<SessionNote> {
    let trimmed_text = text.trim();
    if trimmed_text.is_empty() {
        return Err(MemhubError::InvalidInput(
            "session note text must not be empty".to_string(),
        ));
    }
    if trimmed_text.chars().count() > MAX_TEXT_LEN {
        return Err(MemhubError::InvalidInput(format!(
            "session note text must be {MAX_TEXT_LEN} characters or fewer"
        )));
    }
    if actor.trim().is_empty() {
        return Err(MemhubError::InvalidInput(
            "session note actor must not be empty".to_string(),
        ));
    }

    let mut ctx = db::open_project(start)?;
    let mode = ctx.config.retrieval.mode;
    let tx = ctx.conn.transaction()?;

    tx.execute(
        "INSERT INTO session_notes(project_id, actor, actor_raw, text)
         VALUES (1, ?1, ?2, ?3)",
        params![actor, actor_raw, trimmed_text],
    )?;
    let row_id = tx.last_insert_rowid();

    db::log_write(
        &tx,
        actor,
        "session_notes",
        Some(row_id),
        "insert",
        "mcp log_session_note",
    )?;

    let embed_text = crate::retrieval::note_embed_text(trimmed_text);
    crate::retrieval::eager_embed_in_tx(
        &tx,
        mode,
        crate::retrieval::SourceType::Note,
        row_id,
        &embed_text,
    )?;

    let note = tx.query_row(
        "SELECT id, actor, actor_raw, text, created_at
         FROM session_notes WHERE id = ?1",
        params![row_id],
        row_to_note,
    )?;

    tx.commit()?;
    Ok(note)
}

pub fn list(
    start: &Path,
    limit: usize,
    actor_filter: Option<&str>,
    since_days: Option<i64>,
) -> Result<Vec<SessionNote>> {
    if limit == 0 {
        return Err(MemhubError::InvalidInput(
            "session note list limit must be greater than zero".to_string(),
        ));
    }
    if let Some(days) = since_days
        && days < 0
    {
        return Err(MemhubError::InvalidInput(
            "session note --since-days must not be negative".to_string(),
        ));
    }

    let ctx = db::open_project(start)?;
    let conn = &ctx.conn;

    let mut sql = String::from(
        "SELECT id, actor, actor_raw, text, created_at
         FROM session_notes
         WHERE project_id = 1",
    );
    if actor_filter.is_some() {
        sql.push_str(" AND actor = ?2");
    }
    if let Some(days) = since_days {
        sql.push_str(&format!(
            " AND created_at >= datetime('now', '-{days} days')"
        ));
    }
    sql.push_str(" ORDER BY created_at DESC, id DESC LIMIT ?1");

    let mut stmt = conn.prepare(&sql)?;
    let rows = match actor_filter {
        Some(actor) => stmt
            .query_map(params![limit as i64, actor], row_to_note)?
            .collect::<std::result::Result<Vec<_>, _>>()?,
        None => stmt
            .query_map(params![limit as i64], row_to_note)?
            .collect::<std::result::Result<Vec<_>, _>>()?,
    };
    Ok(rows)
}

fn row_to_note(row: &rusqlite::Row<'_>) -> rusqlite::Result<SessionNote> {
    Ok(SessionNote {
        id: row.get(0)?,
        actor: row.get(1)?,
        actor_raw: row.get(2)?,
        text: row.get(3)?,
        created_at: row.get(4)?,
    })
}
