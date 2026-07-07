use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Transaction, params};

use crate::commands::search;
use crate::db;
use crate::export::{EXPORT_VERSION, Export, v1};
use crate::sync_md;
use crate::{MemhubError, Result};

pub struct ImportSummary {
    pub source: PathBuf,
    pub target_root: PathBuf,
    pub forced: bool,
    pub facts: usize,
    pub decisions: usize,
    pub tasks: usize,
    pub commands: usize,
    pub pending_writes: usize,
    pub writes_log: usize,
    pub session_notes: usize,
    pub project_state: usize,
    pub project_arch: usize,
    /// Doc chunks that already existed in the target and were left
    /// untouched. Ingested docs are export-excluded, re-ingestable cache
    /// (decisions 86/90): import neither carries nor wipes them, and they
    /// are not counted by `count_durable_rows`, so a docs-only target
    /// passes the no-`--force` emptiness guard. Surfaced so the operator
    /// knows pre-existing docs survived rather than assuming a clean slate.
    pub retained_doc_chunks: usize,
}

pub fn run(start: &Path, source: &Path, force: bool) -> Result<ImportSummary> {
    let raw = fs::read_to_string(source)?;
    let payload: Export = serde_json::from_str(&raw)?;

    if payload.memhub_export_version != EXPORT_VERSION {
        return Err(MemhubError::InvalidInput(format!(
            "unsupported memhub_export_version: {} (this build supports {})",
            payload.memhub_export_version, EXPORT_VERSION
        )));
    }

    let mut ctx = db::open_project(start)?;

    let retained_doc_chunks = count_doc_chunks(&ctx.conn)?;

    if !force {
        let total = count_durable_rows(&ctx.conn)?;
        if total > 0 {
            return Err(MemhubError::InvalidInput(format!(
                "target memhub project at {} has {} durable row(s); pass --force to overwrite",
                ctx.paths.repo_root.display(),
                total
            )));
        }
    }

    let summary_counts = (
        payload.facts.len(),
        payload.decisions.len(),
        payload.tasks.len(),
        payload.commands.len(),
        payload.pending_writes.len(),
        payload.writes_log.len(),
        payload.session_notes.len(),
        payload.project_state.len(),
        payload.project_arch.len(),
    );

    let tx = ctx.conn.transaction()?;
    tx.execute_batch("PRAGMA defer_foreign_keys = ON")?;

    wipe_durable_tables(&tx)?;
    insert_facts(&tx, &payload.facts)?;
    insert_decisions(&tx, &payload.decisions)?;
    insert_tasks(&tx, &payload.tasks)?;
    insert_commands(&tx, &payload.commands)?;
    insert_pending_writes(&tx, &payload.pending_writes)?;
    insert_writes_log(&tx, &payload.writes_log)?;
    insert_session_notes(&tx, &payload.session_notes)?;
    insert_narrative(&tx, "project_state", &payload.project_state)?;
    insert_narrative(&tx, "project_arch", &payload.project_arch)?;

    search::sync_decision_chunks(&tx)?;

    let reason = if force {
        format!("imported from {} (forced)", source.display())
    } else {
        format!("imported from {}", source.display())
    };
    db::log_write(&tx, "cli:user", "import", None, "import", &reason)?;

    tx.commit()?;

    sync_md::sync_project(&ctx.paths.repo_root)?;

    Ok(ImportSummary {
        source: source.to_path_buf(),
        target_root: ctx.paths.repo_root.clone(),
        forced: force,
        facts: summary_counts.0,
        decisions: summary_counts.1,
        tasks: summary_counts.2,
        commands: summary_counts.3,
        pending_writes: summary_counts.4,
        writes_log: summary_counts.5,
        session_notes: summary_counts.6,
        project_state: summary_counts.7,
        project_arch: summary_counts.8,
        retained_doc_chunks,
    })
}

fn count_doc_chunks(conn: &rusqlite::Connection) -> Result<usize> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM doc_chunks WHERE project_id = 1",
        [],
        |row| row.get(0),
    )?;
    Ok(count as usize)
}

fn count_durable_rows(conn: &rusqlite::Connection) -> Result<i64> {
    let mut total: i64 = 0;
    for table in [
        "facts",
        "decisions",
        "tasks",
        "commands",
        "pending_writes",
        "session_notes",
        "project_state",
        "project_arch",
    ] {
        let count: i64 = conn.query_row(
            &format!("SELECT COUNT(*) FROM {table} WHERE project_id = 1"),
            [],
            |row| row.get(0),
        )?;
        total += count;
    }
    Ok(total)
}

fn wipe_durable_tables(tx: &Transaction<'_>) -> Result<()> {
    tx.execute(
        "DELETE FROM chunks WHERE project_id = 1 AND source_type = 'decision'",
        [],
    )?;
    for table in [
        "writes_log",
        "pending_writes",
        "commands",
        "tasks",
        "facts",
        "session_notes",
        "project_state",
        "project_arch",
    ] {
        tx.execute(&format!("DELETE FROM {table} WHERE project_id = 1"), [])?;
    }
    tx.execute("DELETE FROM decisions WHERE project_id = 1", [])?;
    Ok(())
}

fn insert_facts(tx: &Transaction<'_>, rows: &[v1::Fact]) -> Result<()> {
    // `superseded_by` is a self-referential FK; the enclosing import
    // transaction sets `PRAGMA defer_foreign_keys = ON`, so an old fact
    // that points at a not-yet-inserted newer fact resolves at commit
    // (same handling decisions already rely on).
    let mut stmt = tx.prepare(
        "INSERT INTO facts(id, project_id, key, value, confidence, source, verified_at, created_at, superseded_by)
         VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for fact in rows {
        stmt.execute(params![
            fact.id,
            fact.key,
            fact.value,
            fact.confidence,
            fact.source,
            fact.verified_at,
            fact.created_at,
            fact.superseded_by,
        ])?;
    }
    Ok(())
}

fn insert_decisions(tx: &Transaction<'_>, rows: &[v1::Decision]) -> Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO decisions(id, project_id, title, rationale, status, decided_at, superseded_by, source, summary)
         VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
    )?;
    for decision in rows {
        stmt.execute(params![
            decision.id,
            decision.title,
            decision.rationale,
            decision.status,
            decision.decided_at,
            decision.superseded_by,
            decision.source,
            decision.summary,
        ])?;
    }
    Ok(())
}

fn insert_tasks(tx: &Transaction<'_>, rows: &[v1::Task]) -> Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO tasks(id, project_id, title, status, notes, created_at, updated_at)
         VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6)",
    )?;
    for task in rows {
        stmt.execute(params![
            task.id,
            task.title,
            task.status,
            task.notes,
            task.created_at,
            task.updated_at,
        ])?;
    }
    Ok(())
}

fn insert_commands(tx: &Transaction<'_>, rows: &[v1::Command]) -> Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO commands(id, project_id, kind, cmdline, last_exit_code, last_run_at, success_count, fail_count)
         VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for command in rows {
        stmt.execute(params![
            command.id,
            command.kind,
            command.cmdline,
            command.last_exit_code,
            command.last_run_at,
            command.success_count,
            command.fail_count,
        ])?;
    }
    Ok(())
}

fn insert_pending_writes(tx: &Transaction<'_>, rows: &[v1::PendingWrite]) -> Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO pending_writes(
             id, project_id, kind, payload_json, rationale, status,
             actor, actor_raw, created_at, provenance_json, reviewed_at
         )
         VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
    )?;
    for pending in rows {
        stmt.execute(params![
            pending.id,
            pending.kind,
            pending.payload_json,
            pending.rationale,
            pending.status,
            pending.actor,
            pending.actor_raw,
            pending.created_at,
            pending.provenance_json,
            pending.reviewed_at,
        ])?;
    }
    Ok(())
}

fn insert_writes_log(tx: &Transaction<'_>, rows: &[v1::WriteLogEntry]) -> Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO writes_log(id, project_id, actor, table_name, row_id, action, reason, at)
         VALUES (?1, 1, ?2, ?3, ?4, ?5, ?6, ?7)",
    )?;
    for entry in rows {
        stmt.execute(params![
            entry.id,
            entry.actor,
            entry.table_name,
            entry.row_id,
            entry.action,
            entry.reason,
            entry.at,
        ])?;
    }
    Ok(())
}

fn insert_session_notes(tx: &Transaction<'_>, rows: &[v1::SessionNote]) -> Result<()> {
    let mut stmt = tx.prepare(
        "INSERT INTO session_notes(id, project_id, actor, actor_raw, text, created_at)
         VALUES (?1, 1, ?2, ?3, ?4, ?5)",
    )?;
    for note in rows {
        stmt.execute(params![
            note.id,
            note.actor,
            note.actor_raw,
            note.text,
            note.created_at,
        ])?;
    }
    Ok(())
}

fn insert_narrative(tx: &Transaction<'_>, table: &str, rows: &[v1::NarrativeEntry]) -> Result<()> {
    // `table` is a static caller-controlled identifier — never user input.
    let sql = format!(
        "INSERT INTO {table}(id, project_id, body, actor, actor_raw, created_at)
         VALUES (?1, 1, ?2, ?3, ?4, ?5)"
    );
    let mut stmt = tx.prepare(&sql)?;
    for entry in rows {
        stmt.execute(params![
            entry.id,
            entry.body,
            entry.actor,
            entry.actor_raw,
            entry.created_at,
        ])?;
    }
    Ok(())
}
