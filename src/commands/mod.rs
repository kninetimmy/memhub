pub mod audit_md;
pub mod bootstrap_k9;
pub mod command;
pub mod decision;
pub mod doc;
pub mod doctor;
pub mod eval;
pub mod export;
pub mod fact;
pub mod gc;
pub mod global;
pub mod import;
pub mod index;
pub mod ingest_git;
pub mod init;
pub mod install_manifest;
pub mod integrations;
pub mod metrics;
pub mod narrative;
pub mod pending_write;
pub mod render;
pub mod review;
pub mod search;
pub mod session_note;
pub mod stats;
pub mod status;
pub mod sync;
pub mod sync_md;
pub mod task;
pub mod transcript;
pub mod upgrade;
pub mod wrapup_policy;

use crate::{MemhubError, Result};

pub const DEFAULT_ACTOR: &str = "cli:user";
pub const MAX_ACTOR_LEN: usize = 64;
pub const MAX_SOURCE_LEN: usize = 64;

pub fn validate_actor(actor: &str) -> Result<()> {
    let trimmed = actor.trim();
    if trimmed.is_empty() {
        return Err(MemhubError::InvalidInput(
            "--actor cannot be empty".to_string(),
        ));
    }
    if trimmed.len() > MAX_ACTOR_LEN {
        return Err(MemhubError::InvalidInput(format!(
            "--actor must be {MAX_ACTOR_LEN} characters or fewer"
        )));
    }
    Ok(())
}

/// Enforce the `source` vocabulary documented in
/// `docs/reference/memhub-prd-source-vocabulary-addendum.md` §1.
/// Accepts `user`, `git`, `observed`, `agent:<id>`, and `user+agent:<id>`,
/// where `<id>` is a conservative normalized identifier.
pub fn validate_source(source: &str) -> Result<()> {
    let trimmed = source.trim();
    if trimmed.is_empty() {
        return Err(MemhubError::InvalidInput(
            "source cannot be empty".to_string(),
        ));
    }
    if trimmed.len() > MAX_SOURCE_LEN {
        return Err(MemhubError::InvalidInput(format!(
            "source must be {MAX_SOURCE_LEN} characters or fewer"
        )));
    }

    let valid = match trimmed {
        "user" | "git" | "observed" => true,
        other => {
            if let Some(id) = other.strip_prefix("user+agent:") {
                is_valid_agent_id(id)
            } else if let Some(id) = other.strip_prefix("agent:") {
                is_valid_agent_id(id)
            } else {
                false
            }
        }
    };

    if !valid {
        return Err(MemhubError::InvalidInput(format!(
            "invalid source '{source}'; expected one of user, git, observed, agent:<id>, user+agent:<id>"
        )));
    }
    Ok(())
}

fn is_valid_agent_id(id: &str) -> bool {
    !id.is_empty()
        && id.chars().all(|c| {
            c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-' || c == '_' || c == '.'
        })
}

#[cfg(test)]
mod tests {
    use super::validate_source;

    #[test]
    fn validate_source_accepts_documented_vocabulary() {
        validate_source("user").expect("user");
        validate_source("git").expect("git");
        validate_source("observed").expect("observed");
        validate_source("agent:codex").expect("agent:codex");
        validate_source("agent:opencode").expect("agent:opencode");
        validate_source("user+agent:claude-code").expect("user+agent:claude-code");
        validate_source("user+agent:opencode").expect("user+agent:opencode");
        validate_source("user+agent:my_client.v2").expect("user+agent:my_client.v2");
    }

    #[test]
    fn validate_source_rejects_typos_and_unknown_tokens() {
        assert!(validate_source("").is_err());
        assert!(validate_source("agnet:codex").is_err());
        assert!(validate_source("user+agnet:codex").is_err());
        assert!(validate_source("agent:").is_err());
        assert!(validate_source("agent:Codex").is_err()); // uppercase rejected
        assert!(validate_source("agent:has space").is_err());
        assert!(validate_source("cli:user").is_err()); // actor, not source
        assert!(validate_source("user+agent:").is_err());
    }
}
