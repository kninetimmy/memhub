use std::path::PathBuf;
use std::process;

use clap::{Parser, Subcommand, ValueEnum};
use serde_json::json;

use crate::Result;
use crate::commands;
use crate::commands::{DEFAULT_ACTOR, validate_actor};
use crate::models::{InitResult, PendingWriteRecord, StatsSummary};

fn resolve_actor(actor: Option<&str>) -> Result<String> {
    match actor {
        Some(value) => {
            validate_actor(value)?;
            Ok(value.trim().to_string())
        }
        None => Ok(DEFAULT_ACTOR.to_string()),
    }
}

fn print_stats_human(s: &StatsSummary) {
    println!("memhub stats — project: {}", s.project_name);
    println!("Repo: {}", s.repo_root.display());
    println!("Window: {}", s.window_label);
    println!();
    println!("Totals");
    println!(
        "  Facts: {} ({} stale{})",
        s.facts,
        s.stale_facts,
        match s.stale_ratio {
            Some(r) => format!(", {:.0}% stale", r * 100.0),
            None => String::new(),
        }
    );
    println!("  Decisions: {}", s.decisions);
    println!("  Tasks: {} open / {} total", s.tasks_open, s.tasks_total);
    println!("  Commands: {}", s.commands);
    println!("  Commits: {}", s.commits);
    println!("  Files: {}", s.files);
    println!("  Search chunks: {}", s.chunks);
    println!("  Pending writes (open): {}", s.pending_writes_now);
    println!("  Writes logged (all time): {}", s.writes_logged_total);
    println!();
    println!("Activity ({})", s.window_label);
    println!("  Writes: {}", s.writes_in_window);
    if !s.writes_by_actor.is_empty() {
        println!("  By actor:");
        for row in &s.writes_by_actor {
            println!("    {:<24} {}", row.label, row.count);
        }
    }
    if !s.writes_by_table.is_empty() {
        println!("  By table:");
        for row in &s.writes_by_table {
            println!("    {:<24} {}", row.label, row.count);
        }
    }
    println!();
    println!("Pending writes ({})", s.window_label);
    println!("  Created: {}", s.pending_created_in_window);
    println!(
        "  Reviewed: {}{}",
        s.pending_reviewed_in_window,
        match s.review_rate {
            Some(r) => format!(" (review rate: {:.0}%)", r * 100.0),
            None => String::new(),
        }
    );
    if !s.pending_by_status.is_empty() {
        print!("  By status (all time):");
        for row in &s.pending_by_status {
            print!(" {}={}", row.label, row.count);
        }
        println!();
    }
    println!();
    if !s.top_command_kinds.is_empty() {
        println!("Top commands (by runs)");
        for c in &s.top_command_kinds {
            let conf = c
                .confidence
                .map(|v| format!("conf={:.2}", v))
                .unwrap_or_else(|| "conf=n/a".to_string());
            println!(
                "  {:<10} {}  ({}/{} runs)  {}",
                c.kind,
                conf,
                c.success_count,
                c.success_count + c.fail_count,
                c.cmdline,
            );
        }
        println!();
    }
    if !s.recent_facts.is_empty() {
        println!("Recent facts");
        for f in &s.recent_facts {
            let stamp = f.verified_at.as_deref().unwrap_or("never verified");
            let stale = if f.is_stale { " [stale]" } else { "" };
            println!("  {}  {}{}", stamp, f.key, stale);
        }
        println!();
    }
    println!(
        "Note: \"writes\" counts mutations recorded in writes_log. Read activity is not tracked in this slice; see PRD §17."
    );
}

fn print_stats_json(s: &StatsSummary) {
    let payload = json!({
        "project_name": s.project_name,
        "repo_root": s.repo_root.display().to_string(),
        "window": {
            "label": s.window_label,
            "days": s.window_days,
        },
        "totals": {
            "facts": s.facts,
            "stale_facts": s.stale_facts,
            "stale_ratio": s.stale_ratio,
            "decisions": s.decisions,
            "tasks_total": s.tasks_total,
            "tasks_open": s.tasks_open,
            "commands": s.commands,
            "commits": s.commits,
            "files": s.files,
            "chunks": s.chunks,
            "pending_writes_now": s.pending_writes_now,
            "writes_logged_total": s.writes_logged_total,
        },
        "activity": {
            "writes_in_window": s.writes_in_window,
            "writes_by_actor": s.writes_by_actor.iter().map(|r| json!({
                "label": r.label,
                "count": r.count,
            })).collect::<Vec<_>>(),
            "writes_by_table": s.writes_by_table.iter().map(|r| json!({
                "label": r.label,
                "count": r.count,
            })).collect::<Vec<_>>(),
        },
        "pending_writes": {
            "created_in_window": s.pending_created_in_window,
            "reviewed_in_window": s.pending_reviewed_in_window,
            "review_rate": s.review_rate,
            "by_status_all_time": s.pending_by_status.iter().map(|r| json!({
                "status": r.label,
                "count": r.count,
            })).collect::<Vec<_>>(),
        },
        "top_command_kinds": s.top_command_kinds.iter().map(|c| json!({
            "kind": c.kind,
            "cmdline": c.cmdline,
            "success_count": c.success_count,
            "fail_count": c.fail_count,
            "confidence": c.confidence,
            "last_run_at": c.last_run_at,
        })).collect::<Vec<_>>(),
        "recent_facts": s.recent_facts.iter().map(|f| json!({
            "key": f.key,
            "verified_at": f.verified_at,
            "is_stale": f.is_stale,
        })).collect::<Vec<_>>(),
        "notes": [
            "writes counts mutations recorded in writes_log; read activity is not tracked in this slice (PRD §17 read counter deferred)",
        ],
    });
    println!("{payload}");
}

fn pending_write_record_to_json(row: &PendingWriteRecord) -> serde_json::Value {
    json!({
        "id": row.id,
        "kind": row.kind,
        "status": row.status,
        "actor": row.actor,
        "actor_raw": row.actor_raw,
        "rationale": row.rationale,
        "payload_json": row.payload_json,
        "provenance_json": row.provenance_json,
        "created_at": row.created_at,
        "reviewed_at": row.reviewed_at,
    })
}

fn print_init_result(result: &InitResult) {
    println!("Initialized memhub at {}", result.repo_root.display());
    println!("Database: {}", result.db_path.display());
    println!(
        "Config created: {}",
        if result.config_created { "yes" } else { "no" }
    );
    println!(
        ".memhub existed already: {}",
        if result.memhub_preexisting {
            "yes"
        } else {
            "no"
        }
    );
    println!(
        ".gitignore updated: {}",
        if result.gitignore_updated {
            "yes"
        } else {
            "no"
        }
    );
    if result.migrations_applied.is_empty() {
        println!("Migrations applied: none");
    } else {
        println!(
            "Migrations applied: {}",
            result.migrations_applied.join(", ")
        );
    }
}

#[derive(Debug, Parser)]
#[command(
    name = "memhub",
    version,
    about = "Local-first project memory for Codex and Claude Code."
)]
pub struct Cli {
    #[command(subcommand)]
    pub command: TopLevelCommand,
}

#[derive(Debug, Subcommand)]
pub enum TopLevelCommand {
    Init {
        #[arg(long, value_name = "PATH")]
        from_backup: Option<PathBuf>,
    },
    Status,
    Stats {
        #[arg(long, value_enum, default_value_t = StatsWindowArg::ThirtyDays)]
        window: StatsWindowArg,
        #[arg(long)]
        json: bool,
    },
    SyncMd,
    Serve,
    IngestGit {
        #[arg(long)]
        since: Option<String>,
    },
    Search {
        query: String,
        #[arg(long, default_value_t = 10)]
        limit: usize,
    },
    Fact {
        #[command(subcommand)]
        command: FactCommand,
    },
    Decision {
        #[command(subcommand)]
        command: DecisionCommand,
    },
    Task {
        #[command(subcommand)]
        command: TaskCommand,
    },
    Command {
        #[command(subcommand)]
        command: CommandCommand,
    },
    Export {
        path: PathBuf,
    },
    Import {
        path: PathBuf,
        #[arg(long)]
        force: bool,
    },
    Review {
        #[command(subcommand)]
        command: ReviewCommand,
    },
    Integrations {
        #[command(subcommand)]
        command: IntegrationsCommand,
    },
    Note {
        #[command(subcommand)]
        command: NoteCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum NoteCommand {
    List {
        #[arg(long, default_value_t = 25)]
        limit: usize,
        #[arg(long)]
        actor: Option<String>,
        #[arg(long, value_name = "DAYS")]
        since_days: Option<i64>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum IntegrationsCommand {
    Status,
    EnableK9 {
        #[arg(long, value_name = "PATH")]
        agent_docs_path: Option<String>,
        #[arg(long)]
        force: bool,
    },
    DisableK9,
    CheckK9,
}

#[derive(Debug, Clone, ValueEnum)]
pub enum StatsWindowArg {
    #[value(name = "7d")]
    SevenDays,
    #[value(name = "30d")]
    ThirtyDays,
    #[value(name = "90d")]
    NinetyDays,
    #[value(name = "all")]
    All,
}

impl StatsWindowArg {
    fn to_window(&self) -> commands::stats::StatsWindow {
        match self {
            Self::SevenDays => commands::stats::StatsWindow::Days(7),
            Self::ThirtyDays => commands::stats::StatsWindow::Days(30),
            Self::NinetyDays => commands::stats::StatsWindow::Days(90),
            Self::All => commands::stats::StatsWindow::All,
        }
    }
}

#[derive(Debug, Clone, ValueEnum)]
pub enum PendingStatus {
    Pending,
    Accepted,
    Rejected,
    Expired,
    All,
}

impl PendingStatus {
    fn as_filter(&self) -> Option<&'static str> {
        match self {
            Self::Pending => Some("pending"),
            Self::Accepted => Some("accepted"),
            Self::Rejected => Some("rejected"),
            Self::Expired => Some("expired"),
            Self::All => None,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum ReviewCommand {
    List {
        #[arg(long, value_enum, default_value_t = PendingStatus::Pending)]
        status: PendingStatus,
        #[arg(long, default_value_t = 25)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
    Show {
        id: i64,
        #[arg(long)]
        json: bool,
    },
    Accept {
        id: i64,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        actor: Option<String>,
    },
    Reject {
        id: i64,
        #[arg(long)]
        reason: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        actor: Option<String>,
    },
    Expire {
        #[arg(long, default_value_t = 30)]
        older_than_days: i64,
    },
}

#[derive(Debug, Subcommand)]
pub enum FactCommand {
    Add {
        key: String,
        value: String,
        #[arg(long, default_value = "user")]
        source: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        actor: Option<String>,
    },
    List,
}

#[derive(Debug, Subcommand)]
pub enum DecisionCommand {
    Add {
        title: String,
        #[arg(long)]
        rationale: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        actor: Option<String>,
    },
    List,
}

#[derive(Debug, Clone, ValueEnum)]
pub enum TaskStatus {
    Open,
    Done,
    Blocked,
    All,
}

impl TaskStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Open => "open",
            Self::Done => "done",
            Self::Blocked => "blocked",
            Self::All => "all",
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum TaskCommand {
    Add {
        title: String,
        #[arg(long)]
        notes: Option<String>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        actor: Option<String>,
    },
    List {
        #[arg(long, value_enum)]
        status: Option<TaskStatus>,
    },
    Done {
        id: i64,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        actor: Option<String>,
    },
}

#[derive(Debug, Subcommand)]
pub enum CommandCommand {
    List,
    Verify {
        #[arg(value_enum)]
        kind: CommandKind,
        cmdline: String,
        #[arg(long)]
        exit_code: i64,
    },
}

#[derive(Debug, Clone, ValueEnum)]
pub enum CommandKind {
    Build,
    Test,
    Run,
    Lint,
    Other,
}

impl CommandKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Test => "test",
            Self::Run => "run",
            Self::Lint => "lint",
            Self::Other => "other",
        }
    }
}

pub fn run(cli: Cli) -> Result<()> {
    let cwd = std::env::current_dir()?;

    match cli.command {
        TopLevelCommand::Init { from_backup } => match from_backup {
            None => {
                let result = commands::init::run(&cwd)?;
                print_init_result(&result);
            }
            Some(backup_path) => {
                let (result, import_summary) = commands::init::run_with_backup(&cwd, &backup_path)?;
                print_init_result(&result);
                println!("Restored from {}", import_summary.source.display());
                println!("  facts: {}", import_summary.facts);
                println!("  decisions: {}", import_summary.decisions);
                println!("  tasks: {}", import_summary.tasks);
                println!("  commands: {}", import_summary.commands);
                println!("  pending writes: {}", import_summary.pending_writes);
                println!("  writes log entries: {}", import_summary.writes_log);
            }
        },
        TopLevelCommand::Status => {
            let summary = commands::status::run(&cwd)?;
            println!("Project: {}", summary.project_name);
            println!("Repo root: {}", summary.repo_root.display());
            println!("Database: {}", summary.db_path.display());
            println!("Config: {}", summary.config_path.display());
            println!("Schema version: {}", summary.schema_version);
            println!("Facts: {} ({} stale)", summary.facts, summary.stale_facts);
            println!("Decisions: {}", summary.decisions);
            println!(
                "Tasks: {} total / {} open",
                summary.tasks_total, summary.tasks_open
            );
            println!("Commands: {}", summary.commands);
            println!("Commits: {}", summary.commits);
            println!("Files: {}", summary.files);
            println!("Search chunks: {}", summary.chunks);
            println!("Pending writes: {}", summary.pending_writes);
            println!("Writes logged: {}", summary.writes_logged);
            println!("Deny patterns: {}", summary.deny_patterns);
            println!(
                "K9 detected: {}",
                if summary.k9_detected { "yes" } else { "no" }
            );
            println!(
                "K9 integration: {} (agent_docs_path: {})",
                if summary.k9_enabled {
                    "enabled"
                } else {
                    "disabled"
                },
                summary.k9_agent_docs_path
            );
            if let Some(drift) = &summary.k9_drift {
                println!("  note: {drift}");
            }
        }
        TopLevelCommand::Stats {
            window,
            json: as_json,
        } => {
            let summary = commands::stats::run(&cwd, window.to_window())?;
            if as_json {
                print_stats_json(&summary);
            } else {
                print_stats_human(&summary);
            }
        }
        TopLevelCommand::SyncMd => {
            let result = commands::sync_md::run(&cwd)?;
            if result.updated_files.is_empty() {
                println!("Managed markdown is already up to date.");
            } else {
                println!("Updated managed markdown:");
                for path in result.updated_files {
                    println!("  {}", path.display());
                }
                if !result.backup_files.is_empty() {
                    println!("Backups created:");
                    for path in result.backup_files {
                        println!("  {}", path.display());
                    }
                }
            }
        }
        TopLevelCommand::Serve => {
            crate::mcp::serve(&cwd)?;
        }
        TopLevelCommand::IngestGit { since } => {
            let summary = commands::ingest_git::run(&cwd, since.as_deref())?;
            println!(
                "Git ingestion complete for {}",
                summary.since.as_deref().unwrap_or("entire history")
            );
            println!("Commits processed: {}", summary.commits_seen);
            println!("Unique files touched: {}", summary.unique_files_seen);
            println!(
                "Commit-file links recorded: {}",
                summary.commit_file_links_seen
            );
            println!("Denied files skipped: {}", summary.denied_files_skipped);
        }
        TopLevelCommand::Search { query, limit } => {
            let response = commands::search::run(&cwd, &query, limit)?;
            println!("Matcher: {}", response.matcher);

            if response.results.is_empty() {
                println!("No matches for '{}'.", response.query);
            } else {
                match &response.results[0] {
                    crate::models::SearchResult::FileHistory(_) => {
                        for result in response.results {
                            if let crate::models::SearchResult::FileHistory(hit) = result {
                                println!(
                                    "{} {} {} {}\n  {}",
                                    hit.committed_at,
                                    hit.change_type,
                                    hit.commit_sha,
                                    hit.path,
                                    hit.message
                                );
                            }
                        }
                    }
                    crate::models::SearchResult::Decision(_) => {
                        for result in response.results {
                            if let crate::models::SearchResult::Decision(hit) = result {
                                println!(
                                    "[{}] {} (score: {:.3}, decided: {})\n  {}",
                                    hit.decision_id,
                                    hit.title,
                                    hit.score,
                                    hit.decided_at,
                                    hit.rationale
                                );
                            }
                        }
                    }
                }
            }
        }
        TopLevelCommand::Fact { command } => match command {
            FactCommand::Add {
                key,
                value,
                source,
                json: as_json,
                actor,
            } => {
                let actor = resolve_actor(actor.as_deref())?;
                let (id, created) = commands::fact::add(&cwd, &key, &value, &source, &actor)?;
                if as_json {
                    let payload = json!({
                        "id": id,
                        "key": key,
                        "value": value,
                        "source": source,
                        "created": created,
                    });
                    println!("{payload}");
                } else {
                    println!(
                        "{} fact {id}: {key}",
                        if created { "Created" } else { "Updated" }
                    );
                }
            }
            FactCommand::List => {
                let facts = commands::fact::list(&cwd)?;
                if facts.is_empty() {
                    println!("No facts recorded.");
                } else {
                    for fact in facts {
                        let verified_at = fact.verified_at.unwrap_or_else(|| "n/a".to_string());
                        let stale_marker = if fact.is_stale { " [stale]" } else { "" };
                        println!(
                            "[{}] {} = {}{} (source: {}, confidence: {:.2}, verified: {}, created: {})",
                            fact.id,
                            fact.key,
                            fact.value,
                            stale_marker,
                            fact.source,
                            fact.confidence,
                            verified_at,
                            fact.created_at
                        );
                    }
                }
            }
        },
        TopLevelCommand::Decision { command } => match command {
            DecisionCommand::Add {
                title,
                rationale,
                json: as_json,
                actor,
            } => {
                let actor = resolve_actor(actor.as_deref())?;
                let id = commands::decision::add(&cwd, &title, &rationale, &actor)?;
                if as_json {
                    let payload = json!({
                        "id": id,
                        "title": title,
                    });
                    println!("{payload}");
                } else {
                    println!("Created decision {id}: {title}");
                }
            }
            DecisionCommand::List => {
                let decisions = commands::decision::list(&cwd)?;
                if decisions.is_empty() {
                    println!("No decisions recorded.");
                } else {
                    for decision in decisions {
                        println!(
                            "[{}] {} [{}] at {}\n  rationale: {}",
                            decision.id,
                            decision.title,
                            decision.status,
                            decision.decided_at,
                            decision.rationale
                        );
                    }
                }
            }
        },
        TopLevelCommand::Task { command } => match command {
            TaskCommand::Add {
                title,
                notes,
                json: as_json,
                actor,
            } => {
                let actor = resolve_actor(actor.as_deref())?;
                let id = commands::task::add(&cwd, &title, notes.as_deref(), &actor)?;
                if as_json {
                    let payload = json!({
                        "id": id,
                        "title": title,
                    });
                    println!("{payload}");
                } else {
                    println!("Created task {id}: {title}");
                }
            }
            TaskCommand::List { status } => {
                let tasks = commands::task::list(&cwd, status.as_ref().map(TaskStatus::as_str))?;
                if tasks.is_empty() {
                    println!("No tasks recorded.");
                } else {
                    for task in tasks {
                        let notes = task.notes.unwrap_or_default();
                        println!(
                            "[{}] {} [{}] created: {} updated: {}\n  notes: {}",
                            task.id,
                            task.title,
                            task.status,
                            task.created_at,
                            task.updated_at,
                            if notes.is_empty() { "(none)" } else { &notes }
                        );
                    }
                }
            }
            TaskCommand::Done {
                id,
                json: as_json,
                actor,
            } => {
                let actor = resolve_actor(actor.as_deref())?;
                commands::task::done(&cwd, id, &actor)?;
                if as_json {
                    let payload = json!({
                        "id": id,
                        "status": "done",
                    });
                    println!("{payload}");
                } else {
                    println!("Marked task {id} as done.");
                }
            }
        },
        TopLevelCommand::Export { path } => {
            let summary = commands::export::run(&cwd, &path)?;
            println!(
                "Exported memhub project to {}",
                summary.destination.display()
            );
            println!("  facts: {}", summary.facts);
            println!("  decisions: {}", summary.decisions);
            println!("  tasks: {}", summary.tasks);
            println!("  commands: {}", summary.commands);
            println!("  pending writes: {}", summary.pending_writes);
            println!("  writes log entries: {}", summary.writes_log);
        }
        TopLevelCommand::Import { path, force } => {
            let summary = commands::import::run(&cwd, &path, force)?;
            println!(
                "Imported memhub project from {}{}",
                summary.source.display(),
                if summary.forced { " (forced)" } else { "" }
            );
            println!("Target: {}", summary.target_root.display());
            println!("  facts: {}", summary.facts);
            println!("  decisions: {}", summary.decisions);
            println!("  tasks: {}", summary.tasks);
            println!("  commands: {}", summary.commands);
            println!("  pending writes: {}", summary.pending_writes);
            println!("  writes log entries: {}", summary.writes_log);
        }
        TopLevelCommand::Command { command } => match command {
            CommandCommand::List => {
                let commands = commands::command::list(&cwd)?;
                if commands.is_empty() {
                    println!("No commands recorded yet.");
                } else {
                    for command in commands {
                        let confidence = command
                            .confidence()
                            .map(|value| format!("{value:.2}"))
                            .unwrap_or_else(|| "n/a".to_string());
                        println!(
                            "[{}] {} => {} (last_exit: {}, last_run: {}, success: {}, fail: {}, confidence: {})",
                            command.id,
                            command.kind,
                            command.cmdline,
                            command
                                .last_exit_code
                                .map(|value| value.to_string())
                                .unwrap_or_else(|| "n/a".to_string()),
                            command.last_run_at.unwrap_or_else(|| "n/a".to_string()),
                            command.success_count,
                            command.fail_count,
                            confidence
                        );
                    }
                }
            }
            CommandCommand::Verify {
                kind,
                cmdline,
                exit_code,
            } => {
                let kind_name = kind.as_str();
                let (id, created) =
                    commands::command::verify(&cwd, kind_name, &cmdline, exit_code)?;
                if exit_code == 0 {
                    println!(
                        "{} command {id}: {} => {}",
                        if created { "Verified" } else { "Updated" },
                        kind_name,
                        cmdline
                    );
                } else {
                    println!(
                        "{} failed command {id}: {} => {} (exit: {})",
                        if created { "Recorded" } else { "Updated" },
                        kind_name,
                        cmdline,
                        exit_code
                    );
                }
            }
        },
        TopLevelCommand::Review { command } => match command {
            ReviewCommand::List {
                status,
                limit,
                json: as_json,
            } => {
                let rows = commands::review::list(&cwd, status.as_filter(), limit)?;
                if as_json {
                    let payload = json!({
                        "status": status.as_filter(),
                        "pending_writes": rows
                            .iter()
                            .map(pending_write_record_to_json)
                            .collect::<Vec<_>>(),
                    });
                    println!("{payload}");
                } else if rows.is_empty() {
                    println!("No pending writes match this filter.");
                } else {
                    for row in rows {
                        println!(
                            "[{}] kind={} status={} actor={} created={}{}",
                            row.id,
                            row.kind,
                            row.status,
                            row.actor,
                            row.created_at,
                            row.reviewed_at
                                .as_deref()
                                .map(|ts| format!(" reviewed={ts}"))
                                .unwrap_or_default(),
                        );
                        println!("  rationale: {}", row.rationale);
                        println!("  payload: {}", row.payload_json);
                    }
                }
            }
            ReviewCommand::Show { id, json: as_json } => {
                let row = commands::review::show(&cwd, id)?;
                if as_json {
                    let payload = pending_write_record_to_json(&row);
                    println!("{payload}");
                } else {
                    println!("[{}] kind={} status={}", row.id, row.kind, row.status);
                    println!("Actor: {} (raw: {})", row.actor, row.actor_raw);
                    println!("Created: {}", row.created_at);
                    if let Some(reviewed) = row.reviewed_at {
                        println!("Reviewed: {reviewed}");
                    }
                    println!("Rationale: {}", row.rationale);
                    println!("Payload: {}", row.payload_json);
                    println!("Provenance: {}", row.provenance_json);
                }
            }
            ReviewCommand::Accept {
                id,
                json: as_json,
                actor,
            } => {
                let actor = resolve_actor(actor.as_deref())?;
                let outcome = commands::review::accept(&cwd, id, &actor)?;
                if as_json {
                    let payload = json!({
                        "pending_id": outcome.pending_id,
                        "kind": outcome.kind,
                        "durable_table": outcome.durable_table,
                        "durable_id": outcome.durable_id,
                    });
                    println!("{payload}");
                } else {
                    println!(
                        "Accepted pending write {} ({}) -> {} row {}",
                        outcome.pending_id, outcome.kind, outcome.durable_table, outcome.durable_id
                    );
                }
            }
            ReviewCommand::Reject {
                id,
                reason,
                json: as_json,
                actor,
            } => {
                let actor = resolve_actor(actor.as_deref())?;
                commands::review::reject(&cwd, id, reason.as_deref(), &actor)?;
                if as_json {
                    let payload = json!({
                        "pending_id": id,
                    });
                    println!("{payload}");
                } else {
                    println!("Rejected pending write {id}");
                }
            }
            ReviewCommand::Expire { older_than_days } => {
                let summary = commands::review::expire(&cwd, older_than_days)?;
                println!(
                    "Expired {} pending write(s) older than {} day(s)",
                    summary.expired, summary.older_than_days
                );
            }
        },
        TopLevelCommand::Integrations { command } => match command {
            IntegrationsCommand::Status => {
                let status = commands::integrations::status(&cwd)?;
                println!(
                    "K9: detected={}, enabled={}, agent_docs_path={}",
                    if status.k9.detected { "yes" } else { "no" },
                    if status.k9.enabled { "yes" } else { "no" },
                    status.k9.agent_docs_path
                );
                if let Some(drift) = status.k9.drift {
                    println!("  note: {drift}");
                }
            }
            IntegrationsCommand::EnableK9 {
                agent_docs_path,
                force,
            } => {
                commands::integrations::enable_k9(&cwd, agent_docs_path.as_deref(), force)?;
                println!("K9 integration enabled.");
            }
            IntegrationsCommand::DisableK9 => {
                commands::integrations::disable_k9(&cwd)?;
                println!("K9 integration disabled.");
            }
            IntegrationsCommand::CheckK9 => {
                let enabled = commands::integrations::check_k9(&cwd);
                process::exit(if enabled { 0 } else { 1 });
            }
        },
        TopLevelCommand::Note { command } => match command {
            NoteCommand::List {
                limit,
                actor,
                since_days,
                json: as_json,
            } => {
                let rows = commands::session_note::list(&cwd, limit, actor.as_deref(), since_days)?;
                if as_json {
                    let payload = json!({
                        "session_notes": rows
                            .iter()
                            .map(|n| json!({
                                "id": n.id,
                                "actor": n.actor,
                                "actor_raw": n.actor_raw,
                                "text": n.text,
                                "created_at": n.created_at,
                            }))
                            .collect::<Vec<_>>(),
                    });
                    println!("{payload}");
                } else if rows.is_empty() {
                    println!("No session notes match this filter.");
                } else {
                    for note in rows {
                        println!(
                            "[{}] {} actor={} (raw: {})",
                            note.id, note.created_at, note.actor, note.actor_raw
                        );
                        println!("  {}", note.text);
                    }
                }
            }
        },
    }

    Ok(())
}
