use std::path::Path;

use crate::config::{
    DEFAULT_AGENT_DOCS_PATH, IntegrationsConfig, K9Config, ProjectConfig, detect_k9,
};
use crate::db;
use crate::{MemhubError, Result};

#[derive(Debug)]
pub struct K9IntegrationState {
    pub detected: bool,
    pub enabled: bool,
    pub agent_docs_path: String,
    pub drift: Option<String>,
}

#[derive(Debug)]
pub struct IntegrationsStatus {
    pub k9: K9IntegrationState,
}

pub fn status(start: &Path) -> Result<IntegrationsStatus> {
    let ctx = db::open_project(start)?;
    Ok(IntegrationsStatus {
        k9: k9_state(&ctx.paths.repo_root, &ctx.config.integrations),
    })
}

pub fn enable_k9(start: &Path, agent_docs_path: Option<&str>, force: bool) -> Result<()> {
    let ctx = db::open_project(start)?;
    let resolved_path = agent_docs_path
        .map(|p| p.to_string())
        .or_else(|| {
            ctx.config
                .integrations
                .k9
                .as_ref()
                .map(|cfg| cfg.agent_docs_path.clone())
        })
        .unwrap_or_else(|| DEFAULT_AGENT_DOCS_PATH.to_string());

    let detected = detect_k9(&ctx.paths.repo_root, &resolved_path);
    if !detected && !force {
        return Err(MemhubError::InvalidInput(format!(
            "K9 not detected at {}/project_state.md; pass --force to enable anyway",
            resolved_path
        )));
    }

    let mut config = ctx.config.clone();
    config.integrations.k9 = Some(K9Config {
        enabled: true,
        agent_docs_path: resolved_path,
    });
    config.save(&ctx.paths.config_path)?;

    db::log_write(
        &ctx.conn,
        "cli:user",
        "config",
        None,
        "update",
        "integrations enable k9",
    )?;
    Ok(())
}

pub fn disable_k9(start: &Path) -> Result<()> {
    let ctx = db::open_project(start)?;
    let Some(existing) = ctx.config.integrations.k9.clone() else {
        return Err(MemhubError::InvalidInput(
            "K9 integration is not configured for this project".to_string(),
        ));
    };

    let mut config = ctx.config.clone();
    config.integrations.k9 = Some(K9Config {
        enabled: false,
        agent_docs_path: existing.agent_docs_path,
    });
    config.save(&ctx.paths.config_path)?;

    db::log_write(
        &ctx.conn,
        "cli:user",
        "config",
        None,
        "update",
        "integrations disable k9",
    )?;
    Ok(())
}

pub(crate) fn k9_state(repo_root: &Path, integrations: &IntegrationsConfig) -> K9IntegrationState {
    let agent_docs_path = integrations
        .k9
        .as_ref()
        .map(|cfg| cfg.agent_docs_path.clone())
        .unwrap_or_else(|| DEFAULT_AGENT_DOCS_PATH.to_string());
    let detected = detect_k9(repo_root, &agent_docs_path);
    let enabled = integrations
        .k9
        .as_ref()
        .map(|cfg| cfg.enabled)
        .unwrap_or(false);

    let drift = match (enabled, detected, integrations.k9.is_some()) {
        (true, false, _) => Some(format!(
            "K9 enabled in config but {}/project_state.md is missing",
            agent_docs_path
        )),
        (false, true, false) => {
            Some("K9 detected; run `memhub integrations enable k9` to enable".to_string())
        }
        _ => None,
    };

    K9IntegrationState {
        detected,
        enabled,
        agent_docs_path,
        drift,
    }
}

pub fn check_k9(start: &Path) -> bool {
    let Ok(ctx) = db::open_project(start) else {
        return false;
    };
    ctx.config
        .integrations
        .k9
        .as_ref()
        .map(|cfg| cfg.enabled)
        .unwrap_or(false)
}

pub fn apply_k9_detection_on_init(repo_root: &Path, config: &mut ProjectConfig) -> bool {
    let path = config
        .integrations
        .k9
        .as_ref()
        .map(|cfg| cfg.agent_docs_path.clone())
        .unwrap_or_else(|| DEFAULT_AGENT_DOCS_PATH.to_string());
    if detect_k9(repo_root, &path) {
        config.integrations.k9 = Some(K9Config {
            enabled: true,
            agent_docs_path: path,
        });
        true
    } else {
        false
    }
}
