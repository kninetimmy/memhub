use std::path::Path;

use crate::Result;
use crate::models::RenderResult;

pub fn run(start: &Path, actor: &str) -> Result<RenderResult> {
    crate::render::render_project(start, actor)
}
