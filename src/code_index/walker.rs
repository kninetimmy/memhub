//! Git-aware file walker for the code index (M11 PR1, decision 107).
//!
//! The index set is `git ls-files` filtered through the project's
//! existing deny-list ([`PathMatcher`]). Driving it off git (rather than
//! a raw directory walk) means untracked scratch is ignored and
//! `.gitignore` is respected for free, with no extra config.

use std::path::Path;
use std::process::Command;

use crate::Result;
use crate::config::PathMatcher;
use crate::errors::MemhubError;

/// List repo-relative paths tracked by git, minus deny-listed ones.
///
/// Paths use forward slashes (git's native form, even on Windows) and are
/// returned sorted for deterministic refresh order. `-z` keeps paths with
/// spaces or unusual characters intact (git would otherwise quote them).
pub fn list_tracked_files(repo_root: &Path, matcher: &PathMatcher) -> Result<Vec<String>> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("ls-files")
        .arg("-z")
        .output()?;

    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(MemhubError::ExternalCommand {
            command: format!("git -C {} ls-files", repo_root.display()),
            stderr: if stderr.is_empty() {
                "unknown git error".to_string()
            } else {
                stderr
            },
        });
    }

    let mut paths: Vec<String> = String::from_utf8_lossy(&output.stdout)
        .split('\0')
        .map(str::trim)
        .filter(|p| !p.is_empty())
        .filter(|p| !matcher.is_denied(p))
        .map(str::to_string)
        .collect();
    paths.sort();
    paths.dedup();
    Ok(paths)
}

/// The current `HEAD` commit sha, or `None` when it cannot be resolved
/// (e.g. a repo with no commits yet). Stamped into `index_meta` so
/// `code status` can report the indexed HEAD; never load-bearing for
/// staleness, which is decided per-file.
pub fn current_head(repo_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .arg("rev-parse")
        .arg("HEAD")
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sha = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if sha.is_empty() { None } else { Some(sha) }
}
