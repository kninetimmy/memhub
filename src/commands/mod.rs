pub mod bootstrap_k9;
pub mod command;
pub mod decision;
pub mod export;
pub mod fact;
pub mod import;
pub mod ingest_git;
pub mod init;
pub mod integrations;
pub mod narrative;
pub mod pending_write;
pub mod render;
pub mod review;
pub mod search;
pub mod session_note;
pub mod stats;
pub mod status;
pub mod sync_md;
pub mod task;

use crate::{MemhubError, Result};

pub const DEFAULT_ACTOR: &str = "cli:user";
pub const MAX_ACTOR_LEN: usize = 64;

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
