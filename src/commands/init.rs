use std::path::Path;

use crate::Result;
use crate::db;
use crate::models::InitResult;

pub fn run(repo_root: &Path) -> Result<InitResult> {
    let (_, result) = db::init_project(repo_root)?;
    Ok(result)
}
