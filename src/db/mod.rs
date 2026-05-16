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
pub const CONFIG_EXAMPLE_FILENAME: &str = "config.example.toml";

/// The latest schema version this build will migrate a DB to. Exposed so
/// tests can verify that `open_project` brought a DB up to head without
/// hand-coding the version string in every assertion.
pub fn latest_schema_version() -> &'static str {
    migrations::latest_version()
}

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
    init_project_inner(repo_root, false)
}

pub fn init_project_for_recovery(repo_root: &Path) -> Result<(ProjectContext, InitResult)> {
    init_project_inner(repo_root, true)
}

fn init_project_inner(
    repo_root: &Path,
    allow_missing_db_recovery: bool,
) -> Result<(ProjectContext, InitResult)> {
    let paths = ProjectPaths::for_repo_root(repo_root);
    let memhub_preexisting = paths.memhub_dir.exists();

    // Guard against "user accidentally deleted their DB but still has a
    // live local config that points at memory we don't want to silently
    // recreate." The presence of `.memhub/config.toml` (the gitignored
    // live config) is the signal of in-use state. A repo that's only
    // freshly cloned will have `.memhub/` with the tracked
    // `config.example.toml` but no `config.toml` — that's a legitimate
    // fresh-init scenario, not a data-loss case.
    if paths.config_path.exists() && !paths.db_path.exists() && !allow_missing_db_recovery {
        return Err(MemhubError::MissingDatabase {
            memhub_dir: paths.memhub_dir.clone(),
            db_path: paths.db_path.clone(),
        });
    }

    fs::create_dir_all(&paths.memhub_dir)?;

    let gitignore_updated = ensure_gitignore(repo_root)?;
    let repo_name = repo_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("memhub");

    let config_created =
        create_config_if_missing(&paths.memhub_dir, &paths.config_path, repo_name)?;

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

    if !paths.db_path.exists() {
        return Err(MemhubError::MissingDatabase {
            memhub_dir: paths.memhub_dir.clone(),
            db_path: paths.db_path.clone(),
        });
    }

    if !paths.config_path.exists() {
        let repo_name = paths
            .repo_root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("memhub");
        create_config_if_missing(&paths.memhub_dir, &paths.config_path, repo_name)?;
    }

    let config = ProjectConfig::load(&paths.config_path)?;
    let mut conn = open_connection(&paths.db_path)?;
    let _ = migrations::apply_all(&mut conn)?;
    upsert_project(&conn, &paths.repo_root)?;

    // Opportunistic, gated, never-fails token-accounting scrape
    // (decision 74 component B, task #29). Off by default; a no-op
    // unless the user opted in via `memhub metrics enable`.
    crate::metrics::session_scraper::scrape_if_enabled(&conn, &config.metrics, &paths.repo_root);

    // Post-scrape upkeep (decision 74, task #30): attribute recall rows
    // to the freshly-updated session windows and prune past retention.
    // Master-switch gated, errors swallowed — same posture as the
    // scrape above. Runs after it so the reconciler sees current bounds.
    crate::metrics::maintenance::run_if_enabled(&conn, &config.metrics);

    Ok(ProjectContext {
        paths,
        config,
        conn,
    })
}

/// Seed `.memhub/config.toml` if it is missing.
///
/// On a fresh machine the canonical config can travel with the repo as
/// `.memhub/config.example.toml` (tracked in git). When that file is
/// present we copy it verbatim, validating it parses as a
/// `ProjectConfig` first so a corrupt example fails fast instead of
/// writing garbage. When the example is absent, fall back to the
/// code-defined defaults seeded with the repo directory name.
///
/// Returns `true` iff the config file was newly created.
fn create_config_if_missing(
    memhub_dir: &Path,
    config_path: &Path,
    repo_name: &str,
) -> Result<bool> {
    if config_path.exists() {
        return Ok(false);
    }

    let example_path = memhub_dir.join(CONFIG_EXAMPLE_FILENAME);
    if example_path.exists() {
        let raw = fs::read_to_string(&example_path)?;
        // Validate so a corrupt example doesn't write nonsense that errors
        // on the very next `ProjectConfig::load` call.
        let _: ProjectConfig = toml::from_str(&raw)?;
        fs::write(config_path, raw)?;
    } else {
        ProjectConfig::default_for_repo_name(repo_name).save(config_path)?;
    }
    Ok(true)
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
            migrations::latest_version()
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
    let entries = [
        ".memhub/",
        "agent_docs/PROJECT.md",
        "agent_docs/PROJECT_LEDGER.md",
    ];

    let existing = if gitignore_path.exists() {
        fs::read_to_string(&gitignore_path)?
    } else {
        String::new()
    };

    let existing_entries: Vec<_> = existing.lines().map(normalize_gitignore_entry).collect();
    let missing: Vec<_> = entries
        .iter()
        .copied()
        .filter(|entry| {
            !existing_entries
                .iter()
                .any(|line| line == &normalize_gitignore_entry(entry))
        })
        .collect();

    if missing.is_empty() {
        return Ok(false);
    }

    let mut updated = existing;
    if !updated.is_empty() && !updated.ends_with('\n') {
        updated.push('\n');
    }
    for entry in missing {
        updated.push_str(entry);
        updated.push('\n');
    }

    fs::write(gitignore_path, updated)?;
    Ok(true)
}

fn normalize_gitignore_entry(entry: &str) -> String {
    entry.trim().trim_matches('/').to_string()
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
