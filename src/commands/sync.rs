//! Cross-machine Drive sync (M10). memhub stays **offline** — every
//! function here reads or writes only local files. The agent's Drive
//! access is the transport; these commands are the brain it drives.
//!
//! Design anchor:
//! `docs/reference/memhub-prd-addendum-m10-drive-sync.md`.
//!
//! Command surface (all local-file, all offline):
//! - `enable` / `disable` / `enablement_status` — per-repo opt-in.
//! - `snapshot` — clean single-file DB copy + manifest for upload.
//! - `check` — fast-forward verdict of local vs a downloaded snapshot.
//! - `adopt` — gated replace of the local DB with a snapshot.
//! - `commit` — record the post-push baseline in the marker.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::Connection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::config::SyncConfig;
use crate::db;
use crate::sync_md;
use crate::{MemhubError, Result};

/// File names inside a `<project-id>` Drive folder.
pub const SNAPSHOT_FILENAME: &str = "project.sqlite";
pub const MANIFEST_FILENAME: &str = "manifest.json";

/// Sub-namespace inside `[sync] drive_subpath` so memhub owns its own
/// folder even when the synced directory is shared with other tools.
/// The canonical layout is `<drive_subpath>/memhub/<project_id>/`.
pub const DRIVE_NAMESPACE: &str = "memhub";

/// Bumped only on an incompatible manifest shape change. Additive
/// fields ride on `#[serde(default)]` like the export format.
pub const MANIFEST_VERSION: u32 = 1;

/// Logical content version of a memhub DB. Divergence is decided from
/// this, **never** from file bytes — SQLite files are not byte-stable
/// for identical content (page reordering, `VACUUM`), so a file hash
/// would report "changed" on every comparison.
///
/// `writes_log` is appended to by every durable mutation, so its
/// `max_id` / `count` are a cheap monotonic human signal. But equality
/// hinges on `digest` — a hash of the **durable content tables**
/// themselves, not the log. The log records *that* a fact was added,
/// not the fact's key/value; two repos that each added one fact log
/// near-identical rows (differing only by a second-granularity
/// timestamp), so a log-based digest gives dangerous false "equal"
/// verdicts. Hashing the content tables means only genuinely identical
/// content — e.g. one side adopted the other's snapshot byte-for-byte
/// — compares equal, regardless of timing or page layout.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LogicalVersion {
    pub writes_log_max_id: i64,
    pub writes_log_count: i64,
    pub digest: String,
}

/// Durable content tables and the columns that define their content,
/// mirroring what `memhub export` carries. Order is fixed so the
/// digest is deterministic.
const CONTENT_TABLES: &[(&str, &[&str])] = &[
    ("facts", &["id", "key", "value", "confidence", "source", "verified_at", "created_at"]),
    (
        "decisions",
        &["id", "title", "rationale", "status", "decided_at", "superseded_by", "source", "summary"],
    ),
    ("tasks", &["id", "title", "status", "notes", "created_at", "updated_at"]),
    (
        "commands",
        &["id", "kind", "cmdline", "last_exit_code", "last_run_at", "success_count", "fail_count"],
    ),
    (
        "pending_writes",
        &["id", "kind", "payload_json", "rationale", "status", "actor", "actor_raw", "created_at", "reviewed_at"],
    ),
    ("session_notes", &["id", "actor", "actor_raw", "text", "created_at"]),
    ("project_state", &["id", "body", "actor", "actor_raw", "created_at"]),
    ("project_arch", &["id", "body", "actor", "actor_raw", "created_at"]),
];

impl LogicalVersion {
    pub fn read(conn: &Connection) -> Result<Self> {
        let (max_id, count): (Option<i64>, i64) = conn.query_row(
            "SELECT MAX(id), COUNT(*) FROM writes_log WHERE project_id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        let mut hasher = Sha256::new();
        for (table, cols) in CONTENT_TABLES {
            // 0x1d (group separator) delimits tables.
            hasher.update([0x1d]);
            hasher.update(table.as_bytes());
            // Each row is rendered as its columns joined by char(31)
            // (unit separator) inside SQLite; nulls coalesce to empty,
            // so a row is always a single non-null TEXT value.
            let sql = format!(
                "SELECT {} FROM {} WHERE project_id = 1 ORDER BY id",
                row_expr(cols),
                table
            );
            let mut stmt = conn.prepare(&sql)?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                let rendered: String = row.get(0)?;
                hasher.update([0x1e]); // record separator
                hasher.update(rendered.as_bytes());
            }
        }
        let digest = hasher.finalize().iter().map(|b| format!("{b:02x}")).collect();

        Ok(Self {
            writes_log_max_id: max_id.unwrap_or(0),
            writes_log_count: count,
            digest,
        })
    }
}

/// A SQLite expression that renders a row's columns into one non-null
/// TEXT value, separated by the unit-separator char so column
/// boundaries can't blur.
fn row_expr(cols: &[&str]) -> String {
    cols.iter()
        .map(|c| format!("COALESCE(CAST({c} AS TEXT),'')"))
        .collect::<Vec<_>>()
        .join("||char(31)||")
}

/// Sidecar written next to a snapshot in the Drive folder. Carries the
/// logical version (divergence), schema version (the §6 upgrade
/// guard), and the file checksum (integrity against a torn download).
/// The checksum is **of the snapshot file**, so the manifest never
/// includes itself.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Manifest {
    pub manifest_version: u32,
    pub project_id: String,
    pub schema_version: String,
    pub logical_version: LogicalVersion,
    pub file_sha256: String,
    pub machine_id: String,
    pub created_at: String,
    pub memhub_version: String,
}

impl Manifest {
    pub fn load(path: &Path) -> Result<Self> {
        let raw = fs::read_to_string(path)?;
        Ok(serde_json::from_str(&raw)?)
    }
}

#[derive(Debug)]
pub struct SnapshotSummary {
    pub out_dir: PathBuf,
    pub snapshot_path: PathBuf,
    pub manifest_path: PathBuf,
    pub project_id: String,
    pub schema_version: String,
    pub logical_version: LogicalVersion,
    pub file_sha256: String,
    pub bytes: u64,
}

/// Produce a consistent single-file snapshot of the repo DB plus its
/// `manifest.json` under `out_dir`. Uses SQLite `VACUUM INTO` so a
/// live WAL'd DB is captured cleanly (never a raw byte copy — §7).
pub fn snapshot(start: &Path, out_dir: &Path) -> Result<SnapshotSummary> {
    let ctx = db::open_project(start)?;
    require_enabled(&ctx.config.sync)?;

    let project_id = resolve_project_id(&ctx.paths.repo_root, &ctx.config.sync)?;
    let schema_version: String = ctx.conn.query_row(
        "SELECT schema_version FROM projects WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    let logical_version = LogicalVersion::read(&ctx.conn)?;
    let created_at: String =
        ctx.conn
            .query_row("SELECT CURRENT_TIMESTAMP", [], |row| row.get(0))?;

    fs::create_dir_all(out_dir)?;
    let snapshot_path = out_dir.join(SNAPSHOT_FILENAME);
    // `VACUUM INTO` refuses to overwrite an existing file; clear any
    // stale snapshot from a previous run first.
    if snapshot_path.exists() {
        fs::remove_file(&snapshot_path)?;
    }
    vacuum_into(&ctx.conn, &snapshot_path)?;

    let file_sha256 = sha256_file(&snapshot_path)?;
    let bytes = fs::metadata(&snapshot_path)?.len();

    let manifest = Manifest {
        manifest_version: MANIFEST_VERSION,
        project_id: project_id.clone(),
        schema_version: schema_version.clone(),
        logical_version: logical_version.clone(),
        file_sha256: file_sha256.clone(),
        machine_id: machine_id(),
        created_at,
        memhub_version: env!("CARGO_PKG_VERSION").to_string(),
    };
    let manifest_path = out_dir.join(MANIFEST_FILENAME);
    fs::write(&manifest_path, serde_json::to_string_pretty(&manifest)?)?;

    Ok(SnapshotSummary {
        out_dir: out_dir.to_path_buf(),
        snapshot_path,
        manifest_path,
        project_id,
        schema_version,
        logical_version,
        file_sha256,
        bytes,
    })
}

/// Per-machine last-sync marker, stored at `.memhub/sync_marker.json`
/// (gitignored). Records the logical version the two sides agreed on at
/// the last successful sync. Only **one** version is needed: a sync
/// (pull or push) leaves local and Drive byte-identical, so they share
/// a single baseline. Divergence is then "did local move off the
/// baseline?" and "did Drive move off the baseline?", computed
/// independently.
pub const MARKER_FILENAME: &str = "sync_marker.json";

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SyncMarker {
    pub project_id: String,
    /// Logical version both sides shared at last sync.
    pub baseline: LogicalVersion,
    /// sha256 of the snapshot at last sync (identity/integrity aid).
    #[serde(default)]
    pub baseline_file_sha256: String,
    pub synced_at: String,
    /// `"pull"` or `"push"` — informational.
    #[serde(default)]
    pub last_action: String,
}

pub fn marker_path(memhub_dir: &Path) -> PathBuf {
    memhub_dir.join(MARKER_FILENAME)
}

pub fn load_marker(memhub_dir: &Path) -> Result<Option<SyncMarker>> {
    let path = marker_path(memhub_dir);
    if !path.exists() {
        return Ok(None);
    }
    Ok(Some(serde_json::from_str(&fs::read_to_string(&path)?)?))
}

pub fn save_marker(memhub_dir: &Path, marker: &SyncMarker) -> Result<()> {
    fs::write(
        marker_path(memhub_dir),
        serde_json::to_string_pretty(marker)?,
    )?;
    Ok(())
}

/// Fast-forward verdict of the local DB against a Drive snapshot, by
/// exact analogy to git. `NoRemote` = nothing at the given path;
/// `Diverged` with `baseline_present == false` = first sync, no
/// baseline to fast-forward from (the skill phrases that gently).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncVerdict {
    UpToDate,
    LocalAhead,
    DriveAhead,
    Diverged,
    NoRemote,
}

impl SyncVerdict {
    pub fn as_str(&self) -> &'static str {
        match self {
            SyncVerdict::UpToDate => "up-to-date",
            SyncVerdict::LocalAhead => "local-ahead",
            SyncVerdict::DriveAhead => "drive-ahead",
            SyncVerdict::Diverged => "diverged",
            SyncVerdict::NoRemote => "no-remote",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
pub struct CheckReport {
    pub verdict: SyncVerdict,
    pub baseline_present: bool,
    pub project_id: String,
    pub local_logical: LogicalVersion,
    pub remote_logical: Option<LogicalVersion>,
    pub local_schema: String,
    pub remote_schema: Option<String>,
    /// True when the remote snapshot's schema is newer than this
    /// binary can open — adopt must be refused; run `memhub upgrade`.
    pub schema_blocks_adopt: bool,
    /// Set when the remote manifest's project_id does not match this
    /// repo's — a wrong-folder snapshot the caller must not adopt.
    pub project_id_mismatch: Option<String>,
    pub remote_machine_id: Option<String>,
    pub remote_created_at: Option<String>,
}

/// Compare the local DB against the snapshot at `remote_dir` (a
/// directory holding `project.sqlite` + `manifest.json`, or a path to a
/// `manifest.json` directly). Reads only the manifest — never the
/// multi-MB snapshot — so status is cheap.
pub fn check(start: &Path, remote: &Path) -> Result<CheckReport> {
    let ctx = db::open_project(start)?;
    require_enabled(&ctx.config.sync)?;
    let project_id = resolve_project_id(&ctx.paths.repo_root, &ctx.config.sync)?;
    let local_logical = LogicalVersion::read(&ctx.conn)?;
    let local_schema: String = ctx.conn.query_row(
        "SELECT schema_version FROM projects WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    let marker = load_marker(&ctx.paths.memhub_dir)?;
    let baseline_present = marker.is_some();

    let manifest = read_remote_manifest(remote)?;
    let Some(manifest) = manifest else {
        return Ok(CheckReport {
            verdict: SyncVerdict::NoRemote,
            baseline_present,
            project_id,
            local_logical,
            remote_logical: None,
            local_schema,
            remote_schema: None,
            schema_blocks_adopt: false,
            project_id_mismatch: None,
            remote_machine_id: None,
            remote_created_at: None,
        });
    };

    let project_id_mismatch = (manifest.project_id != project_id).then(|| manifest.project_id.clone());
    let schema_blocks_adopt = schema_ordinal(&manifest.schema_version) > schema_ordinal(&local_schema);

    // Did each side move off the shared baseline? With no baseline this
    // is the first sync: equal logical → already in step, else the
    // operator must choose (Diverged, baseline_present=false).
    let verdict = match &marker {
        None => {
            if manifest.logical_version == local_logical {
                SyncVerdict::UpToDate
            } else {
                SyncVerdict::Diverged
            }
        }
        Some(m) => {
            let local_changed = local_logical != m.baseline;
            let drive_changed = manifest.logical_version != m.baseline;
            match (local_changed, drive_changed) {
                (false, false) => SyncVerdict::UpToDate,
                (true, false) => SyncVerdict::LocalAhead,
                (false, true) => SyncVerdict::DriveAhead,
                (true, true) => SyncVerdict::Diverged,
            }
        }
    };

    Ok(CheckReport {
        verdict,
        baseline_present,
        project_id,
        local_logical,
        remote_logical: Some(manifest.logical_version),
        local_schema,
        remote_schema: Some(manifest.schema_version),
        schema_blocks_adopt,
        project_id_mismatch,
        remote_machine_id: Some(manifest.machine_id),
        remote_created_at: Some(manifest.created_at),
    })
}

/// Resolve `remote` to its manifest. Accepts a directory (looks for
/// `manifest.json` inside) or a manifest file path directly. `None`
/// when no manifest is present — the `NoRemote` case.
fn read_remote_manifest(remote: &Path) -> Result<Option<Manifest>> {
    let manifest_path = if remote.is_dir() {
        remote.join(MANIFEST_FILENAME)
    } else {
        remote.to_path_buf()
    };
    if !manifest_path.exists() {
        return Ok(None);
    }
    Ok(Some(Manifest::load(&manifest_path)?))
}

/// Numeric prefix of a migration-style schema version (`"0016_x"` →
/// 16). Schema versions are zero-padded ordinals, so comparing the
/// leading number orders them; unparseable → 0 (treated as oldest).
fn schema_ordinal(schema_version: &str) -> u32 {
    schema_version
        .split('_')
        .next()
        .and_then(|n| n.parse().ok())
        .unwrap_or(0)
}

/// The snapshot file paired with a `remote` argument: inside it when a
/// directory, or its sibling `project.sqlite` when a manifest path.
fn remote_snapshot_file(remote: &Path) -> PathBuf {
    if remote.is_dir() {
        remote.join(SNAPSHOT_FILENAME)
    } else {
        remote.with_file_name(SNAPSHOT_FILENAME)
    }
}

fn remove_sidecars(db_path: &Path) {
    for suffix in ["-wal", "-shm"] {
        let p = PathBuf::from(format!("{}{}", db_path.display(), suffix));
        let _ = fs::remove_file(p);
    }
}

#[derive(Debug)]
pub struct AdoptSummary {
    pub project_id: String,
    pub adopted_from_machine: String,
    pub previous_schema: String,
    pub new_schema: String,
    pub baseline: LogicalVersion,
    pub backup_path: PathBuf,
}

/// Replace the local DB with a downloaded Drive snapshot. Destructive,
/// so it **requires `force`** (the CLI `--yes`); the courier skill only
/// reaches here after the operator confirms a `status` verdict.
///
/// Three checks are **hard refusals regardless of `force`**: a
/// project-id mismatch (wrong Drive folder), a snapshot schema newer
/// than this binary (run `memhub upgrade` first — §6), and a sha256
/// that disagrees with the manifest (torn/partial download). The
/// just-replaced DB is copied to `.memhub/backups/sync/` first as a
/// single most-recent safety net for the swap itself.
pub fn adopt(start: &Path, remote: &Path, force: bool) -> Result<AdoptSummary> {
    let manifest = read_remote_manifest(remote)?.ok_or_else(|| {
        MemhubError::InvalidInput("no snapshot manifest found at the given path".into())
    })?;
    let snapshot_file = remote_snapshot_file(remote);
    if !snapshot_file.exists() {
        return Err(MemhubError::InvalidInput(format!(
            "manifest present but {} is missing at {}",
            SNAPSHOT_FILENAME,
            snapshot_file.display()
        )));
    }

    let ctx = db::open_project(start)?;
    require_enabled(&ctx.config.sync)?;
    let project_id = resolve_project_id(&ctx.paths.repo_root, &ctx.config.sync)?;
    let previous_schema: String = ctx.conn.query_row(
        "SELECT schema_version FROM projects WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    let memhub_dir = ctx.paths.memhub_dir.clone();
    let db_path = ctx.paths.db_path.clone();
    let repo_root = ctx.paths.repo_root.clone();

    // ── Hard refusals (independent of `force`) ──────────────────────
    if manifest.project_id != project_id {
        return Err(MemhubError::InvalidInput(format!(
            "snapshot is for project '{}', not this repo's '{}'; refusing to adopt a \
             wrong-folder snapshot",
            manifest.project_id, project_id
        )));
    }
    if schema_ordinal(&manifest.schema_version) > schema_ordinal(&previous_schema) {
        return Err(MemhubError::InvalidInput(format!(
            "snapshot schema {} is newer than this binary ({}); run `memhub upgrade` first, \
             then retry",
            manifest.schema_version, previous_schema
        )));
    }
    let actual_sha = sha256_file(&snapshot_file)?;
    if actual_sha != manifest.file_sha256 {
        return Err(MemhubError::InvalidInput(
            "snapshot sha256 does not match its manifest (corrupt or partial download); \
             not adopting"
                .into(),
        ));
    }

    // ── Confirmation gate ───────────────────────────────────────────
    if !force {
        return Err(MemhubError::InvalidInput(
            "adopt overwrites the local DB with the Drive snapshot; pass --yes to confirm".into(),
        ));
    }

    // Close the local connection before swapping the file underneath it.
    drop(ctx);

    // Single most-recent safety copy of the DB being replaced.
    let backup_dir = memhub_dir.join("backups").join("sync");
    fs::create_dir_all(&backup_dir)?;
    let backup_path = backup_dir.join("last-replaced.sqlite");
    if db_path.exists() {
        fs::copy(&db_path, &backup_path)?;
    }

    // Drop stale WAL/SHM so they aren't replayed onto the new file, then
    // stage the incoming snapshot beside the DB and rename it into place
    // (same-dir rename is atomic on one filesystem).
    remove_sidecars(&db_path);
    let incoming = memhub_dir.join("project.sqlite.incoming");
    fs::copy(&snapshot_file, &incoming)?;
    fs::rename(&incoming, &db_path)?;

    // Reopen: `open_project` migrates forward if the snapshot was older.
    let ctx = db::open_project(&repo_root)?;
    let new_schema: String = ctx.conn.query_row(
        "SELECT schema_version FROM projects WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    let synced_at: String =
        ctx.conn
            .query_row("SELECT CURRENT_TIMESTAMP", [], |row| row.get(0))?;
    drop(ctx);

    // The agreed baseline is the snapshot's logical version (local now
    // holds exactly that content; a forward migration changes schema,
    // not `writes_log`).
    save_marker(
        &memhub_dir,
        &SyncMarker {
            project_id: project_id.clone(),
            baseline: manifest.logical_version.clone(),
            baseline_file_sha256: manifest.file_sha256.clone(),
            synced_at,
            last_action: "pull".into(),
        },
    )?;

    // Refresh the local managed markdown view from the adopted DB.
    sync_md::sync_project(&repo_root)?;

    Ok(AdoptSummary {
        project_id,
        adopted_from_machine: manifest.machine_id,
        previous_schema,
        new_schema,
        baseline: manifest.logical_version,
        backup_path,
    })
}

#[derive(Debug)]
pub struct CommitSummary {
    pub project_id: String,
    pub baseline: LogicalVersion,
}

/// Record that the local DB now equals the snapshot at `remote` — call
/// after a successful push so the next `status` reads `up-to-date`. The
/// snapshot's manifest is authoritative for what was pushed.
pub fn commit(start: &Path, remote: &Path) -> Result<CommitSummary> {
    let manifest = read_remote_manifest(remote)?.ok_or_else(|| {
        MemhubError::InvalidInput("no snapshot manifest found at the given path".into())
    })?;
    let ctx = db::open_project(start)?;
    require_enabled(&ctx.config.sync)?;
    let project_id = resolve_project_id(&ctx.paths.repo_root, &ctx.config.sync)?;
    if manifest.project_id != project_id {
        return Err(MemhubError::InvalidInput(format!(
            "snapshot project '{}' does not match this repo's '{}'",
            manifest.project_id, project_id
        )));
    }
    let synced_at: String =
        ctx.conn
            .query_row("SELECT CURRENT_TIMESTAMP", [], |row| row.get(0))?;
    save_marker(
        &ctx.paths.memhub_dir,
        &SyncMarker {
            project_id: project_id.clone(),
            baseline: manifest.logical_version.clone(),
            baseline_file_sha256: manifest.file_sha256.clone(),
            synced_at,
            last_action: "push".into(),
        },
    )?;
    Ok(CommitSummary {
        project_id,
        baseline: manifest.logical_version,
    })
}

const SYNC_ACTOR: &str = "cli:user";

#[derive(Debug)]
pub struct EnableResult {
    pub already_enabled: bool,
    /// The resolved Drive-folder id, or the resolution error message
    /// (e.g. "no git remote") so `enable` can guide a no-remote repo to
    /// set `[sync] project_id` without itself failing.
    pub project_id: std::result::Result<String, String>,
}

/// Opt this repo into cross-machine sync (`[sync] enabled = true`).
/// Mirrors `memhub global enable`: idempotent, logs the config change.
pub fn enable(start: &Path) -> Result<EnableResult> {
    let ctx = db::open_project(start)?;
    let already_enabled = ctx.config.sync.enabled;
    let project_id =
        resolve_project_id(&ctx.paths.repo_root, &ctx.config.sync).map_err(|e| e.to_string());

    let mut new_config = ctx.config.clone();
    new_config.sync.enabled = true;
    new_config.save(&ctx.paths.config_path)?;
    db::log_write(&ctx.conn, SYNC_ACTOR, "config", None, "update", "sync enable")?;

    Ok(EnableResult {
        already_enabled,
        project_id,
    })
}

/// Opt this repo back out. Non-destructive: the marker and any local
/// backups stay; the `sync` commands simply refuse again.
pub fn disable(start: &Path) -> Result<()> {
    let ctx = db::open_project(start)?;
    let mut new_config = ctx.config.clone();
    new_config.sync.enabled = false;
    new_config.save(&ctx.paths.config_path)?;
    db::log_write(&ctx.conn, SYNC_ACTOR, "config", None, "update", "sync disable")?;
    Ok(())
}

#[derive(Debug)]
pub struct SyncStatus {
    pub enabled: bool,
    pub project_id: std::result::Result<String, String>,
    pub drive_subpath: String,
    /// Canonical `<drive_subpath>/memhub/<project_id>` push/pull dir, or
    /// the resolution-error message when `drive_subpath` is unset or the
    /// project id cannot be derived.
    pub remote_dir: std::result::Result<String, String>,
    pub local_logical: LogicalVersion,
    pub local_schema: String,
    pub marker: Option<SyncMarker>,
}

/// Enablement + identity view (no Drive comparison). Mirrors
/// `memhub global status`; works whether or not sync is enabled.
pub fn enablement_status(start: &Path) -> Result<SyncStatus> {
    let ctx = db::open_project(start)?;
    let project_id =
        resolve_project_id(&ctx.paths.repo_root, &ctx.config.sync).map_err(|e| e.to_string());
    let local_logical = LogicalVersion::read(&ctx.conn)?;
    let local_schema: String = ctx.conn.query_row(
        "SELECT schema_version FROM projects WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    let marker = load_marker(&ctx.paths.memhub_dir)?;
    let remote_dir = resolve_remote_dir(&ctx.paths.repo_root, &ctx.config.sync)
        .map(|p| p.display().to_string())
        .map_err(|e| e.to_string());
    Ok(SyncStatus {
        enabled: ctx.config.sync.enabled,
        project_id,
        drive_subpath: ctx.config.sync.drive_subpath.clone(),
        remote_dir,
        local_logical,
        local_schema,
        marker,
    })
}

/// `[sync] enabled = false` → refuse with an actionable hint. Mirrors
/// how the global-store commands gate on `[global] enabled`.
pub fn require_enabled(cfg: &SyncConfig) -> Result<()> {
    if cfg.enabled {
        Ok(())
    } else {
        Err(MemhubError::InvalidInput(
            "cross-machine sync is disabled for this repo; run `memhub sync enable` first".into(),
        ))
    }
}

/// The Drive-folder identity for this repo. Prefers an explicit
/// `[sync] project_id` override (the no-remote escape hatch); otherwise
/// derives a stable id from the git remote URL, which both machines
/// share. Errors with a clear instruction when neither is available.
pub fn resolve_project_id(repo_root: &Path, cfg: &SyncConfig) -> Result<String> {
    let override_id = cfg.project_id.trim();
    if !override_id.is_empty() {
        return Ok(override_id.to_string());
    }
    match git_remote_url(repo_root) {
        Some(url) => Ok(remote_to_id(&url)),
        None => Err(MemhubError::InvalidInput(
            "no git remote to derive a sync project id from; set `[sync] project_id` in \
             .memhub/config.toml to pin one"
                .into(),
        )),
    }
}

/// Canonical Drive snapshot directory for this repo:
/// `<drive_subpath>/memhub/<project_id>`. This is the single source of
/// truth for the layout the skills used to hand-concatenate in prose.
/// Errors when `[sync] drive_subpath` is unset or the project id cannot
/// be resolved (no git remote and no `[sync] project_id` override).
pub fn resolve_remote_dir(repo_root: &Path, cfg: &SyncConfig) -> Result<PathBuf> {
    let subpath = cfg.drive_subpath.trim();
    if subpath.is_empty() {
        return Err(MemhubError::InvalidInput(
            "no `[sync] drive_subpath` set in .memhub/config.toml; set it to the absolute \
             path of the synced Drive folder (e.g. your Google Drive for Desktop mount) \
             before syncing"
                .into(),
        ));
    }
    let base = expand_home(subpath)?;
    let project_id = resolve_project_id(repo_root, cfg)?;
    Ok(base.join(DRIVE_NAMESPACE).join(project_id))
}

/// Expand a leading `~` / `~/` (or `~\` on Windows) in `drive_subpath`
/// to the machine home directory. rclone mounts on Linux commonly live
/// under `~` (e.g. `~/gdrive/memhub-sync`), and the config example
/// itself advertises a `~/Library/CloudStorage/...` macOS path — but
/// `Path::join` treats a literal `~` as a directory named `~`, so an
/// un-expanded tilde silently writes the snapshot into a bogus `./~`
/// tree. Only a leading `~` is expanded (no `~user` form); any other
/// path is returned verbatim, so absolute paths are unaffected.
fn expand_home(subpath: &str) -> Result<PathBuf> {
    if subpath == "~" {
        return db::home_dir();
    }
    if let Some(rest) = subpath
        .strip_prefix("~/")
        .or_else(|| subpath.strip_prefix("~\\"))
    {
        return Ok(db::home_dir()?.join(rest));
    }
    Ok(PathBuf::from(subpath))
}

/// Open the project and resolve its canonical remote dir from config.
/// Used by the CLI no-arg default and the MCP `sync_*` tools so neither
/// has to reconstruct `<drive_subpath>/memhub/<project_id>` by hand.
pub fn default_remote_dir(start: &Path) -> Result<PathBuf> {
    let ctx = db::open_project(start)?;
    resolve_remote_dir(&ctx.paths.repo_root, &ctx.config.sync)
}

/// `git -C <root> remote get-url origin`, trimmed. `None` when there is
/// no remote or git is unavailable — the caller turns that into the
/// "set project_id" instruction. OS-agnostic: relies only on `git` on
/// PATH, which both supported platforms have.
fn git_remote_url(repo_root: &Path) -> Option<String> {
    let output = Command::new("git")
        .arg("-C")
        .arg(repo_root)
        .args(["remote", "get-url", "origin"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let url = String::from_utf8_lossy(&output.stdout).trim().to_string();
    if url.is_empty() { None } else { Some(url) }
}

/// `<repo-slug>-<8 hex of sha256(normalized url)>`. Human-legible in a
/// Drive listing while still collision-resistant. Normalization folds
/// the trivial spellings of the same remote (trailing `.git`, trailing
/// slash, case) so the two machines agree.
fn remote_to_id(url: &str) -> String {
    let normalized = normalize_remote(url);
    let mut hasher = Sha256::new();
    hasher.update(normalized.as_bytes());
    let hash = hasher.finalize();
    let short: String = hash.iter().take(4).map(|b| format!("{b:02x}")).collect();
    format!("{}-{}", repo_slug(&normalized), short)
}

/// Canonicalize a git remote to `host/owner/repo` so that the SSH,
/// HTTPS, and `scheme://` spellings of one repo — plus trailing `.git`,
/// trailing slash, and case — all fold to the same id. Without this, a
/// Mac cloned over HTTPS and a Windows cloned over SSH would point at
/// different Drive folders and silently never sync.
fn normalize_remote(url: &str) -> String {
    let mut s = url.trim().to_ascii_lowercase();
    s = s.trim_end_matches('/').to_string();
    s = s.strip_suffix(".git").unwrap_or(&s).to_string();

    // Drop any URL scheme (`https://`, `ssh://`, `git://`, …).
    if let Some((_, rest)) = s.split_once("://") {
        s = rest.to_string();
    }
    // Drop userinfo (`git@host…`), keeping only what's after the `@`
    // that precedes the host.
    if let Some(at) = s.find('@') {
        let before_slash = s.find('/').map(|i| at < i).unwrap_or(true);
        if before_slash {
            s = s[at + 1..].to_string();
        }
    }
    // SCP-style `host:owner/repo` → `host/owner/repo`. Only the first
    // colon (the host/path separator) is rewritten.
    if let Some(colon) = s.find(':') {
        let is_path_sep = !s[..colon].contains('/');
        if is_path_sep {
            s.replace_range(colon..colon + 1, "/");
        }
    }
    s
}

/// Last path-ish segment of the remote, reduced to `[a-z0-9-]`, capped.
/// Falls back to `repo` when nothing usable remains.
fn repo_slug(normalized_url: &str) -> String {
    let tail = normalized_url
        .rsplit(['/', ':'])
        .find(|s| !s.is_empty())
        .unwrap_or("repo");
    let slug: String = tail
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '-' })
        .collect();
    let slug = slug.trim_matches('-');
    if slug.is_empty() {
        "repo".to_string()
    } else {
        slug.chars().take(32).collect()
    }
}

/// Best-effort, OS-agnostic host label for "who pushed this". Not a
/// security boundary — just human context in the manifest.
///
/// Prefers the `hostname` binary, which exists on macOS, Linux, and
/// Windows; the per-platform env var is a fallback. (On macOS+zsh
/// `HOSTNAME` is a *shell* variable, not an environment one, so the
/// env path alone reports nothing in a non-interactive shell.)
/// `unknown-host` when all paths fail.
fn machine_id() -> String {
    if let Ok(out) = Command::new("hostname").output()
        && out.status.success()
    {
        let name = String::from_utf8_lossy(&out.stdout).trim().to_string();
        if !name.is_empty() {
            return name;
        }
    }
    let env_var = if cfg!(windows) { "COMPUTERNAME" } else { "HOSTNAME" };
    std::env::var(env_var)
        .ok()
        .map(|s| s.trim().to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "unknown-host".to_string())
}

/// `VACUUM INTO '<path>'`. SQLite parses the destination as a string
/// expression; we interpolate an escaped literal (double any single
/// quote) rather than bind, since `VACUUM` is not a prepared-parameter
/// statement. The path is memhub-internal, never user SQL.
fn vacuum_into(conn: &Connection, dest: &Path) -> Result<()> {
    let dest = dest.to_str().ok_or_else(|| {
        MemhubError::InvalidInput("snapshot path is not valid UTF-8".into())
    })?;
    let escaped = dest.replace('\'', "''");
    conn.execute_batch(&format!("VACUUM INTO '{escaped}';"))?;
    Ok(())
}

pub fn sha256_file(path: &Path) -> Result<String> {
    let mut file = File::open(path)?;
    let mut hasher = Sha256::new();
    let mut buf = [0u8; 64 * 1024];
    loop {
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        hasher.update(&buf[..n]);
    }
    Ok(hasher.finalize().iter().map(|b| format!("{b:02x}")).collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{fact, init};
    use tempfile::tempdir;

    fn enable_sync(repo: &Path) {
        let ctx = db::open_project(repo).expect("open");
        let mut cfg = ctx.config.clone();
        cfg.sync.enabled = true;
        cfg.sync.project_id = "test-proj-abcd1234".to_string();
        cfg.save(&ctx.paths.config_path).expect("save config");
    }

    #[test]
    fn normalize_remote_folds_all_spellings_of_one_repo() {
        let canonical = "github.com/kninetimmy/memhub";
        for spelling in [
            "git@github.com:kninetimmy/memhub.git",
            "git@github.com:kninetimmy/memhub",
            "https://github.com/kninetimmy/memhub.git",
            "https://github.com/kninetimmy/memhub/",
            "https://github.com/KNinetimmy/Memhub",
            "ssh://git@github.com/kninetimmy/memhub.git",
        ] {
            assert_eq!(
                normalize_remote(spelling),
                canonical,
                "spelling {spelling:?} should canonicalize to {canonical:?}"
            );
        }
    }

    #[test]
    fn remote_to_id_is_stable_and_legible() {
        // SSH and HTTPS forms of the same repo must yield the SAME id —
        // a Mac-over-HTTPS / Windows-over-SSH clone must land in one
        // Drive folder.
        let id_ssh = remote_to_id("git@github.com:kninetimmy/memhub.git");
        let id_https = remote_to_id("https://github.com/kninetimmy/memhub/");
        assert_eq!(id_ssh, id_https, "ssh and https forms must share an id");
        assert!(id_ssh.starts_with("memhub-"), "id carries repo slug: {id_ssh}");
        assert!(
            id_ssh.rsplit('-').next().unwrap().chars().all(|c| c.is_ascii_hexdigit()),
            "id ends in a hex hash: {id_ssh}"
        );
    }

    #[test]
    fn resolve_project_id_prefers_config_override() {
        let cfg = SyncConfig {
            enabled: true,
            project_id: "  pinned-id  ".into(),
            drive_subpath: String::new(),
        };
        let got = resolve_project_id(Path::new("/nonexistent"), &cfg).expect("override");
        assert_eq!(got, "pinned-id", "override is trimmed and used verbatim");
    }

    #[test]
    fn resolve_remote_dir_joins_namespace_and_project_id() {
        let cfg = SyncConfig {
            enabled: true,
            project_id: "pinned-id".into(),
            drive_subpath: "/mnt/drive/memhub-sync".into(),
        };
        let dir = resolve_remote_dir(Path::new("/nonexistent"), &cfg).expect("resolve");
        assert_eq!(
            dir,
            Path::new("/mnt/drive/memhub-sync").join("memhub").join("pinned-id"),
            "canonical layout is <drive_subpath>/memhub/<project_id>"
        );
    }

    #[test]
    fn resolve_remote_dir_expands_leading_tilde() {
        // rclone mounts on Linux (and the advertised macOS CloudStorage
        // path) commonly start with `~`; it must resolve to $HOME, not a
        // literal `~` directory.
        let home = db::home_dir().expect("home dir for test");
        let cfg = SyncConfig {
            enabled: true,
            project_id: "pinned-id".into(),
            drive_subpath: "~/gdrive/memhub-sync".into(),
        };
        let dir = resolve_remote_dir(Path::new("/nonexistent"), &cfg).expect("resolve");
        assert_eq!(
            dir,
            home.join("gdrive")
                .join("memhub-sync")
                .join("memhub")
                .join("pinned-id"),
            "leading ~/ expands to $HOME"
        );
        assert!(
            !dir.components().any(|c| c.as_os_str() == "~"),
            "no literal ~ component survives: {dir:?}"
        );
    }

    #[test]
    fn expand_home_leaves_absolute_paths_verbatim() {
        // An absolute path (the macOS/Windows setups in use today) must
        // be untouched — only a *leading* tilde is special.
        assert_eq!(
            expand_home("/mnt/drive/memhub-sync").expect("abs"),
            PathBuf::from("/mnt/drive/memhub-sync"),
        );
    }

    #[test]
    fn resolve_remote_dir_errors_without_drive_subpath() {
        let cfg = SyncConfig {
            enabled: true,
            project_id: "pinned-id".into(),
            drive_subpath: "   ".into(),
        };
        let err = resolve_remote_dir(Path::new("/nonexistent"), &cfg)
            .expect_err("must require drive_subpath");
        assert!(matches!(err, MemhubError::InvalidInput(_)));
    }

    #[test]
    fn snapshot_is_disabled_until_opted_in() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let out = temp.path().join("out");
        let err = snapshot(temp.path(), &out).expect_err("must refuse when disabled");
        assert!(matches!(err, MemhubError::InvalidInput(_)));
        assert!(!out.exists(), "no files written when disabled");
    }

    #[test]
    fn snapshot_writes_consistent_db_and_manifest() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        enable_sync(temp.path());

        // A durable write so the logical version is non-trivial.
        fact::add(temp.path(), "build-cmd", "cargo build", "user", "cli:user").expect("fact");

        let out = temp.path().join("drive").join("proj");
        let summary = snapshot(temp.path(), &out).expect("snapshot");

        assert!(summary.snapshot_path.exists(), "snapshot db written");
        assert!(summary.manifest_path.exists(), "manifest written");
        assert_eq!(summary.project_id, "test-proj-abcd1234");
        assert!(summary.bytes > 0);
        assert!(
            summary.logical_version.writes_log_count > 0,
            "the fact write is reflected in the logical version"
        );

        // Manifest round-trips and its checksum matches the file on disk.
        let manifest = Manifest::load(&summary.manifest_path).expect("load manifest");
        assert_eq!(manifest.manifest_version, MANIFEST_VERSION);
        assert_eq!(manifest.file_sha256, summary.file_sha256);
        assert_eq!(
            manifest.file_sha256,
            sha256_file(&summary.snapshot_path).expect("rehash"),
            "manifest checksum must equal the snapshot file's actual hash"
        );
        assert_eq!(manifest.logical_version, summary.logical_version);

        // The snapshot is a real SQLite file (consistent VACUUM INTO copy).
        let header = fs::read(&summary.snapshot_path).expect("read");
        assert!(header.starts_with(b"SQLite format 3\0"), "valid sqlite file");
    }

    /// Write a manifest with a chosen logical/schema version into `dir`,
    /// so status tests can stand in for "what another machine pushed".
    fn write_remote_manifest(
        dir: &Path,
        project_id: &str,
        logical: LogicalVersion,
        schema: &str,
    ) {
        fs::create_dir_all(dir).expect("mkdir");
        let manifest = Manifest {
            manifest_version: MANIFEST_VERSION,
            project_id: project_id.to_string(),
            schema_version: schema.to_string(),
            logical_version: logical,
            file_sha256: "deadbeef".into(),
            machine_id: "other-machine".into(),
            created_at: "2026-05-22 00:00:00".into(),
            memhub_version: "0.1.0".into(),
        };
        fs::write(
            dir.join(MANIFEST_FILENAME),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .expect("write manifest");
    }

    fn local_state(repo: &Path) -> (LogicalVersion, String) {
        let ctx = db::open_project(repo).expect("open");
        let lv = LogicalVersion::read(&ctx.conn).expect("logical");
        let schema: String = ctx
            .conn
            .query_row("SELECT schema_version FROM projects WHERE id = 1", [], |r| {
                r.get(0)
            })
            .expect("schema");
        (lv, schema)
    }

    #[test]
    fn status_reports_no_remote_when_path_empty() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        enable_sync(temp.path());
        let report = check(temp.path(), &temp.path().join("empty")).expect("status");
        assert_eq!(report.verdict, SyncVerdict::NoRemote);
        assert!(report.remote_logical.is_none());
    }

    #[test]
    fn status_first_sync_equal_is_up_to_date_else_diverged() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        enable_sync(temp.path());
        let (local, schema) = local_state(temp.path());

        // No marker yet. Equal logical → up-to-date.
        let same = temp.path().join("same");
        write_remote_manifest(&same, "test-proj-abcd1234", local.clone(), &schema);
        assert_eq!(
            check(temp.path(), &same).expect("status").verdict,
            SyncVerdict::UpToDate
        );

        // No marker, different logical → diverged (operator chooses).
        let diff = temp.path().join("diff");
        let bumped = LogicalVersion {
            writes_log_max_id: local.writes_log_max_id + 5,
            writes_log_count: local.writes_log_count + 5,
            digest: "different-digest".into(),
        };
        write_remote_manifest(&diff, "test-proj-abcd1234", bumped, &schema);
        let report = check(temp.path(), &diff).expect("status");
        assert_eq!(report.verdict, SyncVerdict::Diverged);
        assert!(!report.baseline_present, "first-sync diverge has no baseline");
    }

    #[test]
    fn status_local_ahead_and_drive_ahead_with_baseline() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        enable_sync(temp.path());
        let (baseline, schema) = local_state(temp.path());

        // Pretend the last sync agreed on the current local version.
        let ctx = db::open_project(temp.path()).expect("open");
        save_marker(
            &ctx.paths.memhub_dir,
            &SyncMarker {
                project_id: "test-proj-abcd1234".into(),
                baseline: baseline.clone(),
                baseline_file_sha256: "deadbeef".into(),
                synced_at: "2026-05-22 00:00:00".into(),
                last_action: "pull".into(),
            },
        )
        .expect("save marker");

        // Drive still at baseline, but local moved on → local-ahead.
        let remote = temp.path().join("remote");
        write_remote_manifest(&remote, "test-proj-abcd1234", baseline.clone(), &schema);
        fact::add(temp.path(), "k", "v", "user", "cli:user").expect("fact");
        assert_eq!(
            check(temp.path(), &remote).expect("status").verdict,
            SyncVerdict::LocalAhead
        );

        // Now drive moves ahead of the baseline too while local also
        // changed → diverged; and if local were AT baseline it'd be
        // drive-ahead. Re-derive a fresh baseline == current local to
        // isolate the drive-ahead case.
        let (fresh_local, _) = local_state(temp.path());
        save_marker(
            &ctx.paths.memhub_dir,
            &SyncMarker {
                project_id: "test-proj-abcd1234".into(),
                baseline: fresh_local.clone(),
                baseline_file_sha256: "deadbeef".into(),
                synced_at: "2026-05-22 00:00:00".into(),
                last_action: "push".into(),
            },
        )
        .expect("save marker");
        let drive_ahead = LogicalVersion {
            writes_log_max_id: fresh_local.writes_log_max_id + 10,
            writes_log_count: fresh_local.writes_log_count + 10,
            digest: "drive-moved-on".into(),
        };
        write_remote_manifest(&remote, "test-proj-abcd1234", drive_ahead, &schema);
        assert_eq!(
            check(temp.path(), &remote).expect("status").verdict,
            SyncVerdict::DriveAhead
        );
    }

    #[test]
    fn status_flags_newer_schema_and_project_mismatch() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        enable_sync(temp.path());
        let (local, _) = local_state(temp.path());

        let remote = temp.path().join("remote");
        // A wrong-folder snapshot from a far-future schema.
        write_remote_manifest(&remote, "some-other-project", local, "9999_future_schema");
        let report = check(temp.path(), &remote).expect("status");
        assert!(report.schema_blocks_adopt, "newer schema blocks adopt");
        assert_eq!(
            report.project_id_mismatch.as_deref(),
            Some("some-other-project"),
            "mismatched project id is surfaced"
        );
    }

    #[test]
    fn schema_ordinal_parses_migration_prefix() {
        assert_eq!(schema_ordinal("0016_global_accept_markers"), 16);
        assert_eq!(schema_ordinal("0001_initial"), 1);
        assert_eq!(schema_ordinal("garbage"), 0);
        assert!(schema_ordinal("9999_future") > schema_ordinal("0016_x"));
    }

    fn fact_keys(repo: &Path) -> Vec<String> {
        let ctx = db::open_project(repo).expect("open");
        let mut stmt = ctx
            .conn
            .prepare("SELECT key FROM facts WHERE project_id = 1 ORDER BY key")
            .expect("prepare");
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .expect("query");
        rows.filter_map(|r| r.ok()).collect()
    }

    /// A fresh repo opted into sync with the shared test project id.
    fn new_synced_repo() -> tempfile::TempDir {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        enable_sync(temp.path());
        temp
    }

    #[test]
    fn adopt_refuses_without_force_and_leaves_local_intact() {
        let a = new_synced_repo();
        fact::add(a.path(), "alpha", "1", "user", "cli:user").expect("fact");
        let drive = a.path().join("drive");
        snapshot(a.path(), &drive).expect("snapshot");

        let b = new_synced_repo();
        fact::add(b.path(), "beta", "2", "user", "cli:user").expect("fact");

        let err = adopt(b.path(), &drive, false).expect_err("must require --yes");
        assert!(matches!(err, MemhubError::InvalidInput(_)));
        assert_eq!(fact_keys(b.path()), vec!["beta"], "local DB untouched");
    }

    #[test]
    fn adopt_round_trip_replaces_local_sets_baseline_up_to_date() {
        let a = new_synced_repo();
        fact::add(a.path(), "alpha", "1", "user", "cli:user").expect("fact");
        let drive = a.path().join("drive");
        snapshot(a.path(), &drive).expect("snapshot");

        let b = new_synced_repo();
        fact::add(b.path(), "beta", "2", "user", "cli:user").expect("fact");
        // Sanity: before adopt, b and a differ → diverged (no baseline).
        assert_eq!(
            check(b.path(), &drive).expect("status").verdict,
            SyncVerdict::Diverged
        );

        let summary = adopt(b.path(), &drive, true).expect("adopt");
        assert!(summary.backup_path.exists(), "replaced DB was backed up");

        // b now holds a's content, not its own.
        assert_eq!(fact_keys(b.path()), vec!["alpha"], "adopted a's data");

        // And the marker makes a re-check up-to-date.
        assert_eq!(
            check(b.path(), &drive).expect("status").verdict,
            SyncVerdict::UpToDate
        );
    }

    #[test]
    fn adopt_hard_refuses_mismatch_newer_schema_and_bad_checksum() {
        let a = new_synced_repo();
        fact::add(a.path(), "alpha", "1", "user", "cli:user").expect("fact");
        let b = new_synced_repo();

        // Wrong project id.
        let mismatched = a.path().join("mismatch");
        snapshot(a.path(), &mismatched).expect("snapshot");
        rewrite_manifest(&mismatched, |m| m.project_id = "other".into());
        assert!(adopt(b.path(), &mismatched, true).is_err(), "project mismatch refused");

        // Newer schema than this binary.
        let newer = a.path().join("newer");
        snapshot(a.path(), &newer).expect("snapshot");
        rewrite_manifest(&newer, |m| m.schema_version = "9999_future".into());
        assert!(adopt(b.path(), &newer, true).is_err(), "newer schema refused");

        // Checksum disagreement (tampered snapshot).
        let tampered = a.path().join("tampered");
        snapshot(a.path(), &tampered).expect("snapshot");
        {
            use std::io::Write;
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(tampered.join(SNAPSHOT_FILENAME))
                .expect("open snapshot");
            f.write_all(b"corruption").expect("tamper");
        }
        assert!(adopt(b.path(), &tampered, true).is_err(), "bad checksum refused");

        // Through all refusals, b's DB is still its pristine empty self.
        assert!(fact_keys(b.path()).is_empty(), "no partial adopt occurred");
    }

    #[test]
    fn commit_records_push_baseline_as_up_to_date() {
        let a = new_synced_repo();
        fact::add(a.path(), "alpha", "1", "user", "cli:user").expect("fact");
        let drive = a.path().join("drive");
        snapshot(a.path(), &drive).expect("snapshot");

        // Before commit there is no baseline; equal logical → up-to-date,
        // but commit is what records the agreement after a push.
        commit(a.path(), &drive).expect("commit");
        let ctx = db::open_project(a.path()).expect("open");
        let marker = load_marker(&ctx.paths.memhub_dir).expect("load").expect("marker");
        assert_eq!(marker.last_action, "push");

        // A further local write now reads as local-ahead, proving the
        // baseline took.
        fact::add(a.path(), "gamma", "3", "user", "cli:user").expect("fact");
        assert_eq!(
            check(a.path(), &drive).expect("status").verdict,
            SyncVerdict::LocalAhead
        );
    }

    fn rewrite_manifest(dir: &Path, f: impl FnOnce(&mut Manifest)) {
        let path = dir.join(MANIFEST_FILENAME);
        let mut manifest = Manifest::load(&path).expect("load manifest");
        f(&mut manifest);
        fs::write(&path, serde_json::to_string_pretty(&manifest).unwrap()).expect("rewrite");
    }
}
