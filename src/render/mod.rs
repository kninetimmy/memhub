use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OptionalExtension, params};

use crate::Result;
use crate::commands::metrics::query_period_totals;
use crate::db;
use crate::metrics::formatter::render_period_block;
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
    token_accounting_section: Option<String>,
}

pub fn render_project(start: &Path, actor: &str) -> Result<RenderResult> {
    let ctx = db::open_project(start)?;

    let snapshot = build_snapshot(&ctx.conn, &ctx.config.project_name, ctx.config.metrics.enabled)?;
    let project_md = format_project_md(&snapshot);
    let ledger_md = format_ledger_md(&snapshot);

    let output_dir = ctx.paths.repo_root.join(&ctx.config.render.output_dir);
    fs::create_dir_all(&output_dir)?;

    let backup_dir = ctx.paths.memhub_dir.join("backups").join("rendered");

    let project_path = output_dir.join(PROJECT_FILENAME);
    let ledger_path = output_dir.join(LEDGER_FILENAME);

    // Phase 1 — prepare all files. Backups and temp writes happen up front so
    // any failure here leaves the existing rendered outputs untouched. Either
    // both staged files exist or neither destination is at risk.
    let staged = [
        stage_rendered_file(&project_path, &project_md, &backup_dir)?,
        stage_rendered_file(&ledger_path, &ledger_md, &backup_dir)?,
    ];

    // Phase 2 — commit each temp into place. `fs::rename` is an atomic replace
    // on both Unix and Windows, so the per-file swap has no missing-file
    // window. The irreducible inconsistency window is between the two renames,
    // and the prior content of each file remains recoverable from `backup_dir`.
    let mut written = Vec::new();
    let mut backups = Vec::new();
    for (index, item) in staged.iter().enumerate() {
        if let Err(err) = fs::rename(&item.temp_path, &item.dest_path) {
            // Best-effort cleanup of unrenamed temps from this render.
            let _ = fs::remove_file(&item.temp_path);
            for later in &staged[index + 1..] {
                let _ = fs::remove_file(&later.temp_path);
            }
            return Err(err.into());
        }
        written.push(item.dest_path.clone());
        if let Some(backup) = item.backup_path.clone() {
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

struct StagedFile {
    dest_path: PathBuf,
    temp_path: PathBuf,
    backup_path: Option<PathBuf>,
}

fn stage_rendered_file(path: &Path, content: &str, backup_dir: &Path) -> Result<StagedFile> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let backup_path = if path.exists() {
        Some(sync_md::create_backup(path, backup_dir)?)
    } else {
        None
    };

    let temp_path = sync_md::temp_path_for(path)?;
    fs::write(&temp_path, content)?;

    Ok(StagedFile {
        dest_path: path.to_path_buf(),
        temp_path,
        backup_path,
    })
}

fn build_snapshot(conn: &Connection, project_name: &str, metrics_enabled: bool) -> Result<RenderSnapshot> {
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
        token_accounting_section: if metrics_enabled {
            load_token_accounting_section(conn)?
        } else {
            None
        },
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
        "SELECT id, title, rationale, status, decided_at, source, summary
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
                summary: row.get(6)?,
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

fn load_token_accounting_section(conn: &Connection) -> Result<Option<String>> {
    let totals = query_period_totals(conn, 7)?;
    if totals.is_empty() {
        return Ok(None);
    }
    Ok(Some(render_period_block("Last 7 days", &totals)))
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
            out.push_str(strip_leading_heading(&entry.body, "## Currently building").trim_end());
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
            out.push_str(strip_leading_heading(&entry.body, "## Architecture").trim_end());
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

    if let Some(section) = &s.token_accounting_section {
        out.push('\n');
        out.push_str("## Token Accounting (last 7 days)\n\n");
        out.push_str(section);
        out.push('\n');
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

/// Wrap-up convention is to draft narrative bodies as standalone mini-docs
/// that lead with the section heading they belong under. Render adds its own
/// fixed section wrapper, so if the body also starts with that heading the
/// output ends up with two identical headings back-to-back. Strip a leading
/// matching heading (case-sensitive) so either body style renders cleanly.
fn strip_leading_heading<'a>(body: &'a str, heading: &str) -> &'a str {
    let trimmed = body.trim_start_matches(['\n', '\r']);
    let Some(rest) = trimmed.strip_prefix(heading) else {
        return body;
    };
    // Require end-of-string or a line break after the heading so we don't
    // truncate paragraphs that merely begin with the same text.
    if rest.is_empty() || rest.starts_with('\n') || rest.starts_with("\r\n") {
        rest.trim_start_matches(['\n', '\r'])
    } else {
        body
    }
}

fn escape_table_cell(text: &str) -> String {
    text.replace('|', "\\|")
        .replace('\n', " ")
        .replace('\r', " ")
}

#[cfg(test)]
mod tests {
    use super::strip_leading_heading;

    #[test]
    fn strips_matching_leading_heading() {
        let body = "## Currently building\n\nM8 details here.\n";
        assert_eq!(
            strip_leading_heading(body, "## Currently building"),
            "M8 details here.\n",
        );
    }

    #[test]
    fn leaves_body_alone_when_heading_does_not_match() {
        let body = "## Different heading\n\nbody\n";
        assert_eq!(
            strip_leading_heading(body, "## Currently building"),
            body,
        );
    }

    #[test]
    fn tolerates_leading_blank_lines_before_heading() {
        let body = "\n\n## Architecture\n\nbody\n";
        assert_eq!(
            strip_leading_heading(body, "## Architecture"),
            "body\n",
        );
    }

    #[test]
    fn does_not_strip_when_heading_text_is_only_a_prefix_of_a_paragraph() {
        let body = "## Currently building tools that...\n";
        assert_eq!(
            strip_leading_heading(body, "## Currently building"),
            body,
        );
    }

    // --- Token Accounting section rendering ---

    fn render_project_md_for_test(temp: &tempfile::TempDir) -> String {
        let ctx = crate::db::open_project(temp.path()).expect("open_project");
        let snapshot =
            super::build_snapshot(&ctx.conn, "test", ctx.config.metrics.enabled).expect("build_snapshot");
        super::format_project_md(&snapshot)
    }

    #[test]
    fn token_accounting_section_absent_when_metrics_disabled() {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        // metrics.enabled is false by default — no enable call
        let md = render_project_md_for_test(&temp);
        assert!(!md.contains("Token Accounting"));
    }

    #[test]
    fn token_accounting_section_absent_when_enabled_but_no_rows() {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        crate::commands::metrics::enable(temp.path()).expect("enable");
        let md = render_project_md_for_test(&temp);
        assert!(!md.contains("Token Accounting"));
    }

    #[test]
    fn token_accounting_section_present_when_enabled_and_rows_exist() {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        crate::commands::metrics::enable(temp.path()).expect("enable");
        let ctx = crate::db::open_project(temp.path()).expect("open_project");
        ctx.conn
            .execute(
                "INSERT INTO recall_metrics \
                 (ts, query_hash, bundle_tokens, ledger_tokens, rerank_used, result_count) \
                 VALUES (datetime('now'), 'abc123', 150, 800, 0, 3)",
                [],
            )
            .expect("insert recall_metrics");
        drop(ctx);
        let md = render_project_md_for_test(&temp);
        assert!(md.contains("## Token Accounting (last 7 days)"));
        assert!(md.contains("Recalls:"));
    }
}
