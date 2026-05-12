use std::fs;
use std::path::{Path, PathBuf};

use crate::commands::{decision, task};
use crate::db;
use crate::{MemhubError, Result};

pub const BOOTSTRAP_ACTOR: &str = "k9:bootstrap";

#[derive(Debug, Clone)]
pub struct DecisionDraft {
    pub title: String,
    pub rationale: String,
}

#[derive(Debug, Clone)]
pub struct TaskDraft {
    pub title: String,
    pub notes: Option<String>,
    pub status: String,
}

#[derive(Debug)]
pub struct BootstrapSummary {
    pub dry_run: bool,
    pub agent_docs_path: PathBuf,
    pub decisions: Vec<DecisionDraft>,
    pub tasks: Vec<TaskDraft>,
    pub tasks_skipped_completed: usize,
    pub files_read: Vec<PathBuf>,
    pub files_missing: Vec<PathBuf>,
}

pub fn run(start: &Path, dry_run: bool) -> Result<BootstrapSummary> {
    let ctx = db::open_project(start)?;

    let k9_cfg = ctx.config.integrations.k9.as_ref().ok_or_else(|| {
        MemhubError::InvalidInput(
            "K9 integration is not configured; run `memhub integrations enable-k9` first"
                .to_string(),
        )
    })?;
    if !k9_cfg.enabled {
        return Err(MemhubError::InvalidInput(
            "K9 integration is disabled; enable it with `memhub integrations enable-k9`"
                .to_string(),
        ));
    }

    let decision_count: i64 =
        ctx.conn
            .query_row("SELECT COUNT(*) FROM decisions", [], |row| row.get(0))?;
    let task_count: i64 = ctx
        .conn
        .query_row("SELECT COUNT(*) FROM tasks", [], |row| row.get(0))?;
    if decision_count + task_count > 0 {
        return Err(MemhubError::InvalidInput(format!(
            "bootstrap-k9 only runs on an empty database; found {decision_count} decision(s) and {task_count} task(s). Use `memhub decision add` / `task add` for incremental writes."
        )));
    }

    let agent_docs_dir = ctx.paths.repo_root.join(&k9_cfg.agent_docs_path);
    let decisions_path = agent_docs_dir.join("project_decisions.md");
    let backlog_path = agent_docs_dir.join("project_backlog.md");
    drop(ctx);

    let mut files_read = Vec::new();
    let mut files_missing = Vec::new();

    let decisions = if decisions_path.exists() {
        files_read.push(decisions_path.clone());
        let raw = fs::read_to_string(&decisions_path)?;
        parse_decisions(&raw)
    } else {
        files_missing.push(decisions_path.clone());
        Vec::new()
    };

    let (tasks, tasks_skipped_completed) = if backlog_path.exists() {
        files_read.push(backlog_path.clone());
        let raw = fs::read_to_string(&backlog_path)?;
        parse_backlog(&raw)
    } else {
        files_missing.push(backlog_path.clone());
        (Vec::new(), 0)
    };

    if files_read.is_empty() {
        return Err(MemhubError::InvalidInput(format!(
            "no K9 source files found under {} (expected project_decisions.md and/or project_backlog.md)",
            agent_docs_dir.display()
        )));
    }

    if !dry_run {
        for d in &decisions {
            decision::add(start, &d.title, &d.rationale, BOOTSTRAP_ACTOR)?;
        }
        for t in &tasks {
            task::add_with_status(
                start,
                &t.title,
                t.notes.as_deref(),
                &t.status,
                BOOTSTRAP_ACTOR,
            )?;
        }
    }

    Ok(BootstrapSummary {
        dry_run,
        agent_docs_path: agent_docs_dir,
        decisions,
        tasks,
        tasks_skipped_completed,
        files_read,
        files_missing,
    })
}

pub fn parse_decisions(raw: &str) -> Vec<DecisionDraft> {
    let mut out = Vec::new();
    let mut current_title: Option<String> = None;
    let mut current_body: Vec<String> = Vec::new();

    let flush = |title: &mut Option<String>,
                 body: &mut Vec<String>,
                 out: &mut Vec<DecisionDraft>| {
        if let Some(t) = title.take() {
            let rationale = body.join("\n").trim().to_string();
            if !t.is_empty() {
                out.push(DecisionDraft {
                    title: t,
                    rationale,
                });
            }
            body.clear();
        }
    };

    for line in raw.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("## ") {
            flush(&mut current_title, &mut current_body, &mut out);
            current_title = Some(strip_date_prefix(rest.trim()).to_string());
        } else if current_title.is_some() {
            current_body.push(line.to_string());
        }
    }
    flush(&mut current_title, &mut current_body, &mut out);

    out
}

fn strip_date_prefix(s: &str) -> &str {
    let bytes = s.as_bytes();
    if bytes.len() >= 13
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[10] == b' '
        && bytes[11] == b'-'
        && bytes[12] == b' '
        && bytes[..4].iter().all(|b| b.is_ascii_digit())
        && bytes[5..7].iter().all(|b| b.is_ascii_digit())
        && bytes[8..10].iter().all(|b| b.is_ascii_digit())
    {
        &s[13..]
    } else {
        s
    }
}

pub fn parse_backlog(raw: &str) -> (Vec<TaskDraft>, usize) {
    let mut out = Vec::new();
    let mut skipped = 0usize;
    let mut current_title: Option<String> = None;
    let mut current_sub: Vec<String> = Vec::new();

    let flush =
        |title: &mut Option<String>,
         sub: &mut Vec<String>,
         out: &mut Vec<TaskDraft>,
         skipped: &mut usize| {
            if let Some(t) = title.take() {
                let (status, notes) = classify_backlog_body(sub);
                if status == "completed" {
                    *skipped += 1;
                } else if !t.is_empty() {
                    out.push(TaskDraft {
                        title: t,
                        notes,
                        status,
                    });
                }
                sub.clear();
            }
        };

    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("- ") {
            flush(
                &mut current_title,
                &mut current_sub,
                &mut out,
                &mut skipped,
            );
            current_title = Some(rest.trim().trim_end_matches('.').to_string());
        } else if line.starts_with("  ") && current_title.is_some() {
            current_sub.push(line.trim().to_string());
        } else if line.trim().is_empty() {
            continue;
        } else {
            flush(
                &mut current_title,
                &mut current_sub,
                &mut out,
                &mut skipped,
            );
        }
    }
    flush(
        &mut current_title,
        &mut current_sub,
        &mut out,
        &mut skipped,
    );

    (out, skipped)
}

fn classify_backlog_body(sub: &[String]) -> (String, Option<String>) {
    let mut status = "open".to_string();
    let mut notes_lines: Vec<String> = Vec::new();
    for line in sub {
        if let Some(rest) = line.strip_prefix("Status:") {
            let raw = rest.trim().to_ascii_lowercase();
            status = match raw.as_str() {
                "completed" | "done" => "completed".to_string(),
                "blocked" => "blocked".to_string(),
                "open" | "" => "open".to_string(),
                _ => "open".to_string(),
            };
        } else {
            notes_lines.push(line.clone());
        }
    }
    let notes = if notes_lines.is_empty() {
        None
    } else {
        Some(notes_lines.join("\n"))
    };
    (status, notes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dated_decision_headings() {
        let raw = "\
# Project Decisions

---

## 2026-04-21 - First decision

- Rationale line one.
- Rationale line two.

## 2026-04-22 - Second decision

- Body.
";
        let out = parse_decisions(raw);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].title, "First decision");
        assert!(out[0].rationale.contains("Rationale line one."));
        assert!(out[0].rationale.contains("Rationale line two."));
        assert_eq!(out[1].title, "Second decision");
        assert_eq!(out[1].rationale.trim(), "- Body.");
    }

    #[test]
    fn parses_undated_decision_headings() {
        let raw = "\
## Plain title

Body text.
";
        let out = parse_decisions(raw);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].title, "Plain title");
        assert_eq!(out[0].rationale.trim(), "Body text.");
    }

    #[test]
    fn parses_backlog_with_mixed_statuses() {
        let raw = "\
# Project Backlog

## Items

- `M1-001` - First task.
  Status: open
  Notes: Some notes here.

- `M1-002` - Second task.
  Status: completed
  Notes: Already done.

- `M1-003` - Third task.
  Status: blocked
  Scope: src/foo.rs
";
        let (tasks, skipped) = parse_backlog(raw);
        assert_eq!(skipped, 1);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].title, "`M1-001` - First task");
        assert_eq!(tasks[0].status, "open");
        assert!(tasks[0].notes.as_deref().unwrap().contains("Some notes"));
        assert_eq!(tasks[1].title, "`M1-003` - Third task");
        assert_eq!(tasks[1].status, "blocked");
        assert!(tasks[1].notes.as_deref().unwrap().contains("Scope:"));
    }

    #[test]
    fn backlog_entry_without_status_defaults_to_open() {
        let raw = "- Plain task with no fields.\n";
        let (tasks, skipped) = parse_backlog(raw);
        assert_eq!(skipped, 0);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, "open");
        assert_eq!(tasks[0].notes, None);
    }
}
