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
    pub decided_at: Option<String>,
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

    let decision_count: i64 = ctx
        .conn
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
            decision::add_with_decided_at(
                start,
                &d.title,
                &d.rationale,
                d.decided_at.as_deref(),
                None,
                "user",
                BOOTSTRAP_ACTOR,
            )?;
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
    let mut current_decided_at: Option<String> = None;

    for line in raw.lines() {
        let trimmed = line.trim_start();
        if let Some(rest) = trimmed.strip_prefix("## ") {
            flush_with_date(
                &mut current_title,
                &mut current_decided_at,
                &mut current_body,
                &mut out,
            );
            let (date, title) = extract_date_and_title(rest.trim());
            current_title = Some(title.to_string());
            current_decided_at = date;
        } else if current_title.is_some() {
            current_body.push(line.to_string());
        }
    }
    flush_with_date(
        &mut current_title,
        &mut current_decided_at,
        &mut current_body,
        &mut out,
    );

    out
}

fn flush_with_date(
    title: &mut Option<String>,
    decided_at: &mut Option<String>,
    body: &mut Vec<String>,
    out: &mut Vec<DecisionDraft>,
) {
    if let Some(t) = title.take() {
        let rationale = body.join("\n").trim().to_string();
        let date = decided_at.take();
        if !t.is_empty() {
            out.push(DecisionDraft {
                title: t,
                rationale,
                decided_at: date,
            });
        }
        body.clear();
    } else {
        decided_at.take();
    }
}

/// Parse a K9-canonical decision heading of the form
/// `YYYY-MM-DD <sep> Title`, where `<sep>` is an ASCII hyphen or an em-dash
/// (U+2014). Returns `(Some("YYYY-MM-DD 00:00:00"), "Title")` on success,
/// `(None, original)` otherwise.
pub(crate) fn extract_date_and_title(s: &str) -> (Option<String>, &str) {
    let bytes = s.as_bytes();
    if bytes.len() < 12
        || bytes[4] != b'-'
        || bytes[7] != b'-'
        || bytes[10] != b' '
        || !bytes[..4].iter().all(|b| b.is_ascii_digit())
        || !bytes[5..7].iter().all(|b| b.is_ascii_digit())
        || !bytes[8..10].iter().all(|b| b.is_ascii_digit())
    {
        return (None, s);
    }

    let rest = &s[11..];
    let title_offset = if let Some(after) = rest.strip_prefix("- ") {
        s.len() - after.len()
    } else if let Some(after) = rest.strip_prefix("\u{2014} ") {
        s.len() - after.len()
    } else {
        return (None, s);
    };

    let date_str = &s[..10];
    let title = s[title_offset..].trim_start();
    (Some(format!("{date_str} 00:00:00")), title)
}

pub fn parse_backlog(raw: &str) -> (Vec<TaskDraft>, usize) {
    struct CurrentTask {
        title: String,
        heading_done: bool,
        body_lines: Vec<String>,
    }

    let mut tasks: Vec<CurrentTask> = Vec::new();
    let mut current: Option<CurrentTask> = None;

    for line in raw.lines() {
        if let Some(rest) = line.strip_prefix("### ") {
            if let Some(t) = current.take() {
                tasks.push(t);
            }
            let (title, heading_done) = parse_backlog_heading(rest);
            current = Some(CurrentTask {
                title,
                heading_done,
                body_lines: Vec::new(),
            });
        } else if let Some(t) = current.as_mut() {
            t.body_lines.push(line.to_string());
        }
    }
    if let Some(t) = current.take() {
        tasks.push(t);
    }

    let mut out = Vec::new();
    let mut skipped = 0usize;
    for task in tasks {
        if task.title.is_empty() {
            continue;
        }
        let status = classify_backlog_task(task.heading_done, &task.body_lines);
        if status == "completed" {
            skipped += 1;
            continue;
        }
        let notes_joined = task.body_lines.join("\n");
        let notes_trimmed = notes_joined.trim();
        let notes = if notes_trimmed.is_empty() {
            None
        } else {
            Some(notes_trimmed.to_string())
        };
        out.push(TaskDraft {
            title: task.title,
            notes,
            status,
        });
    }

    (out, skipped)
}

fn parse_backlog_heading(s: &str) -> (String, bool) {
    let trimmed = s.trim();
    let mut done = false;

    let lower = trimmed.to_ascii_lowercase();
    let after_done = if let Some(idx) = lower.find("**done**") {
        let tail = &lower[idx + "**done**".len()..];
        let tail_skip = tail.trim_start_matches(|c: char| {
            c.is_whitespace() || matches!(c, '—' | '-' | ':' | '(' | '[')
        });
        let is_suffix = tail_skip.is_empty() || tail_skip.starts_with("pr");
        if is_suffix {
            done = true;
            trimmed[..idx]
                .trim_end_matches(|c: char| {
                    c == '—' || c == '-' || c == ':' || c == '(' || c == '[' || c.is_whitespace()
                })
                .to_string()
        } else {
            trimmed.to_string()
        }
    } else {
        trimmed.to_string()
    };

    let after_strike = after_done.trim();
    let final_title = if after_strike.starts_with("~~")
        && after_strike.ends_with("~~")
        && after_strike.len() >= 4
    {
        done = true;
        after_strike[2..after_strike.len() - 2].trim().to_string()
    } else {
        after_strike.to_string()
    };

    (final_title, done)
}

fn classify_backlog_task(heading_done: bool, body_lines: &[String]) -> String {
    if heading_done {
        return "completed".to_string();
    }

    let mut inline_done_pr = false;
    let mut bulleted_status: Option<String> = None;
    let mut status_clause: Option<String> = None;
    let mut legacy_status: Option<String> = None;

    for line in body_lines {
        if !inline_done_pr && line_has_inline_done_pr_marker(line) {
            inline_done_pr = true;
        }
        if bulleted_status.is_none()
            && let Some(v) = extract_bulleted_status(line)
        {
            bulleted_status = Some(v);
        }
        if status_clause.is_none()
            && let Some(v) = extract_status_clause(line)
        {
            status_clause = Some(v);
        }
        if legacy_status.is_none()
            && let Some(v) = extract_legacy_status(line)
        {
            legacy_status = Some(v);
        }
    }

    if inline_done_pr {
        return "completed".to_string();
    }
    if let Some(v) = bulleted_status {
        return map_k9_status(&v);
    }
    if let Some(v) = status_clause {
        return map_legacy_status(&v);
    }
    if let Some(v) = legacy_status {
        return map_legacy_status(&v);
    }
    "open".to_string()
}

fn line_has_inline_done_pr_marker(line: &str) -> bool {
    let lower = line.to_ascii_lowercase();
    let mut from = 0;
    while let Some(rel) = lower[from..].find("**done**") {
        let abs = from + rel;
        let after = &lower[abs + "**done**".len()..];
        let after = after.trim_start_matches(|c: char| {
            c.is_whitespace() || matches!(c, '—' | '-' | ':' | '(' | '[' | ',')
        });
        let after = skip_optional_action_clause(after);
        let after = after.trim_start_matches(|c: char| {
            c.is_whitespace() || matches!(c, '—' | '-' | ':' | '(' | '[' | ',')
        });
        if let Some(rest) = after.strip_prefix("pr") {
            let is_pr_word = rest.chars().next().is_none_or(|c| !c.is_alphabetic());
            if is_pr_word {
                let rest = rest.trim_start_matches(|c: char| c.is_whitespace() || c == '#');
                if rest.chars().next().is_some_and(|c| c.is_ascii_digit()) {
                    return true;
                }
            }
        }
        from = abs + "**done**".len();
    }
    false
}

fn skip_optional_action_clause(s: &str) -> &str {
    for verb in ["merged", "shipped", "landed", "released"] {
        if let Some(rest) = s.strip_prefix(verb)
            && rest.chars().next().is_none_or(|c| !c.is_alphanumeric())
        {
            return skip_optional_iso_date(rest);
        }
    }
    s
}

fn skip_optional_iso_date(s: &str) -> &str {
    let trimmed = s.trim_start();
    let bytes = trimmed.as_bytes();
    if bytes.len() >= 10
        && bytes[4] == b'-'
        && bytes[7] == b'-'
        && bytes[..4].iter().all(|b| b.is_ascii_digit())
        && bytes[5..7].iter().all(|b| b.is_ascii_digit())
        && bytes[8..10].iter().all(|b| b.is_ascii_digit())
    {
        &trimmed[10..]
    } else {
        s
    }
}

fn extract_status_clause(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    for prefix in ["**status:**", "**status.**"] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let raw = rest.split_whitespace().next()?;
            let cleaned = raw.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '-');
            if cleaned.is_empty() {
                return None;
            }
            return Some(cleaned.to_string());
        }
    }
    None
}

fn extract_bulleted_status(line: &str) -> Option<String> {
    let trimmed = line.trim_start();
    let lower = trimmed.to_ascii_lowercase();
    for prefix in [
        "- **status.**",
        "- **status:**",
        "* **status.**",
        "* **status:**",
    ] {
        if let Some(rest) = lower.strip_prefix(prefix) {
            let raw = rest.split_whitespace().next()?;
            let cleaned = raw.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '-');
            if cleaned.is_empty() {
                return None;
            }
            return Some(cleaned.to_string());
        }
    }
    None
}

fn extract_legacy_status(line: &str) -> Option<String> {
    let trimmed = line.trim();
    let lower = trimmed.to_ascii_lowercase();
    let rest = lower.strip_prefix("status:")?;
    let raw = rest.split_whitespace().next()?;
    let cleaned = raw.trim_end_matches(|c: char| !c.is_alphanumeric() && c != '-');
    if cleaned.is_empty() {
        return None;
    }
    Some(cleaned.to_string())
}

fn map_k9_status(value: &str) -> String {
    match value {
        "done" => "completed".to_string(),
        "blocked" => "blocked".to_string(),
        _ => "open".to_string(),
    }
}

fn map_legacy_status(value: &str) -> String {
    match value {
        "done" | "completed" | "shipped" | "landed" | "released" => "completed".to_string(),
        "blocked" => "blocked".to_string(),
        _ => "open".to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_dated_decision_headings_with_ascii_hyphen() {
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
        assert_eq!(out[0].decided_at.as_deref(), Some("2026-04-21 00:00:00"));
        assert!(out[0].rationale.contains("Rationale line one."));
        assert!(out[0].rationale.contains("Rationale line two."));
        assert_eq!(out[1].title, "Second decision");
        assert_eq!(out[1].decided_at.as_deref(), Some("2026-04-22 00:00:00"));
        assert_eq!(out[1].rationale.trim(), "- Body.");
    }

    #[test]
    fn parses_dated_decision_headings_with_em_dash() {
        let raw = "\
# Project Decisions

## 2026-05-01 \u{2014} K9 canonical heading

- Body line.

## 2026-05-02 \u{2014} Another decision

Body prose.
";
        let out = parse_decisions(raw);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0].title, "K9 canonical heading");
        assert_eq!(out[0].decided_at.as_deref(), Some("2026-05-01 00:00:00"));
        assert_eq!(out[1].title, "Another decision");
        assert_eq!(out[1].decided_at.as_deref(), Some("2026-05-02 00:00:00"));
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
        assert!(out[0].decided_at.is_none());
        assert_eq!(out[0].rationale.trim(), "Body text.");
    }

    #[test]
    fn extract_date_and_title_handles_separators() {
        assert_eq!(
            extract_date_and_title("2026-04-21 - Title"),
            (Some("2026-04-21 00:00:00".to_string()), "Title")
        );
        assert_eq!(
            extract_date_and_title("2026-04-21 \u{2014} Title"),
            (Some("2026-04-21 00:00:00".to_string()), "Title")
        );
        assert_eq!(extract_date_and_title("Plain title"), (None, "Plain title"));
        // En-dash is intentionally NOT accepted — K9 canonical is em-dash only.
        assert_eq!(
            extract_date_and_title("2026-04-21 \u{2013} Title"),
            (None, "2026-04-21 \u{2013} Title")
        );
        // Malformed date shape.
        assert_eq!(
            extract_date_and_title("2026/04/21 - Title"),
            (None, "2026/04/21 - Title")
        );
    }

    #[test]
    fn parses_k9_canonical_h3_items() {
        let raw = "\
# Project Backlog

## Items

### Normalize log timestamps
- **Scope.** LogParser assumes local time; needs UTC normalization.
- **Status.** triaged
- **Design note.** Prefer UTC in-memory.

### Add tracing
- **Status.** done
- **Design note.** Shipped earlier.

### Wait on upstream fix
- **Status.** blocked
- **Affected files.** src/foo.rs
";
        let (tasks, skipped) = parse_backlog(raw);
        assert_eq!(skipped, 1, "the 'done' item should be skipped");
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].title, "Normalize log timestamps");
        assert_eq!(tasks[0].status, "open");
        assert!(tasks[0].notes.as_deref().unwrap().contains("LogParser"));
        assert_eq!(tasks[1].title, "Wait on upstream fix");
        assert_eq!(tasks[1].status, "blocked");
        assert!(tasks[1].notes.as_deref().unwrap().contains("src/foo.rs"));
    }

    #[test]
    fn heading_suffix_done_marker_skips_task() {
        let raw = "\
### Hugging Face integration — **done**
- **Status.** done

### Pending work
- **Status.** triaged
";
        let (tasks, skipped) = parse_backlog(raw);
        assert_eq!(skipped, 1);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Pending work");
    }

    #[test]
    fn heading_suffix_done_with_pr_ref_skips_and_strips_metadata() {
        let raw = "\
### Encrypted drive support — **done** PR #253 (`6cfae14`)
- Shipped earlier.

### Still active
- **Status.** triaged
";
        let (tasks, skipped) = parse_backlog(raw);
        assert_eq!(skipped, 1);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Still active");
    }

    #[test]
    fn strikethrough_heading_skips_task() {
        let raw = "\
### ~~Old approach~~
- This was abandoned.

### Still active
- **Status.** triaged
";
        let (tasks, skipped) = parse_backlog(raw);
        assert_eq!(skipped, 1);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Still active");
    }

    #[test]
    fn inline_done_pr_marker_skips_task() {
        let raw = "\
### Some completed item
- Hit by **done** PR #253 last week.
- **Status.** in-progress

### Still in flight
- **Status.** planning
";
        let (tasks, skipped) = parse_backlog(raw);
        assert_eq!(skipped, 1);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Still in flight");
        assert_eq!(tasks[0].status, "open");
    }

    #[test]
    fn inline_done_with_merged_date_clause_skips_task() {
        // Real Free-AI-SSD shape: F2/F2a/F3/X13 status lines all use
        // `**done** — merged YYYY-MM-DD (PR #N, ...)`.
        let raw = "\
### F2 — Live model list fetch
**Status:** **done** — merged 2026-05-07 (PR #202, squash commit `dbc2510`).

### F3 — PrepApp 2-tab restructure
**Status:** **done** — merged 2026-04-21 (PR #164, merge commit `953fb1b`). Stages 2-3 shipped in the same PR.

### Still active
- **Status.** triaged
";
        let (tasks, skipped) = parse_backlog(raw);
        assert_eq!(skipped, 2);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Still active");
    }

    #[test]
    fn inline_done_with_shipped_or_landed_verb_clause_skips_task() {
        let raw = "\
### Item A
**Status:** **done** — shipped 2026-05-01 (PR #100).

### Item B
**Status:** **done** — landed 2026-04-30 (PR #101).

### Item C
**Status:** **done** — released 2026-04-29 (PR #102).

### Still open
- **Status.** in-progress
";
        let (tasks, skipped) = parse_backlog(raw);
        assert_eq!(skipped, 3);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Still open");
    }

    #[test]
    fn inline_done_with_verb_clause_but_no_pr_stays_open() {
        // Without a PR # reference, the verb clause alone is not strong
        // enough evidence to skip — fall through to other signals.
        let raw = "\
### Loose end
**Status:** **done** — merged 2026-05-07
- **Status.** in-progress
";
        let (tasks, skipped) = parse_backlog(raw);
        assert_eq!(skipped, 0);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, "open");
    }

    #[test]
    fn bolded_status_clause_with_shipped_vocab_skips_task() {
        // Real Free-AI-SSD shape: X25's status line uses bare "Shipped"
        // without a bolded **done** marker.
        let raw = "\
### X25 — Extend File.Replace retry
**Status:** Shipped — PR #155 (`2f7dcd8`), v1.2.9.

### X26 — Landed elsewhere
**Status:** Landed — PR #200.

### X27 — Released sample
**Status:** Released — PR #201.

### Still active
- **Status.** triaged
";
        let (tasks, skipped) = parse_backlog(raw);
        assert_eq!(skipped, 3);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Still active");
    }

    #[test]
    fn bolded_status_clause_with_non_done_vocab_stays_open() {
        // The status-clause path should not over-match on "filed",
        // "in-progress", "planning", or other non-done vocabulary.
        let raw = "\
### Filed
**Status:** filed 2026-05-10 (post-C3 UX polish).

### In flight
**Status:** in-progress — exploring approaches.
";
        let (tasks, skipped) = parse_backlog(raw);
        assert_eq!(skipped, 0);
        assert_eq!(tasks.len(), 2);
        assert_eq!(tasks[0].status, "open");
        assert_eq!(tasks[1].status, "open");
    }

    #[test]
    fn k9_status_vocab_maps_to_memhub_status() {
        let raw = "\
### Triaged item
- **Status.** triaged

### Planning item
- **Status.** planning

### In-progress item
- **Status.** in-progress

### Blocked item
- **Status.** blocked
";
        let (tasks, skipped) = parse_backlog(raw);
        assert_eq!(skipped, 0);
        assert_eq!(tasks.len(), 4);
        assert_eq!(tasks[0].status, "open");
        assert_eq!(tasks[1].status, "open");
        assert_eq!(tasks[2].status, "open");
        assert_eq!(tasks[3].status, "blocked");
    }

    #[test]
    fn legacy_status_line_is_tolerated_fallback() {
        let raw = "\
### Memhub-style item
Body prose here.
Status: blocked

### Another memhub-style
Status: completed
";
        let (tasks, skipped) = parse_backlog(raw);
        assert_eq!(skipped, 1);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Memhub-style item");
        assert_eq!(tasks[0].status, "blocked");
    }

    #[test]
    fn preamble_before_first_h3_is_ignored() {
        let raw = "\
# Project Backlog

## How to pull from this file

When asked to tackle X, read the item first.
- This bullet should NOT become a task.
- Neither should this one.

## Items

### Real item
- **Status.** triaged
";
        let (tasks, skipped) = parse_backlog(raw);
        assert_eq!(skipped, 0);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Real item");
    }

    #[test]
    fn item_without_status_defaults_to_open() {
        let raw = "\
### Plain item
Just a description, no status field at all.
";
        let (tasks, skipped) = parse_backlog(raw);
        assert_eq!(skipped, 0);
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].status, "open");
        assert!(
            tasks[0]
                .notes
                .as_deref()
                .unwrap()
                .contains("Just a description")
        );
    }

    #[test]
    fn synthetic_freeai_ssd_regression() {
        let raw = "\
# Project Backlog

## Unified label scheme

- **`C#`** — Cross-OS work
- **`W#`** — Windows-only
- **`M#`** — Mac-only

## Priority order

1. Some intro list
2. More intro

## Items

### C1 — First real item
- **Scope.** Some scope.
- **Status.** triaged
- Don't expand scope into a general retry framework. This is narrowly
  `File.Replace`-specific.

### C2 — Second real item — **done**
- Shipped earlier.

### ~~C3 — Third item with strikethrough~~
- Abandoned.

### C4 — Fourth item
- Hit by **done** PR #253 last week.
- **Status.** blocked

### C5 — Fifth item
- **Status.** in-progress
";
        let (tasks, skipped) = parse_backlog(raw);
        assert_eq!(
            skipped, 3,
            "C2 heading-done, C3 strikethrough, C4 inline-done-PR"
        );
        assert_eq!(tasks.len(), 2);
        assert!(tasks[0].title.starts_with("C1"));
        assert!(tasks[1].title.starts_with("C5"));
        for t in &tasks {
            assert!(
                !t.title.starts_with("**`"),
                "preamble bullet leaked as task"
            );
        }
    }
}
