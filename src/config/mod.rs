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
