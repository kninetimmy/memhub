use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use crate::commands;
use crate::commands::narrative::DEFAULT_HISTORY_LIMIT;
use crate::config::RetrievalMode;
use crate::retrieval::SourceType;

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
    /// Ingest and manage external reference documents (opt-in to recall).
    Doc {
        #[command(subcommand)]
        command: DocCommand,
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
    Metrics {
        #[command(subcommand)]
        command: MetricsCommand,
    },
    /// Machine-global memory: opt this repo in/out and inspect the
    /// shared `~/.memhub/global.sqlite` store (M9).
    Global {
        #[command(subcommand)]
        command: GlobalCommand,
    },
    /// Rebuild + install memhub and bring every memhub instance on this
    /// machine (each known repo DB + the global store) to head, with a
    /// one-time fix for the `~/.local/bin` PATH shadow. Run from the
    /// memhub source repo.
    Upgrade {
        /// Also include (and remember) this repo root even if memhub has
        /// never opened it. Repeatable. The registry bootstrap hatch.
        #[arg(long, value_name = "PATH")]
        also: Vec<PathBuf>,
        /// Report what would happen — no install, no symlink change, no
        /// migration.
        #[arg(long)]
        dry_run: bool,
        /// Assume "yes" to the prompt before replacing a non-symlink
        /// `~/.local/bin/memhub` shadow.
        #[arg(long)]
        yes: bool,
        /// Skip resyncing installed agent skill wrappers
        /// (`~/.claude/commands/`, `~/.codex/skills/`) from
        /// `templates/skills/`. The binary + DB migrate still run.
        #[arg(long)]
        no_skills: bool,
        /// Internal: set on the re-exec'd freshly installed binary to
        /// run only the migrate + verify pass.
        #[arg(long, hide = true)]
        finish: bool,
        #[arg(long)]
        json: bool,
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
pub enum MetricsCommand {
    /// Show token-accounting status: config, DB counts, recent sessions.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Enable the token-accounting subsystem and auto-detect the Claude
    /// transcripts dir if not already set.
    Enable {
        #[arg(long)]
        json: bool,
    },
    /// Disable the token-accounting subsystem.
    Disable {
        #[arg(long)]
        json: bool,
    },
    /// Force a transcript scrape + reconcile + prune pass and report counts.
    Rescan {
        #[arg(long)]
        json: bool,
    },
    /// Explicitly run the retention pruner and report how many rows were
    /// deleted.
    Prune {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum GlobalCommand {
    /// Opt this repo into machine-global memory; create the store on
    /// first enable anywhere on the machine.
    Enable {
        #[arg(long)]
        json: bool,
    },
    /// Opt this repo back out. Non-destructive; the store is kept.
    Disable {
        #[arg(long)]
        json: bool,
    },
    /// Show enablement, store path, schema version, and row counts.
    Status {
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
    Doc,
}

impl RecallSourceTypeArg {
    pub(crate) fn to_source_type(&self) -> SourceType {
        match self {
            Self::Fact => SourceType::Fact,
            Self::Decision => SourceType::Decision,
            Self::Task => SourceType::Task,
            Self::Doc => SourceType::DocChunk,
        }
    }
}

#[derive(Debug, Clone, ValueEnum)]
pub enum RecallModeArg {
    Fts,
    Hybrid,
}

impl RecallModeArg {
    pub(crate) fn to_mode(&self) -> RetrievalMode {
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
    pub(crate) fn to_window(&self) -> commands::stats::StatsWindow {
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
    pub(crate) fn as_filter(&self) -> Option<&'static str> {
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
        /// Write to the machine-global store instead of this repo's
        /// DB. Requires `memhub global enable` in this repo (M9).
        #[arg(long)]
        global: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        actor: Option<String>,
    },
    /// Copy an existing repo fact into the machine-global store
    /// (copy, not move — the repo row stays and still wins locally).
    Promote {
        id: i64,
        /// Target the machine-global store. Required (the only
        /// promotion target in M9).
        #[arg(long)]
        global: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        actor: Option<String>,
    },
    List,
}

#[derive(Debug, Subcommand)]
pub enum DocCommand {
    /// Ingest (or re-ingest) a markdown file. Unchanged content is a
    /// no-op; changed content replaces every chunk.
    Add {
        /// Path to the markdown file to ingest.
        file: PathBuf,
        /// Override the document title (defaults to the first heading or
        /// the file name).
        #[arg(long)]
        title: Option<String>,
        /// Ingest into the machine-global store (a broadly-applicable
        /// guide visible to every repo). Requires `memhub global
        /// enable` in this repo (M9).
        #[arg(long)]
        global: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        actor: Option<String>,
    },
    /// List ingested documents.
    Ls {
        /// List documents in the machine-global store instead of this
        /// repo's. Requires `memhub global enable` in this repo (M9).
        #[arg(long)]
        global: bool,
        #[arg(long)]
        json: bool,
    },
    /// Remove a document (and its chunks) by id or path.
    Rm {
        ident: String,
        /// Remove from the machine-global store instead of this repo's.
        /// Requires `memhub global enable` in this repo (M9).
        #[arg(long)]
        global: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        actor: Option<String>,
    },
    /// Show a document's metadata and chunk breadcrumbs by id or path.
    Show {
        ident: String,
        /// Show from the machine-global store instead of this repo's.
        /// Requires `memhub global enable` in this repo (M9).
        #[arg(long)]
        global: bool,
        #[arg(long)]
        json: bool,
    },
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
        /// Write to the machine-global store instead of this repo's
        /// DB. Requires `memhub global enable` in this repo (M9).
        #[arg(long)]
        global: bool,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        actor: Option<String>,
    },
    /// Copy an existing repo decision into the machine-global store
    /// (copy, not move — the repo row stays and still wins locally).
    Promote {
        id: i64,
        /// Target the machine-global store. Required (the only
        /// promotion target in M9).
        #[arg(long)]
        global: bool,
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
    pub(crate) fn as_str(&self) -> &'static str {
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
    pub(crate) fn as_str(&self) -> &'static str {
        match self {
            Self::Build => "build",
            Self::Test => "test",
            Self::Run => "run",
            Self::Lint => "lint",
            Self::Other => "other",
        }
    }
}
