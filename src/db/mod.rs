mod migrations;

use std::fs;
use std::path::{Path, PathBuf};

use log::debug;
use rusqlite::{Connection, params};

use crate::config::ProjectConfig;
use crate::models::InitResult;
use crate::{MemhubError, Result};

pub const MEMHUB_DIR: &str = ".memhub";
pub const DB_FILENAME: &str = "project.sqlite";
pub const CONFIG_FILENAME: &str = "config.toml";

#[derive(Debug, Clone)]
pub struct ProjectPaths {
    pub repo_root: PathBuf,
    pub memhub_dir: PathBuf,
    pub db_path: PathBuf,
    pub config_path: PathBuf,
}

pub struct ProjectContext {
    pub paths: ProjectPaths,
    pub config: ProjectConfig,
    pub conn: Connection,
}

impl ProjectPaths {
    pub fn for_repo_root(repo_root: &Path) -> Self {
        let repo_root = repo_root.to_path_buf();
        let memhub_dir = repo_root.join(MEMHUB_DIR);

        Self {
            db_path: memhub_dir.join(DB_FILENAME),
            config_path: memhub_dir.join(CONFIG_FILENAME),
            memhub_dir,
            repo_root,
        }
    }
}

pub fn init_project(repo_root: &Path) -> Result<(ProjectContext, InitResult)> {
    let paths = ProjectPaths::for_repo_root(repo_root);
    let memhub_preexisting = paths.memhub_dir.exists();
    fs::create_dir_all(&paths.memhub_dir)?;

    let gitignore_updated = ensure_gitignore(repo_root)?;
    let repo_name = repo_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("memhub");

    let config_created = if paths.config_path.exists() {
        false
    } else {
        let config = ProjectConfig::default_for_repo_name(repo_name);
        config.save(&paths.config_path)?;
        true
    };

    let config = ProjectConfig::load(&paths.config_path)?;
    let mut conn = open_connection(&paths.db_path)?;
    let migrations_applied = migrations::apply_all(&mut conn)?;
    upsert_project(&conn, repo_root)?;

    let result = InitResult {
        config_created,
        db_path: paths.db_path.clone(),
        gitignore_updated,
        memhub_preexisting,
        migrations_applied,
        repo_root: paths.repo_root.clone(),
    };

    Ok((
        ProjectContext {
            paths,
            config,
            conn,
        },
        result,
    ))
}

pub fn open_project(start: &Path) -> Result<ProjectContext> {
    let paths = discover_paths(start)?;

    if !paths.config_path.exists() {
        let repo_name = paths
            .repo_root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("memhub");
        ProjectConfig::default_for_repo_name(repo_name).save(&paths.config_path)?;
    }

    let config = ProjectConfig::load(&paths.config_path)?;
    let mut conn = open_connection(&paths.db_path)?;
    let _ = migrations::apply_all(&mut conn)?;
    upsert_project(&conn, &paths.repo_root)?;

    Ok(ProjectContext {
        paths,
        config,
        conn,
    })
}

fn open_connection(path: &Path) -> Result<Connection> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let conn = Connection::open(path)?;
    conn.execute_batch(
        "PRAGMA foreign_keys = ON;
         PRAGMA journal_mode = WAL;
         PRAGMA busy_timeout = 5000;",
    )?;
    Ok(conn)
}

fn upsert_project(conn: &Connection, repo_root: &Path) -> Result<()> {
    debug!("upserting project metadata for {}", repo_root.display());
    conn.execute(
        "INSERT INTO projects(id, root_path, schema_version)
         VALUES (1, ?1, ?2)
         ON CONFLICT(id) DO UPDATE SET
             root_path = excluded.root_path,
             schema_version = excluded.schema_version",
        params![
            repo_root.to_string_lossy().to_string(),
            migrations::LATEST_VERSION
        ],
    )?;
    Ok(())
}

pub fn discover_paths(start: &Path) -> Result<ProjectPaths> {
    for candidate in start.ancestors() {
        let paths = ProjectPaths::for_repo_root(candidate);
        if paths.memhub_dir.exists() {
            return Ok(paths);
        }
    }

    Err(MemhubError::NotInitialized {
        start: start.to_path_buf(),
    })
}

fn ensure_gitignore(repo_root: &Path) -> Result<bool> {
    let gitignore_path = repo_root.join(".gitignore");
    let entry = ".memhub/";

    let existing = if gitignore_path.exists() {
        fs::read_to_string(&gitignore_path)?
    } else {
        String::new()
    };

    if existing.lines().any(|line| line.trim() == entry) {
        return Ok(false);
    }

    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    updated.push_str(entry);
    updated.push('\n');

    fs::write(gitignore_path, updated)?;
    Ok(true)
}

pub fn log_write(
    conn: &Connection,
    actor: &str,
    table_name: &str,
    row_id: Option<i64>,
    action: &str,
    reason: &str,
) -> Result<()> {
    conn.execute(
        "INSERT INTO writes_log(project_id, actor, table_name, row_id, action, reason)
         VALUES (1, ?1, ?2, ?3, ?4, ?5)",
        params![actor, table_name, row_id, action, reason],
    )?;
    Ok(())
}
