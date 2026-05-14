use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::Connection;

use crate::Result;
use crate::db;
use crate::export::{EXPORT_VERSION, Export, v1};

pub struct ExportSummary {
    pub destination: PathBuf,
    pub facts: usize,
    pub decisions: usize,
    pub tasks: usize,
    pub commands: usize,
    pub pending_writes: usize,
    pub writes_log: usize,
    pub session_notes: usize,
    pub project_state: usize,
    pub project_arch: usize,
}

pub fn run(start: &Path, destination: &Path) -> Result<ExportSummary> {
    let ctx = db::open_project(start)?;
    let conn = &ctx.conn;

    let (root_path_at_export, project_created_at, source_schema_version) = read_project_meta(conn)?;
    let exported_at: String = conn.query_row("SELECT CURRENT_TIMESTAMP", [], |row| row.get(0))?;

    let facts = read_facts(conn)?;
    let decisions = read_decisions(conn)?;
    let tasks = read_tasks(conn)?;
    let commands = read_commands(conn)?;
    let pending_writes = read_pending_writes(conn)?;
    let writes_log = read_writes_log(conn)?;
    let session_notes = read_session_notes(conn)?;
    let project_state = read_narrative(conn, "project_state")?;
    let project_arch = read_narrative(conn, "project_arch")?;

    let summary = ExportSummary {
        destination: destination.to_path_buf(),
        facts: facts.len(),
        decisions: decisions.len(),
        tasks: tasks.len(),
        commands: commands.len(),
        pending_writes: pending_writes.len(),
        writes_log: writes_log.len(),
        session_notes: session_notes.len(),
        project_state: project_state.len(),
        project_arch: project_arch.len(),
    };

    let payload = Export {
        memhub_export_version: EXPORT_VERSION,
        exported_at,
        exported_by: format!("memhub {}", env!("CARGO_PKG_VERSION")),
        source_schema_version,
        project: v1::ProjectMeta {
            root_path_at_export,
            created_at: project_created_at,
        },
        facts,
        decisions,
        tasks,
        commands,
        pending_writes,
        writes_log,
        session_notes,
        project_state,
        project_arch,
    };

    if let Some(parent) = destination.parent() {
        if !parent.as_os_str().is_empty() {
            fs::create_dir_all(parent)?;
        }
    }

    let json = serde_json::to_string_pretty(&payload)?;
    fs::write(destination, json)?;

    Ok(summary)
}

fn read_project_meta(conn: &Connection) -> Result<(String, String, String)> {
    Ok(conn.query_row(
        "SELECT root_path, created_at, schema_version FROM projects WHERE id = 1",
        [],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )?)
}

fn read_facts(conn: &Connection) -> Result<Vec<v1::Fact>> {
    let mut stmt = conn.prepare(
        "SELECT id, key, value, confidence, source, verified_at, created_at
         FROM facts
         WHERE project_id = 1
         ORDER BY id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(v1::Fact {
            id: row.get(0)?,
            key: row.get(1)?,
            value: row.get(2)?,
            confidence: row.get(3)?,
            source: row.get(4)?,
            verified_at: row.get(5)?,
            created_at: row.get(6)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn read_decisions(conn: &Connection) -> Result<Vec<v1::Decision>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, rationale, status, decided_at, superseded_by, source
         FROM decisions
         WHERE project_id = 1
         ORDER BY id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(v1::Decision {
            id: row.get(0)?,
            title: row.get(1)?,
            rationale: row.get(2)?,
            status: row.get(3)?,
            decided_at: row.get(4)?,
            superseded_by: row.get(5)?,
            source: row.get(6)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn read_tasks(conn: &Connection) -> Result<Vec<v1::Task>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, status, notes, created_at, updated_at
         FROM tasks
         WHERE project_id = 1
         ORDER BY id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(v1::Task {
            id: row.get(0)?,
            title: row.get(1)?,
            status: row.get(2)?,
            notes: row.get(3)?,
            created_at: row.get(4)?,
            updated_at: row.get(5)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn read_commands(conn: &Connection) -> Result<Vec<v1::Command>> {
    let mut stmt = conn.prepare(
        "SELECT id, kind, cmdline, last_exit_code, last_run_at, success_count, fail_count
         FROM commands
         WHERE project_id = 1
         ORDER BY id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(v1::Command {
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

fn read_pending_writes(conn: &Connection) -> Result<Vec<v1::PendingWrite>> {
    let mut stmt = conn.prepare(
        "SELECT id, kind, payload_json, rationale, status, actor, actor_raw,
                created_at, provenance_json, reviewed_at
         FROM pending_writes
         WHERE project_id = 1
         ORDER BY id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(v1::PendingWrite {
            id: row.get(0)?,
            kind: row.get(1)?,
            payload_json: row.get(2)?,
            rationale: row.get(3)?,
            status: row.get(4)?,
            actor: row.get(5)?,
            actor_raw: row.get(6)?,
            created_at: row.get(7)?,
            provenance_json: row.get(8)?,
            reviewed_at: row.get(9)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn read_writes_log(conn: &Connection) -> Result<Vec<v1::WriteLogEntry>> {
    let mut stmt = conn.prepare(
        "SELECT id, actor, table_name, row_id, action, reason, at
         FROM writes_log
         WHERE project_id = 1
         ORDER BY id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(v1::WriteLogEntry {
            id: row.get(0)?,
            actor: row.get(1)?,
            table_name: row.get(2)?,
            row_id: row.get(3)?,
            action: row.get(4)?,
            reason: row.get(5)?,
            at: row.get(6)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn read_session_notes(conn: &Connection) -> Result<Vec<v1::SessionNote>> {
    let mut stmt = conn.prepare(
        "SELECT id, actor, actor_raw, text, created_at
         FROM session_notes
         WHERE project_id = 1
         ORDER BY id",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(v1::SessionNote {
            id: row.get(0)?,
            actor: row.get(1)?,
            actor_raw: row.get(2)?,
            text: row.get(3)?,
            created_at: row.get(4)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn read_narrative(conn: &Connection, table: &str) -> Result<Vec<v1::NarrativeEntry>> {
    // `table` is a static caller-controlled identifier from this module — never
    // user input — so format-interpolating it is safe. SQLite does not allow
    // table names as bind parameters.
    let sql = format!(
        "SELECT id, body, actor, actor_raw, created_at
         FROM {table}
         WHERE project_id = 1
         ORDER BY id"
    );
    let mut stmt = conn.prepare(&sql)?;
    let rows = stmt.query_map([], |row| {
        Ok(v1::NarrativeEntry {
            id: row.get(0)?,
            body: row.get(1)?,
            actor: row.get(2)?,
            actor_raw: row.get(3)?,
            created_at: row.get(4)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}
