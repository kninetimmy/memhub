use std::fs;
use std::path::PathBuf;
use std::process;

use clap::{Parser, Subcommand, ValueEnum};
use serde_json::json;

use crate::Result;
use crate::commands;
use crate::commands::{DEFAULT_ACTOR, validate_actor};
use crate::config::RetrievalMode;
use crate::models::{InitResult, NarrativeEntry, NarrativeKind, PendingWriteRecord, StatsSummary};
use crate::retrieval;
use crate::retrieval::{RecallOptions, RecallResponse, SourceType};
use crate::{MemhubError, commands::narrative::DEFAULT_HISTORY_LIMIT};

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

fn run_narrative(cwd: &std::path::Path, kind: NarrativeKind, command: NarrativeCommand) -> Result<()> {
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
                            entry.id, kind.as_str(), entry.actor, entry.created_at
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

fn narrative_entry_to_json(kind: NarrativeKind, entry: &NarrativeEntry) -> serde_json::Value {
    json!({
        "kind": kind.as_str(),
        "id": entry.id,
        "body": entry.body,
        "actor": entry.actor,
        "actor_raw": entry.actor_raw,
        "created_at": entry.created_at,
    })
}

fn index_status_to_json(s: &commands::index::IndexStatusSummary) -> serde_json::Value {
    json!({
        "model": s.model,
        "mode": recall_mode_label(s.mode),
        "facts": { "total": s.facts_total, "embedded": s.facts_embedded },
        "decisions": { "total": s.decisions_total, "embedded": s.decisions_embedded },
        "tasks": { "total": s.tasks_total, "embedded": s.tasks_embedded },
        "total_embeddings": s.total_embeddings,
        "missing_count": s.missing_count,
        "stale_ratio": s.stale_ratio,
    })
}

fn print_index_status(s: &commands::index::IndexStatusSummary) {
    println!("Embedding model: {}", s.model);
    println!("Retrieval mode:  {}", recall_mode_label(s.mode));
    println!(
        "Facts:     {} embedded / {} total",
        s.facts_embedded, s.facts_total,
    );
    println!(
        "Decisions: {} embedded / {} total",
        s.decisions_embedded, s.decisions_total,
    );
    println!(
        "Tasks:     {} embedded / {} total",
        s.tasks_embedded, s.tasks_total,
    );
    println!("Total embeddings: {}", s.total_embeddings);
    println!(
        "Missing: {} ({:.0}% of source rows lack embeddings)",
        s.missing_count,
        s.stale_ratio * 100.0,
    );
    if s.missing_count > 0 {
        println!("Run `memhub index rebuild` (or /reindex) to refresh.");
    }
}

fn recall_mode_label(mode: RetrievalMode) -> &'static str {
    match mode {
        RetrievalMode::Fts => "fts",
        RetrievalMode::Hybrid => "hybrid",
    }
}

fn recall_response_to_json(response: &RecallResponse) -> serde_json::Value {
    let results = response
        .results
        .iter()
        .map(|hit| {
            json!({
                "rank": hit.rank,
                "source_type": hit.source_type,
                "source_id": hit.source_id,
                "title": hit.title,
                "body": hit.body,
                "score": hit.score,
                "fts_score": hit.fts_score,
                "vector_score": hit.vector_score,
                "confidence": hit.confidence,
                "stale": hit.stale,
                "source": hit.source,
                "created_at": hit.created_at,
                "rerank_score": hit.rerank_score,
            })
        })
        .collect::<Vec<_>>();
    let warnings = response
        .warnings
        .iter()
        .map(|w| {
            json!({
                "kind": w.kind,
                "stale_count": w.stale_count,
                "total_count": w.total_count,
                "reason": w.reason,
                "fix": w.fix,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "query": response.query,
        "mode": recall_mode_label(response.mode),
        "results": results,
        "candidate_count": response.candidate_count,
        "returned_count": response.returned_count,
        "warnings": warnings,
        "provenance": {
            "matcher": response.matcher,
            "elapsed_ms": response.elapsed_ms,
        },
    })
}

fn print_recall_human(response: &RecallResponse) {
    println!(
        "Query: {}  (mode: {}, matcher: {}, elapsed: {} ms)",
        response.query,
        recall_mode_label(response.mode),
        response.matcher,
        response.elapsed_ms,
    );
    println!(
        "Candidates: {} | Returned: {}",
        response.candidate_count, response.returned_count,
    );
    if response.results.is_empty() {
        println!("No matches.");
    } else {
        println!();
        for hit in &response.results {
            let stale_tag = if hit.stale { " [stale]" } else { "" };
            let source_label = if hit.source.is_empty() {
                String::new()
            } else {
                format!(" source={}", hit.source)
            };
            println!(
                "#{rank} [{stype}:{sid}] {title}{stale}  score={score:.3} (fts={fts:.3}, vec={vec:.3}){src}",
                rank = hit.rank,
                stype = hit.source_type,
                sid = hit.source_id,
                title = hit.title,
                stale = stale_tag,
                score = hit.score,
                fts = hit.fts_score,
                vec = hit.vector_score,
                src = source_label,
            );
            if !hit.body.is_empty() {
                println!("    {}", hit.body);
            }
        }
    }
    if !response.warnings.is_empty() {
        println!();
        println!("Warnings:");
        for warn in &response.warnings {
            println!(
                "  {} ({}/{}): {} — {}",
                warn.kind, warn.stale_count, warn.total_count, warn.reason, warn.fix,
            );
        }
    }
}

fn eval_summary_to_json(summary: &commands::eval::EvalSummary) -> serde_json::Value {
    let outcomes = summary
        .outcomes
        .iter()
        .map(|o| {
            json!({
                "id": o.id,
                "query": o.query,
                "kind": match o.kind {
                    commands::eval::GoldenKind::Match => "match",
                    commands::eval::GoldenKind::Empty => "empty",
                },
                "passed": o.passed,
                "matched_rank": o.matched_rank,
                "matched_score": o.matched_score,
                "returned_count": o.returned_count,
                "failure_reason": o.failure_reason,
            })
        })
        .collect::<Vec<_>>();
    json!({
        "golden_path": summary.golden_path.display().to_string(),
        "mode": recall_mode_label(summary.mode),
        "k": summary.k,
        "totals": {
            "queries": summary.total_queries,
            "match_queries": summary.match_queries,
            "empty_queries": summary.empty_queries,
            "match_passes": summary.match_passes,
            "empty_passes": summary.empty_passes,
            "safety_failures": summary.safety_failures,
        },
        "recall_at_k": summary.recall_at_k,
        "elapsed_ms": summary.elapsed_ms,
        "outcomes": outcomes,
    })
}

fn print_eval_summary(summary: &commands::eval::EvalSummary) {
    println!(
        "memhub eval retrieval — {} ({} queries)",
        summary.golden_path.display(),
        summary.total_queries,
    );
    println!(
        "Mode: {}  |  K: {}  |  Elapsed: {} ms",
        recall_mode_label(summary.mode),
        summary.k,
        summary.elapsed_ms,
    );
    println!(
        "Recall@{k}: {passes}/{total} = {pct:.1}%",
        k = summary.k,
        passes = summary.match_passes,
        total = summary.match_queries,
        pct = summary.recall_at_k * 100.0,
    );
    if summary.empty_queries > 0 {
        println!(
            "Safety: {pass}/{total} empty-query probes returned no results{failed}",
            pass = summary.empty_passes,
            total = summary.empty_queries,
            failed = if summary.safety_failures > 0 {
                format!("  [{} FAILED]", summary.safety_failures)
            } else {
                String::new()
            },
        );
    }
    println!();
    println!("Per-query outcomes:");
    for outcome in &summary.outcomes {
        let glyph = if outcome.passed { "PASS" } else { "FAIL" };
        let kind = match outcome.kind {
            commands::eval::GoldenKind::Match => "match",
            commands::eval::GoldenKind::Empty => "empty",
        };
        let detail = match outcome.matched_rank {
            Some(rank) => format!(
                "rank {rank}, score {score:.3}",
                rank = rank,
                score = outcome.matched_score.unwrap_or(0.0),
            ),
            None => match outcome.kind {
                commands::eval::GoldenKind::Empty => {
                    format!("{} hit(s) returned", outcome.returned_count)
                }
                commands::eval::GoldenKind::Match => {
                    format!("{} hit(s) returned, no match", outcome.returned_count)
                }
            },
        };
        println!(
            "  [{glyph}] {id} ({kind}) — {detail}",
            id = outcome.id,
        );
        if let Some(reason) = &outcome.failure_reason {
            println!("        {reason}");
        }
    }
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
    Viz {
        #[arg(long, default_value = "127.0.0.1")]
        host: String,
        #[arg(long, default_value_t = 0)]
        port: u16,
        #[arg(long)]
        open: bool,
    },
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
    State {
        #[command(subcommand)]
        command: NarrativeCommand,
    },
    Arch {
        #[command(subcommand)]
        command: NarrativeCommand,
    },
    Render,
    Index {
        #[command(subcommand)]
        command: IndexCommand,
    },
    Recall {
        query: String,
        #[arg(long, value_enum, value_name = "TYPE")]
        source_type: Vec<RecallSourceTypeArg>,
        #[arg(long, value_name = "N")]
        max_results: Option<usize>,
        #[arg(long, value_enum)]
        mode: Option<RecallModeArg>,
        #[arg(long)]
        include_stale: bool,
        #[arg(long)]
        accepted_only: bool,
        /// Disable the cross-encoder re-ranker for this call. By default
        /// the value of `[retrieval] use_reranker` is honored; this flag
        /// forces the re-ranker off without touching config.
        #[arg(long)]
        no_rerank: bool,
        /// Override `[retrieval.scoring] min_rerank_score` for this
        /// call. Ignored in fts mode and when the re-ranker is off.
        /// Negative values disable the floor; positive values tighten
        /// nonsense rejection.
        #[arg(long, value_name = "F")]
        min_rerank_score: Option<f32>,
        #[arg(long)]
        json: bool,
    },
    Eval {
        #[command(subcommand)]
        command: EvalCommand,
    },
}

#[derive(Debug, Subcommand)]
pub enum EvalCommand {
    Retrieval {
        #[arg(long, value_name = "PATH")]
        golden: Option<PathBuf>,
        #[arg(long, default_value_t = commands::eval::DEFAULT_K)]
        k: usize,
        #[arg(long, value_enum)]
        mode: Option<RecallModeArg>,
        /// Disable the cross-encoder re-ranker for every query in this
        /// eval run. Use for A/B comparisons against the rerank-on baseline.
        #[arg(long)]
        no_rerank: bool,
        /// Override `[retrieval.scoring] min_rerank_score` for every
        /// query in this eval run. Used to sweep the cross-encoder
        /// score floor (decisions 70, 71). Ignored when mode resolves
        /// to fts or when the re-ranker is disabled. Negative values
        /// disable the floor; positive values tighten nonsense
        /// rejection at the cost of recall on borderline matches.
        #[arg(long, value_name = "F")]
        min_rerank_score: Option<f32>,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum IndexCommand {
    Status {
        #[arg(long)]
        json: bool,
    },
    Rebuild {
        #[arg(long)]
        json: bool,
        #[arg(long)]
        actor: Option<String>,
    },
}

#[derive(Debug, Clone, ValueEnum)]
pub enum RecallSourceTypeArg {
    Fact,
    Decision,
    Task,
}

impl RecallSourceTypeArg {
    fn to_source_type(&self) -> SourceType {
        match self {
            Self::Fact => SourceType::Fact,
            Self::Decision => SourceType::Decision,
            Self::Task => SourceType::Task,
        }
    }
}

#[derive(Debug, Clone, ValueEnum)]
pub enum RecallModeArg {
    Fts,
    Hybrid,
}

impl RecallModeArg {
    fn to_mode(&self) -> RetrievalMode {
        match self {
            Self::Fts => RetrievalMode::Fts,
            Self::Hybrid => RetrievalMode::Hybrid,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum NarrativeCommand {
    Set {
        body: Option<String>,
        #[arg(long, value_name = "PATH")]
        from_file: Option<PathBuf>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        actor: Option<String>,
    },
    Show {
        #[arg(long)]
        json: bool,
    },
    History {
        #[arg(long, default_value_t = DEFAULT_HISTORY_LIMIT)]
        limit: usize,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum NoteCommand {
    Add {
        text: Option<String>,
        #[arg(long, value_name = "PATH")]
        from_file: Option<PathBuf>,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        actor: Option<String>,
    },
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
    BootstrapK9 {
        #[arg(long)]
        dry_run: bool,
        #[arg(long)]
        json: bool,
    },
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
        /// Optional natural-language paraphrase. Prepended to the embed
        /// text and cross-encoder rerank input so jargon-titled
        /// decisions surface for plain-English queries (decision 72).
        #[arg(long)]
        summary: Option<String>,
        #[arg(long, default_value = "user")]
        source: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        actor: Option<String>,
    },
    /// Backfill (or overwrite) the natural-language summary on an
    /// existing decision. Empty string clears the summary back to NULL.
    /// Re-embeds the row inside the same transaction.
    SetSummary {
        id: i64,
        summary: String,
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
                summary,
                source,
                json: as_json,
                actor,
            } => {
                let actor = resolve_actor(actor.as_deref())?;
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
            DecisionCommand::List => {
                let decisions = commands::decision::list(&cwd)?;
                if decisions.is_empty() {
                    println!("No decisions recorded.");
                } else {
                    for decision in decisions {
                        println!(
                            "[{}] {} [{}] at {} (source: {})\n  rationale: {}",
                            decision.id,
                            decision.title,
                            decision.status,
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
            println!();
            println!("Next steps:");
            println!(
                "  Embeddings for imported rows are not yet built. Run `memhub index` to"
            );
            println!("  enable vector recall on this machine.");
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
            IntegrationsCommand::BootstrapK9 { dry_run, json: as_json } => {
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
                        if summary.dry_run { "dry run" } else { "applied" }
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
                        "  facts: {}  decisions: {}  tasks: {}",
                        summary.facts, summary.decisions, summary.tasks,
                    );
                }
            }
        },
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
                let golden_path = golden.unwrap_or_else(|| {
                    cwd.join(commands::eval::DEFAULT_GOLDEN_PATH)
                });
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
        },
        TopLevelCommand::Render => {
            let result = commands::render::run(&cwd, DEFAULT_ACTOR)?;
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
    crate::dashboard::serve_blocking(
        cwd,
        crate::dashboard::DashboardOptions { host, port, open },
    )
}

#[cfg(not(feature = "viz"))]
fn run_viz(_cwd: &std::path::Path, _host: String, _port: u16, _open: bool) -> Result<()> {
    Err(MemhubError::InvalidInput(
        "`memhub viz` was compiled out; rebuild with `--features viz`".to_string(),
    ))
}
