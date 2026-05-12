use std::path::Path;

use crate::commands::import::{self, ImportSummary};
use crate::db;
use crate::models::InitResult;
use crate::sync_md;
use crate::{MemhubError, Result};

pub fn run(repo_root: &Path) -> Result<InitResult> {
    let (_, result) = db::init_project(repo_root)?;
    let _ = sync_md::sync_project(repo_root)?;
    Ok(result)
}

pub fn run_with_backup(repo_root: &Path, backup: &Path) -> Result<(InitResult, ImportSummary)> {
    let paths = db::ProjectPaths::for_repo_root(repo_root);
    if paths.db_path.exists() {
        return Err(MemhubError::InvalidInput(format!(
            "memhub database already exists at {}; use `memhub import --force <path>` to overwrite it",
            paths.db_path.display()
        )));
    }

    let (_, init_result) = db::init_project_for_recovery(repo_root)?;
    let import_summary = import::run(repo_root, backup, false)?;
    Ok((init_result, import_summary))
}
