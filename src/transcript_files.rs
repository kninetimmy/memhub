//! Agent transcript path and session-id conventions shared by the optional
//! metrics scraper and the always-available transcript archiver.
//!
//! This module deliberately lives outside `metrics`: hibernating token
//! accounting must not disable archival, and the two callers must still use
//! one definition of each agent's on-disk layout.

use std::fs;
use std::path::{Path, PathBuf};

pub(crate) fn detect_codex_sessions_dir() -> Option<PathBuf> {
    let home = crate::db::home_dir().ok()?;
    let candidate = home.join(".codex/sessions");
    candidate.is_dir().then_some(candidate)
}

pub(crate) fn detect_claude_transcripts_dir(repo_root: &Path) -> Option<PathBuf> {
    let home = crate::db::home_dir().ok()?;
    let abs = repo_root.canonicalize().ok()?;
    let candidate = home
        .join(".claude/projects")
        .join(encode_claude_project_dir(&abs));
    candidate.is_dir().then_some(candidate)
}

pub(crate) fn encode_claude_project_dir(abs: &Path) -> String {
    let path_str = abs.to_string_lossy();
    let stripped = path_str.strip_prefix(r"\\?\").unwrap_or(path_str.as_ref());
    stripped.replace(['/', '\\', ':'], "-")
}

pub fn claude_session_id_from_path(path: &Path) -> Option<String> {
    path.file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())
        .map(str::to_string)
}

pub fn codex_session_id_from_path(path: &Path) -> Option<String> {
    let stem = path
        .file_stem()
        .and_then(|s| s.to_str())
        .filter(|s| !s.is_empty())?;
    let parts: Vec<&str> = stem.split('-').collect();
    if parts.len() < 5 {
        return None;
    }
    let uuid = parts[parts.len() - 5..].join("-");
    Some(format!("codex:{uuid}"))
}

pub fn find_claude_transcript(dir: &Path, session_id: &str) -> Option<PathBuf> {
    let candidate = dir.join(format!("{session_id}.jsonl"));
    candidate.is_file().then_some(candidate)
}

pub fn find_codex_transcript(dir: &Path, session_id: &str) -> Option<PathBuf> {
    for l1 in read_subdirs(dir) {
        for l2 in read_subdirs(&l1) {
            for l3 in read_subdirs(&l2) {
                let files = match fs::read_dir(&l3) {
                    Ok(entries) => entries,
                    Err(_) => continue,
                };
                for entry in files.flatten() {
                    let path = entry.path();
                    if path.extension().and_then(|e| e.to_str()) == Some("jsonl")
                        && codex_session_id_from_path(&path).as_deref() == Some(session_id)
                    {
                        return Some(path);
                    }
                }
            }
        }
    }
    None
}

fn read_subdirs(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    if let Ok(entries) = fs::read_dir(dir) {
        for entry in entries.flatten() {
            if entry.file_type().map(|t| t.is_dir()).unwrap_or(false) {
                out.push(entry.path());
            }
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn claude_project_dir_encoding_matches_agent_layouts() {
        assert_eq!(
            encode_claude_project_dir(Path::new(r"\\?\C:\Users\Kninetimmy\memhub")),
            "C--Users-Kninetimmy-memhub"
        );
        assert_eq!(
            encode_claude_project_dir(Path::new("/Users/foo/memhub")),
            "-Users-foo-memhub"
        );
    }
}
