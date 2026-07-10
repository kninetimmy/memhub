use std::fs;
use std::path::PathBuf;
use std::process;

mod args;
mod output;

#[cfg(feature = "metrics")]
pub use args::MetricsCommand;
pub use args::{
    AuditCommand, Cli, CodeCommand, CommandCommand, CommandKind, DecisionCommand, DocCommand,
    EvalCommand, FactCommand, GlobalCommand, IndexCommand, IntegrationsCommand, NarrativeCommand,
    NoteCommand, PendingStatus, RecallModeArg, RecallSourceTypeArg, ReviewCommand, StatsWindowArg,
    SyncCommand, TaskCommand, TaskStatus, TopLevelCommand, TranscriptAgentArg, TranscriptCommand,
};
use output::{
    audit_md_report_to_json, code_status_to_json, doctor_report_to_json, eval_summary_to_json,
    import_summary_to_json, index_status_to_json, init_result_to_json, locate_eval_summary_to_json,
    locate_response_to_json, narrative_entry_to_json,
    pending_write_record_to_json, print_audit_md_report_human, print_code_status,
    print_doctor_report_human, print_eval_summary, print_index_status, print_init_result,
    print_locate, print_locate_eval_summary, print_recall_human,
    print_review_stale_report_human, print_stats_human, print_stats_json,
    print_status_checks_human, print_wrapup_policy_human, recall_response_to_json,
    review_stale_report_to_json, status_checks_to_json, status_summary_to_json,
    wrapup_policy_report_to_json,
};
#[cfg(feature = "metrics")]
use output::{metrics_status_to_json, print_metrics_status_human};
use serde_json::json;

use crate::commands;
use crate::commands::{DEFAULT_ACTOR, validate_actor};
use crate::models::NarrativeKind;
use crate::retrieval;
use crate::retrieval::{RecallOptions, RecallSurface};
use crate::{MemhubError, Result};

/// One-time disclosure printed the first time a global write creates
/// `~/.memhub/global.sqlite`. The store is machine-wide and visible to
/// every repo on this machine that opts in.
fn print_global_store_created() {
    println!(
        "Created machine-global store at ~/.memhub/global.sqlite — \
         visible to recall in every repo on this machine that runs \
         `memhub global enable`."
    );
}

/// Approval gate for `memhub transcript archive` (issue #96). The archive
/// is UNREDACTED, so this fails closed: `--yes` pre-authorizes;
/// otherwise, on an interactive terminal we prompt, and on a non-TTY we
/// refuse without archiving. Mirrors the `upgrade`/`sync adopt` gate.
fn transcript_archive_approved(yes: bool) -> bool {
    use std::io::{IsTerminal, Write};
    if yes {
        return true;
    }
    if !std::io::stdin().is_terminal() {
        return false;
    }
    print!("Archive this UNREDACTED transcript? [y/N] ");
    let _ = std::io::stdout().flush();
    let mut line = String::new();
    if std::io::stdin().read_line(&mut line).is_err() {
        return false;
    }
    matches!(line.trim().to_ascii_lowercase().as_str(), "y" | "yes")
}

/// `promote` only supports the machine-global target in M9. Reject a
/// missing `--global` with a clear message rather than a silent no-op.
fn require_global_target(global: bool) -> Result<()> {
    if global {
        Ok(())
    } else {
        Err(MemhubError::InvalidInput(
            "promote requires --global (the only promotion target in M9)".to_string(),
        ))
    }
}

fn resolve_actor(actor: Option<&str>) -> Result<String> {
    match actor {
        Some(value) => {
            validate_actor(value)?;
            Ok(value.trim().to_string())
        }
        None => Ok(DEFAULT_ACTOR.to_string()),
    }
}

fn run_narrative(
    cwd: &std::path::Path,
    kind: NarrativeKind,
    command: NarrativeCommand,
) -> Result<()> {
    match command {
        NarrativeCommand::Set {
            body,
            from_file,
            json: as_json,
            actor,
        } => {
            let body_text = resolve_narrative_body(kind, body, from_file)?;
            let actor = resolve_actor(actor.as_deref())?;
            let entry = commands::narrative::set(cwd, kind, &body_text, &actor, &actor)?;
            if as_json {
                println!("{}", narrative_entry_to_json(kind, &entry));
            } else {
                println!(
                    "Recorded {} entry {} ({} chars) at {}",
                    kind.as_str(),
                    entry.id,
                    entry.body.chars().count(),
                    entry.created_at
                );
            }
        }
        NarrativeCommand::Show { json: as_json } => {
            let maybe_entry = commands::narrative::show(cwd, kind)?;
            if as_json {
                let payload = match &maybe_entry {
                    Some(entry) => narrative_entry_to_json(kind, entry),
                    None => json!({ "kind": kind.as_str(), "entry": null }),
                };
                println!("{payload}");
            } else {
                match maybe_entry {
                    Some(entry) => {
                        println!(
                            "[{}] {} (actor: {}, created: {})",
                            entry.id,
                            kind.as_str(),
                            entry.actor,
                            entry.created_at
                        );
                        println!();
                        println!("{}", entry.body);
                    }
                    None => println!("No {} entries recorded.", kind.as_str()),
                }
            }
        }
        NarrativeCommand::History {
            limit,
            json: as_json,
        } => {
            let entries = commands::narrative::history(cwd, kind, limit)?;
            if as_json {
                let payload = json!({
                    "kind": kind.as_str(),
                    "entries": entries
                        .iter()
                        .map(|e| narrative_entry_to_json(kind, e))
                        .collect::<Vec<_>>(),
                });
                println!("{payload}");
            } else if entries.is_empty() {
                println!("No {} entries recorded.", kind.as_str());
            } else {
                for entry in entries {
                    println!(
                        "[{}] {} actor={} ({} chars)",
                        entry.id,
                        entry.created_at,
                        entry.actor,
                        entry.body.chars().count()
                    );
                }
            }
        }
    }
    Ok(())
}

fn resolve_narrative_body(
    kind: NarrativeKind,
    body: Option<String>,
    from_file: Option<PathBuf>,
) -> Result<String> {
    resolve_text_input(&format!("{} set", kind.as_str()), body, from_file)
}

fn resolve_text_input(
    label: &str,
    text: Option<String>,
    from_file: Option<PathBuf>,
) -> Result<String> {
    match (text, from_file) {
        (Some(_), Some(_)) => Err(MemhubError::InvalidInput(format!(
            "{label}: pass either a text argument or --from-file, not both"
        ))),
        (Some(s), None) => Ok(s),
        (None, Some(path)) => fs::read_to_string(&path).map_err(MemhubError::from),
        (None, None) => Err(MemhubError::InvalidInput(format!(
            "{label}: provide a text argument or --from-file <path>"
        ))),
    }
}

pub fn run(cli: Cli) -> Result<()> {
    let cwd = std::env::current_dir()?;

    match cli.command {
        TopLevelCommand::Init {
            from_backup,
            json: as_json,
        } => match from_backup {
            None => {
                let result = commands::init::run(&cwd)?;
                if as_json {
                    println!("{}", json!({ "init": init_result_to_json(&result) }));
                } else {
                    print_init_result(&result);
                }
            }
            Some(backup_path) => {
                let (result, import_summary) = commands::init::run_with_backup(&cwd, &backup_path)?;
                if as_json {
                    let mut init_json = init_result_to_json(&result);
                    init_json["restored_from"] = import_summary_to_json(&import_summary);
                    println!("{}", json!({ "init": init_json }));
                } else {
                    print_init_result(&result);
                    println!("Restored from {}", import_summary.source.display());
                    println!("  facts: {}", import_summary.facts);
                    println!("  decisions: {}", import_summary.decisions);
                    println!("  tasks: {}", import_summary.tasks);
                    println!("  commands: {}", import_summary.commands);
                    println!("  pending writes: {}", import_summary.pending_writes);
                    println!("  writes log entries: {}", import_summary.writes_log);
                }
            }
        },
        TopLevelCommand::Status { json: as_json } => {
            let summary = commands::status::run(&cwd)?;
            // Cheap subsystem-state checks (issue #22) — a subset of
            // `doctor`'s own checks, reused rather than duplicated.
            let checks = commands::status::checks(&cwd)?;
            if as_json {
                let mut status_json = status_summary_to_json(&summary);
                status_json["checks"] = status_checks_to_json(&checks);
                println!("{}", json!({ "status": status_json }));
            } else {
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
                // Wave 3 L4 (issue #47): one-line pointer at the
                // read-only `memhub review stale` audit queue.
                println!("Stale queue: {}", summary.stale_queue);
                // Subsystem-state lines (issue #22): schema, render
                // freshness, retrieval mode, embeddings, sync (when
                // enabled), metrics (when enabled), and K9 — K9 only
                // appears here when `check_k9_coexistence` finds
                // something to report, replacing the old unconditional
                // "K9 detected: no" / "K9 integration: disabled" lines
                // that sprayed on every clean, non-K9 repo.
                print_status_checks_human(&checks);
            }
        }
        TopLevelCommand::Doctor {
            json: as_json,
            strict,
        } => {
            let report = commands::doctor::run(&cwd, strict)?;
            if as_json {
                println!("{}", json!({ "doctor": doctor_report_to_json(&report) }));
            } else {
                print_doctor_report_human(&report);
            }
            process::exit(report.exit_code);
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
                println!("Rendered markdown is already up to date.");
            } else {
                println!("Updated rendered markdown:");
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
        #[cfg(feature = "viz")]
        TopLevelCommand::Viz { host, port, open } => {
            run_viz(&cwd, host, port, open)?;
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
                kind,
                global,
                json: as_json,
                actor,
            } => {
                let actor = resolve_actor(actor.as_deref())?;
                if global {
                    let r = commands::fact::add_global_with_kind(
                        &cwd,
                        &key,
                        &value,
                        kind.as_deref(),
                        &source,
                        &actor,
                    )?;
                    if as_json {
                        println!(
                            "{}",
                            json!({
                                "id": r.id,
                                "key": key,
                                "value": value,
                                "source": source,
                                "kind": kind,
                                "created": r.created,
                                "scope": "global",
                                "store_created": r.store_created,
                            })
                        );
                    } else {
                        if r.store_created {
                            print_global_store_created();
                        }
                        println!(
                            "{} global fact {}: {key}",
                            if r.created { "Created" } else { "Updated" },
                            r.id
                        );
                    }
                } else {
                    let (id, created) = commands::fact::add_with_kind(
                        &cwd,
                        &key,
                        &value,
                        kind.as_deref(),
                        &source,
                        &actor,
                    )?;
                    if as_json {
                        let payload = json!({
                            "id": id,
                            "key": key,
                            "value": value,
                            "source": source,
                            "kind": kind,
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
            }
            FactCommand::Promote {
                id,
                global,
                json: as_json,
                actor,
            } => {
                require_global_target(global)?;
                let actor = resolve_actor(actor.as_deref())?;
                let r = commands::fact::promote(&cwd, id, &actor)?;
                if as_json {
                    println!(
                        "{}",
                        json!({
                            "promoted_from": id,
                            "id": r.id,
                            "created": r.created,
                            "scope": "global",
                            "store_created": r.store_created,
                        })
                    );
                } else {
                    if r.store_created {
                        print_global_store_created();
                    }
                    println!(
                        "Promoted fact {id} → global fact {} ({})",
                        r.id,
                        if r.created {
                            "new"
                        } else {
                            "updated existing key"
                        }
                    );
                }
            }
            FactCommand::List { json: as_json } => {
                let facts = commands::fact::list(&cwd)?;
                if as_json {
                    let payload = json!({
                        "facts": facts
                            .iter()
                            .map(|fact| json!({
                                "id": fact.id,
                                "key": fact.key,
                                "value": fact.value,
                                "source": fact.source,
                                "verified_at": fact.verified_at,
                                "created_at": fact.created_at,
                                "is_stale": fact.is_stale,
                                "superseded_by": fact.superseded_by,
                            }))
                            .collect::<Vec<_>>(),
                    });
                    println!("{payload}");
                } else if facts.is_empty() {
                    println!("No facts recorded.");
                } else {
                    for fact in facts {
                        let verified_at = fact.verified_at.unwrap_or_else(|| "n/a".to_string());
                        let stale_marker = if fact.is_stale { " [stale]" } else { "" };
                        let superseded_marker = match fact.superseded_by {
                            Some(new_id) => format!(" [superseded by #{new_id}]"),
                            None => String::new(),
                        };
                        println!(
                            "[{}] {} = {}{}{} (source: {}, verified: {}, created: {})",
                            fact.id,
                            fact.key,
                            fact.value,
                            stale_marker,
                            superseded_marker,
                            fact.source,
                            verified_at,
                            fact.created_at
                        );
                    }
                }
            }
            FactCommand::Verify {
                ident,
                json: as_json,
                actor,
            } => {
                let actor = resolve_actor(actor.as_deref())?;
                let found = commands::fact::verify(&cwd, &ident, &actor)?;
                match found {
                    Some((id, key)) => {
                        if as_json {
                            println!(
                                "{}",
                                json!({ "verified": true, "id": id, "key": key, "ident": ident })
                            );
                        } else {
                            println!("Verified fact {id}: {key}");
                        }
                    }
                    None => {
                        if as_json {
                            println!("{}", json!({ "verified": false, "ident": ident }));
                        } else {
                            println!("No fact matched {ident}.");
                        }
                        // A miss is a failed operation: exit nonzero so a
                        // script's `memhub fact verify x && ...` doesn't
                        // proceed as though it verified something (F16
                        // convention, mirrors `doc rm`/`doc show`).
                        process::exit(1);
                    }
                }
            }
            FactCommand::Supersede {
                old,
                by,
                json: as_json,
                actor,
            } => {
                let actor = resolve_actor(actor.as_deref())?;
                let outcome = commands::fact::supersede(&cwd, &old, &by, &actor)?;
                if as_json {
                    println!(
                        "{}",
                        json!({
                            "superseded": true,
                            "old_id": outcome.old_id,
                            "old_key": outcome.old_key,
                            "new_id": outcome.new_id,
                            "new_key": outcome.new_key,
                        })
                    );
                } else {
                    println!(
                        "Superseded fact {} ({}) by fact {} ({})",
                        outcome.old_id, outcome.old_key, outcome.new_id, outcome.new_key
                    );
                }
            }
        },
        TopLevelCommand::Decision { command } => match command {
            DecisionCommand::Add {
                title,
                rationale,
                summary,
                source,
                global,
                json: as_json,
                actor,
            } => {
                let actor = resolve_actor(actor.as_deref())?;
                if global {
                    let r = commands::decision::add_global(
                        &cwd,
                        &title,
                        &rationale,
                        summary.as_deref(),
                        &source,
                        &actor,
                    )?;
                    if as_json {
                        println!(
                            "{}",
                            json!({
                                "id": r.id,
                                "title": title,
                                "source": source,
                                "summary": summary,
                                "scope": "global",
                                "store_created": r.store_created,
                            })
                        );
                    } else {
                        if r.store_created {
                            print_global_store_created();
                        }
                        println!("Created global decision {}: {title}", r.id);
                    }
                } else {
                    let id = commands::decision::add_with_decided_at(
                        &cwd,
                        &title,
                        &rationale,
                        None,
                        summary.as_deref(),
                        &source,
                        &actor,
                    )?;
                    if as_json {
                        let payload = json!({
                            "id": id,
                            "title": title,
                            "source": source,
                            "summary": summary,
                        });
                        println!("{payload}");
                    } else {
                        println!("Created decision {id}: {title}");
                    }
                }
            }
            DecisionCommand::Promote {
                id,
                global,
                json: as_json,
                actor,
            } => {
                require_global_target(global)?;
                let actor = resolve_actor(actor.as_deref())?;
                let r = commands::decision::promote(&cwd, id, &actor)?;
                if as_json {
                    println!(
                        "{}",
                        json!({
                            "promoted_from": id,
                            "id": r.id,
                            "scope": "global",
                            "store_created": r.store_created,
                            "title_collision": r.title_collision,
                        })
                    );
                } else {
                    if r.store_created {
                        print_global_store_created();
                    }
                    if r.title_collision {
                        println!(
                            "Warning: a global decision with this title already exists; \
                             inserted a duplicate (decisions have no natural key)."
                        );
                    }
                    println!("Promoted decision {id} → global decision {}", r.id);
                }
            }
            DecisionCommand::SetSummary {
                id,
                summary,
                json: as_json,
                actor,
            } => {
                let actor = resolve_actor(actor.as_deref())?;
                let summary_opt = if summary.trim().is_empty() {
                    None
                } else {
                    Some(summary.as_str())
                };
                commands::decision::set_summary(&cwd, id, summary_opt, &actor)?;
                if as_json {
                    let payload = json!({
                        "id": id,
                        "summary": summary_opt,
                    });
                    println!("{payload}");
                } else if summary_opt.is_some() {
                    println!("Set summary on decision {id}");
                } else {
                    println!("Cleared summary on decision {id}");
                }
            }
            DecisionCommand::Supersede {
                old,
                by,
                json: as_json,
                actor,
            } => {
                let actor = resolve_actor(actor.as_deref())?;
                let outcome = commands::decision::supersede(&cwd, old, by, &actor)?;
                if as_json {
                    println!(
                        "{}",
                        json!({
                            "superseded": true,
                            "old_id": outcome.old_id,
                            "old_title": outcome.old_title,
                            "new_id": outcome.new_id,
                            "new_title": outcome.new_title,
                        })
                    );
                } else {
                    println!(
                        "Superseded decision {} ({}) by decision {} ({})",
                        outcome.old_id, outcome.old_title, outcome.new_id, outcome.new_title
                    );
                }
            }
            DecisionCommand::List { json: as_json } => {
                let decisions = commands::decision::list(&cwd)?;
                if as_json {
                    let payload = json!({
                        "decisions": decisions
                            .iter()
                            .map(|decision| json!({
                                "id": decision.id,
                                "title": decision.title,
                                "rationale": decision.rationale,
                                "status": decision.status,
                                "decided_at": decision.decided_at,
                                "source": decision.source,
                                "summary": decision.summary,
                                "superseded_by": decision.superseded_by,
                            }))
                            .collect::<Vec<_>>(),
                    });
                    println!("{payload}");
                } else if decisions.is_empty() {
                    println!("No decisions recorded.");
                } else {
                    for decision in decisions {
                        let status_label = match decision.superseded_by {
                            Some(new_id) => format!("{} → D{new_id}", decision.status),
                            None => decision.status.clone(),
                        };
                        println!(
                            "[{}] {} [{}] at {} (source: {})\n  rationale: {}",
                            decision.id,
                            decision.title,
                            status_label,
                            decision.decided_at,
                            decision.source,
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
            TaskCommand::List {
                status,
                json: as_json,
            } => {
                let tasks = commands::task::list(&cwd, status.as_ref().map(TaskStatus::as_str))?;
                if as_json {
                    let payload = json!({
                        "tasks": tasks
                            .iter()
                            .map(|task| json!({
                                "id": task.id,
                                "title": task.title,
                                "status": task.status,
                                "notes": task.notes,
                                "created_at": task.created_at,
                                "updated_at": task.updated_at,
                            }))
                            .collect::<Vec<_>>(),
                    });
                    println!("{payload}");
                } else if tasks.is_empty() {
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
        TopLevelCommand::Doc { command } => match command {
            DocCommand::Add {
                file,
                title,
                global,
                json: as_json,
                actor,
            } => {
                let actor = resolve_actor(actor.as_deref())?;
                let outcome = if global {
                    commands::doc::add_global(&cwd, &file, title.as_deref(), &actor)?
                } else {
                    commands::doc::add(&cwd, &file, title.as_deref(), &actor)?
                };
                let status = match outcome.status {
                    commands::doc::IngestStatus::Created => "created",
                    commands::doc::IngestStatus::Updated => "updated",
                    commands::doc::IngestStatus::Unchanged => "unchanged",
                };
                let scope = if global { "global" } else { "repo" };
                if as_json {
                    let payload = json!({
                        "id": outcome.doc_id,
                        "title": outcome.title,
                        "path": outcome.path,
                        "chunks": outcome.chunk_count,
                        "status": status,
                        "scope": scope,
                        "enabled_default_recall": outcome.enabled_default_recall,
                        "store_created": outcome.store_created,
                    });
                    println!("{payload}");
                } else {
                    if outcome.store_created {
                        print_global_store_created();
                    }
                    println!(
                        "{} {} document {}: {} ({} chunks)\n  {}",
                        status,
                        scope,
                        outcome.doc_id,
                        outcome.title,
                        outcome.chunk_count,
                        outcome.path,
                    );
                    if outcome.enabled_default_recall {
                        println!(
                            "  Default doc recall enabled for this {} \
                             (first doc ingested) — strong topical matches now\n  \
                             surface in plain `memhub recall`; \
                             scope to docs only with --source-type doc.",
                            if global { "machine" } else { "repo" }
                        );
                    } else if outcome.status != commands::doc::IngestStatus::Unchanged {
                        println!("  Searchable via: memhub recall \"<query>\" --source-type doc");
                    }
                }
            }
            DocCommand::Ls {
                global,
                json: as_json,
            } => {
                let docs = if global {
                    commands::doc::list_global(&cwd)?
                } else {
                    commands::doc::list(&cwd)?
                };
                let scope = if global { "global" } else { "repo" };
                if as_json {
                    let payload = json!({
                        "docs": docs
                            .iter()
                            .map(|d| {
                                json!({
                                    "id": d.id,
                                    "title": d.title,
                                    "path": d.path,
                                    "chunks": d.chunk_count,
                                    "bytes": d.byte_len,
                                    "source": d.source,
                                    "ingested_at": d.ingested_at,
                                    "scope": scope,
                                })
                            })
                            .collect::<Vec<_>>(),
                    });
                    println!("{payload}");
                } else if docs.is_empty() {
                    println!("No {scope} documents ingested.");
                } else {
                    for d in docs {
                        println!(
                            "[{}] {} ({} chunks, {} bytes) ingested: {}\n  {}",
                            d.id, d.title, d.chunk_count, d.byte_len, d.ingested_at, d.path,
                        );
                    }
                }
            }
            DocCommand::Rm {
                ident,
                global,
                json: as_json,
                actor,
            } => {
                let actor = resolve_actor(actor.as_deref())?;
                let removed = if global {
                    commands::doc::remove_global(&cwd, &ident, &actor)?
                } else {
                    commands::doc::remove(&cwd, &ident, &actor)?
                };
                let scope = if global { "global" } else { "repo" };
                if as_json {
                    println!(
                        "{}",
                        json!({ "removed": removed, "ident": ident, "scope": scope })
                    );
                } else if removed {
                    println!("Removed {scope} document {ident}.");
                } else {
                    println!("No {scope} document matched {ident}.");
                }
                if !removed {
                    // A miss is a failed operation: exit nonzero so a
                    // script's `memhub doc rm x && ...` doesn't proceed as
                    // though it removed something. The JSON/text body above
                    // is still printed (F16).
                    process::exit(1);
                }
            }
            DocCommand::Show {
                ident,
                global,
                json: as_json,
            } => {
                let scope = if global { "global" } else { "repo" };
                let found = if global {
                    commands::doc::show_global(&cwd, &ident)?
                } else {
                    commands::doc::show(&cwd, &ident)?
                };
                match found {
                    None => {
                        if as_json {
                            println!(
                                "{}",
                                json!({ "found": false, "ident": ident, "scope": scope })
                            );
                        } else {
                            println!("No {scope} document matched {ident}.");
                        }
                        // A miss is a failed lookup: exit nonzero (the
                        // `{"found": false}` body is still printed) so it
                        // is not mistaken for a successful show (F16).
                        process::exit(1);
                    }
                    Some((meta, chunks)) => {
                        if as_json {
                            let payload = json!({
                                "id": meta.id,
                                "title": meta.title,
                                "path": meta.path,
                                "bytes": meta.byte_len,
                                "ingested_at": meta.ingested_at,
                                "scope": scope,
                                "chunks": chunks.iter().map(|c| json!({
                                    "id": c.id,
                                    "ord": c.ord,
                                    "heading_path": c.heading_path,
                                    "body": c.body,
                                })).collect::<Vec<_>>(),
                            });
                            println!("{payload}");
                        } else {
                            println!(
                                "[{}] {} ({} chunks)\n  {}",
                                meta.id,
                                meta.title,
                                chunks.len(),
                                meta.path,
                            );
                            for c in chunks {
                                let head = if c.heading_path.is_empty() {
                                    "(preamble)"
                                } else {
                                    &c.heading_path
                                };
                                println!("  #{} {}", c.ord, head);
                            }
                        }
                    }
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
            println!("  session notes: {}", summary.session_notes);
            println!("  project state entries: {}", summary.project_state);
            println!("  project arch entries: {}", summary.project_arch);
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
            println!("  session notes: {}", summary.session_notes);
            println!("  project state entries: {}", summary.project_state);
            println!("  project arch entries: {}", summary.project_arch);
            if summary.retained_doc_chunks > 0 {
                println!(
                    "  retained doc chunks: {} (pre-existing ingested docs; \
                     import does not carry or wipe docs — re-run `memhub doc add` to refresh)",
                    summary.retained_doc_chunks
                );
            }
            println!();
            println!("Next steps:");
            println!(
                "  Embeddings for imported rows are not yet built. Run `memhub index rebuild`"
            );
            println!("  to enable vector recall on this machine.");
        }
        TopLevelCommand::Command { command } => match command {
            CommandCommand::List { json: as_json } => {
                let commands = commands::command::list(&cwd)?;
                if as_json {
                    let payload = json!({
                        "commands": commands
                            .iter()
                            .map(|command| json!({
                                "id": command.id,
                                "kind": command.kind,
                                "cmdline": command.cmdline,
                                "last_exit_code": command.last_exit_code,
                                "last_run_at": command.last_run_at,
                                "success_count": command.success_count,
                                "fail_count": command.fail_count,
                                "confidence": command.confidence(),
                            }))
                            .collect::<Vec<_>>(),
                    });
                    println!("{payload}");
                } else if commands.is_empty() {
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
                    commands::command::verify(&cwd, kind_name, &cmdline, exit_code, DEFAULT_ACTOR)?;
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
                supersede,
                force,
            } => {
                let actor = resolve_actor(actor.as_deref())?;
                let outcome =
                    commands::review::accept(&cwd, id, &actor, supersede.as_deref(), force)?;
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
            ReviewCommand::Stale { json: as_json } => {
                let report = commands::review::stale(&cwd)?;
                if as_json {
                    println!("{}", json!({ "review_stale": review_stale_report_to_json(&report) }));
                } else {
                    print_review_stale_report_human(&report);
                }
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
            IntegrationsCommand::BootstrapK9 {
                dry_run,
                json: as_json,
            } => {
                let summary = commands::bootstrap_k9::run(&cwd, dry_run)?;
                if as_json {
                    let payload = json!({
                        "dry_run": summary.dry_run,
                        "agent_docs_path": summary.agent_docs_path.display().to_string(),
                        "decisions_imported": summary.decisions.len(),
                        "tasks_imported": summary.tasks.len(),
                        "tasks_skipped_completed": summary.tasks_skipped_completed,
                        "files_read": summary.files_read.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                        "files_missing": summary.files_missing.iter().map(|p| p.display().to_string()).collect::<Vec<_>>(),
                        "actor": commands::bootstrap_k9::BOOTSTRAP_ACTOR,
                    });
                    println!("{payload}");
                } else {
                    println!(
                        "Bootstrap from K9 at {} ({})",
                        summary.agent_docs_path.display(),
                        if summary.dry_run {
                            "dry run"
                        } else {
                            "applied"
                        }
                    );
                    println!(
                        "  decisions: {} | tasks: {} (skipped completed: {})",
                        summary.decisions.len(),
                        summary.tasks.len(),
                        summary.tasks_skipped_completed
                    );
                    for p in &summary.files_read {
                        println!("  read: {}", p.display());
                    }
                    for p in &summary.files_missing {
                        println!("  missing: {}", p.display());
                    }
                }
            }
        },
        TopLevelCommand::State { command } => {
            run_narrative(&cwd, NarrativeKind::State, command)?;
        }
        TopLevelCommand::Arch { command } => {
            run_narrative(&cwd, NarrativeKind::Arch, command)?;
        }
        TopLevelCommand::Index { command } => match command {
            IndexCommand::Status { json: as_json } => {
                let summary = commands::index::status(&cwd)?;
                if as_json {
                    println!("{}", index_status_to_json(&summary));
                } else {
                    print_index_status(&summary);
                }
            }
            IndexCommand::Rebuild {
                json: as_json,
                actor,
            } => {
                let actor = resolve_actor(actor.as_deref())?;
                let summary = commands::index::rebuild(&cwd, &actor)?;
                if as_json {
                    let payload = json!({
                        "model": summary.model,
                        "facts": summary.facts,
                        "decisions": summary.decisions,
                        "tasks": summary.tasks,
                        "doc_chunks": summary.doc_chunks,
                        "deleted": summary.deleted,
                        "elapsed_ms": summary.elapsed_ms,
                    });
                    println!("{payload}");
                } else {
                    println!(
                        "Rebuilt embeddings for model {} in {} ms (deleted {})",
                        summary.model, summary.elapsed_ms, summary.deleted,
                    );
                    println!(
                        "  facts: {}  decisions: {}  tasks: {}  doc chunks: {}",
                        summary.facts, summary.decisions, summary.tasks, summary.doc_chunks,
                    );
                }
            }
        },
        #[cfg(feature = "metrics")]
        TopLevelCommand::Metrics { command } => match command {
            MetricsCommand::Status { json: as_json } => {
                let s = commands::metrics::status(&cwd)?;
                if as_json {
                    println!("{}", metrics_status_to_json(&s));
                } else {
                    print_metrics_status_human(&s);
                }
            }
            MetricsCommand::Enable { json: as_json } => {
                let r = commands::metrics::enable(&cwd)?;
                if as_json {
                    let payload = json!({
                        "already_enabled": r.already_enabled,
                        "enabled": r.config.enabled,
                        "claude_transcripts_dir": r.config.claude_transcripts_dir,
                        "auto_detected": r.auto_detected_dir.is_some(),
                        "auto_detected_dir": r.auto_detected_dir,
                    });
                    println!("{payload}");
                } else if r.already_enabled && r.auto_detected_dir.is_none() {
                    println!("Metrics already enabled.");
                } else {
                    println!("Metrics enabled.");
                    if let Some(dir) = &r.auto_detected_dir {
                        println!("Claude transcripts dir auto-detected: {dir}");
                    } else if r.config.claude_transcripts_dir.is_empty() {
                        println!(
                            "Note: claude_transcripts_dir not detected. Set manually in \
                             .memhub/config.toml under [metrics] if session accounting is desired."
                        );
                    } else {
                        println!(
                            "Claude transcripts dir: {}",
                            r.config.claude_transcripts_dir
                        );
                    }
                }
            }
            MetricsCommand::Disable { json: as_json } => {
                commands::metrics::disable(&cwd)?;
                if as_json {
                    println!("{}", json!({ "enabled": false }));
                } else {
                    println!("Metrics disabled.");
                }
            }
            MetricsCommand::Rescan { json: as_json } => {
                let r = commands::metrics::rescan(&cwd)?;
                if as_json {
                    let payload = json!({
                        "recalls_attributed": r.recalls_attributed,
                        "recalls_pruned": r.recalls_pruned,
                        "sessions_pruned": r.sessions_pruned,
                        "recall_rows": r.recall_rows,
                        "session_rows": r.session_rows,
                        "attributed_rows": r.attributed_rows,
                    });
                    println!("{payload}");
                } else {
                    println!("Rescan complete.");
                    println!("  Recalls attributed: {}", r.recalls_attributed);
                    println!("  Recalls pruned:     {}", r.recalls_pruned);
                    println!("  Sessions pruned:    {}", r.sessions_pruned);
                    println!("  Total recall rows:  {}", r.recall_rows);
                    println!("  Total sessions:     {}", r.session_rows);
                    println!("  Attributed rows:    {}", r.attributed_rows);
                }
            }
            MetricsCommand::Prune { json: as_json } => {
                let r = commands::metrics::prune(&cwd)?;
                if as_json {
                    let payload = json!({
                        "recalls_pruned": r.recalls_pruned,
                        "sessions_pruned": r.sessions_pruned,
                        "retention_days": r.retention_days,
                    });
                    println!("{payload}");
                } else if r.retention_days == 0 {
                    println!("Retention is set to 0 (keep forever); nothing pruned.");
                } else {
                    println!(
                        "Pruned {} recall rows, {} session rows (retention: {} days).",
                        r.recalls_pruned, r.sessions_pruned, r.retention_days
                    );
                }
            }
            MetricsCommand::Calibrate {
                model,
                json: as_json,
            } => {
                let r = commands::metrics::calibrate(&cwd, model)?;
                if as_json {
                    let payload = json!({
                        "cl100k_tokens": r.cl100k_tokens,
                        "real_tokens": r.real_tokens,
                        "previous_factor": r.previous_factor,
                        "factor": r.factor,
                        "model": r.model,
                    });
                    println!("{payload}");
                } else {
                    println!("Tokenizer calibrated against Anthropic count_tokens.");
                    println!("  Model:          {}", r.model);
                    println!("  cl100k tokens:  {}", r.cl100k_tokens);
                    println!("  real tokens:    {}", r.real_tokens);
                    println!(
                        "  factor:         {:.4}  (was {:.4})",
                        r.factor, r.previous_factor
                    );
                    println!(
                        "Saved to [metrics] calibration_factor. Future token \
                         estimates are scaled by it; existing rows are unchanged."
                    );
                }
            }
        },
        TopLevelCommand::Global { command } => match command {
            GlobalCommand::Enable { json: as_json } => {
                let r = commands::global::enable(&cwd)?;
                if as_json {
                    println!(
                        "{}",
                        json!({
                            "enabled": true,
                            "already_enabled": r.already_enabled,
                            "store_created": r.store_created,
                            "path": r.path.display().to_string(),
                        })
                    );
                } else {
                    if r.store_created {
                        print_global_store_created();
                    }
                    if r.already_enabled {
                        println!("Machine-global memory already enabled for this repo.");
                    } else {
                        println!(
                            "Machine-global memory enabled for this repo.\n  Store: {}",
                            r.path.display()
                        );
                    }
                }
            }
            GlobalCommand::Disable { json: as_json } => {
                commands::global::disable(&cwd)?;
                if as_json {
                    println!("{}", json!({ "enabled": false }));
                } else {
                    println!(
                        "Machine-global memory disabled for this repo \
                         (store kept on disk; recall stops merging it)."
                    );
                }
            }
            GlobalCommand::Status { json: as_json } => {
                let s = commands::global::status(&cwd)?;
                if as_json {
                    println!(
                        "{}",
                        json!({
                            "enabled": s.enabled,
                            "path": s.path.display().to_string(),
                            "exists": s.exists,
                            "schema_version": s.schema_version,
                            "facts": s.fact_count,
                            "decisions": s.decision_count,
                            "doc_chunks": s.doc_chunk_count,
                        })
                    );
                } else {
                    println!("Machine-global memory");
                    println!(
                        "  Enabled (this repo): {}",
                        if s.enabled { "yes" } else { "no" }
                    );
                    println!("  Store path:          {}", s.path.display());
                    println!(
                        "  Store exists:        {}",
                        if s.exists { "yes" } else { "no" }
                    );
                    if s.exists {
                        println!(
                            "  Schema version:      {}",
                            s.schema_version.as_deref().unwrap_or("unknown")
                        );
                        println!("  Facts:               {}", s.fact_count);
                        println!("  Decisions:           {}", s.decision_count);
                        println!("  Doc chunks:          {}", s.doc_chunk_count);
                    }
                }
            }
        },
        TopLevelCommand::Upgrade {
            also,
            dry_run,
            yes,
            no_skills,
            no_gc,
            finish,
            staged,
            allow_self_stage,
            verify_last,
            json: as_json,
        } => {
            commands::upgrade::run(
                &cwd,
                commands::upgrade::UpgradeArgs {
                    also,
                    dry_run,
                    json: as_json,
                    finish,
                    staged,
                    allow_self_stage,
                    yes,
                    no_skills,
                    no_gc,
                    verify_last,
                },
            )?;
        }
        TopLevelCommand::Sync { command } => match command {
            SyncCommand::Snapshot {
                out_dir,
                force,
                json: as_json,
            } => {
                let out_dir = match out_dir {
                    Some(p) => p,
                    None => commands::sync::default_remote_dir(&cwd)?,
                };
                let summary = commands::sync::snapshot(&cwd, &out_dir, force)?;
                if as_json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "project_id": summary.project_id,
                            "snapshot_path": summary.snapshot_path.display().to_string(),
                            "manifest_path": summary.manifest_path.display().to_string(),
                            "schema_version": summary.schema_version,
                            "logical_version": {
                                "writes_log_max_id": summary.logical_version.writes_log_max_id,
                                "writes_log_count": summary.logical_version.writes_log_count,
                            },
                            "file_sha256": summary.file_sha256,
                            "bytes": summary.bytes,
                        })
                    );
                } else {
                    println!("Wrote memhub snapshot for project '{}'", summary.project_id);
                    println!("  snapshot: {}", summary.snapshot_path.display());
                    println!("  manifest: {}", summary.manifest_path.display());
                    println!("  schema:   {}", summary.schema_version);
                    println!(
                        "  logical:  writes_log max_id={} count={}",
                        summary.logical_version.writes_log_max_id,
                        summary.logical_version.writes_log_count
                    );
                    println!("  size:     {} bytes", summary.bytes);
                    println!(
                        "  sha256:   {}",
                        &summary.file_sha256[..summary.file_sha256.len().min(16)]
                    );
                }
            }
            SyncCommand::Enable { json: as_json } => {
                let result = commands::sync::enable(&cwd)?;
                if as_json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "enabled": true,
                            "already_enabled": result.already_enabled,
                            "project_id": result.project_id.as_deref().ok(),
                            "project_id_error": result.project_id.as_ref().err(),
                        })
                    );
                } else if result.already_enabled {
                    println!("Cross-machine sync already enabled for this repo.");
                } else {
                    println!("Cross-machine sync enabled for this repo.");
                }
                match &result.project_id {
                    Ok(id) => println!("  Drive folder project id: {id}"),
                    Err(e) => println!(
                        "  ⚠ {e}\n  set `[sync] project_id` in .memhub/config.toml before syncing"
                    ),
                }
            }
            SyncCommand::Disable { json: as_json } => {
                commands::sync::disable(&cwd)?;
                if as_json {
                    println!("{}", serde_json::json!({ "enabled": false }));
                } else {
                    println!("Cross-machine sync disabled for this repo (marker kept).");
                }
            }
            SyncCommand::Status { json: as_json } => {
                let s = commands::sync::enablement_status(&cwd)?;
                if as_json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "enabled": s.enabled,
                            "project_id": s.project_id.as_deref().ok(),
                            "project_id_error": s.project_id.as_ref().err(),
                            "drive_subpath": s.drive_subpath,
                            "remote_dir": s.remote_dir.as_deref().ok(),
                            "remote_dir_error": s.remote_dir.as_ref().err(),
                            "local_schema": s.local_schema,
                            "local_logical": {
                                "writes_log_max_id": s.local_logical.writes_log_max_id,
                                "writes_log_count": s.local_logical.writes_log_count,
                                "digest": s.local_logical.digest,
                            },
                            "marker": s.marker,
                        })
                    );
                } else {
                    println!(
                        "cross-machine sync: {}",
                        if s.enabled { "enabled" } else { "disabled" }
                    );
                    match &s.project_id {
                        Ok(id) => println!("  project id: {id}"),
                        Err(e) => println!("  project id: <unresolved> ({e})"),
                    }
                    if !s.drive_subpath.is_empty() {
                        println!("  drive subpath hint: {}", s.drive_subpath);
                    }
                    match &s.remote_dir {
                        Ok(dir) => println!("  remote dir: {dir}"),
                        Err(e) => println!("  remote dir: <unresolved> ({e})"),
                    }
                    println!("  local schema: {}", s.local_schema);
                    println!(
                        "  local logical: max_id={} count={} digest={}",
                        s.local_logical.writes_log_max_id,
                        s.local_logical.writes_log_count,
                        &s.local_logical.digest[..s.local_logical.digest.len().min(12)]
                    );
                    match &s.marker {
                        Some(m) => println!(
                            "  last sync: {} ({}) baseline digest={}",
                            m.synced_at,
                            m.last_action,
                            &m.baseline.digest[..m.baseline.digest.len().min(12)]
                        ),
                        None => println!("  last sync: never"),
                    }
                }
            }
            SyncCommand::Check {
                remote,
                json: as_json,
            } => {
                let remote = match remote {
                    Some(p) => p,
                    None => commands::sync::default_remote_dir(&cwd)?,
                };
                let report = commands::sync::check(&cwd, &remote)?;
                if as_json {
                    println!("{}", serde_json::to_string(&report)?);
                } else {
                    println!("sync status for project '{}'", report.project_id);
                    println!("  verdict: {}", report.verdict.as_str());
                    if !report.baseline_present {
                        println!("  (no prior sync baseline — first sync)");
                    }
                    println!(
                        "  local:  schema={} logical(max_id={}, count={})",
                        report.local_schema,
                        report.local_logical.writes_log_max_id,
                        report.local_logical.writes_log_count
                    );
                    match (&report.remote_schema, &report.remote_logical) {
                        (Some(schema), Some(logical)) => println!(
                            "  drive:  schema={} logical(max_id={}, count={}){}{}",
                            schema,
                            logical.writes_log_max_id,
                            logical.writes_log_count,
                            report
                                .remote_machine_id
                                .as_deref()
                                .map(|m| format!("  from {m}"))
                                .unwrap_or_default(),
                            report
                                .remote_created_at
                                .as_deref()
                                .map(|t| format!(" @ {t}"))
                                .unwrap_or_default(),
                        ),
                        _ => println!("  drive:  no snapshot found at {}", remote.display()),
                    }
                    if let Some(other) = &report.project_id_mismatch {
                        println!(
                            "  ⚠ project id mismatch: snapshot is '{other}', not this repo — do NOT adopt"
                        );
                    }
                    if report.schema_blocks_adopt {
                        println!(
                            "  ⚠ snapshot schema is newer than this binary — run `memhub upgrade` before adopting"
                        );
                    }
                }
            }
            SyncCommand::Adopt {
                remote,
                yes,
                json: as_json,
            } => {
                let remote = match remote {
                    Some(p) => p,
                    None => commands::sync::default_remote_dir(&cwd)?,
                };
                let summary = commands::sync::adopt(&cwd, &remote, yes)?;
                if as_json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "project_id": summary.project_id,
                            "adopted_from_machine": summary.adopted_from_machine,
                            "previous_schema": summary.previous_schema,
                            "new_schema": summary.new_schema,
                            "baseline": {
                                "writes_log_max_id": summary.baseline.writes_log_max_id,
                                "writes_log_count": summary.baseline.writes_log_count,
                            },
                            "backup_path": summary.backup_path.display().to_string(),
                        })
                    );
                } else {
                    println!(
                        "Adopted Drive snapshot for '{}' (from {})",
                        summary.project_id, summary.adopted_from_machine
                    );
                    if summary.previous_schema != summary.new_schema {
                        println!(
                            "  migrated schema {} → {}",
                            summary.previous_schema, summary.new_schema
                        );
                    }
                    println!(
                        "  replaced DB backed up to {}",
                        summary.backup_path.display()
                    );
                    println!("  run `memhub render` to refresh the local view");
                }
            }
            SyncCommand::Commit {
                remote,
                json: as_json,
            } => {
                let remote = match remote {
                    Some(p) => p,
                    None => commands::sync::default_remote_dir(&cwd)?,
                };
                let summary = commands::sync::commit(&cwd, &remote)?;
                if as_json {
                    println!(
                        "{}",
                        serde_json::json!({
                            "project_id": summary.project_id,
                            "baseline": {
                                "writes_log_max_id": summary.baseline.writes_log_max_id,
                                "writes_log_count": summary.baseline.writes_log_count,
                            },
                        })
                    );
                } else {
                    println!(
                        "Recorded push baseline for '{}' (logical max_id={}, count={})",
                        summary.project_id,
                        summary.baseline.writes_log_max_id,
                        summary.baseline.writes_log_count
                    );
                }
            }
        },
        TopLevelCommand::Gc {
            dry_run,
            json: as_json,
        } => {
            let outcome = commands::gc::run(&cwd, dry_run)?;
            if as_json {
                println!("{}", serde_json::to_string(&outcome)?);
            } else {
                println!(
                    "memhub gc ({}): {}",
                    outcome.root.display(),
                    outcome.summary()
                );
                for line in &outcome.details {
                    println!("  {line}");
                }
            }
        }
        TopLevelCommand::Recall {
            query,
            source_type,
            max_results,
            mode,
            include_stale,
            accepted_only,
            no_rerank,
            min_rerank_score,
            json: as_json,
        } => {
            let opts = RecallOptions {
                query,
                mode: mode.map(|m| m.to_mode()),
                max_results: max_results.unwrap_or(0),
                source_types: source_type
                    .iter()
                    .map(RecallSourceTypeArg::to_source_type)
                    .collect(),
                include_stale: if include_stale { Some(true) } else { None },
                accepted_only: if accepted_only { Some(true) } else { None },
                use_reranker: if no_rerank { Some(false) } else { None },
                min_rerank_score,
                log_metrics: true,
                surface: Some(RecallSurface::Cli),
            };
            let response = retrieval::recall(&cwd, opts)?;
            if as_json {
                println!("{}", recall_response_to_json(&response));
            } else {
                print_recall_human(&response);
            }
        }
        TopLevelCommand::Eval { command } => match command {
            EvalCommand::Retrieval {
                golden,
                k,
                mode,
                no_rerank,
                min_rerank_score,
                json: as_json,
            } => {
                let golden_path =
                    golden.unwrap_or_else(|| cwd.join(commands::eval::DEFAULT_GOLDEN_PATH));
                let opts = commands::eval::EvalOptions {
                    golden_path,
                    k,
                    mode: mode.map(|m| m.to_mode()),
                    use_reranker: if no_rerank { Some(false) } else { None },
                    min_rerank_score,
                };
                let summary = commands::eval::run_retrieval(&cwd, opts)?;
                if as_json {
                    println!("{}", eval_summary_to_json(&summary));
                } else {
                    print_eval_summary(&summary);
                }
            }
            EvalCommand::Locate {
                golden,
                k,
                rerank,
                min_rerank_score,
                json: as_json,
            } => {
                let golden_path =
                    golden.unwrap_or_else(|| cwd.join(commands::eval::DEFAULT_CODE_GOLDEN_PATH));
                let opts = commands::eval::LocateEvalOptions {
                    golden_path,
                    k,
                    use_reranker: rerank,
                    min_rerank_score,
                };
                let summary = commands::eval::run_locate(&cwd, opts)?;
                if as_json {
                    println!("{}", locate_eval_summary_to_json(&summary));
                } else {
                    print_locate_eval_summary(&summary);
                }
            }
        },
        TopLevelCommand::Locate {
            query,
            limit,
            rerank,
            no_refresh,
            json: as_json,
        } => {
            let opts = crate::code_index::locate::LocateOptions {
                query,
                limit,
                use_reranker: rerank,
                no_refresh,
            };
            let response = crate::code_index::locate::locate(&cwd, opts)?;
            if as_json {
                println!("{}", locate_response_to_json(&response));
            } else {
                print_locate(&response);
            }
        }
        TopLevelCommand::Code { command } => match command {
            CodeCommand::Index {
                rebuild,
                json: as_json,
            } => {
                if rebuild {
                    crate::code_index::remove_index(&cwd)?;
                }
                let summary = crate::code_index::refresh(&cwd)?;
                if as_json {
                    let payload = json!({
                        "rebuild": rebuild,
                        "files_total": summary.files_total,
                        "chunks_total": summary.chunks_total,
                        "new_files": summary.new_files,
                        "changed_files": summary.changed_files,
                        "unchanged_files": summary.unchanged_files,
                        "deleted_files": summary.deleted_files,
                        "skipped_files": summary.skipped_files,
                        "excluded_files": summary.excluded_files,
                        "binary_skipped": summary.binary_skipped,
                        "embedded_chunks": summary.embedded_chunks,
                        "head": summary.head,
                    });
                    println!("{payload}");
                } else {
                    println!(
                        "Code index {} ({} files, {} chunks)",
                        if rebuild { "rebuilt" } else { "refreshed" },
                        summary.files_total,
                        summary.chunks_total,
                    );
                    println!(
                        "  new: {}  changed: {}  unchanged: {}  deleted: {}  skipped: {}  excluded: {}",
                        summary.new_files,
                        summary.changed_files,
                        summary.unchanged_files,
                        summary.deleted_files,
                        summary.skipped_files,
                        summary.excluded_files,
                    );
                    if summary.embedded_chunks > 0 {
                        println!("  embedded chunks: {}", summary.embedded_chunks);
                    }
                    if let Some(head) = &summary.head {
                        println!("  HEAD: {head}");
                    }
                }
            }
            CodeCommand::Status { json: as_json } => {
                let status = crate::code_index::status(&cwd)?;
                if as_json {
                    println!("{}", code_status_to_json(&status));
                } else {
                    print_code_status(&status);
                }
            }
            CodeCommand::Rm { json: as_json } => {
                let outcome = crate::code_index::remove_index(&cwd)?;
                if as_json {
                    println!(
                        "{}",
                        json!({
                            "removed": outcome.removed,
                            "db_path": outcome.db_path,
                        })
                    );
                } else if outcome.removed {
                    println!("Removed code index at {}", outcome.db_path.display());
                } else {
                    println!("No code index to remove at {}", outcome.db_path.display());
                }
            }
        },
        TopLevelCommand::Audit { command } => match command {
            AuditCommand::Md {
                json: as_json,
                strict,
            } => {
                let report = commands::audit_md::run(&cwd, strict)?;
                if as_json {
                    println!("{}", json!({ "audit_md": audit_md_report_to_json(&report) }));
                } else {
                    print_audit_md_report_human(&report);
                }
                process::exit(report.exit_code);
            }
        },
        TopLevelCommand::WrapupPolicy { json: as_json } => {
            let report = commands::wrapup_policy::run(&cwd)?;
            if as_json {
                println!(
                    "{}",
                    json!({ "wrapup_policy": wrapup_policy_report_to_json(&report) })
                );
            } else {
                print_wrapup_policy_human(&report);
            }
        }
        TopLevelCommand::Transcript {
            command: TranscriptCommand::Archive {
                agent,
                session_id,
                yes,
                json: as_json,
            },
        } => {
            // Loud, unmissable warning at the surface — always, before the
            // gate — because the archive is stored unredacted (Q8).
            eprintln!("{}", commands::transcript::UNREDACTED_WARNING);

            if !transcript_archive_approved(yes) {
                return Err(MemhubError::InvalidInput(
                    "transcript archive not approved. Pass --yes to archive (required on a \
                     non-interactive terminal); the archive is UNREDACTED."
                        .to_string(),
                ));
            }

            let report =
                commands::transcript::archive(&cwd, agent.to_agent(), &session_id, true)?;
            if as_json {
                println!(
                    "{}",
                    json!({ "transcript_archive": {
                        "session_id": report.session_id,
                        "agent": report.agent,
                        "source_path": report.source_path.to_string_lossy(),
                        "archive_path": report.archive_path.to_string_lossy(),
                        "source_bytes": report.source_bytes,
                        "archive_bytes": report.archive_bytes,
                        "replaced_existing": report.replaced_existing,
                        "pruned": report.pruned,
                    } })
                );
            } else {
                println!(
                    "Archived {} transcript for session {} -> {} ({} -> {} bytes){}",
                    report.agent,
                    report.session_id,
                    report.archive_path.display(),
                    report.source_bytes,
                    report.archive_bytes,
                    if report.replaced_existing {
                        " (replaced prior archive)"
                    } else {
                        ""
                    },
                );
                if report.pruned > 0 {
                    println!("Pruned {} archive(s) past the retention horizon.", report.pruned);
                }
            }
        }
        TopLevelCommand::Render { actor } => {
            let actor = resolve_actor(actor.as_deref())?;
            let result = commands::render::run(&cwd, &actor)?;
            println!("Rendered to {}", result.output_dir.display());
            for path in &result.written_files {
                println!("  wrote: {}", path.display());
            }
            if result.backup_files.is_empty() {
                println!("Backups: none (no prior files to back up)");
            } else {
                println!("Backups:");
                for path in &result.backup_files {
                    println!("  {}", path.display());
                }
            }
        }
        TopLevelCommand::Note { command } => match command {
            NoteCommand::Add {
                text,
                from_file,
                json: as_json,
                actor,
            } => {
                let body = resolve_text_input("note add", text, from_file)?;
                let actor = resolve_actor(actor.as_deref())?;
                let note = commands::session_note::add(&cwd, &body, &actor, &actor)?;
                if as_json {
                    let payload = json!({
                        "id": note.id,
                        "actor": note.actor,
                        "actor_raw": note.actor_raw,
                        "text": note.text,
                        "created_at": note.created_at,
                    });
                    println!("{payload}");
                } else {
                    println!(
                        "Recorded note {} ({} chars) at {}",
                        note.id,
                        note.text.chars().count(),
                        note.created_at
                    );
                }
            }
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

#[cfg(feature = "viz")]
fn run_viz(cwd: &std::path::Path, host: String, port: u16, open: bool) -> Result<()> {
    crate::dashboard::serve_blocking(cwd, crate::dashboard::DashboardOptions { host, port, open })
}
