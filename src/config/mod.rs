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
pub const DEFAULT_MIN_VECTOR_SCORE: f64 = 0.7;
pub const DEFAULT_ACCEPTED_ONLY: bool = false;
pub const DEFAULT_INCLUDE_STALE: bool = false;
pub const DEFAULT_USE_RERANKER: bool = true;
pub const DEFAULT_RERANK_CANDIDATE_POOL: usize = 20;

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
    /// Minimum cosine similarity for a row to enter the candidate set via
    /// the vector path. Rows below this floor are dropped before scoring,
    /// so pure-nonsense queries don't surface low-confidence vector noise
    /// in hybrid mode. FTS hits are unaffected. Ignored in fts mode.
    #[serde(default = "default_min_vector_score")]
    pub min_vector_score: f64,
}

impl Default for RetrievalScoringConfig {
    fn default() -> Self {
        Self {
            fts_weight: DEFAULT_FTS_WEIGHT,
            vector_weight: DEFAULT_VECTOR_WEIGHT,
            stale_penalty: DEFAULT_STALE_PENALTY,
            min_vector_score: DEFAULT_MIN_VECTOR_SCORE,
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
fn default_min_vector_score() -> f64 {
    DEFAULT_MIN_VECTOR_SCORE
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
