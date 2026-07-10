use std::path::PathBuf;

use clap::{Parser, Subcommand, ValueEnum};

use crate::code_index;
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
        #[arg(long)]
        json: bool,
    },
    Status {
        #[arg(long)]
        json: bool,
    },
    /// One read-only health command across project / config / DB
    /// integrity / retrieval-metrics / integrations. Absorbs
    /// `/check-init`. Exit 0 unless an `error`-level check fires;
    /// `--strict` additionally fails on any `warn`. Writes to no table;
    /// no MCP tool.
    Doctor {
        #[arg(long)]
        json: bool,
        /// Promote `warn` to a failing exit code (still 1, never 2).
        #[arg(long)]
        strict: bool,
    },
    Stats {
        #[arg(long, value_enum, default_value_t = StatsWindowArg::ThirtyDays)]
        window: StatsWindowArg,
        #[arg(long)]
        json: bool,
    },
    SyncMd,
    Serve,
    #[cfg(feature = "viz")]
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
    Render {
        #[arg(long)]
        actor: Option<String>,
    },
    Index {
        #[command(subcommand)]
        command: IndexCommand,
    },
    #[cfg(feature = "metrics")]
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
        /// (`~/.claude/commands/`, `~/.codex/skills/`,
        /// `~/.config/opencode/skills/`, `~/.config/opencode/commands/`)
        /// from `templates/skills/`. The binary + DB migrate still run.
        #[arg(long)]
        no_skills: bool,
        /// Skip the `target/` build-artifact GC step (see `memhub gc`).
        /// The binary + DB migrate still run.
        #[arg(long)]
        no_gc: bool,
        /// Internal: set on the re-exec'd freshly installed binary to
        /// run only the migrate + verify pass.
        #[arg(long, hide = true)]
        finish: bool,
        /// Internal: set on the Windows staged temp copy so it runs the
        /// real orchestration instead of recursing into another stage.
        #[arg(long, hide = true)]
        staged: bool,
        /// Windows only: permit the staged self-relaunch when no TTY is
        /// attached (CI/scripts). The invoking shell will receive exit
        /// code 3 (handed off; result pending) — poll `memhub upgrade
        /// --verify-last`, or read the final `memhub upgrade:` line.
        #[arg(long)]
        allow_self_stage: bool,
        /// Report the outcome of the most recent `memhub upgrade` from
        /// ~/.memhub/last_upgrade.json and exit 0 (ok) / 1 (failed or no
        /// record) / 3 (handed off, still pending). Does not rebuild;
        /// useful after a staged Windows run whose shell only saw exit 3.
        #[arg(long)]
        verify_last: bool,
        #[arg(long)]
        json: bool,
    },
    /// Cross-machine Drive sync (M10). memhub stays offline; these
    /// commands operate on local files inside an OS-level synced folder
    /// (Google Drive for Desktop, or an rclone mount on Linux), which
    /// moves the snapshot between your machines. Opt in per repo with
    /// `memhub sync enable`.
    Sync {
        #[command(subcommand)]
        command: SyncCommand,
    },
    /// Reclaim disk by deleting superseded build artifacts in this
    /// repo's `target/` (Cargo never garbage-collects old hashes).
    /// Keeps only the newest build set of memhub-owned artifacts;
    /// third-party dependency rlibs are never touched. Also auto-runs
    /// inside `memhub upgrade`.
    Gc {
        /// Report what would be freed without deleting anything.
        #[arg(long)]
        dry_run: bool,
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
    /// Locate code by meaning: blend FTS + vector over the sibling code
    /// index and return ranked `path:line-range` breadcrumbs with snippets
    /// (M11). Refreshes the index to the working tree first, unless
    /// `--no-refresh` is passed. Read-only — never returns full files,
    /// never edits.
    Locate {
        query: String,
        #[arg(long, default_value_t = code_index::locate::DEFAULT_LOCATE_LIMIT)]
        limit: usize,
        /// Apply the bundled cross-encoder re-ranker over the candidate
        /// pool. Off by default: fusion (reranker off) is the default and
        /// wins Recall@3, while `--rerank` wins single-best-guess Recall@1
        /// (decisions 122/123).
        #[arg(long)]
        rerank: bool,
        /// Skip the pre-query freshness pass (`git ls-files` + per-file
        /// stat, plus resolving `HEAD`) and query the index exactly as it
        /// last stood. Stale-by-choice: an explicit opt-in for tight
        /// repeat-locate loops on a warm index where sub-100ms matters
        /// more than picking up edits since the last refresh. Default
        /// behavior (refresh every call) is unchanged without this flag.
        #[arg(long)]
        no_refresh: bool,
        #[arg(long)]
        json: bool,
    },
    /// Manage the sibling code index at `.memhub/code_index.sqlite`
    /// (M11). It is gitignored, never exported, never synced, never read
    /// by recall.
    Code {
        #[command(subcommand)]
        command: CodeCommand,
    },
    /// Read-only linter over the repo's root memory files (Wave 2 C5,
    /// issue #32).
    Audit {
        #[command(subcommand)]
        command: AuditCommand,
    },
    /// Render the full `/wrap-up` policy text for the resolved
    /// `[wrap_up] verbosity` level (Wave 6 W1+W2, issue #95). Read-only:
    /// no DB writes, and no `project.sqlite` open at all — verbosity is
    /// a config-only value.
    WrapupPolicy {
        #[arg(long)]
        json: bool,
    },
    /// Archive a session's RAW transcript into
    /// `.memhub/transcripts/<date>-<session-id>.jsonl.zst` with a pointer
    /// row (Wave 6 W3, issue #96). Per-machine opt-in behind `[wrap_up]
    /// verbosity = "transcript"`. The archive is UNREDACTED, so archiving
    /// requires an explicit `--yes` and refuses on a non-TTY without it.
    /// Transcripts are never embedded, recalled, or exported.
    Transcript {
        #[command(subcommand)]
        command: TranscriptCommand,
    },
}

/// Agent selector for `memhub transcript archive` — picks the transcript
/// directory + session-id convention (Claude file-stem ids vs Codex
/// `codex:<uuid>` ids), reusing the metrics scraper's mapping.
#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum TranscriptAgentArg {
    Claude,
    Codex,
}

impl TranscriptAgentArg {
    pub fn to_agent(self) -> commands::transcript::Agent {
        match self {
            TranscriptAgentArg::Claude => commands::transcript::Agent::Claude,
            TranscriptAgentArg::Codex => commands::transcript::Agent::Codex,
        }
    }
}

#[derive(Debug, Subcommand)]
pub enum TranscriptCommand {
    /// Copy the session JSONL to a compressed archive under `.memhub/`.
    Archive {
        /// Which agent's transcript directory + session-id convention to
        /// use.
        #[arg(long, value_enum, value_name = "AGENT")]
        agent: TranscriptAgentArg,
        /// Session id to archive. Claude: the transcript file stem (the
        /// session UUID). Codex: `codex:<uuid>` (a bare uuid is accepted
        /// and prefixed).
        #[arg(long, value_name = "ID")]
        session_id: String,
        /// Required to actually archive — the copy is UNREDACTED. Without
        /// it, memhub prompts on an interactive terminal and refuses
        /// (fails closed) on a non-TTY.
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum AuditCommand {
    /// Check `CLAUDE.md` / `AGENTS.md` for drift and bloat: token-budget
    /// size, `AGENTS.md == generate_agents_md(CLAUDE.md)` parity, the
    /// managed-block pointer, and the N4 keystone phrases. Exit 0 by
    /// default (findings printed either way); `--strict` exits 1 iff at
    /// least one finding fired. Read-only: no DB writes.
    Md {
        #[arg(long)]
        json: bool,
        /// Exit nonzero if any finding fired (severity is not
        /// distinguished for this purpose — any finding counts).
        #[arg(long)]
        strict: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum CodeCommand {
    /// Bring the index in line with the working tree (lazy staleness diff).
    /// `--rebuild` drops and rebuilds the whole index from scratch.
    Index {
        /// Drop the sibling DB and rebuild every chunk from scratch.
        #[arg(long)]
        rebuild: bool,
        #[arg(long)]
        json: bool,
    },
    /// Show index counts, schema version, and HEAD staleness. Read-only;
    /// never creates the index.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Delete the sibling code index. It is a regenerable cache, so this is
    /// a wipe, not data loss — rebuild with `memhub code index`.
    Rm {
        #[arg(long)]
        json: bool,
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
    /// Recall@1/@K harness for the M11 code locator over the sibling code
    /// index (task 65, decision 107). A/B the cross-encoder reranker on code
    /// against FTS+vector fusion: run once plain, once with `--rerank`.
    Locate {
        /// Code golden set (defaults to tests/code_locate_golden.json).
        #[arg(long, value_name = "PATH")]
        golden: Option<PathBuf>,
        #[arg(long, default_value_t = commands::eval::DEFAULT_K)]
        k: usize,
        /// Run the bundled cross-encoder reranker over the candidate pool.
        /// Off by default (mirrors `memhub locate`); the whole point of this
        /// harness is to measure whether flipping it on helps on code.
        #[arg(long)]
        rerank: bool,
        /// Harness-side cross-encoder floor: drop returned hits whose rerank
        /// logit is below this before scoring. Ignored without `--rerank`.
        /// Sweep it to decide whether locate needs a nonsense-rejection floor.
        /// Use the `=` form for negative values: `--min-rerank-score=-2`.
        #[arg(long, value_name = "F")]
        min_rerank_score: Option<f32>,
        #[arg(long)]
        json: bool,
    },
}

#[cfg(feature = "metrics")]
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
    /// Calibrate the cl100k token estimate against Anthropic's real
    /// tokenizer (one-time; the only command that uses the network).
    /// Reads ANTHROPIC_API_KEY from the environment and sends a fixed
    /// bundled corpus — never your project's content — to count_tokens.
    Calibrate {
        /// Model id for the count_tokens request (default: a current
        /// Claude model; the tokenizer is shared across the family).
        #[arg(long)]
        model: Option<String>,
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

/// Cross-machine Drive sync (M10). All subcommands operate on **local
/// files only** — an OS-level synced folder (Drive for Desktop / rclone
/// mount) moves the snapshot between machines out of band.
#[derive(Debug, Subcommand)]
pub enum SyncCommand {
    /// Write a consistent single-file DB snapshot + manifest.json into
    /// the given directory (typically inside the synced folder, which
    /// carries it to your other machines).
    Snapshot {
        /// Output directory; `project.sqlite` and `manifest.json` are
        /// written inside it. Omit to use the canonical
        /// `<drive_subpath>/memhub/<project_id>` from config.
        out_dir: Option<PathBuf>,
        /// Overwrite the remote even when it is drive-ahead of or
        /// diverged from local (last-writer-wins). Without it a push
        /// refuses rather than clobber newer remote state.
        #[arg(long)]
        force: bool,
        #[arg(long)]
        json: bool,
    },
    /// Opt this repo into cross-machine sync.
    Enable {
        #[arg(long)]
        json: bool,
    },
    /// Opt this repo back out. Non-destructive.
    Disable {
        #[arg(long)]
        json: bool,
    },
    /// Show enablement, the Drive-folder project id, local logical
    /// version, and the last-sync marker. No Drive comparison.
    Status {
        #[arg(long)]
        json: bool,
    },
    /// Compare the local DB against the Drive-synced snapshot and
    /// report the fast-forward verdict (up-to-date / local-ahead /
    /// drive-ahead / diverged). Reads only the manifest.
    Check {
        /// Directory holding the downloaded `project.sqlite` +
        /// `manifest.json` (or a path to `manifest.json`). Omit to use
        /// the canonical `<drive_subpath>/memhub/<project_id>` from config.
        remote: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Replace the local DB with the Drive-synced snapshot. Requires
    /// `--yes`; refuses on project-id mismatch, a newer snapshot
    /// schema, or a checksum that disagrees with the manifest.
    Adopt {
        /// Directory holding the downloaded `project.sqlite` +
        /// `manifest.json` (or a path to `manifest.json`). Omit to use
        /// the canonical `<drive_subpath>/memhub/<project_id>` from config.
        remote: Option<PathBuf>,
        /// Confirm the destructive overwrite of the local DB.
        #[arg(long)]
        yes: bool,
        #[arg(long)]
        json: bool,
    },
    /// Record that the local DB now equals a just-pushed snapshot, so
    /// the next `status` reads up-to-date. Call after a successful
    /// push (snapshot written into the synced folder).
    Commit {
        /// Directory holding the pushed `project.sqlite` +
        /// `manifest.json` (or a path to `manifest.json`). Omit to use
        /// the canonical `<drive_subpath>/memhub/<project_id>` from config.
        remote: Option<PathBuf>,
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
    Note,
}

impl RecallSourceTypeArg {
    pub(crate) fn to_source_type(&self) -> SourceType {
        match self {
            Self::Fact => SourceType::Fact,
            Self::Decision => SourceType::Decision,
            Self::Task => SourceType::Task,
            Self::Doc => SourceType::DocChunk,
            Self::Note => SourceType::Note,
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
        /// Acknowledge a detected contradiction by retiring the named row in
        /// favor of this write (L3 demote-with-link). A fact id or key; a
        /// numeric decision id. Also the escape when the accept-time probe
        /// (issue #48) blocks on a reranked conflict.
        #[arg(long)]
        supersede: Option<String>,
        /// Proceed even if the accept-time probe detects a contradiction
        /// (overwrite/insert as-is; for a same-key fact the prior value is
        /// logged to writes_log). The escape for a same-key overwrite, where
        /// --supersede cannot apply.
        #[arg(long)]
        force: bool,
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
    /// Read-only lifecycle audit queue (Wave 3 L4): facts near the
    /// staleness horizon, done tasks aged past threshold, expired
    /// pending writes, and docs whose on-disk hash no longer matches
    /// `documents.content_hash`. Mutates nothing — each row only
    /// suggests the existing verb that addresses it.
    Stale {
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum FactCommand {
    Add {
        key: String,
        value: String,
        #[arg(long, default_value = "user")]
        source: String,
        /// Optional lightweight tag for the writing agent (issue #97).
        /// Purely additive and unenforced -- any non-empty string is
        /// accepted, no CHECK constraint. Suggested vocabulary: gotcha,
        /// env, preference, command, constraint.
        #[arg(long)]
        kind: Option<String>,
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
    List {
        #[arg(long)]
        json: bool,
    },
    /// Refresh a fact's `verified_at` to now. Touches nothing else
    /// durable — no confidence reset, no source rewrite, no
    /// add-upsert dedupe path (unlike `fact add`). Accepts a numeric
    /// id or an exact key. CLI only — never exposed over MCP, since
    /// agent self-verification is exactly what the untrusted-writer
    /// guardrail forbids.
    Verify {
        ident: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        actor: Option<String>,
    },
    /// Mark a fact superseded by another fact — demote-with-link, no-loss
    /// (Wave 3 L3). The old fact is NOT deleted: it stays present, is
    /// tagged with the replacement id, penalized in recall, and annotated
    /// in render. Both `<old>` and `--by <new>` accept a numeric id or an
    /// exact key. Durable + user-gated (CLI only); the MCP surface can only
    /// stage a `propose_supersede` for `memhub review accept`.
    Supersede {
        /// The fact being retired (numeric id or exact key).
        old: String,
        /// The fact that replaces it (numeric id or exact key).
        #[arg(long = "by", value_name = "NEW")]
        by: String,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        actor: Option<String>,
    },
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
    /// Retire a decision by supersession — demote-with-link, no-loss (Wave
    /// 3 L3, Q2: decisions retire by supersession, not age). The old
    /// decision's status flips to 'superseded' and links to the new one;
    /// it is NOT deleted (still rendered, still recallable but penalized)
    /// and drops out of the active-decisions list. Durable + user-gated
    /// (CLI only); MCP can only stage a `propose_supersede`.
    Supersede {
        /// The decision being retired (numeric id).
        old: i64,
        /// The decision that replaces it (numeric id).
        #[arg(long = "by", value_name = "NEW")]
        by: i64,
        #[arg(long)]
        json: bool,
        #[arg(long)]
        actor: Option<String>,
    },
    List {
        #[arg(long)]
        json: bool,
    },
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
        #[arg(long)]
        json: bool,
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
    List {
        #[arg(long)]
        json: bool,
    },
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
