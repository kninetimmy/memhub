//! Machine-wide upgrade registry (M9 / `memhub upgrade`).
//!
//! A self-maintaining list of every repo memhub has actually opened on
//! this machine, kept in the machine-global store
//! (`~/.memhub/global.sqlite`). `memhub upgrade` reads it to enumerate
//! instances deterministically instead of scanning the filesystem.
//!
//! Hard rules:
//! - The registry write is **best-effort and never fatal** — a recall
//!   or any other command must not fail because the global store is
//!   busy or unwritable.
//! - It **never creates the global store**. When no global store
//!   exists the registry simply does not exist yet; the common
//!   repo-with-no-global path pays only a single `stat`.
//! - Registry membership is **not** M9 global-memory opt-in. Recall
//!   never reads `known_projects`; global merge stays gated on the
//!   repo's own `[global] enabled`. Populating this table must not
//!   change recall output (the eval-regression guarantee).

use std::path::{Path, PathBuf};

use log::debug;
use rusqlite::{Connection, params};

use crate::Result;

/// One registry row: a repo root memhub has opened on this machine.
#[derive(Debug, Clone)]
pub struct KnownProject {
    pub root_path: PathBuf,
    pub last_seen: String,
    pub last_schema: Option<String>,
}

/// Canonicalize when the path exists so the `root_path` primary key
/// dedups symlinked / relative spellings of the same repo; fall back to
/// the path as-given otherwise.
///
/// `pub(crate)` because the global accept-marker (replay-safe global
/// pending-write accept) must key a repo by the *same* canonical form
/// the registry uses — both are machine-global, repo-keyed tables that
/// have to agree on what "this repo" is.
pub(crate) fn canonical(path: &Path) -> String {
    path.canonicalize()
        .unwrap_or_else(|_| path.to_path_buf())
        .to_string_lossy()
        .to_string()
}

/// Test seam: integration tests legitimately use tempdir repos, which
/// the production ephemeral guard below would (correctly) refuse to
/// register. A test sets `MEMHUB_REGISTRY_TMP_OK=1` to exercise the
/// registry against a tempdir. Never set in production.
fn registry_allows_tmp() -> bool {
    std::env::var_os("MEMHUB_REGISTRY_TMP_OK").is_some()
}

/// A repo living under the OS temp directory is ephemeral by
/// definition (it does not survive a reboot), so registering it for a
/// cross-session, machine-wide concern like `memhub upgrade` is
/// meaningless and only produces dead rows. Excluding it at write time
/// is the boring, deterministic fix; `prune_dead` mops up anything
/// that slips through or dies later.
fn is_ephemeral(path: &Path) -> bool {
    if registry_allows_tmp() {
        return false;
    }
    // Compare against BOTH the raw temp dir and its canonical form.
    // On macOS `$TMPDIR` is `/var/folders/...` whose canonical form is
    // `/private/var/folders/...`; an existing repo canonicalizes to the
    // `/private` form while a non-existent path falls back to the raw
    // form. Matching either prefix makes the guard robust to that
    // asymmetry and to paths that do not exist on disk.
    let tmp = std::env::temp_dir();
    let tmp_c = tmp.canonicalize().unwrap_or_else(|_| tmp.clone());
    let path_c = path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    path_c.starts_with(&tmp) || path_c.starts_with(&tmp_c)
}

/// Ensure just the one registry table exists on this connection. We do
/// NOT run the full migration sweep here: keeping the per-command hot
/// path cheap is the whole point, and migrating the global store is the
/// job of `open_global` / `memhub upgrade`, not of an opportunistic
/// touch. `CREATE TABLE IF NOT EXISTS` is a no-op once present.
fn ensure_table(conn: &Connection) -> Result<()> {
    conn.execute_batch(
        "CREATE TABLE IF NOT EXISTS known_projects (
            root_path   TEXT PRIMARY KEY,
            last_seen   TEXT NOT NULL DEFAULT CURRENT_TIMESTAMP,
            last_schema TEXT
        )",
    )?;
    Ok(())
}

/// Single guarded UPSERT. A brand-new repo always inserts. An existing
/// row is rewritten only when its recorded schema changed or its
/// `last_seen` is older than an hour — so the steady state is one cheap
/// statement that touches no rows.
fn upsert_debounced(conn: &Connection, root_path: &str, schema: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO known_projects(root_path, last_seen, last_schema)
         VALUES (?1, CURRENT_TIMESTAMP, ?2)
         ON CONFLICT(root_path) DO UPDATE SET
             last_seen   = CURRENT_TIMESTAMP,
             last_schema = excluded.last_schema
         WHERE excluded.last_schema IS NOT known_projects.last_schema
            OR known_projects.last_seen < datetime('now', '-1 hour')",
        params![root_path, schema],
    )?;
    Ok(())
}

/// Unconditional UPSERT for explicit registration (`memhub upgrade
/// --also <path>`). Not debounced: an explicit ask always records now.
fn upsert_now(conn: &Connection, root_path: &str, schema: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO known_projects(root_path, last_seen, last_schema)
         VALUES (?1, CURRENT_TIMESTAMP, ?2)
         ON CONFLICT(root_path) DO UPDATE SET
             last_seen   = CURRENT_TIMESTAMP,
             last_schema = excluded.last_schema",
        params![root_path, schema],
    )?;
    Ok(())
}

/// Record (debounced) that `repo_root` was opened, but only if the
/// machine-global store already exists. Best-effort: any failure is
/// logged at debug and swallowed so the caller's command still
/// succeeds. This is the function `db::open_project` calls on every
/// open.
pub fn record_open_best_effort(repo_root: &Path, schema_version: &str) {
    if let Err(e) = record_open_inner(repo_root, schema_version) {
        debug!("registry: skipped recording {} ({e})", repo_root.display());
    }
}

fn record_open_inner(repo_root: &Path, schema_version: &str) -> Result<()> {
    if !super::global_store_exists()? || is_ephemeral(repo_root) {
        return Ok(());
    }
    let path = super::global_db_path()?;
    let conn = super::open_connection(&path)?;
    ensure_table(&conn)?;
    upsert_debounced(&conn, &canonical(repo_root), schema_version)?;
    Ok(())
}

/// Explicitly register a repo root in the machine-global registry so a
/// repo memhub has never opened still shows up in `memhub upgrade`.
/// Returns `false` when there is no global store to persist into (the
/// path is still handled for the current run, just not remembered).
pub fn register(repo_root: &Path, schema_version: &str) -> Result<bool> {
    if !super::global_store_exists()? || is_ephemeral(repo_root) {
        return Ok(false);
    }
    let path = super::global_db_path()?;
    let conn = super::open_connection(&path)?;
    ensure_table(&conn)?;
    upsert_now(&conn, &canonical(repo_root), schema_version)?;
    Ok(true)
}

/// Every repo root the machine-global registry knows about. Empty when
/// no global store exists yet (the bootstrap case — repos self-register
/// on their next open).
pub fn list_known() -> Result<Vec<KnownProject>> {
    if !super::global_store_exists()? {
        return Ok(Vec::new());
    }
    let path = super::global_db_path()?;
    let conn = super::open_connection(&path)?;
    ensure_table(&conn)?;
    let mut stmt = conn.prepare(
        "SELECT root_path, last_seen, last_schema
         FROM known_projects
         ORDER BY root_path",
    )?;
    let rows = stmt
        .query_map([], |r| {
            Ok(KnownProject {
                root_path: PathBuf::from(r.get::<_, String>(0)?),
                last_seen: r.get(1)?,
                last_schema: r.get(2)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// True iff `root` still has a memhub project DB on disk. A registry
/// row whose repo DB is gone is dead weight (deleted repo, vanished
/// throwaway clone) and should be pruned.
fn root_is_live(root: &Path) -> bool {
    root.join(super::MEMHUB_DIR)
        .join(super::DB_FILENAME)
        .exists()
}

/// Registry roots that no longer have a memhub project on disk. Used by
/// `memhub upgrade --dry-run` to report what *would* be pruned without
/// mutating anything.
pub fn dead_roots() -> Result<Vec<PathBuf>> {
    Ok(list_known()?
        .into_iter()
        .map(|kp| kp.root_path)
        .filter(|p| !root_is_live(p))
        .collect())
}

/// Self-heal: delete registry rows whose repo DB no longer exists.
/// Returns how many rows were removed. A no-op (returns 0) when there
/// is no global store. This is what keeps the `memhub upgrade` table
/// honest as repos come and go.
pub fn prune_dead() -> Result<usize> {
    if !super::global_store_exists()? {
        return Ok(0);
    }
    let dead = dead_roots()?;
    if dead.is_empty() {
        return Ok(0);
    }
    let path = super::global_db_path()?;
    let conn = super::open_connection(&path)?;
    ensure_table(&conn)?;
    let mut removed = 0;
    for root in &dead {
        removed += conn.execute(
            "DELETE FROM known_projects WHERE root_path = ?1",
            params![root.to_string_lossy().to_string()],
        )?;
    }
    Ok(removed)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn temp_dir_repos_are_ephemeral_unless_test_seam_set() {
        // SAFETY: this is the only test in this module; it runs in the
        // lib test binary where no other thread toggles this var.
        let probe = std::env::temp_dir().join("memhub-ephemeral-probe");

        unsafe { std::env::remove_var("MEMHUB_REGISTRY_TMP_OK") };
        assert!(
            is_ephemeral(&probe),
            "a repo under the OS temp dir must be excluded from the registry"
        );
        assert!(
            !is_ephemeral(Path::new("/Users/someone/code/realrepo")),
            "a normal home-tree repo must NOT be treated as ephemeral"
        );

        unsafe { std::env::set_var("MEMHUB_REGISTRY_TMP_OK", "1") };
        assert!(
            !is_ephemeral(&probe),
            "the test seam disables the ephemeral guard so tempdir \
             repos can exercise the registry"
        );
        unsafe { std::env::remove_var("MEMHUB_REGISTRY_TMP_OK") };
    }
}
