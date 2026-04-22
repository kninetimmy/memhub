use std::fs;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::Result;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectConfig {
    pub project_name: String,
    pub auto_sync_md: bool,
    pub log_level: String,
}

impl ProjectConfig {
    pub fn default_for_repo_name(repo_name: &str) -> Self {
        Self {
            project_name: repo_name.to_string(),
            auto_sync_md: false,
            log_level: "info".to_string(),
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
