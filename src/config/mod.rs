pub mod deny;
pub mod integrations;

use std::fs;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

pub use deny::{DenyList, PathMatcher, default_patterns};
pub use integrations::{
    DEFAULT_AGENT_DOCS_PATH, IntegrationsConfig, K9_DETECTION_FILENAME, K9Config, detect_k9,
};

use crate::Result;

pub const DEFAULT_RENDER_OUTPUT_DIR: &str = ".memhub/rendered";

pub const DEFAULT_RECALL_MAX_RESULTS: usize = 6;
pub const DEFAULT_FTS_WEIGHT: f64 = 0.5;
pub const DEFAULT_VECTOR_WEIGHT: f64 = 0.5;
pub const DEFAULT_STALE_PENALTY: f64 = 0.3;
/// Blended-score demotion applied to a superseded fact/decision in recall
/// (Wave 3 L3, decision 145's demote-with-link ruling). A superseded row is
/// never excluded — it is kept, tagged `superseded_by: N`, and pushed down.
/// Set above `DEFAULT_STALE_PENALTY` (0.3) because supersession is an
/// explicit "this was replaced" signal, stronger than mere age, yet still a
/// demotion rather than a filter. It stacks additively with the stale
/// penalty (a row that is both stale and superseded sinks furthest) and,
/// like the stale penalty, is a peer, independent signal in `score()`.
pub const DEFAULT_SUPERSEDED_PENALTY: f64 = 0.4;
/// Default cross-encoder score floor for hybrid-mode candidates after
/// re-ranking. Calibrated empirically against memhub's own golden set
/// (decision 71, task #22): the gibberish safety probe rerank-scores at
/// ~+1.25; the next legitimate match drops out at 2.5. 2.0 sits in the
/// middle of the safe band [1.5, 2.4]. Gives parity with the retired
/// `min_vector_score = 0.7` floor on R@3 and safety probe pass.
pub const DEFAULT_MIN_RERANK_SCORE: f32 = 2.0;
/// Cross-encoder floor a doc chunk must clear to enter the *default*
/// recall bundle when `include_docs_in_default` is on (decision
/// extending 86). Calibrated empirically (recall.rs
/// `doc_default_recall_floor_routes_by_task_relevance`): an on-topic
/// doc chunk reranks around +1.6 while off-topic chunks sit near
/// −11, so 0.0 — the ms-marco-MiniLM relevant/irrelevant sign
/// boundary — cleanly routes by task with wide margin both ways.
/// Note this is *below* `DEFAULT_MIN_RERANK_SCORE` (2.0): doc chunks
/// rerank in a lower band than facts/decisions, so a "stricter =
/// higher" floor would wrongly filter genuinely relevant docs.
/// Anti-displacement comes from the deeply negative off-topic
/// scores, not a high threshold. Only consulted for chunks that
/// entered via the default-inclusion path; explicit
/// `--source-type doc` keeps the normal `min_rerank_score`.
pub const DEFAULT_DOC_MIN_RERANK_SCORE: f32 = 0.0;
pub const DEFAULT_ACCEPTED_ONLY: bool = false;
/// Default stale-fact handling in recall. `true` = keep aged facts in the
/// result set but **demote** them (`scoring.stale_penalty`) and flag them
/// `stale: true`, rather than silently dropping them. This is the Q1
/// currency ruling (decision 145): "demote + flag, not silent exclusion —
/// a bad default hides valid memories; demote is the no-loss posture."
/// Set to `false` to restore hard exclusion of stale facts. The staleness
/// horizon itself is `DEFAULT_FACT_STALE_AFTER_DAYS`.
pub const DEFAULT_INCLUDE_STALE: bool = true;
/// Recall's fact-staleness horizon in days. A fact is stale (and therefore
/// demoted, per `DEFAULT_INCLUDE_STALE`) when it has never been verified or
/// was last verified more than this many days ago. Kept identical to the
/// long-standing hardcoded window (`models::FACT_STALE_AFTER_DAYS`, which
/// still drives the non-recall fact-hygiene surfaces — `fact list`, render,
/// stats) so promoting it to `[retrieval] fact_stale_after_days` does not
/// move the horizon, only how stale rows are handled.
pub const DEFAULT_FACT_STALE_AFTER_DAYS: i64 = crate::models::FACT_STALE_AFTER_DAYS;
pub const DEFAULT_USE_RERANKER: bool = true;
pub const DEFAULT_RERANK_CANDIDATE_POOL: usize = 20;
/// Docs are opt-in to default recall (decision 86). Auto-flipped to
/// true by the first successful `memhub doc add` in a project so the
/// user-pointed write that establishes docs also wires up retrieval.
pub const DEFAULT_INCLUDE_DOCS_IN_DEFAULT: bool = false;

/// Token-accounting subsystem defaults. Master switch ships off so
/// new installs and pre-decision-74 installs stay silent until the
/// user opts in via `memhub metrics enable`. Sub-switches default on
/// so a single `enable` lights up both component A (recall proxy) and
/// component B (transcript scraper); B can be disabled independently
/// if the transcript shape shifts. See decision 74.
pub const DEFAULT_METRICS_ENABLED: bool = false;
pub const DEFAULT_METRICS_RECALL_PROXY: bool = true;
pub const DEFAULT_METRICS_SESSION_ACCOUNTING: bool = true;
pub const DEFAULT_METRICS_TOKENIZER: &str = "tiktoken-cl100k";
pub const DEFAULT_METRICS_RETENTION_DAYS: u32 = 90;
/// Tokenizer calibration multiplier (task 63). `1.0` is an uncalibrated
/// passthrough; `memhub metrics calibrate` measures and writes back the
/// real cl100k→Anthropic ratio. Per-machine, never committed.
pub const DEFAULT_METRICS_CALIBRATION_FACTOR: f64 = 1.0;

/// Machine-global memory (M9). Off by default and per-repo: a repo
/// opts into reading from / writing to the machine-wide
/// `~/.memhub/global.sqlite` store via `memhub global enable`. The
/// global store itself is machine-wide; this flag is the per-repo
/// consumption + write gate. `include_docs_in_default` mirrors the
/// repo-scoped flag and auto-flips on the first `doc add --global`.
pub const DEFAULT_GLOBAL_ENABLED: bool = false;
pub const DEFAULT_GLOBAL_INCLUDE_DOCS_IN_DEFAULT: bool = false;

/// Cross-machine Drive sync (M10). Off by default and per-repo: a repo
/// opts in via `memhub sync enable`. memhub itself stays offline — this
/// only governs the local-file `sync snapshot|status|adopt|commit`
/// commands; the agent's Drive access is the transport. `project_id` is
/// normally derived from the git remote URL and left empty here; it is
/// only set when a repo has no git remote and the operator must pin an
/// identity for the Drive folder. See addendum
/// `docs/reference/memhub-prd-addendum-m10-drive-sync.md`.
pub const DEFAULT_SYNC_ENABLED: bool = false;

#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum RetrievalMode {
    /// FTS5-only recall. Embeddings table is not populated on writes.
    #[default]
    Fts,
    /// Hybrid SQL+RAG recall. Writes eagerly embed source rows.
    Hybrid,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalScoringConfig {
    #[serde(default = "default_fts_weight")]
    pub fts_weight: f64,
    #[serde(default = "default_vector_weight")]
    pub vector_weight: f64,
    #[serde(default = "default_stale_penalty")]
    pub stale_penalty: f64,
    /// Blended-score demotion for a superseded fact/decision (Wave 3 L3).
    /// A second, independent demotion signal alongside `stale_penalty`:
    /// superseded rows are demoted (never excluded) and tagged
    /// `superseded_by: N`. Defaults to `DEFAULT_SUPERSEDED_PENALTY`.
    #[serde(default = "default_superseded_penalty")]
    pub superseded_penalty: f64,
    /// Minimum cross-encoder relevance score for a candidate to survive
    /// the re-rank pass. MiniLM gives positive logits to relevant docs
    /// and negative logits to nonsense; a floor near 0 cleanly separates
    /// the two without the cosine-band overlap that doomed the legacy
    /// `min_vector_score` knob (decisions 70, 71). Ignored in fts mode
    /// and when `use_reranker = false`.
    #[serde(default = "default_min_rerank_score")]
    pub min_rerank_score: f32,
    /// Cross-encoder floor a doc chunk must clear to survive into the
    /// *default* bundle when `[retrieval] include_docs_in_default` is
    /// on. Defaults to 0.0 — the cross-encoder's own relevance sign
    /// boundary — which routes on-topic docs in and off-topic docs
    /// out by a wide margin (see DEFAULT_DOC_MIN_RERANK_SCORE).
    /// Ignored for explicit `--source-type doc` scopes (those use the
    /// normal floor) and whenever the re-ranker does not run.
    #[serde(default = "default_doc_min_rerank_score")]
    pub doc_min_rerank_score: f32,
}

impl Default for RetrievalScoringConfig {
    fn default() -> Self {
        Self {
            fts_weight: DEFAULT_FTS_WEIGHT,
            vector_weight: DEFAULT_VECTOR_WEIGHT,
            stale_penalty: DEFAULT_STALE_PENALTY,
            superseded_penalty: DEFAULT_SUPERSEDED_PENALTY,
            min_rerank_score: DEFAULT_MIN_RERANK_SCORE,
            doc_min_rerank_score: DEFAULT_DOC_MIN_RERANK_SCORE,
        }
    }
}

fn default_fts_weight() -> f64 {
    DEFAULT_FTS_WEIGHT
}
fn default_vector_weight() -> f64 {
    DEFAULT_VECTOR_WEIGHT
}
fn default_stale_penalty() -> f64 {
    DEFAULT_STALE_PENALTY
}
fn default_superseded_penalty() -> f64 {
    DEFAULT_SUPERSEDED_PENALTY
}
fn default_min_rerank_score() -> f32 {
    DEFAULT_MIN_RERANK_SCORE
}
fn default_doc_min_rerank_score() -> f32 {
    DEFAULT_DOC_MIN_RERANK_SCORE
}
fn default_include_docs_in_default() -> bool {
    DEFAULT_INCLUDE_DOCS_IN_DEFAULT
}
fn default_max_results() -> usize {
    DEFAULT_RECALL_MAX_RESULTS
}
fn default_accepted_only() -> bool {
    DEFAULT_ACCEPTED_ONLY
}
fn default_include_stale() -> bool {
    DEFAULT_INCLUDE_STALE
}
fn default_fact_stale_after_days() -> i64 {
    DEFAULT_FACT_STALE_AFTER_DAYS
}
fn default_use_reranker() -> bool {
    DEFAULT_USE_RERANKER
}
fn default_rerank_candidate_pool() -> usize {
    DEFAULT_RERANK_CANDIDATE_POOL
}
fn default_metrics_enabled() -> bool {
    DEFAULT_METRICS_ENABLED
}
fn default_metrics_recall_proxy() -> bool {
    DEFAULT_METRICS_RECALL_PROXY
}
fn default_metrics_session_accounting() -> bool {
    DEFAULT_METRICS_SESSION_ACCOUNTING
}
fn default_metrics_transcripts_dir() -> String {
    String::new()
}
fn default_metrics_tokenizer() -> String {
    DEFAULT_METRICS_TOKENIZER.to_string()
}
fn default_metrics_retention_days() -> u32 {
    DEFAULT_METRICS_RETENTION_DAYS
}
fn default_metrics_calibration_factor() -> f64 {
    DEFAULT_METRICS_CALIBRATION_FACTOR
}
fn default_global_enabled() -> bool {
    DEFAULT_GLOBAL_ENABLED
}
fn default_global_include_docs_in_default() -> bool {
    DEFAULT_GLOBAL_INCLUDE_DOCS_IN_DEFAULT
}
fn default_sync_enabled() -> bool {
    DEFAULT_SYNC_ENABLED
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RetrievalConfig {
    #[serde(default)]
    pub mode: RetrievalMode,
    #[serde(default = "default_max_results")]
    pub default_max_results: usize,
    #[serde(default = "default_accepted_only")]
    pub accepted_only_by_default: bool,
    #[serde(default = "default_include_stale")]
    pub include_stale_by_default: bool,
    /// Age in days after which a fact is treated as stale by recall — never
    /// verified, or last verified more than this many days ago. Stale facts
    /// are kept but demoted (`scoring.stale_penalty`) and flagged
    /// `stale: true` when `include_stale_by_default` is on (the default),
    /// or excluded when it is off. Defaults to the established 90-day window
    /// (`DEFAULT_FACT_STALE_AFTER_DAYS`); this key promotes that formerly
    /// hardcoded horizon to config without changing its length.
    #[serde(default = "default_fact_stale_after_days")]
    pub fact_stale_after_days: i64,
    /// Apply the bundled cross-encoder re-ranker (ms-marco-MiniLM-L-6-v2)
    /// to hybrid recall results. Adds ~275 ms per recall at pool=20 and
    /// lifts Recall@1 by ~17pp on memhub's own golden set (decision 68).
    /// Ignored in fts mode. On by default; set to `false` to skip.
    #[serde(default = "default_use_reranker")]
    pub use_reranker: bool,
    /// Number of top-blended candidates to feed into the cross-encoder
    /// before the final truncate to `max_results`. Only consulted when
    /// `use_reranker = true` and mode = hybrid.
    #[serde(default = "default_rerank_candidate_pool")]
    pub rerank_candidate_pool: usize,
    /// When true, plain `memhub recall` (no `--source-type`) makes
    /// ingested doc chunks eligible for the default bundle, gated by
    /// `scoring.doc_min_rerank_score`. Off by default (decision 86);
    /// the first successful `memhub doc add` in a project flips this
    /// to true in that repo's `.memhub/config.toml`. Explicit
    /// `--source-type doc` recall is unaffected by this flag.
    #[serde(default = "default_include_docs_in_default")]
    pub include_docs_in_default: bool,
    #[serde(default)]
    pub scoring: RetrievalScoringConfig,
}

impl Default for RetrievalConfig {
    fn default() -> Self {
        Self {
            mode: RetrievalMode::default(),
            default_max_results: DEFAULT_RECALL_MAX_RESULTS,
            accepted_only_by_default: DEFAULT_ACCEPTED_ONLY,
            include_stale_by_default: DEFAULT_INCLUDE_STALE,
            fact_stale_after_days: DEFAULT_FACT_STALE_AFTER_DAYS,
            use_reranker: DEFAULT_USE_RERANKER,
            rerank_candidate_pool: DEFAULT_RERANK_CANDIDATE_POOL,
            include_docs_in_default: DEFAULT_INCLUDE_DOCS_IN_DEFAULT,
            scoring: RetrievalScoringConfig::default(),
        }
    }
}

/// Opt-in token-accounting config (decision 74). Off by default;
/// users opt in per machine via `memhub metrics enable`. Component A
/// (recall_proxy) is local arithmetic over recall responses; component
/// B (session_accounting) scrapes agent transcript JSONL for real
/// input/output/cache token totals. Transcript dirs are auto-resolved
/// on first enable and written back to the local config; an empty
/// string means "not yet resolved".
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MetricsConfig {
    #[serde(default = "default_metrics_enabled")]
    pub enabled: bool,
    #[serde(default = "default_metrics_recall_proxy")]
    pub recall_proxy: bool,
    #[serde(default = "default_metrics_session_accounting")]
    pub session_accounting: bool,
    #[serde(default = "default_metrics_transcripts_dir")]
    pub claude_transcripts_dir: String,
    #[serde(default = "default_metrics_transcripts_dir")]
    pub codex_transcripts_dir: String,
    #[serde(default = "default_metrics_tokenizer")]
    pub tokenizer: String,
    #[serde(default = "default_metrics_retention_days")]
    pub retention_days: u32,
    /// Multiplier applied to every cl100k token estimate to approximate
    /// Anthropic's real tokenizer (task 63). `1.0` is uncalibrated;
    /// `memhub metrics calibrate` measures and writes back the ratio.
    /// Per-machine and never committed — calibration is a property of the
    /// local binary's tokenizer, not of the repo.
    #[serde(default = "default_metrics_calibration_factor")]
    pub calibration_factor: f64,
}

impl Default for MetricsConfig {
    fn default() -> Self {
        Self {
            enabled: DEFAULT_METRICS_ENABLED,
            recall_proxy: DEFAULT_METRICS_RECALL_PROXY,
            session_accounting: DEFAULT_METRICS_SESSION_ACCOUNTING,
            claude_transcripts_dir: String::new(),
            codex_transcripts_dir: String::new(),
            tokenizer: DEFAULT_METRICS_TOKENIZER.to_string(),
            retention_days: DEFAULT_METRICS_RETENTION_DAYS,
            calibration_factor: DEFAULT_METRICS_CALIBRATION_FACTOR,
        }
    }
}

/// Opt-in machine-global memory config (M9). Per-repo; off by default.
/// When `enabled`, recall in this repo merges hits from
/// `~/.memhub/global.sqlite` (tagged `scope: "global"`) and
/// `--global` writes / accepted global proposals are permitted.
/// Disabled or store-absent → recall is byte-identical to pre-M9.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalConfig {
    #[serde(default = "default_global_enabled")]
    pub enabled: bool,
    /// Mirrors `[retrieval] include_docs_in_default` for the global
    /// corpus. Canonical baseline false; the first successful
    /// `memhub doc add --global` flips the local config to true so the
    /// user-pointed write that establishes global docs also wires up
    /// their default-bundle retrieval.
    #[serde(default = "default_global_include_docs_in_default")]
    pub include_docs_in_default: bool,
}

impl Default for GlobalConfig {
    fn default() -> Self {
        Self {
            enabled: DEFAULT_GLOBAL_ENABLED,
            include_docs_in_default: DEFAULT_GLOBAL_INCLUDE_DOCS_IN_DEFAULT,
        }
    }
}

/// Opt-in cross-machine Drive sync config (M10). Per-repo; off by
/// default. When `enabled`, the `memhub sync` family operates on local
/// files (snapshot/status/adopt/commit); an OS-level synced folder
/// (Google Drive for Desktop, or an rclone mount on Linux) moves the
/// snapshot between machines out of band. memhub never makes a network
/// call. Disabled → the `sync` subcommands refuse with a hint to run
/// `memhub sync enable`.
///
/// `project_id` overrides the git-remote-derived Drive folder identity
/// and is only needed for a repo with no git remote; empty means
/// "derive from the git remote URL". `drive_subpath` is a human-facing
/// hint for the skill (where under the user's Drive the memhub folder
/// lives); memhub does not read or resolve it — the agent does.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncConfig {
    #[serde(default = "default_sync_enabled")]
    pub enabled: bool,
    #[serde(default)]
    pub project_id: String,
    #[serde(default)]
    pub drive_subpath: String,
}

impl Default for SyncConfig {
    fn default() -> Self {
        Self {
            enabled: DEFAULT_SYNC_ENABLED,
            project_id: String::new(),
            drive_subpath: String::new(),
        }
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RenderConfig {
    #[serde(default = "default_render_output_dir")]
    pub output_dir: String,
}

impl Default for RenderConfig {
    fn default() -> Self {
        Self {
            output_dir: default_render_output_dir(),
        }
    }
}

fn default_render_output_dir() -> String {
    DEFAULT_RENDER_OUTPUT_DIR.to_string()
}

/// MCP-only path confinement for the agent-facing `doc_add` tool
/// (Wave-0 F11, decision Q39). `mcp::doc_add_impl` canonicalizes the
/// agent-supplied path and accepts it only when it resolves under the
/// repo root or one of these entries, with the repo's `deny_list`
/// still applied on top. The CLI `memhub doc add` path is user-typed
/// and is NOT gated by this list — it stays unrestricted. Default
/// empty: an untouched install is pure repo-root confinement, byte-
/// identical to a config with no `[doc]` section at all.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct DocConfig {
    #[serde(default)]
    pub allowed_dirs: Vec<PathBuf>,
}

/// Opt-in config for `memhub audit md` (Wave 2 C5, issue #32 / decision
/// Q25): when `user_md_path` is set, the audit also size-checks that
/// user-global orientation file (e.g. `~/.claude/CLAUDE.md`) alongside
/// this repo's own `CLAUDE.md`. Empty string means "unset" (matching
/// the `sync`/`metrics` transcripts-dir convention elsewhere in this
/// struct) — `memhub audit md` never reads outside the repo unless this
/// is explicitly set. Per-machine: do NOT commit a real path back into
/// `.memhub/config.example.toml`.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct AuditConfig {
    #[serde(default)]
    pub user_md_path: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub project_name: String,
    pub auto_sync_md: bool,
    pub log_level: String,
    #[serde(default)]
    pub deny_list: DenyList,
    #[serde(default)]
    pub integrations: IntegrationsConfig,
    #[serde(default)]
    pub render: RenderConfig,
    #[serde(default)]
    pub retrieval: RetrievalConfig,
    #[serde(default)]
    pub metrics: MetricsConfig,
    #[serde(default)]
    pub global: GlobalConfig,
    #[serde(default)]
    pub sync: SyncConfig,
    #[serde(default)]
    pub doc: DocConfig,
    #[serde(default)]
    pub audit: AuditConfig,
}

impl ProjectConfig {
    pub fn default_for_repo_name(repo_name: &str) -> Self {
        Self {
            project_name: repo_name.to_string(),
            auto_sync_md: false,
            log_level: "info".to_string(),
            deny_list: DenyList::default(),
            integrations: IntegrationsConfig::default(),
            render: RenderConfig::default(),
            retrieval: RetrievalConfig::default(),
            metrics: MetricsConfig::default(),
            global: GlobalConfig::default(),
            sync: SyncConfig::default(),
            doc: DocConfig::default(),
            audit: AuditConfig::default(),
        }
    }

    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)?;
        Ok(toml::from_str(&raw)?)
    }

    pub fn save(&self, path: &Path) -> Result<()> {
        let raw = toml::to_string_pretty(self)?;
        fs::write(path, raw)?;
        Ok(())
    }
}
