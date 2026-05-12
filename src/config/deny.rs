use globset::{Glob, GlobSet, GlobSetBuilder};
use serde::{Deserialize, Serialize};

use crate::{MemhubError, Result};

const DEFAULT_PATTERNS: &[&str] = &[
    ".env",
    ".env.*",
    "*.pem",
    "*.key",
    "*.p12",
    "*.pfx",
    "id_rsa",
    "id_rsa.*",
    "id_dsa",
    "id_dsa.*",
    "id_ecdsa",
    "id_ecdsa.*",
    "id_ed25519",
    "id_ed25519.*",
    "secrets/**",
    ".aws/credentials",
    ".gcloud/credentials*",
    ".gnupg/**",
];

pub fn default_patterns() -> Vec<String> {
    DEFAULT_PATTERNS.iter().map(|s| (*s).to_string()).collect()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DenyList {
    pub patterns: Vec<String>,
}

impl Default for DenyList {
    fn default() -> Self {
        Self {
            patterns: default_patterns(),
        }
    }
}

pub struct PathMatcher {
    set: GlobSet,
    pattern_count: usize,
}

impl PathMatcher {
    pub fn from_patterns(patterns: &[String]) -> Result<Self> {
        let mut builder = GlobSetBuilder::new();
        for pattern in patterns {
            let glob = Glob::new(pattern).map_err(|err| {
                MemhubError::InvalidInput(format!("invalid deny-list pattern '{pattern}': {err}"))
            })?;
            builder.add(glob);
        }
        let set = builder.build().map_err(|err| {
            MemhubError::InvalidInput(format!("failed to compile deny-list: {err}"))
        })?;
        Ok(Self {
            set,
            pattern_count: patterns.len(),
        })
    }

    pub fn is_denied(&self, path: &str) -> bool {
        let normalized = path.trim_start_matches("./");
        if self.set.is_match(normalized) {
            return true;
        }
        for (idx, _) in normalized.match_indices('/') {
            let suffix = &normalized[idx + 1..];
            if !suffix.is_empty() && self.set.is_match(suffix) {
                return true;
            }
        }
        false
    }

    pub fn pattern_count(&self) -> usize {
        self.pattern_count
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn matcher(patterns: &[&str]) -> PathMatcher {
        let owned: Vec<String> = patterns.iter().map(|s| (*s).to_string()).collect();
        PathMatcher::from_patterns(&owned).expect("compile deny patterns")
    }

    #[test]
    fn default_patterns_are_non_empty() {
        assert!(!default_patterns().is_empty());
        assert!(DenyList::default().patterns.iter().any(|p| p == ".env"));
    }

    #[test]
    fn matches_documented_examples() {
        let m = matcher(&[
            ".env",
            ".env.*",
            "*.pem",
            "secrets/**",
            "id_rsa",
            "id_rsa.*",
        ]);
        assert!(m.is_denied(".env"));
        assert!(m.is_denied(".env.production"));
        assert!(m.is_denied("server.pem"));
        assert!(m.is_denied("secrets/api/key.json"));
        assert!(m.is_denied("id_rsa"));
        assert!(m.is_denied("id_rsa.pub"));
        assert!(!m.is_denied("src/main.rs"));
        assert!(!m.is_denied("README.md"));
    }

    #[test]
    fn matches_nested_path_segments() {
        let m = matcher(&["*.pem", ".env"]);
        assert!(m.is_denied("config/server.pem"));
        assert!(m.is_denied("config/.env"));
        assert!(!m.is_denied("config/server.txt"));
    }

    #[test]
    fn fail_closed_on_invalid_pattern() {
        let bad = vec!["[".to_string()];
        let result = PathMatcher::from_patterns(&bad);
        assert!(result.is_err(), "expected invalid pattern to fail");
    }
}
