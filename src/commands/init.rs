use std::path::Path;

use crate::Result;
use crate::db;
use crate::models::InitResult;
use crate::sync_md;

pub fn run(repo_root: &Path) -> Result<InitResult> {
    let (_, result) = db::init_project(repo_root)?;
    let _ = sync_md::sync_project(repo_root)?;
    Ok(result)
}
