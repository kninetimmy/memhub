pub mod deny;
pub mod integrations;

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub use deny::{default_patterns, DenyList, PathMatcher};
pub use integrations::{
    detect_k9, IntegrationsConfig, K9Config, DEFAULT_AGENT_DOCS_PATH, K9_DETECTION_FILENAME,
};

use crate::Result;

pub const DEFAULT_RENDER_OUTPUT_DIR: &str = ".memhub/rendered";

pub const DEFAULT_RECALL_MAX_RESULTS: usize = 6;
pub const DEFAULT_FTS_WEIGHT: f64 = 0.5;
pub const DEFAULT_VECTOR_WEIGHT: f64 = 0.5;
pub const DEFAULT_STALE_PENALTY: f64 = 0.3;
/// Default cross-encoder score floor for hybrid-mode candidates after
/// re-ranking. Calibrated empirically against memhub's own golden set
/// (decision 71, task #22): the gibberish safety probe rerank-scores at
/// ~+1.25; the next legitimate match drops out at 2.5. 2.0 sits in the
/// middle of the safe band [1.5, 2.4]. Gives parity with the retired
/// `min_vector_score = 0.7` floor on R@3 and safety probe pass.
pub const DEFAULT_MIN_RERANK_SCORE: f32 = 2.0;
pub const DEFAULT_ACCEPTED_ONLY: bool = false;
pub const DEFAULT_INCLUDE_STALE: bool = false;
pub const DEFAULT_USE_RERANKER: bool = true;
pub const DEFAULT_RERANK_CANDIDATE_POOL: usize = 20;

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
    /// Minimum cross-encoder relevance score for a candidate to survive
    /// the re-rank pass. MiniLM gives positive logits to relevant docs
    /// and negative logits to nonsense; a floor near 0 cleanly separates
    /// the two without the cosine-band overlap that doomed the legacy
    /// `min_vector_score` knob (decisions 70, 71). Ignored in fts mode
    /// and when `use_reranker = false`.
    #[serde(default = "default_min_rerank_score")]
    pub min_rerank_score: f32,
}

impl Default for RetrievalScoringConfig {
    fn default() -> Self {
        Self {
            fts_weight: DEFAULT_FTS_WEIGHT,
            vector_weight: DEFAULT_VECTOR_WEIGHT,
            stale_penalty: DEFAULT_STALE_PENALTY,
            min_rerank_score: DEFAULT_MIN_RERANK_SCORE,
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
fn default_min_rerank_score() -> f32 {
    DEFAULT_MIN_RERANK_SCORE
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
            use_reranker: DEFAULT_USE_RERANKER,
            rerank_candidate_pool: DEFAULT_RERANK_CANDIDATE_POOL,
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
