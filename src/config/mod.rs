pub mod deny;
pub mod integrations;

use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

pub use deny::{DenyList, PathMatcher, default_patterns};
pub use integrations::{
    DEFAULT_AGENT_DOCS_PATH, IntegrationsConfig, K9_DETECTION_FILENAME, K9Config, detect_k9,
};

use crate::Result;

pub const DEFAULT_RENDER_OUTPUT_DIR: &str = "agent_docs";

pub const DEFAULT_RECALL_MAX_RESULTS: usize = 6;
pub const DEFAULT_FTS_WEIGHT: f64 = 0.5;
pub const DEFAULT_VECTOR_WEIGHT: f64 = 0.5;
pub const DEFAULT_STALE_PENALTY: f64 = 0.3;
pub const DEFAULT_ACCEPTED_ONLY: bool = false;
pub const DEFAULT_INCLUDE_STALE: bool = false;

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
}

impl Default for RetrievalScoringConfig {
    fn default() -> Self {
        Self {
            fts_weight: DEFAULT_FTS_WEIGHT,
            vector_weight: DEFAULT_VECTOR_WEIGHT,
            stale_penalty: DEFAULT_STALE_PENALTY,
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
fn default_max_results() -> usize {
    DEFAULT_RECALL_MAX_RESULTS
}
fn default_accepted_only() -> bool {
    DEFAULT_ACCEPTED_ONLY
}
fn default_include_stale() -> bool {
    DEFAULT_INCLUDE_STALE
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
