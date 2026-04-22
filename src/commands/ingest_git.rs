use std::collections::HashSet;
use std::path::Path;
use std::process::Command;

use rusqlite::params;

use crate::Result;
use crate::db;
use crate::errors::MemhubError;
use crate::models::GitIngestSummary;

#[derive(Debug)]
struct GitCommit {
    sha: String,
    author: String,
    committed_at: String,
    message: String,
    files: Vec<GitFileChange>,
}

#[derive(Debug)]
struct GitFileChange {
    change_type: String,
    path: String,
}

pub fn run(start: &Path, since: Option<&str>) -> Result<GitIngestSummary> {
    let mut ctx = db::open_project(start)?;
    let commits = load_git_history(&ctx.paths.repo_root, since)?;

    let mut unique_files = HashSet::new();
    let mut commit_file_links_seen = 0usize;

    let tx = ctx.conn.transaction()?;
    for commit in &commits {
        tx.execute(
            "INSERT INTO commits(sha, project_id, author, committed_at, message)
             VALUES (?1, 1, ?2, ?3, ?4)
             ON CONFLICT(sha) DO UPDATE SET
                 author = excluded.author,
                 committed_at = excluded.committed_at,
                 message = excluded.message",
            params![
                commit.sha.as_str(),
                commit.author.as_str(),
                commit.committed_at.as_str(),
                commit.message.as_str()
            ],
        )?;

        for file in &commit.files {
            tx.execute(
                "INSERT INTO files(project_id, path, last_seen_commit, language)
                 VALUES (1, ?1, ?2, ?3)
                 ON CONFLICT(project_id, path) DO UPDATE SET
                     last_seen_commit = CASE
                         WHEN files.last_seen_commit IS NULL THEN excluded.last_seen_commit
                         WHEN (
                             SELECT committed_at FROM commits WHERE sha = excluded.last_seen_commit
                         ) >= COALESCE((
                             SELECT committed_at FROM commits WHERE sha = files.last_seen_commit
                         ), '')
                         THEN excluded.last_seen_commit
                         ELSE files.last_seen_commit
                     END,
                     language = COALESCE(files.language, excluded.language)",
                params![
                    file.path.as_str(),
                    commit.sha.as_str(),
                    infer_language(&file.path)
                ],
            )?;

            let file_id: i64 = tx.query_row(
                "SELECT id FROM files WHERE project_id = 1 AND path = ?1",
                [file.path.as_str()],
                |row| row.get(0),
            )?;

            tx.execute(
                "INSERT INTO commit_files(commit_sha, file_id, change_type)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(commit_sha, file_id) DO UPDATE SET
                     change_type = excluded.change_type",
                params![commit.sha.as_str(), file_id, file.change_type.as_str()],
            )?;

            unique_files.insert(file.path.clone());
            commit_file_links_seen += 1;
        }
    }

    db::log_write(&tx, "cli:user", "commits", None, "ingest", "ingest-git")?;

    tx.commit()?;

    Ok(GitIngestSummary {
        since: since.map(str::to_string),
        commits_seen: commits.len(),
        unique_files_seen: unique_files.len(),
        commit_file_links_seen,
    })
}

fn load_git_history(repo_root: &Path, since: Option<&str>) -> Result<Vec<GitCommit>> {
    let mut command = Command::new("git");
    command
        .arg("-C")
        .arg(repo_root)
        .arg("log")
        .arg("--date=iso-strict")
        .arg("--pretty=format:commit\x1f%H\x1f%an\x1f%aI\x1f%s")
        .arg("--name-status")
        .arg("--find-renames");

    if let Some(since) = since.map(str::trim).filter(|value| !value.is_empty()) {
        command.arg(format!("{since}..HEAD"));
    }

    let output = command.output()?;
    if !output.status.success() {
        let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
        return Err(MemhubError::ExternalCommand {
            command: format!("git -C {} log", repo_root.display()),
            stderr: if stderr.is_empty() {
                "unknown git error".to_string()
            } else {
                stderr
            },
        });
    }

    parse_git_log(&String::from_utf8_lossy(&output.stdout))
}

fn parse_git_log(output: &str) -> Result<Vec<GitCommit>> {
    let mut commits = Vec::new();
    let mut current: Option<GitCommit> = None;

    for line in output.lines() {
        if line.is_empty() {
            continue;
        }

        if let Some(rest) = line.strip_prefix("commit\x1f") {
            if let Some(commit) = current.take() {
                commits.push(commit);
            }

            let parts: Vec<_> = rest.split('\x1f').collect();
            if parts.len() != 4 {
                return Err(MemhubError::InvalidInput(format!(
                    "unexpected git log header: {line}"
                )));
            }

            current = Some(GitCommit {
                sha: parts[0].to_string(),
                author: parts[1].to_string(),
                committed_at: parts[2].to_string(),
                message: parts[3].to_string(),
                files: Vec::new(),
            });
            continue;
        }

        let commit = current.as_mut().ok_or_else(|| {
            MemhubError::InvalidInput("git log output contained file rows before a commit".into())
        })?;
        if let Some(change) = parse_file_change(line) {
            commit.files.push(change);
        }
    }

    if let Some(commit) = current.take() {
        commits.push(commit);
    }

    Ok(commits)
}

fn parse_file_change(line: &str) -> Option<GitFileChange> {
    let mut parts = line.split('\t');
    let status = parts.next()?.trim();
    let primary = status.chars().next()?;
    let change_type = primary.to_string();

    let path = match primary {
        'R' | 'C' => {
            parts.next()?;
            parts.next()?
        }
        _ => parts.next()?,
    };

    Some(GitFileChange {
        change_type,
        path: path.replace('\\', "/"),
    })
}

fn infer_language(path: &str) -> Option<&'static str> {
    let extension = path.rsplit('.').next()?;
    match extension.to_ascii_lowercase().as_str() {
        "rs" => Some("rust"),
        "toml" => Some("toml"),
        "md" => Some("markdown"),
        "json" => Some("json"),
        "yaml" | "yml" => Some("yaml"),
        "ts" | "tsx" => Some("typescript"),
        "js" | "jsx" => Some("javascript"),
        "py" => Some("python"),
        "sql" => Some("sql"),
        _ => None,
    }
}
