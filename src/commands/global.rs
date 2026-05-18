//! Machine-global memory store (M9).
//!
//! The global store is one optional SQLite per machine at
//! `~/.memhub/global.sqlite`, structurally identical to a repo DB.
//! Enablement is *per-repo*: a repo opts into reading from / writing
//! to the global store via `memhub global enable`. Recall behavior of
//! global hits is governed by the active repo's `[retrieval]` config —
//! the global store has no config of its own.

use std::path::{Path, PathBuf};

use crate::Result;
use crate::config::RetrievalMode;
use crate::db::{self, ProjectContext};

const GLOBAL_ACTOR: &str = "cli:user";

/// Error a global write hits when the active repo has not opted in.
fn disabled_err() -> crate::MemhubError {
    crate::MemhubError::InvalidInput(
        "machine-global memory is disabled for this repo; run `memhub global enable` first"
            .to_string(),
    )
}

/// Gate every global write on the active repo having opted in.
pub fn ensure_enabled(cfg: &crate::config::ProjectConfig) -> Result<()> {
    if cfg.global.enabled {
        Ok(())
    } else {
        Err(disabled_err())
    }
}

/// Scaffolding shared by every born-global / promote write: verify the
/// repo opted in, capture whether this call will create the store
/// (for the one-time disclosure), open the global DB, and surface the
/// *repo's* retrieval mode so global rows embed consistently with how
/// this machine's repos recall.
pub struct GlobalWrite {
    pub ctx: ProjectContext,
    pub store_created: bool,
    pub mode: RetrievalMode,
}

pub fn begin_write(start: &Path) -> Result<GlobalWrite> {
    let repo = db::open_project(start)?;
    ensure_enabled(&repo.config)?;
    let store_created = !db::global_store_exists()?;
    let ctx = db::open_global()?;
    Ok(GlobalWrite {
        ctx,
        store_created,
        mode: repo.config.retrieval.mode,
    })
}

#[derive(Debug)]
pub struct EnableResult {
    pub already_enabled: bool,
    pub store_created: bool,
    pub path: PathBuf,
}

/// Opt this repo into machine-global memory and ensure the store
/// exists (created + migrated on first enable anywhere on the
/// machine).
pub fn enable(start: &Path) -> Result<EnableResult> {
    let repo = db::open_project(start)?;
    let already_enabled = repo.config.global.enabled;

    let store_created = !db::global_store_exists()?;
    // Touch the store so `enable` is also "create if absent".
    let _ = db::open_global()?;

    let mut new_config = repo.config.clone();
    new_config.global.enabled = true;
    new_config.save(&repo.paths.config_path)?;

    db::log_write(
        &repo.conn,
        GLOBAL_ACTOR,
        "config",
        None,
        "update",
        "global enable",
    )?;

    Ok(EnableResult {
        already_enabled,
        store_created,
        path: db::global_db_path()?,
    })
}

/// Opt this repo back out. Non-destructive: the store and its rows
/// remain on disk; recall simply stops merging the global corpus and
/// global writes refuse again.
pub fn disable(start: &Path) -> Result<()> {
    let repo = db::open_project(start)?;
    let mut new_config = repo.config.clone();
    new_config.global.enabled = false;
    new_config.save(&repo.paths.config_path)?;

    db::log_write(
        &repo.conn,
        GLOBAL_ACTOR,
        "config",
        None,
        "update",
        "global disable",
    )?;
    Ok(())
}

#[derive(Debug)]
pub struct GlobalStatus {
    pub enabled: bool,
    pub path: PathBuf,
    pub exists: bool,
    pub schema_version: Option<String>,
    pub fact_count: i64,
    pub decision_count: i64,
    pub doc_chunk_count: i64,
}

pub fn status(start: &Path) -> Result<GlobalStatus> {
    let repo = db::open_project(start)?;
    let path = db::global_db_path()?;
    let exists = db::global_store_exists()?;

    let (schema_version, fact_count, decision_count, doc_chunk_count) = if exists {
        let g = db::open_global()?;
        let schema_version: Option<String> = g
            .conn
            .query_row(
                "SELECT schema_version FROM projects WHERE id = 1",
                [],
                |r| r.get(0),
            )
            .ok();
        let count = |table: &str| -> i64 {
            g.conn
                .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
                .unwrap_or(0)
        };
        (
            schema_version,
            count("facts"),
            count("decisions"),
            count("doc_chunks"),
        )
    } else {
        (None, 0, 0, 0)
    };

    Ok(GlobalStatus {
        enabled: repo.config.global.enabled,
        path,
        exists,
        schema_version,
        fact_count,
        decision_count,
        doc_chunk_count,
    })
}
