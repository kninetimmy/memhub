mod migrations;
pub mod registry;

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
/// Machine-global store (M9). One optional DB per machine at
/// `~/.memhub/global.sqlite`, structurally identical to a repo DB
/// (same embedded migrations; `project_id = 1` is per-database).
pub const GLOBAL_MEMHUB_DIRNAME: &str = ".memhub";
pub const GLOBAL_DB_FILENAME: &str = "global.sqlite";

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

    // Self-maintaining machine-wide upgrade registry (decision 96).
    // Best-effort, debounced, and a no-op unless a machine-global store
    // already exists — never creates it, never fails the command, and
    // (critically) never read by recall, so the M9 eval-regression
    // guarantee holds: a populated `known_projects` cannot change recall
    // output, which stays gated on this repo's `[global] enabled`.
    registry::record_open_best_effort(&paths.repo_root, migrations::latest_version());

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
         PRAGMA busy_timeout = 5000;
         PRAGMA recursive_triggers = OFF;",
    )?;
    Ok(conn)
}

fn upsert_project(conn: &Connection, repo_root: &Path) -> Result<()> {
    debug!("upserting project metadata for {}", repo_root.display());
    // `schema_version` is only ever ratcheted upward: an older binary
    // opening a newer DB must not stomp the recorded version down to its
    // own (it would then be writing into a schema it doesn't understand).
    // Migration ids are zero-padded `NNNN_name`, so a byte-wise `MAX`
    // matches version order. `apply_all` refuses outright before we get
    // here when the DB is genuinely newer; this is the belt-and-braces.
    conn.execute(
        "INSERT INTO projects(id, root_path, schema_version)
         VALUES (1, ?1, ?2)
         ON CONFLICT(id) DO UPDATE SET
             root_path = excluded.root_path,
             schema_version = MAX(projects.schema_version, excluded.schema_version)",
        params![
            repo_root.to_string_lossy().to_string(),
            migrations::latest_version()
        ],
    )?;
    Ok(())
}

/// Resolve the machine home directory without a third-party crate
/// (boring, offline). `$HOME` on Unix/macOS, `%USERPROFILE%` on
/// Windows.
pub fn home_dir() -> Result<PathBuf> {
    for key in ["HOME", "USERPROFILE"] {
        if let Some(val) = std::env::var_os(key)
            && !val.is_empty()
        {
            return Ok(PathBuf::from(val));
        }
    }
    Err(MemhubError::InvalidInput(
        "cannot resolve home directory ($HOME / %USERPROFILE% unset); \
         machine-global memory is unavailable"
            .to_string(),
    ))
}

/// `~/.memhub/global.sqlite`. The machine-global store path.
pub fn global_db_path() -> Result<PathBuf> {
    Ok(home_dir()?
        .join(GLOBAL_MEMHUB_DIRNAME)
        .join(GLOBAL_DB_FILENAME))
}

/// Read a DB's recorded schema version WITHOUT applying migrations.
/// Used by `memhub upgrade --dry-run` to preview which instances are
/// behind head without the side effect of bringing them to head.
/// Returns `Ok(None)` when the file is missing or has no `projects`
/// row yet.
pub fn probe_schema_version(db_path: &Path) -> Result<Option<String>> {
    if !db_path.exists() {
        return Ok(None);
    }
    let conn = Connection::open(db_path)?;
    let v: Option<String> = conn
        .query_row(
            "SELECT schema_version FROM projects WHERE id = 1",
            [],
            |row| row.get(0),
        )
        .ok();
    Ok(v)
}

/// True iff the machine-global store file already exists on disk.
/// Used to print a one-time disclosure on the first global write and
/// to gate `open_global_if_exists`.
pub fn global_store_exists() -> Result<bool> {
    Ok(global_db_path()?.exists())
}

fn global_paths() -> Result<ProjectPaths> {
    let db_path = global_db_path()?;
    let memhub_dir = db_path
        .parent()
        .map(Path::to_path_buf)
        .unwrap_or_else(|| PathBuf::from(GLOBAL_MEMHUB_DIRNAME));
    Ok(ProjectPaths {
        repo_root: memhub_dir.clone(),
        db_path,
        // The global store has no per-machine TOML; recall behavior is
        // governed entirely by the active repo's config. This path is
        // a never-read placeholder so `ProjectContext` stays uniform.
        config_path: memhub_dir.join("global.config.toml"),
        memhub_dir,
    })
}

/// Open (creating + migrating if necessary) the machine-global store.
///
/// Structurally identical to `open_project` but: fixed `~/.memhub`
/// path, no config discovery, no metrics scrape. The returned
/// `ProjectContext.config` is a defaulted placeholder — callers must
/// drive retrieval/embedding behavior from the *active repo's* config,
/// never from this field.
pub fn open_global() -> Result<ProjectContext> {
    let paths = global_paths()?;
    let mut conn = open_connection(&paths.db_path)?;
    let _ = migrations::apply_all(&mut conn)?;
    upsert_project(&conn, &paths.repo_root)?;
    Ok(ProjectContext {
        config: ProjectConfig::default_for_repo_name("<global>"),
        paths,
        conn,
    })
}

/// Open the machine-global store only if it already exists. Recall
/// must not fail in the common repo-with-no-global case, so this
/// returns `None` rather than creating the store.
pub fn open_global_if_exists() -> Result<Option<ProjectContext>> {
    if !global_store_exists()? {
        return Ok(None);
    }
    Ok(Some(open_global()?))
}

pub fn discover_paths(start: &Path) -> Result<ProjectPaths> {
    // The machine-global store lives at `~/.memhub` and shares the
    // `.memhub` dirname with per-repo stores. Without this guard,
    // discovery walking up from a cwd that is *not* inside any repo
    // reaches `$HOME`, finds `~/.memhub`, and returns it as a project.
    // `open_project` then sees no `~/.memhub/project.sqlite` and raises
    // `MissingDatabase`, whose remedy ("remove ~/.memhub") would delete
    // the machine-global store. Never treat the global-store dir as a
    // repo project unless a repo DB actually lives alongside it (a real,
    // if unusual, project rooted at `$HOME`).
    let global_memhub_dir = home_dir().ok().map(|h| h.join(GLOBAL_MEMHUB_DIRNAME));

    for candidate in start.ancestors() {
        let paths = ProjectPaths::for_repo_root(candidate);
        if paths.memhub_dir.exists() {
            if is_global_only_store(&paths, global_memhub_dir.as_deref()) {
                continue;
            }
            return Ok(paths);
        }
    }

    Err(MemhubError::NotInitialized {
        start: start.to_path_buf(),
    })
}

/// True iff `paths` points at the machine-global store directory and
/// that directory holds *only* the global store (no repo
/// `project.sqlite`). Such a dir must be skipped by project discovery
/// so it is never mistaken for — or destructively "recovered" as — a
/// per-repo project.
fn is_global_only_store(paths: &ProjectPaths, global_memhub_dir: Option<&Path>) -> bool {
    let Some(global_dir) = global_memhub_dir else {
        return false;
    };
    same_dir(&paths.memhub_dir, global_dir) && !paths.db_path.exists()
}

/// Path equality that tolerates relative/symlinked inputs by
/// canonicalizing when possible, falling back to literal comparison.
fn same_dir(a: &Path, b: &Path) -> bool {
    match (a.canonicalize(), b.canonicalize()) {
        (Ok(a), Ok(b)) => a == b,
        _ => a == b,
    }
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
