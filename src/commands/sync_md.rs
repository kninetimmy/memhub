use std::path::Path;

use crate::Result;
use crate::models::MarkdownSyncResult;

pub fn run(start: &Path) -> Result<MarkdownSyncResult> {
    crate::sync_md::sync_project(start)
}
