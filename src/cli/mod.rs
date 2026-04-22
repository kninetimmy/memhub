use clap::{Parser, Subcommand, ValueEnum};

use crate::Result;
use crate::commands;

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
    Init,
    Status,
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
}

#[derive(Debug, Subcommand)]
pub enum FactCommand {
    Add {
        key: String,
        value: String,
        #[arg(long, default_value = "user")]
        source: String,
    },
    List,
}

#[derive(Debug, Subcommand)]
pub enum DecisionCommand {
    Add {
        title: String,
        #[arg(long)]
        rationale: String,
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
    },
    List {
        #[arg(long, value_enum)]
        status: Option<TaskStatus>,
    },
    Done {
        id: i64,
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
        TopLevelCommand::Init => {
            let result = commands::init::run(&cwd)?;
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
        TopLevelCommand::Status => {
            let summary = commands::status::run(&cwd)?;
            println!("Project: {}", summary.project_name);
            println!("Repo root: {}", summary.repo_root.display());
            println!("Database: {}", summary.db_path.display());
            println!("Config: {}", summary.config_path.display());
            println!("Schema version: {}", summary.schema_version);
            println!("Facts: {}", summary.facts);
            println!("Decisions: {}", summary.decisions);
            println!(
                "Tasks: {} total / {} open",
                summary.tasks_total, summary.tasks_open
            );
            println!("Commands: {}", summary.commands);
            println!("Commits: {}", summary.commits);
            println!("Files: {}", summary.files);
            println!("Search chunks: {}", summary.chunks);
            println!("Writes logged: {}", summary.writes_logged);
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
            FactCommand::Add { key, value, source } => {
                let (id, created) = commands::fact::add(&cwd, &key, &value, &source)?;
                println!(
                    "{} fact {id}: {key}",
                    if created { "Created" } else { "Updated" }
                );
            }
            FactCommand::List => {
                let facts = commands::fact::list(&cwd)?;
                if facts.is_empty() {
                    println!("No facts recorded.");
                } else {
                    for fact in facts {
                        let verified_at = fact.verified_at.unwrap_or_else(|| "n/a".to_string());
                        println!(
                            "[{}] {} = {} (source: {}, confidence: {:.2}, verified: {}, created: {})",
                            fact.id,
                            fact.key,
                            fact.value,
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
            DecisionCommand::Add { title, rationale } => {
                let id = commands::decision::add(&cwd, &title, &rationale)?;
                println!("Created decision {id}: {title}");
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
            TaskCommand::Add { title, notes } => {
                let id = commands::task::add(&cwd, &title, notes.as_deref())?;
                println!("Created task {id}: {title}");
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
            TaskCommand::Done { id } => {
                commands::task::done(&cwd, id)?;
                println!("Marked task {id} as done.");
            }
        },
        TopLevelCommand::Command { command } => match command {
            CommandCommand::List => {
                let commands = commands::command::list(&cwd)?;
                if commands.is_empty() {
                    println!("No commands recorded yet.");
                } else {
                    for command in commands {
                        println!(
                            "[{}] {} => {} (last_exit: {}, last_run: {}, success: {}, fail: {})",
                            command.id,
                            command.kind,
                            command.cmdline,
                            command
                                .last_exit_code
                                .map(|value| value.to_string())
                                .unwrap_or_else(|| "n/a".to_string()),
                            command.last_run_at.unwrap_or_else(|| "n/a".to_string()),
                            command.success_count,
                            command.fail_count
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
    }

    Ok(())
}
