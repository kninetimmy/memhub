use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;
use crate::db;
use crate::models::{
    Decision, Fact, FACT_STALE_AFTER_DAYS, NarrativeEntry, RenderResult, SessionNote, Task,
};
use crate::sync_md;

const PROJECT_FILENAME: &str = "PROJECT.md";
const LEDGER_FILENAME: &str = "PROJECT_LEDGER.md";
const SESSION_NOTE_RENDER_LIMIT: usize = 10;
const RECENT_WRITE_RENDER_LIMIT: usize = 50;
const RECENT_WRITE_WINDOW_DAYS: i64 = 30;

#[derive(Debug)]
struct RecentWrite {
    at: String,
    actor: String,
    table_name: String,
    action: String,
    reason: String,
}

#[derive(Debug)]
struct RenderSnapshot {
    project_name: String,
    generated_at: String,
    memhub_version: &'static str,
    state: Option<NarrativeEntry>,
    arch: Option<NarrativeEntry>,
    decisions: Vec<Decision>,
    tasks: Vec<Task>,
    facts: Vec<Fact>,
    stale_fact_count: i64,
    open_task_count: i64,
    recent_session_notes: Vec<SessionNote>,
    recent_writes: Vec<RecentWrite>,
}

pub fn render_project(start: &Path, actor: &str) -> Result<RenderResult> {
    let ctx = db::open_project(start)?;

    let snapshot = build_snapshot(&ctx.conn, &ctx.config.project_name)?;
    let project_md = format_project_md(&snapshot);
    let ledger_md = format_ledger_md(&snapshot);

    let output_dir = ctx.paths.repo_root.join(&ctx.config.render.output_dir);
    fs::create_dir_all(&output_dir)?;

    let backup_dir = ctx.paths.memhub_dir.join("backups").join("rendered");

    let project_path = output_dir.join(PROJECT_FILENAME);
    let ledger_path = output_dir.join(LEDGER_FILENAME);

    let mut written = Vec::new();
    let mut backups = Vec::new();

    for (path, content) in [(&project_path, &project_md), (&ledger_path, &ledger_md)] {
        let outcome = write_rendered_file(path, content, &backup_dir)?;
        written.push(outcome.path);
        if let Some(backup) = outcome.backup_path {
            backups.push(backup);
        }
    }

    db::log_write(
        &ctx.conn,
        actor,
        "render",
        None,
        "render",
        "memhub render",
    )?;

    Ok(RenderResult {
        output_dir,
        project_md_path: project_path,
        ledger_md_path: ledger_path,
        written_files: written,
        backup_files: backups,
    })
}

struct WriteOutcome {
    path: PathBuf,
    backup_path: Option<PathBuf>,
}

fn write_rendered_file(path: &Path, content: &str, backup_dir: &Path) -> Result<WriteOutcome> {
    let backup_path = if path.exists() {
        Some(sync_md::create_backup(path, backup_dir)?)
    } else {
        None
    };
    sync_md::write_with_replace(path, content)?;
    Ok(WriteOutcome {
        path: path.to_path_buf(),
        backup_path,
    })
}

fn build_snapshot(conn: &Connection, project_name: &str) -> Result<RenderSnapshot> {
    let generated_at: String = conn.query_row(
        "SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now')",
        [],
        |row| row.get(0),
    )?;

    Ok(RenderSnapshot {
        project_name: project_name.to_string(),
        generated_at,
        memhub_version: env!("CARGO_PKG_VERSION"),
        state: load_latest_narrative(conn, "project_state")?,
        arch: load_latest_narrative(conn, "project_arch")?,
        decisions: load_decisions(conn)?,
        tasks: load_tasks(conn)?,
        facts: load_facts(conn)?,
        stale_fact_count: count_stale_facts(conn)?,
        open_task_count: count_open_tasks(conn)?,
        recent_session_notes: load_recent_session_notes(conn)?,
        recent_writes: load_recent_writes(conn)?,
    })
}

fn load_latest_narrative(conn: &Connection, table: &str) -> Result<Option<NarrativeEntry>> {
    let sql = format!(
        "SELECT id, body, actor, actor_raw, created_at
         FROM {table}
         WHERE project_id = 1
         ORDER BY created_at DESC, id DESC
         LIMIT 1"
    );
    let entry = conn
        .query_row(&sql, [], |row| {
            Ok(NarrativeEntry {
                id: row.get(0)?,
                body: row.get(1)?,
                actor: row.get(2)?,
                actor_raw: row.get(3)?,
                created_at: row.get(4)?,
            })
        })
        .optional()?;
    Ok(entry)
}

fn load_decisions(conn: &Connection) -> Result<Vec<Decision>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, rationale, status, decided_at, source
         FROM decisions
         WHERE project_id = 1
         ORDER BY decided_at DESC, id DESC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(Decision {
                id: row.get(0)?,
                title: row.get(1)?,
                rationale: row.get(2)?,
                status: row.get(3)?,
                decided_at: row.get(4)?,
                source: row.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn load_tasks(conn: &Connection) -> Result<Vec<Task>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, status, notes, created_at, updated_at
         FROM tasks
         WHERE project_id = 1
         ORDER BY
             CASE status
                 WHEN 'open' THEN 0
                 WHEN 'blocked' THEN 1
                 WHEN 'done' THEN 2
                 ELSE 3
             END,
             updated_at DESC,
             id DESC",
    )?;
    let rows = stmt
        .query_map([], |row| {
            Ok(Task {
                id: row.get(0)?,
                title: row.get(1)?,
                status: row.get(2)?,
                notes: row.get(3)?,
                created_at: row.get(4)?,
                updated_at: row.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn load_facts(conn: &Connection) -> Result<Vec<Fact>> {
    let mut stmt = conn.prepare(
        "SELECT id, key, value, confidence, source, verified_at, created_at,
                CASE
                    WHEN verified_at IS NULL THEN 1
                    WHEN (julianday('now') - julianday(verified_at)) > ?1 THEN 1
                    ELSE 0
                END AS is_stale
         FROM facts
         WHERE project_id = 1
         ORDER BY key ASC",
    )?;
    let rows = stmt
        .query_map(params![FACT_STALE_AFTER_DAYS], |row| {
            let is_stale_int: i64 = row.get(7)?;
            Ok(Fact {
                id: row.get(0)?,
                key: row.get(1)?,
                value: row.get(2)?,
                confidence: row.get(3)?,
                source: row.get(4)?,
                verified_at: row.get(5)?,
                created_at: row.get(6)?,
                is_stale: is_stale_int != 0,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn count_stale_facts(conn: &Connection) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM facts
         WHERE project_id = 1
           AND (verified_at IS NULL
                OR (julianday('now') - julianday(verified_at)) > ?1)",
        params![FACT_STALE_AFTER_DAYS],
        |row| row.get(0),
    )?;
    Ok(count)
}

fn count_open_tasks(conn: &Connection) -> Result<i64> {
    let count: i64 = conn.query_row(
        "SELECT COUNT(*) FROM tasks
         WHERE project_id = 1 AND status = 'open'",
        [],
        |row| row.get(0),
    )?;
    Ok(count)
}

fn load_recent_session_notes(conn: &Connection) -> Result<Vec<SessionNote>> {
    let mut stmt = conn.prepare(
        "SELECT id, actor, actor_raw, text, created_at
         FROM session_notes
         WHERE project_id = 1
         ORDER BY created_at DESC, id DESC
         LIMIT ?1",
    )?;
    let rows = stmt
        .query_map(params![SESSION_NOTE_RENDER_LIMIT as i64], |row| {
            Ok(SessionNote {
                id: row.get(0)?,
                actor: row.get(1)?,
                actor_raw: row.get(2)?,
                text: row.get(3)?,
                created_at: row.get(4)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn load_recent_writes(conn: &Connection) -> Result<Vec<RecentWrite>> {
    let mut stmt = conn.prepare(
        "SELECT at, actor, table_name, action, reason
         FROM writes_log
         WHERE project_id = 1
           AND at >= datetime('now', '-' || ?1 || ' days')
         ORDER BY at DESC, id DESC
         LIMIT ?2",
    )?;
    let rows = stmt
        .query_map(
            params![RECENT_WRITE_WINDOW_DAYS, RECENT_WRITE_RENDER_LIMIT as i64],
            |row| {
                Ok(RecentWrite {
                    at: row.get(0)?,
                    actor: row.get(1)?,
                    table_name: row.get(2)?,
                    action: row.get(3)?,
                    reason: row.get(4)?,
                })
            },
        )?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

fn format_project_md(s: &RenderSnapshot) -> String {
    let mut out = String::new();
    out.push_str(&render_header(s));
    out.push('\n');
    out.push_str(&format!("# {}\n\n", s.project_name));

    out.push_str("## Currently building\n\n");
    match &s.state {
        Some(entry) => {
            out.push_str(entry.body.trim_end());
            out.push('\n');
            out.push('\n');
            out.push_str(&format!(
                "_Last updated {} by {}._\n\n",
                entry.created_at, entry.actor
            ));
        }
        None => {
            out.push_str(
                "_No project_state recorded. Use `memhub state set <body>` to populate._\n\n",
            );
        }
    }

    out.push_str("## Architecture\n\n");
    match &s.arch {
        Some(entry) => {
            out.push_str(entry.body.trim_end());
            out.push('\n');
            out.push('\n');
            out.push_str(&format!(
                "_Last updated {} by {}._\n\n",
                entry.created_at, entry.actor
            ));
        }
        None => {
            out.push_str(
                "_No project_arch recorded. Use `memhub arch set <body>` to populate._\n\n",
            );
        }
    }

    out.push_str("## Recent session notes\n\n");
    if s.recent_session_notes.is_empty() {
        out.push_str("_No session notes recorded._\n");
    } else {
        for note in &s.recent_session_notes {
            out.push_str(&format!(
                "- **{}** ({}) — {}\n",
                note.created_at,
                note.actor,
                collapse_inline(&note.text)
            ));
        }
    }

    out
}

fn format_ledger_md(s: &RenderSnapshot) -> String {
    let mut out = String::new();
    out.push_str(&render_header(s));
    out.push('\n');
    out.push_str(&format!("# {} — Ledger\n\n", s.project_name));

    out.push_str("## Decisions\n\n");
    if s.decisions.is_empty() {
        out.push_str("_No decisions recorded._\n\n");
    } else {
        out.push_str(&format!(
            "_{} decision(s). Most recent first._\n\n",
            s.decisions.len()
        ));
        for d in &s.decisions {
            out.push_str(&format!("### D{} — {}\n\n", d.id, d.title));
            out.push_str(&format!(
                "**Status:** {} • **Decided:** {} • **Source:** {}\n\n",
                d.status, d.decided_at, d.source
            ));
            if d.rationale.trim().is_empty() {
                out.push_str("_No rationale recorded._\n\n");
            } else {
                out.push_str(d.rationale.trim_end());
                out.push_str("\n\n");
            }
            out.push_str("---\n\n");
        }
    }

    out.push_str("## Backlog\n\n");
    if s.tasks.is_empty() {
        out.push_str("_No tasks recorded._\n\n");
    } else {
        out.push_str(&format!(
            "_{} task(s), {} open. Open first, then by recency._\n\n",
            s.tasks.len(),
            s.open_task_count
        ));
        for t in &s.tasks {
            out.push_str(&format!("### T{} — {}\n\n", t.id, t.title));
            out.push_str(&format!(
                "**Status:** {} • **Updated:** {}\n\n",
                t.status, t.updated_at
            ));
            match t.notes.as_deref() {
                Some(notes) if !notes.trim().is_empty() => {
                    out.push_str(notes.trim_end());
                    out.push_str("\n\n");
                }
                _ => out.push_str("_No notes._\n\n"),
            }
            out.push_str("---\n\n");
        }
    }

    out.push_str("## Facts\n\n");
    if s.facts.is_empty() {
        out.push_str("_No facts recorded._\n\n");
    } else {
        out.push_str(&format!(
            "_{} fact(s), {} stale._\n\n",
            s.facts.len(),
            s.stale_fact_count
        ));
        out.push_str("| Key | Value | Source | Confidence | Verified | Stale |\n");
        out.push_str("|-----|-------|--------|-----------|----------|-------|\n");
        for f in &s.facts {
            out.push_str(&format!(
                "| {} | {} | {} | {:.2} | {} | {} |\n",
                escape_table_cell(&f.key),
                escape_table_cell(&f.value),
                escape_table_cell(&f.source),
                f.confidence,
                escape_table_cell(f.verified_at.as_deref().unwrap_or("never")),
                if f.is_stale { "yes" } else { "no" }
            ));
        }
        out.push('\n');
    }

    out.push_str(&format!(
        "## Recent activity (last {} days)\n\n",
        RECENT_WRITE_WINDOW_DAYS
    ));
    if s.recent_writes.is_empty() {
        out.push_str("_No write activity in window._\n");
    } else {
        out.push_str("| When | Actor | Table | Action | Reason |\n");
        out.push_str("|------|-------|-------|--------|--------|\n");
        for w in &s.recent_writes {
            out.push_str(&format!(
                "| {} | {} | {} | {} | {} |\n",
                escape_table_cell(&w.at),
                escape_table_cell(&w.actor),
                escape_table_cell(&w.table_name),
                escape_table_cell(&w.action),
                escape_table_cell(&w.reason),
            ));
        }
    }

    out
}

fn render_header(s: &RenderSnapshot) -> String {
    format!(
        "<!-- memhub:rendered -->\n\
         <!-- DO NOT EDIT. Generated from .memhub/project.sqlite. -->\n\
         <!-- To change content, use memhub CLI; then re-run `memhub render`. -->\n\
         <!-- Generated at: {} by memhub {} -->\n",
        s.generated_at, s.memhub_version
    )
}

fn collapse_inline(text: &str) -> String {
    text.replace('\n', " ").replace('\r', " ")
}

fn escape_table_cell(text: &str) -> String {
    text.replace('|', "\\|")
        .replace('\n', " ")
        .replace('\r', " ")
}
