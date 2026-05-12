use std::path::Path;

use serde::{Deserialize, Serialize};

pub const DEFAULT_AGENT_DOCS_PATH: &str = "agent_docs";
pub const K9_DETECTION_FILENAME: &str = "project_state.md";

fn default_agent_docs_path() -> String {
    DEFAULT_AGENT_DOCS_PATH.to_string()
}

fn default_true() -> bool {
    true
}

#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct IntegrationsConfig {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub k9: Option<K9Config>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct K9Config {
    #[serde(default = "default_true")]
    pub enabled: bool,
    #[serde(default = "default_agent_docs_path")]
    pub agent_docs_path: String,
}

impl Default for K9Config {
    fn default() -> Self {
        Self {
            enabled: true,
            agent_docs_path: DEFAULT_AGENT_DOCS_PATH.to_string(),
        }
    }
}

pub fn detect_k9(repo_root: &Path, agent_docs_path: &str) -> bool {
    repo_root
        .join(agent_docs_path)
        .join(K9_DETECTION_FILENAME)
        .is_file()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use tempfile::tempdir;

    #[test]
    fn detect_returns_false_for_empty_repo() {
        let temp = tempdir().expect("tempdir");
        assert!(!detect_k9(temp.path(), DEFAULT_AGENT_DOCS_PATH));
    }

    #[test]
    fn detect_returns_true_when_canonical_file_present() {
        let temp = tempdir().expect("tempdir");
        let dir = temp.path().join("agent_docs");
        fs::create_dir_all(&dir).expect("create dir");
        fs::write(dir.join(K9_DETECTION_FILENAME), "# state").expect("write file");

        assert!(detect_k9(temp.path(), DEFAULT_AGENT_DOCS_PATH));
    }

    #[test]
    fn detect_honors_custom_agent_docs_path() {
        let temp = tempdir().expect("tempdir");
        let dir = temp.path().join("docs/k9");
        fs::create_dir_all(&dir).expect("create dir");
        fs::write(dir.join(K9_DETECTION_FILENAME), "# state").expect("write file");

        assert!(!detect_k9(temp.path(), DEFAULT_AGENT_DOCS_PATH));
        assert!(detect_k9(temp.path(), "docs/k9"));
    }

    #[test]
    fn detect_returns_false_when_only_directory_exists() {
        let temp = tempdir().expect("tempdir");
        let dir = temp.path().join("agent_docs");
        fs::create_dir_all(&dir).expect("create dir");

        assert!(!detect_k9(temp.path(), DEFAULT_AGENT_DOCS_PATH));
    }

    #[test]
    fn k9_config_serializes_round_trip() {
        let cfg = K9Config {
            enabled: false,
            agent_docs_path: "custom".to_string(),
        };
        let raw = toml::to_string(&cfg).expect("serialize");
        let parsed: K9Config = toml::from_str(&raw).expect("deserialize");
        assert!(!parsed.enabled);
        assert_eq!(parsed.agent_docs_path, "custom");
    }
}
