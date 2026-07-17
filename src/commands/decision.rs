use std::path::Path;

use rusqlite::{OptionalExtension, Transaction, params};

use crate::Result;
use crate::commands::search;
use crate::db;
use crate::models::Decision;

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
        "SELECT id, title, rationale, status, decided_at, source, summary, superseded_by
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
            superseded_by: row.get(7)?,
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
        "SELECT id, title, rationale, status, decided_at, source, summary, superseded_by
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
            superseded_by: row.get(7)?,
        })
    })?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Outcome of a decision supersession — the demoted old decision and the one
/// it now points to, with titles for a friendly CLI echo.
#[derive(Debug)]
pub struct DecisionSupersedeOutcome {
    pub old_id: i64,
    pub old_title: String,
    pub new_id: i64,
    pub new_title: String,
}

/// Look up a decision by numeric id, returning its title. Decisions have no
/// natural key (unlike facts), so supersession resolves them by id alone.
fn resolve_decision(tx: &Transaction<'_>, id: i64) -> Result<Option<String>> {
    tx.query_row(
        "SELECT title FROM decisions WHERE project_id = 1 AND id = ?1",
        params![id],
        |r| r.get::<_, String>(0),
    )
    .optional()
    .map_err(Into::into)
}

/// Retire `old_id`'s decision by supersession (Wave 3 L3): set its status to
/// 'superseded' and link it to `new_id`. Demote-with-link, no-loss — the row
/// is NOT deleted; it stays present (annotated in render, penalized in
/// recall, and dropped from the active-decisions list). Q2 ruling: decisions
/// retire by supersession, not by age. Title/rationale/summary are unchanged,
/// so no re-embed or FTS/chunk resync is required. Errors if either id is
/// missing or if a decision would supersede itself.
pub fn supersede_in_tx(
    tx: &Transaction<'_>,
    old_id: i64,
    new_id: i64,
    actor: &str,
) -> Result<DecisionSupersedeOutcome> {
    let old_title = resolve_decision(tx, old_id)?
        .ok_or_else(|| crate::MemhubError::InvalidInput(format!("no decision with id {old_id}")))?;
    let new_title = resolve_decision(tx, new_id)?
        .ok_or_else(|| crate::MemhubError::InvalidInput(format!("no decision with id {new_id}")))?;
    if old_id == new_id {
        return Err(crate::MemhubError::InvalidInput(
            "a decision cannot supersede itself".to_string(),
        ));
    }

    tx.execute(
        "UPDATE decisions SET status = 'superseded', superseded_by = ?1
         WHERE project_id = 1 AND id = ?2",
        params![new_id, old_id],
    )?;

    db::log_write(
        tx,
        actor,
        "decisions",
        Some(old_id),
        "supersede",
        &format!("decision supersede {old_id} by {new_id}"),
    )?;

    Ok(DecisionSupersedeOutcome {
        old_id,
        old_title,
        new_id,
        new_title,
    })
}

/// CLI wrapper around [`supersede_in_tx`]: open the project and run the
/// supersession in one transaction.
pub fn supersede(
    start: &Path,
    old_id: i64,
    new_id: i64,
    actor: &str,
) -> Result<DecisionSupersedeOutcome> {
    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;
    let outcome = supersede_in_tx(&tx, old_id, new_id, actor)?;
    tx.commit()?;
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::init;
    use tempfile::tempdir;

    // Wave 3 L3 / Q2 — decisions retire by supersession, not age. The old
    // decision flips to status 'superseded', links to its replacement, and
    // stays present (demote-with-link) but drops out of the active list.
    #[test]
    fn supersede_sets_status_link_and_leaves_active_list() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let old_id = add(
            temp.path(),
            "Use JSON logs",
            "chosen for tooling",
            "user",
            "cli:user",
        )
        .expect("old");
        let new_id = add(
            temp.path(),
            "Use structured tracing",
            "replaces JSON logging",
            "user",
            "cli:user",
        )
        .expect("new");

        let outcome = supersede(temp.path(), old_id, new_id, "cli:user").expect("supersede");
        assert_eq!(outcome.old_id, old_id);
        assert_eq!(outcome.new_id, new_id);

        let all = list(temp.path()).expect("list");
        let old = all
            .iter()
            .find(|d| d.id == old_id)
            .expect("superseded decision must still be present (no-loss)");
        assert_eq!(old.status, "superseded");
        assert_eq!(old.superseded_by, Some(new_id));

        let active = list_active_recent(temp.path(), 10).expect("active");
        assert!(
            !active.iter().any(|d| d.id == old_id),
            "superseded decision must leave the active-recent list"
        );
        assert!(
            active.iter().any(|d| d.id == new_id),
            "the replacement stays active"
        );
    }

    #[test]
    fn supersede_rejects_self_and_missing_decision() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let id = add(temp.path(), "Only", "r", "user", "cli:user").expect("decision");

        assert!(matches!(
            supersede(temp.path(), id, id, "cli:user").expect_err("self must fail"),
            crate::MemhubError::InvalidInput(_)
        ));
        assert!(matches!(
            supersede(temp.path(), id, 9999, "cli:user").expect_err("missing must fail"),
            crate::MemhubError::InvalidInput(_)
        ));
        // Untouched by the failed ops.
        let d = list(temp.path()).expect("list");
        let row = d.iter().find(|d| d.id == id).unwrap();
        assert_eq!(row.status, "active");
        assert_eq!(row.superseded_by, None);
    }
}
