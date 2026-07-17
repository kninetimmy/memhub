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
use std::time::Duration;

use rusqlite::backup::{Backup, StepResult};
use rusqlite::{Connection, OpenFlags};
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

/// Durable content tables and the columns that define their content.
/// Order is fixed so the digest is deterministic.
///
/// This is the authoritative list of what divergence the sync check
/// gates on. It is a **superset** of `memhub export` — a whole-DB sync
/// snapshot (`VACUUM INTO`) carries every table, including `documents` /
/// `doc_chunks`, which `export` deliberately omits — so the digest must
/// cover them too or two machines that each ingest a different doc would
/// compare EQUAL over real divergence (audit finding F2).
///
/// Coverage is hand-maintained but drift-proofed: the
/// `content_tables_cover_the_live_schema` test asserts every live table
/// and column is either digested here or on an explicit exemption list
/// with a stated reason. Adding a durable table/column without updating
/// one of the two lists turns that test red.
///
/// `project_id` is intentionally never digested (it is the constant
/// singleton partition key, always 1, and the digest already filters
/// `WHERE project_id = 1`). `documents` / `doc_chunks` also omit their
/// surrogate `id` and local ingest timestamp so two machines that
/// independently ingest the *same* document at the same path converge to
/// an equal digest; their content identity is the natural
/// (path/title/hash/…) and (doc_id/ord/heading/body) columns. Both
/// exemptions are recorded in `column_exempt` alongside their reasons.
const CONTENT_TABLES: &[(&str, &[&str])] = &[
    (
        "facts",
        &[
            "id",
            "key",
            "value",
            "confidence",
            "source",
            "verified_at",
            "created_at",
            "kind",
            "superseded_by",
        ],
    ),
    (
        "decisions",
        &[
            "id",
            "title",
            "rationale",
            "status",
            "decided_at",
            "superseded_by",
            "source",
            "summary",
        ],
    ),
    (
        "tasks",
        &["id", "title", "status", "notes", "created_at", "updated_at"],
    ),
    (
        "commands",
        &[
            "id",
            "kind",
            "cmdline",
            "last_exit_code",
            "last_run_at",
            "success_count",
            "fail_count",
        ],
    ),
    (
        "pending_writes",
        &[
            "id",
            "kind",
            "payload_json",
            "rationale",
            "status",
            "actor",
            "actor_raw",
            "created_at",
            "provenance_json",
            "reviewed_at",
        ],
    ),
    (
        "session_notes",
        &["id", "actor", "actor_raw", "text", "created_at"],
    ),
    (
        "project_state",
        &["id", "body", "actor", "actor_raw", "created_at"],
    ),
    (
        "project_arch",
        &["id", "body", "actor", "actor_raw", "created_at"],
    ),
    (
        "documents",
        &["path", "title", "content_hash", "byte_len", "source"],
    ),
    ("doc_chunks", &["doc_id", "ord", "heading_path", "body"]),
];

impl LogicalVersion {
    pub fn read(conn: &Connection) -> Result<Self> {
        let (max_id, count): (Option<i64>, i64) = conn.query_row(
            "SELECT MAX(id), COUNT(*) FROM writes_log WHERE project_id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        // Self-describing, length-prefixed encoding (F2). The old scheme
        // COALESCE'd NULL to '' (so NULL and empty string collided) and
        // joined columns with 0x1f/0x1e/0x1d separators that can occur
        // inside TEXT (so embedded separator bytes could blur row/column
        // boundaries). Here every value carries its own length and a
        // NULL/TEXT tag, and rows and tables are framed with control
        // bytes, so the byte stream is a prefix-free encoding of the
        // content — two DBs hash equal iff their digested content is
        // byte-for-byte identical, and NULL ≠ '' ≠ any other value.
        let mut hasher = Sha256::new();
        for (table, cols) in CONTENT_TABLES {
            // Length-framed table name so no table's bytes can bleed into
            // the next.
            hash_len_prefixed(&mut hasher, table.as_bytes());
            // Each column is CAST to TEXT individually (NULL stays NULL —
            // it is not coalesced), so integer/real columns render
            // deterministically while NULL remains distinguishable.
            let select_list = cols
                .iter()
                .map(|c| format!("CAST({c} AS TEXT)"))
                .collect::<Vec<_>>()
                .join(", ");
            let sql =
                format!("SELECT {select_list} FROM {table} WHERE project_id = 1 ORDER BY id");
            let mut stmt = conn.prepare(&sql)?;
            let mut rows = stmt.query([])?;
            while let Some(row) = rows.next()? {
                // 0x01 = "a row follows"; distinguished from the 0x00
                // end-of-table marker below. The column count is fixed per
                // table, so the parse of the framed values is unambiguous.
                hasher.update([ROW_PRESENT]);
                for i in 0..cols.len() {
                    match row.get::<_, Option<String>>(i)? {
                        // 0x00 tag, no length: a NULL, distinct from an
                        // empty string (0x01 tag + length 0).
                        None => hasher.update([VALUE_NULL]),
                        Some(s) => {
                            hasher.update([VALUE_TEXT]);
                            hash_len_prefixed(&mut hasher, s.as_bytes());
                        }
                    }
                }
            }
            // 0x00 closes the table's row stream.
            hasher.update([TABLE_END]);
        }
        let digest = hasher
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();

        Ok(Self {
            writes_log_max_id: max_id.unwrap_or(0),
            writes_log_count: count,
            digest,
        })
    }
}

/// Control bytes for the digest's prefix-free row/value framing (see
/// [`LogicalVersion::read`]). Kept distinct so a row-boundary marker can
/// never be confused with a value's NULL/TEXT tag during the (conceptual)
/// parse that makes the encoding injective.
const ROW_PRESENT: u8 = 0x01;
const TABLE_END: u8 = 0x00;
const VALUE_NULL: u8 = 0x00;
const VALUE_TEXT: u8 = 0x01;

/// Feed `bytes` into `hasher` prefixed by its length as 8 little-endian
/// bytes, so an arbitrary byte payload cannot blur into whatever follows
/// it — the length says exactly where it ends regardless of its content.
fn hash_len_prefixed(hasher: &mut Sha256, bytes: &[u8]) {
    hasher.update((bytes.len() as u64).to_le_bytes());
    hasher.update(bytes);
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
pub fn snapshot(start: &Path, out_dir: &Path, force: bool) -> Result<SnapshotSummary> {
    // Push-side clobber gate (F12). Writing the snapshot into the synced
    // folder *is* the push and overwrites whatever is already there. Refuse
    // to stomp a remote that is ahead of or diverged from local unless
    // explicitly forced — the "lossy case is operator-gated" guarantee has
    // to hold on push, not only on pull (adopt). A first push (no remote
    // yet) reports `no-remote` and proceeds.
    if !force {
        let report = check(start, out_dir)?;
        if matches!(
            report.verdict,
            SyncVerdict::DriveAhead | SyncVerdict::Diverged
        ) {
            return Err(MemhubError::InvalidInput(format!(
                "refusing to overwrite the remote: it is {} relative to local. \
                 Pull first (`memhub sync check`, then `sync adopt`), or pass \
                 --force to overwrite the remote with this machine's state.",
                report.verdict.as_str()
            )));
        }
    }

    let ctx = db::open_project(start)?;
    require_enabled(&ctx.config.sync)?;

    let project_id = resolve_project_id(&ctx.paths.repo_root, &ctx.config.sync)?;
    let schema_version: String = ctx.conn.query_row(
        "SELECT schema_version FROM projects WHERE id = 1",
        [],
        |row| row.get(0),
    )?;
    let logical_version = LogicalVersion::read(&ctx.conn)?;
    let created_at: String = ctx
        .conn
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

    // A snapshot written into the repo's *canonical* remote dir just
    // pushed the DB to the shared destination -- record the same
    // baseline `commit()` would, so a push is atomic with its own
    // marker instead of leaving a stale one until a required second
    // step. A snapshot to anywhere else (an inspection copy, a test
    // fixture dir) must never touch the marker: stamping a baseline for
    // a push that never reached the real remote would let a later
    // check() report a false DriveAhead/UpToDate over a true Diverged.
    if is_canonical_remote_dir(&ctx.paths.repo_root, &ctx.config.sync, out_dir) {
        save_marker(
            &ctx.paths.memhub_dir,
            &SyncMarker {
                project_id: manifest.project_id.clone(),
                baseline: manifest.logical_version.clone(),
                baseline_file_sha256: manifest.file_sha256.clone(),
                synced_at: manifest.created_at.clone(),
                last_action: "push".into(),
            },
        )?;
    }

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

/// Whether `out_dir` is this repo's canonical remote dir (`<drive_subpath>
/// /memhub/<project_id>`, per `resolve_remote_dir`) -- used to decide
/// whether `snapshot()` should record the push baseline automatically.
/// Paths are compared via `fs::canonicalize` (the caller only reaches
/// here after `create_dir_all(out_dir)`, so it's guaranteed to exist) so
/// a `\\?\`-prefixed or differently-cased spelling of the same directory
/// on Windows doesn't produce a false negative.
///
/// Any failure -- `resolve_remote_dir` erroring (no `drive_subpath`
/// configured, the case for every existing unit test's bare
/// `enable_sync` fixture) or either side failing to canonicalize -- is
/// treated as "not canonical". That fails toward the pre-existing
/// behavior (no automatic marker write, `commit()` still required) and
/// is the safe direction: it never risks stamping a baseline for a
/// snapshot that did not actually land at the real remote.
fn is_canonical_remote_dir(repo_root: &Path, cfg: &SyncConfig, out_dir: &Path) -> bool {
    let Ok(remote_dir) = resolve_remote_dir(repo_root, cfg) else {
        return false;
    };
    match (fs::canonicalize(&remote_dir), fs::canonicalize(out_dir)) {
        (Ok(a), Ok(b)) => a == b,
        _ => false,
    }
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

    let project_id_mismatch =
        (manifest.project_id != project_id).then(|| manifest.project_id.clone());
    // The local schema always parses (it is written by our own
    // migrations); a remote schema that does not parse can never be
    // adopted (adopt hard-refuses it), so `check` reports it as blocking
    // rather than as a spurious "older, safe to adopt".
    let local_ordinal = schema_ordinal(&local_schema)?;
    let schema_blocks_adopt = match schema_ordinal(&manifest.schema_version) {
        Ok(remote_ordinal) => remote_ordinal > local_ordinal,
        Err(_) => true,
    };

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
            // Local and remote already hold identical content, regardless
            // of what the stored baseline says. This self-heals a marker
            // that fell behind a real sync (e.g. a push that skipped
            // `commit()` before this baseline-on-push fix existed) so a
            // historically-missed commit doesn't keep reporting a stale
            // verdict forever.
            if manifest.logical_version == local_logical {
                SyncVerdict::UpToDate
            } else {
                let local_changed = local_logical != m.baseline;
                let drive_changed = manifest.logical_version != m.baseline;
                match (local_changed, drive_changed) {
                    (false, false) => SyncVerdict::UpToDate,
                    (true, false) => SyncVerdict::LocalAhead,
                    (false, true) => SyncVerdict::DriveAhead,
                    (true, true) => SyncVerdict::Diverged,
                }
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
/// leading number orders them.
///
/// **Fail-closed** (F4/X6): an empty or non-numeric prefix is an error,
/// never a silent `0`. Collapsing an unparseable version to "oldest"
/// would let a garbage or hostile manifest `schema_version` slip *under*
/// the newer-schema refusal in [`adopt`] and get installed by a binary
/// that may not understand it — the exact hole this hardening closes.
/// This mirrors the upward-only `MAX(schema_version)` ratchet in
/// `db::upsert_project`, which likewise refuses to let an unknown/newer
/// schema be treated as safe.
fn schema_ordinal(schema_version: &str) -> Result<u32> {
    schema_version
        .split('_')
        .next()
        .and_then(|n| n.parse().ok())
        .ok_or_else(|| {
            MemhubError::InvalidInput(format!(
                "schema_version '{schema_version}' is not a parseable migration ordinal \
                 (expected a zero-padded 'NNNN_name')"
            ))
        })
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

/// Name of the local staging copy `adopt` writes before hashing and
/// installing. Sits inside `.memhub/` (same filesystem as the live DB),
/// never in the Drive folder, so the bytes we hash are the bytes we
/// install — Drive cannot rewrite it between the two.
const INCOMING_FILENAME: &str = "project.sqlite.incoming";

/// Bound on how long the online-backup restore waits for a live external
/// writer to release the destination DB before refusing. SQLite's backup
/// step reports BUSY/LOCKED **without writing anything** when it cannot
/// lock the destination, so a bounded retry that then errors out leaves
/// the original DB fully intact — never a torn, half-restored file.
/// ~2 s worst case (20 × 100 ms), comfortably longer than memhub's own
/// millisecond-scale writes yet still a prompt, deterministic give-up.
const RESTORE_BUSY_RETRIES: u32 = 20;
const RESTORE_BUSY_PAUSE: Duration = Duration::from_millis(100);

/// Restore the pages of the staged snapshot into the live DB **in place**
/// via SQLite's online-backup API, instead of deleting/renaming the DB
/// file out from under any process that holds it open. This is the
/// cross-process-safe swap (F4/X6):
///
/// * **Windows:** a DB file another `memhub` process (a CLI invocation or
///   the long-lived MCP server) holds open cannot be renamed or deleted —
///   the OS raises a sharing violation, so the old delete-`-wal`/`-shm`
///   + `rename` swap would fail outright. The backup API instead writes
///   *through* SQLite's own file locking, so a concurrent process is
///   serialized against and always sees a coherent DB.
/// * **POSIX:** renaming over an open DB there does *not* error; it
///   silently orphans the other process onto the old (now-unlinked)
///   inode, and unlinking `-wal`/`-shm` beside a live connection lets it
///   keep writing to a WAL that no longer matches the DB — a silent
///   divergence. Writing through the backup API avoids both.
///
/// A single `step(-1)` copies every page in one operation, holding the
/// destination's write lock for its duration, so other connections
/// observe either the pre- or post-restore DB, never a partial mix. If
/// the destination is locked, `step` copies nothing and reports BUSY; we
/// retry a bounded number of times and then refuse cleanly, leaving the
/// original DB untouched.
fn restore_into_live_db(staged: &Path, dest_path: &Path) -> Result<()> {
    // Source read-only: the backup only reads it, and read-only opening
    // avoids leaving a stray rollback journal beside the staged file.
    let src = Connection::open_with_flags(staged, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    // Destination opened plain (default `busy_timeout` = 0) so each locked
    // step returns BUSY immediately and our own bounded loop — not an
    // unbounded internal wait — governs the retry budget.
    let mut dest = Connection::open(dest_path)?;

    let backup = Backup::new(&src, &mut dest)?;
    let mut attempts = 0u32;
    loop {
        // `-1` copies all remaining pages in a single locked step.
        match backup.step(-1)? {
            StepResult::Done => break,
            // Cannot occur for `step(-1)`, but treat it as "keep copying"
            // rather than assume completion.
            StepResult::More => continue,
            StepResult::Busy | StepResult::Locked => {
                attempts += 1;
                if attempts >= RESTORE_BUSY_RETRIES {
                    return Err(MemhubError::InvalidInput(format!(
                        "the local DB at {} is held open by another memhub process; adopt \
                         retried {RESTORE_BUSY_RETRIES} times without writing anything and \
                         gave up. The DB is unchanged — close other memhub processes and \
                         retry `sync adopt`.",
                        dest_path.display()
                    )));
                }
                std::thread::sleep(RESTORE_BUSY_PAUSE);
            }
            // `StepResult` is `#[non_exhaustive]`; a future rusqlite variant
            // we do not understand must fail loudly rather than be treated
            // as success — the DB may be in an unknown state, so refuse.
            other => {
                return Err(MemhubError::InvalidInput(format!(
                    "online-backup restore returned an unrecognized step result ({other:?}); \
                     refusing to treat it as a completed adopt"
                )));
            }
        }
    }
    Ok(())
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

/// Replace the local DB with the Drive-synced snapshot. Destructive,
/// so it **requires `force`** (the CLI `--yes`); the pull skill
/// (`/catch-up`) only reaches here after the operator confirms a
/// `status` verdict.
///
/// Three checks are **hard refusals `force` cannot override**: a
/// project-id mismatch (wrong Drive folder), a snapshot schema newer
/// than — or unparseable by — this binary (run `memhub upgrade` first —
/// §6), and a sha256 that disagrees with the manifest (torn/partial
/// sync). The manifest-only refusals run first; the checksum is verified
/// against a **local staged copy** (see below), so a snapshot Drive
/// rewrites between hashing and install cannot be installed unverified
/// (TOCTOU, F4/X6).
///
/// Install sequence, ordered so a failure at any stage leaves the
/// original DB intact:
/// 1. stage the remote snapshot into `.memhub/project.sqlite.incoming`;
/// 2. hash the *staged* copy and verify it against the manifest;
/// 3. take a pre-adopt safety copy of the current DB to
///    `.memhub/backups/sync/last-replaced.sqlite` via `VACUUM INTO`, so
///    it captures committed WAL state (a raw file copy could miss it);
/// 4. restore the staged snapshot's pages into the live DB through
///    SQLite's online-backup API (see [`restore_into_live_db`]) — no DB
///    file is ever deleted or renamed under a process that may hold it
///    open, and concurrent memhub processes serialize through SQLite's
///    own locking.
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

    // ── Manifest-only hard refusals (independent of `force`, evaluated
    //    before any local file is touched) ───────────────────────────
    if manifest.project_id != project_id {
        return Err(MemhubError::InvalidInput(format!(
            "snapshot is for project '{}', not this repo's '{}'; refusing to adopt a \
             wrong-folder snapshot",
            manifest.project_id, project_id
        )));
    }
    // Fail closed on an unreadable manifest schema version: it must
    // hard-refuse here, before any local mutation, naming the field —
    // never collapse to ordinal 0 and slip under the newer-schema guard.
    let snapshot_ordinal = schema_ordinal(&manifest.schema_version).map_err(|_| {
        MemhubError::InvalidInput(format!(
            "snapshot manifest field 'schema_version' = '{}' is not a parseable migration \
             ordinal (expected 'NNNN_name'); refusing to adopt a snapshot whose schema \
             version cannot be read",
            manifest.schema_version
        ))
    })?;
    if snapshot_ordinal > schema_ordinal(&previous_schema)? {
        return Err(MemhubError::InvalidInput(format!(
            "snapshot schema {} is newer than this binary ({}); run `memhub upgrade` first, \
             then retry",
            manifest.schema_version, previous_schema
        )));
    }

    // ── Confirmation gate (before the multi-MB staging copy) ─────────
    if !force {
        return Err(MemhubError::InvalidInput(
            "adopt overwrites the local DB with the Drive snapshot; pass --yes to confirm".into(),
        ));
    }

    // ── Stage the incoming snapshot locally, then hash THAT copy ─────
    // Everything from here reads/writes only the staged local copy and
    // the destination; Drive can rewrite `snapshot_file` freely without
    // affecting what we install.
    let incoming = memhub_dir.join(INCOMING_FILENAME);
    if incoming.exists() {
        fs::remove_file(&incoming)?;
    }
    fs::copy(&snapshot_file, &incoming)?;

    let staged_sha = sha256_file(&incoming)?;
    if staged_sha != manifest.file_sha256 {
        // Torn/partial download, or a snapshot that changed on Drive
        // between manifest-write and now: the staged bytes we would
        // install do not match the manifest. Refuse and drop the stage —
        // no local DB state has been touched.
        let _ = fs::remove_file(&incoming);
        return Err(MemhubError::InvalidInput(
            "snapshot sha256 does not match its manifest (corrupt or partial download); \
             not adopting"
                .into(),
        ));
    }

    // ── Pre-adopt safety copy of the DB being replaced ───────────────
    // `VACUUM INTO` from the still-open connection captures committed WAL
    // state that a raw `fs::copy` of `project.sqlite` alone could miss.
    let backup_dir = memhub_dir.join("backups").join("sync");
    fs::create_dir_all(&backup_dir)?;
    let backup_path = backup_dir.join("last-replaced.sqlite");
    // `VACUUM INTO` refuses to overwrite; clear the single prior slot.
    if backup_path.exists() {
        fs::remove_file(&backup_path)?;
    }
    if let Err(e) = vacuum_into(&ctx.conn, &backup_path) {
        let _ = fs::remove_file(&incoming);
        return Err(e);
    }

    // Close our own connection so the in-place restore serializes only
    // against *other* processes, not this one.
    drop(ctx);

    // ── Restore the staged snapshot into the live DB in place ────────
    // Never deletes or renames the DB/-wal/-shm; writes through SQLite's
    // locking so a concurrent process cannot observe a torn DB. On a
    // busy/locked destination this refuses cleanly with the original DB
    // (and the WAL-inclusive backup above) intact.
    let restore_result = restore_into_live_db(&incoming, &db_path);
    // The stage is transient either way.
    let _ = fs::remove_file(&incoming);
    restore_result?;

    // Reopen: `open_project` migrates forward if the snapshot was older.
    let ctx = db::open_project(&repo_root)?;
    let new_schema: String = ctx.conn.query_row(
        "SELECT schema_version FROM projects WHERE id = 1",
        [],
        |row| row.get(0),
    )?;

    // F3 hygiene: a whole-DB snapshot carries the source machine's
    // `session_metrics`, including any that were open (`ended_at IS NULL`)
    // when it was taken. On this machine those are foreign zombies — close
    // them to their own start so they can neither capture local recalls
    // (belt to the reconciler's window cap) nor linger un-prunable (the
    // pruner only deletes ended sessions). Best-effort, never fatal.
    let _ = ctx.conn.execute(
        "UPDATE session_metrics SET ended_at = started_at WHERE ended_at IS NULL",
        [],
    );
    let synced_at: String = ctx
        .conn
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
    let synced_at: String = ctx
        .conn
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
    db::log_write(
        &ctx.conn,
        SYNC_ACTOR,
        "config",
        None,
        "update",
        "sync enable",
    )?;

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
    db::log_write(
        &ctx.conn,
        SYNC_ACTOR,
        "config",
        None,
        "update",
        "sync disable",
    )?;
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
///
/// `pub(crate)` (rather than private) so `commands::audit_md` can reuse
/// it for `[audit] user_md_path` (issue #32) instead of duplicating
/// this logic — same reuse rationale as the `pub(crate)` doctor checks.
pub(crate) fn expand_home(subpath: &str) -> Result<PathBuf> {
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
    let env_var = if cfg!(windows) {
        "COMPUTERNAME"
    } else {
        "HOSTNAME"
    };
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
    let dest = dest
        .to_str()
        .ok_or_else(|| MemhubError::InvalidInput("snapshot path is not valid UTF-8".into()))?;
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
    Ok(hasher
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{doc, fact, init};
    use tempfile::tempdir;

    fn enable_sync(repo: &Path) {
        let ctx = db::open_project(repo).expect("open");
        let mut cfg = ctx.config.clone();
        cfg.sync.enabled = true;
        cfg.sync.project_id = "test-proj-abcd1234".to_string();
        cfg.save(&ctx.paths.config_path).expect("save config");
    }

    /// Like `enable_sync`, but also sets `drive_subpath` so
    /// `resolve_remote_dir`/`default_remote_dir` succeed -- needed to
    /// exercise the canonical-remote-dir baseline-on-push path, which
    /// `enable_sync`'s bare (empty `drive_subpath`) fixture deliberately
    /// leaves unreachable for every other test.
    fn enable_sync_with_drive_subpath(repo: &Path, drive_subpath: &Path) {
        let ctx = db::open_project(repo).expect("open");
        let mut cfg = ctx.config.clone();
        cfg.sync.enabled = true;
        cfg.sync.project_id = "test-proj-abcd1234".to_string();
        cfg.sync.drive_subpath = drive_subpath.display().to_string();
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
        assert!(
            id_ssh.starts_with("memhub-"),
            "id carries repo slug: {id_ssh}"
        );
        assert!(
            id_ssh
                .rsplit('-')
                .next()
                .unwrap()
                .chars()
                .all(|c| c.is_ascii_hexdigit()),
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
            Path::new("/mnt/drive/memhub-sync")
                .join("memhub")
                .join("pinned-id"),
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
        let err = snapshot(temp.path(), &out, false).expect_err("must refuse when disabled");
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
        let summary = snapshot(temp.path(), &out, false).expect("snapshot");

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
        assert!(
            header.starts_with(b"SQLite format 3\0"),
            "valid sqlite file"
        );
    }

    #[test]
    fn snapshot_refuses_to_clobber_a_diverged_remote_without_force() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        enable_sync(temp.path());
        fact::add(temp.path(), "k", "v", "user", "cli:user").expect("fact");
        let (local, schema) = local_state(temp.path());

        // A remote whose logical version differs from local, with no shared
        // baseline marker → `check` reports Diverged.
        let out = temp.path().join("drive").join("proj");
        write_remote_manifest(
            &out,
            "test-proj-abcd1234",
            LogicalVersion {
                writes_log_max_id: local.writes_log_max_id + 5,
                writes_log_count: local.writes_log_count + 5,
                digest: "a-different-remote".into(),
            },
            &schema,
        );

        // Without --force the push must refuse and leave the remote's
        // manifest untouched.
        let err = snapshot(temp.path(), &out, false)
            .expect_err("must refuse to clobber a diverged remote");
        assert!(matches!(err, MemhubError::InvalidInput(_)));
        let before = Manifest::load(&out.join(MANIFEST_FILENAME)).expect("load");
        assert_eq!(before.machine_id, "other-machine", "remote left intact");

        // With --force it overwrites the remote with this machine's state.
        let summary = snapshot(temp.path(), &out, true).expect("forced snapshot");
        assert!(summary.snapshot_path.exists());
        let after = Manifest::load(&summary.manifest_path).expect("load");
        assert_ne!(after.machine_id, "other-machine", "remote replaced");
    }

    /// Write a manifest with a chosen logical/schema version into `dir`,
    /// so status tests can stand in for "what another machine pushed".
    fn write_remote_manifest(dir: &Path, project_id: &str, logical: LogicalVersion, schema: &str) {
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
            .query_row(
                "SELECT schema_version FROM projects WHERE id = 1",
                [],
                |r| r.get(0),
            )
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
        assert!(
            !report.baseline_present,
            "first-sync diverge has no baseline"
        );
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
        assert_eq!(schema_ordinal("0016_global_accept_markers").unwrap(), 16);
        assert_eq!(schema_ordinal("0001_initial").unwrap(), 1);
        // Fail-closed: an unparseable version is an error, never a silent
        // 0 that would slip under the newer-schema adopt guard (F4/X6).
        assert!(
            schema_ordinal("garbage").is_err(),
            "a non-numeric prefix must fail closed, not collapse to 0"
        );
        assert!(schema_ordinal("").is_err(), "empty must fail closed");
        assert!(schema_ordinal("9999_future").unwrap() > schema_ordinal("0016_x").unwrap());
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
        snapshot(a.path(), &drive, false).expect("snapshot");

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
        snapshot(a.path(), &drive, false).expect("snapshot");

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
        snapshot(a.path(), &mismatched, false).expect("snapshot");
        rewrite_manifest(&mismatched, |m| m.project_id = "other".into());
        assert!(
            adopt(b.path(), &mismatched, true).is_err(),
            "project mismatch refused"
        );

        // Newer schema than this binary.
        let newer = a.path().join("newer");
        snapshot(a.path(), &newer, false).expect("snapshot");
        rewrite_manifest(&newer, |m| m.schema_version = "9999_future".into());
        assert!(
            adopt(b.path(), &newer, true).is_err(),
            "newer schema refused"
        );

        // Checksum disagreement (tampered snapshot).
        let tampered = a.path().join("tampered");
        snapshot(a.path(), &tampered, false).expect("snapshot");
        {
            use std::io::Write;
            let mut f = fs::OpenOptions::new()
                .append(true)
                .open(tampered.join(SNAPSHOT_FILENAME))
                .expect("open snapshot");
            f.write_all(b"corruption").expect("tamper");
        }
        assert!(
            adopt(b.path(), &tampered, true).is_err(),
            "bad checksum refused"
        );

        // Through all refusals, b's DB is still its pristine empty self.
        assert!(fact_keys(b.path()).is_empty(), "no partial adopt occurred");
    }

    #[test]
    fn adopt_refuses_unparseable_manifest_schema_before_touching_local() {
        // F4/X6: a manifest whose `schema_version` cannot be parsed as a
        // migration ordinal must hard-refuse *before* any local mutation,
        // rather than collapse to ordinal 0 and slip under the
        // newer-schema guard. project_id still matches so the schema check
        // is the one that fires.
        let a = new_synced_repo();
        fact::add(a.path(), "alpha", "1", "user", "cli:user").expect("fact");
        let b = new_synced_repo();
        fact::add(b.path(), "beta", "2", "user", "cli:user").expect("fact");

        let garbled = a.path().join("garbled-schema");
        snapshot(a.path(), &garbled, false).expect("snapshot");
        rewrite_manifest(&garbled, |m| m.schema_version = "not-an-ordinal".into());

        let err = adopt(b.path(), &garbled, true).expect_err("unparseable schema must refuse");
        match err {
            MemhubError::InvalidInput(msg) => assert!(
                msg.contains("schema_version"),
                "error must name the manifest field: {msg}"
            ),
            other => panic!("expected InvalidInput naming schema_version, got {other:?}"),
        }

        // Not one byte of local state was touched: b keeps its own data
        // and no staging/backup artifact was created.
        assert_eq!(fact_keys(b.path()), vec!["beta"], "local DB untouched");
        assert!(
            !b.path().join(".memhub").join(INCOMING_FILENAME).exists(),
            "no staged copy on a pre-mutation refusal"
        );
        assert!(
            !b.path()
                .join(".memhub")
                .join("backups")
                .join("sync")
                .join("last-replaced.sqlite")
                .exists(),
            "no pre-adopt backup on a pre-mutation refusal"
        );
    }

    #[test]
    fn adopt_refuses_cleanly_when_a_second_client_holds_the_db_locked() {
        // Two-process coordination: while a distinct SQLite client holds a
        // write transaction on b's live DB, adopt must refuse cleanly and
        // leave the DB intact — never a torn, half-replaced file. A second
        // `Connection` is a separate lock-holder even in-process, so this
        // deterministically drives the same exclusion a real second OS
        // process would (no sleeps-and-hope: the blocker holds the lock
        // for the entire adopt call, so the outcome is fixed).
        let a = new_synced_repo();
        fact::add(a.path(), "alpha", "1", "user", "cli:user").expect("fact");
        let drive = a.path().join("drive");
        snapshot(a.path(), &drive, false).expect("snapshot");

        let b = new_synced_repo();
        fact::add(b.path(), "beta", "2", "user", "cli:user").expect("fact");

        let db_path = b.path().join(".memhub").join("project.sqlite");
        let blocker = Connection::open(&db_path).expect("second client");
        blocker
            .execute_batch("BEGIN IMMEDIATE;")
            .expect("hold the write lock");

        let err = adopt(b.path(), &drive, true).expect_err("locked DB must refuse");
        assert!(matches!(
            err,
            MemhubError::InvalidInput(_) | MemhubError::DatabaseBusy { .. }
        ));

        // Release the concurrent writer, then confirm b was never mutated
        // and its DB is a coherent, readable SQLite file (no torn state).
        blocker.execute_batch("ROLLBACK;").expect("release");
        drop(blocker);
        assert_eq!(fact_keys(b.path()), vec!["beta"], "adopt left b intact");
    }

    #[test]
    fn restore_into_live_db_refuses_and_preserves_db_when_destination_locked() {
        // The restore step in isolation: a locked destination must exhaust
        // the bounded retry budget and refuse, copying zero pages, so the
        // live DB keeps its original content. Deterministic — the blocker
        // holds the write lock for the whole restore window.
        let dest = tempdir().expect("tempdir");
        init::run(dest.path()).expect("init");
        fact::add(dest.path(), "keeper", "1", "user", "cli:user").expect("fact");
        let db_path = dest.path().join(".memhub").join("project.sqlite");

        // A staged snapshot with different content to (attempt to) restore.
        let other = tempdir().expect("tempdir");
        init::run(other.path()).expect("init");
        fact::add(other.path(), "incoming", "2", "user", "cli:user").expect("fact");
        let staged = dest.path().join("staged.sqlite");
        {
            let src = db::open_project(other.path()).expect("open other");
            vacuum_into(&src.conn, &staged).expect("stage snapshot");
        }

        let blocker = Connection::open(&db_path).expect("second client");
        blocker
            .execute_batch("BEGIN IMMEDIATE;")
            .expect("hold the write lock");

        let err = restore_into_live_db(&staged, &db_path)
            .expect_err("a locked destination must refuse");
        assert!(matches!(err, MemhubError::InvalidInput(_)));

        blocker.execute_batch("ROLLBACK;").expect("release");
        drop(blocker);

        // The destination still holds its ORIGINAL content, untouched.
        assert_eq!(
            fact_keys(dest.path()),
            vec!["keeper"],
            "restore copied nothing into the locked DB"
        );
    }

    #[test]
    fn adopt_failure_during_restore_keeps_original_db_and_backup() {
        // A "snapshot" that clears every pre-restore gate but is not a
        // valid SQLite DB, so the online-backup restore itself fails
        // *after* the pre-adopt backup is taken. This proves two things at
        // once: (a) the checksum is computed on the STAGED copy and that
        // same copy is what the restore reads (the manifest hash is set to
        // the staged garbage's hash, so the gate passes only because
        // staged bytes == hashed bytes); and (b) a failure past the backup
        // step leaves the original DB intact plus a usable WAL-inclusive
        // backup.
        let b = new_synced_repo();
        fact::add(b.path(), "beta", "2", "user", "cli:user").expect("fact");

        let drive = b.path().join("drive");
        fs::create_dir_all(&drive).expect("mkdir");
        let snap = drive.join(SNAPSHOT_FILENAME);
        fs::write(&snap, b"this is emphatically not a sqlite database").expect("write garbage");
        let garbage_sha = sha256_file(&snap).expect("hash garbage");

        let (local, schema) = local_state(b.path());
        let manifest = Manifest {
            manifest_version: MANIFEST_VERSION,
            project_id: "test-proj-abcd1234".into(),
            schema_version: schema,
            logical_version: local,
            file_sha256: garbage_sha,
            machine_id: "other-machine".into(),
            created_at: "2026-05-22 00:00:00".into(),
            memhub_version: "0.1.0".into(),
        };
        fs::write(
            drive.join(MANIFEST_FILENAME),
            serde_json::to_string_pretty(&manifest).unwrap(),
        )
        .expect("write manifest");

        let err = adopt(b.path(), &drive, true).expect_err("restore of a non-DB must fail");
        assert!(matches!(
            err,
            MemhubError::Sqlite(_) | MemhubError::InvalidInput(_) | MemhubError::DatabaseBusy { .. }
        ));

        // Original DB intact.
        assert_eq!(fact_keys(b.path()), vec!["beta"], "failed restore left b intact");

        // The pre-adopt backup was taken before the doomed restore and is
        // a real, VACUUM-INTO'd SQLite file (WAL-inclusive), and the
        // transient stage was cleaned up.
        let backup = b
            .path()
            .join(".memhub")
            .join("backups")
            .join("sync")
            .join("last-replaced.sqlite");
        assert!(backup.exists(), "pre-adopt backup survives a failed restore");
        let head = fs::read(&backup).expect("read backup");
        assert!(
            head.starts_with(b"SQLite format 3\0"),
            "backup is a valid sqlite file"
        );
        assert!(
            !b.path().join(".memhub").join(INCOMING_FILENAME).exists(),
            "transient stage cleaned up after a failed restore"
        );
    }

    #[test]
    fn commit_records_push_baseline_as_up_to_date() {
        let a = new_synced_repo();
        fact::add(a.path(), "alpha", "1", "user", "cli:user").expect("fact");
        let drive = a.path().join("drive");
        snapshot(a.path(), &drive, false).expect("snapshot");

        // Before commit there is no baseline; equal logical → up-to-date,
        // but commit is what records the agreement after a push.
        commit(a.path(), &drive).expect("commit");
        let ctx = db::open_project(a.path()).expect("open");
        let marker = load_marker(&ctx.paths.memhub_dir)
            .expect("load")
            .expect("marker");
        assert_eq!(marker.last_action, "push");

        // A further local write now reads as local-ahead, proving the
        // baseline took.
        fact::add(a.path(), "gamma", "3", "user", "cli:user").expect("fact");
        assert_eq!(
            check(a.path(), &drive).expect("status").verdict,
            SyncVerdict::LocalAhead
        );
    }

    #[test]
    fn second_push_after_local_write_succeeds_without_force_and_precheck_is_local_ahead() {
        // Regression test for the filed bug: a push into the repo's
        // canonical remote dir must record its own baseline (this fix),
        // rather than leaving the marker absent until an easy-to-forget
        // `sync commit` -- which made every *second* push see the
        // (still markerless) remote as Diverged from local and refuse
        // without --force.
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let drive_root = tempdir().expect("drive tempdir");
        enable_sync_with_drive_subpath(temp.path(), drive_root.path());
        fact::add(temp.path(), "alpha", "1", "user", "cli:user").expect("fact");

        let remote_dir = default_remote_dir(temp.path()).expect("resolve remote dir");
        snapshot(temp.path(), &remote_dir, false).expect("first push");

        // A durable local write after the push.
        fact::add(temp.path(), "beta", "2", "user", "cli:user").expect("fact");

        // Pre-push check must read local-ahead (the first push recorded
        // its own baseline), never diverged.
        assert_eq!(
            check(temp.path(), &remote_dir).expect("status").verdict,
            SyncVerdict::LocalAhead,
            "the baseline from the first push means the new local write reads as local-ahead"
        );

        // The second push must succeed without --force.
        snapshot(temp.path(), &remote_dir, false)
            .expect("second push without --force must succeed");
    }

    #[test]
    fn snapshot_to_foreign_dir_does_not_touch_marker() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let drive_root = tempdir().expect("drive tempdir");
        enable_sync_with_drive_subpath(temp.path(), drive_root.path());
        fact::add(temp.path(), "k", "v", "user", "cli:user").expect("fact");

        let ctx = db::open_project(temp.path()).expect("open");
        assert!(
            load_marker(&ctx.paths.memhub_dir).expect("load").is_none(),
            "no marker before any push"
        );

        // Push to a directory that is NOT the canonical remote dir (an
        // inspection copy, or a test fixture) -- must not create a
        // marker.
        let foreign = temp.path().join("inspect-copy");
        snapshot(temp.path(), &foreign, false).expect("snapshot to foreign dir");
        assert!(
            load_marker(&ctx.paths.memhub_dir).expect("load").is_none(),
            "foreign-dir snapshot must not create a marker"
        );

        // An existing marker (e.g. from a prior real push) must survive a
        // foreign-dir snapshot unchanged.
        let existing = SyncMarker {
            project_id: "test-proj-abcd1234".into(),
            baseline: LogicalVersion {
                writes_log_max_id: 1,
                writes_log_count: 1,
                digest: "existing-baseline".into(),
            },
            baseline_file_sha256: "existing-sha".into(),
            synced_at: "2026-01-01 00:00:00".into(),
            last_action: "push".into(),
        };
        save_marker(&ctx.paths.memhub_dir, &existing).expect("save marker");
        let foreign2 = temp.path().join("inspect-copy-2");
        snapshot(temp.path(), &foreign2, false).expect("snapshot to another foreign dir");
        let after = load_marker(&ctx.paths.memhub_dir)
            .expect("load")
            .expect("marker still present");
        assert_eq!(after.baseline, existing.baseline, "existing marker untouched");
        assert_eq!(
            after.baseline_file_sha256, existing.baseline_file_sha256,
            "existing marker untouched"
        );
        assert_eq!(after.synced_at, existing.synced_at, "existing marker untouched");
    }

    #[test]
    fn status_equal_logical_is_up_to_date_despite_stale_baseline() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        enable_sync(temp.path());
        let (local, schema) = local_state(temp.path());

        // Baseline is stale (matches neither side), but local and remote
        // logical versions are equal -- e.g. a push that skipped
        // `commit()` before this fix, followed by a fresh check with
        // nothing else having changed on either side. Must self-heal to
        // up-to-date rather than derive a verdict from a baseline that
        // neither side actually agreed on.
        let ctx = db::open_project(temp.path()).expect("open");
        let stale_baseline = LogicalVersion {
            writes_log_max_id: local.writes_log_max_id + 99,
            writes_log_count: local.writes_log_count + 99,
            digest: "stale-baseline-neither-side-agrees-with".into(),
        };
        save_marker(
            &ctx.paths.memhub_dir,
            &SyncMarker {
                project_id: "test-proj-abcd1234".into(),
                baseline: stale_baseline,
                baseline_file_sha256: "stale-sha".into(),
                synced_at: "2026-01-01 00:00:00".into(),
                last_action: "push".into(),
            },
        )
        .expect("save marker");

        let remote = temp.path().join("remote");
        write_remote_manifest(&remote, "test-proj-abcd1234", local.clone(), &schema);

        assert_eq!(
            check(temp.path(), &remote).expect("status").verdict,
            SyncVerdict::UpToDate,
            "equal local/remote logical versions must read up-to-date even with a stale baseline"
        );
    }

    fn rewrite_manifest(dir: &Path, f: impl FnOnce(&mut Manifest)) {
        let path = dir.join(MANIFEST_FILENAME);
        let mut manifest = Manifest::load(&path).expect("load manifest");
        f(&mut manifest);
        fs::write(&path, serde_json::to_string_pretty(&manifest).unwrap()).expect("rewrite");
    }

    // ── LogicalVersion digest completeness (audit finding F2) ─────────────
    //
    // CONTENT_TABLES is hand-maintained; these exemption lists are its
    // drift guard. Every live table/column is either digested (in
    // CONTENT_TABLES) or accounted for below with a reason, and
    // `content_tables_cover_the_live_schema` fails if the schema grows a
    // table/column that is neither — the exact hole F2 closes, where an
    // omitted durable table (documents/doc_chunks) let two divergent DBs
    // compare EQUAL.

    /// Whole tables excluded from the content digest, each with the reason
    /// it carries no cross-machine divergence signal.
    const DIGEST_EXEMPT_TABLES: &[(&str, &str)] = &[
        (
            "projects",
            "singleton config/bookkeeping row (schema_version, root_path, metrics-maintenance \
             debounce marker): machine-local identity + migration state, not durable content",
        ),
        (
            "writes_log",
            "append-only mutation log; surfaced separately as writes_log_max_id/count and \
             deliberately excluded from the digest (two DBs that each logged one write log \
             near-identical rows, giving a false 'equal' — see the LogicalVersion module doc)",
        ),
        (
            "schema_migrations",
            "migration bookkeeping ledger; schema state tracked via schema_version, not content",
        ),
        (
            "commits",
            "git-ingestion cache; re-derivable from git history, excluded from `memhub export`",
        ),
        (
            "files",
            "git-ingestion cache; re-derivable from git history, excluded from `memhub export`",
        ),
        (
            "commit_files",
            "git-ingestion cache; re-derivable from git history, excluded from `memhub export`",
        ),
        (
            "chunks",
            "legacy chunk cache feeding chunk_fts; re-derivable retrieval index, excluded from \
             `memhub export`",
        ),
        (
            "embeddings",
            "re-derivable vector cache; rebuilt by `memhub index`/reindex, excluded from \
             `memhub export`",
        ),
        (
            "recall_metrics",
            "opt-in machine-local token-accounting metrics; excluded from `memhub export`",
        ),
        (
            "session_metrics",
            "opt-in machine-local token-accounting metrics; excluded from `memhub export`",
        ),
        (
            "session_turn_metrics",
            "opt-in machine-local token-accounting metrics; excluded from `memhub export`",
        ),
        (
            "known_projects",
            "machine-wide upgrade registry keyed by absolute repo path; machine-local, excluded \
             from `memhub export`",
        ),
        (
            "global_accept_markers",
            "machine-local cross-DB accept crash-recovery markers; excluded from `memhub export`",
        ),
        (
            "session_transcripts",
            "machine-local archive-pointer rows under gitignored .memhub/; excluded from \
             `memhub export`",
        ),
    ];

    /// FTS5 external-content virtual tables. Each is a derived keyword
    /// index over a digested source table, and SQLite manages its shadow
    /// tables (`<base>_data|_idx|_docsize|_config`). Listing the bases —
    /// not every shadow table — keeps the exemption drift-proof: a NEW fts
    /// family whose base is unlisted leaves its virtual + shadow tables
    /// unaccounted-for and fails the coverage test.
    const DIGEST_EXEMPT_FTS_BASES: &[&str] = &[
        "chunk_fts",
        "facts_fts",
        "decisions_fts",
        "tasks_fts",
        "doc_chunks_fts",
        "session_notes_fts",
    ];

    /// Columns of an otherwise-digested table that are intentionally NOT
    /// digested, each with a reason. (`project_id` is exempt on every
    /// table and handled in `column_exempt`, not listed per-table.)
    const DIGEST_EXEMPT_COLUMNS: &[(&str, &str, &str)] = &[
        (
            "documents",
            "id",
            "surrogate rowid; a document's content identity is (path,title,content_hash,\
             byte_len,source), so omitting id lets two machines that ingest the same doc \
             converge to an equal digest",
        ),
        (
            "documents",
            "ingested_at",
            "local ingest timestamp; machine-specific, not content",
        ),
        (
            "doc_chunks",
            "id",
            "surrogate rowid; a chunk's content identity is (doc_id,ord,heading_path,body)",
        ),
        (
            "doc_chunks",
            "created_at",
            "local ingest timestamp; machine-specific, not content",
        ),
    ];

    fn live_columns(conn: &Connection, table: &str) -> Vec<String> {
        let mut stmt = conn
            .prepare("SELECT name FROM pragma_table_info(?1)")
            .expect("prepare pragma_table_info");
        stmt.query_map([table], |r| r.get::<_, String>(0))
            .expect("pragma query")
            .map(|r| r.expect("column name"))
            .collect()
    }

    fn table_exempt(table: &str) -> bool {
        if DIGEST_EXEMPT_TABLES.iter().any(|(t, _)| *t == table) {
            return true;
        }
        DIGEST_EXEMPT_FTS_BASES
            .iter()
            .any(|base| table == *base || table.starts_with(&format!("{base}_")))
    }

    fn column_exempt(table: &str, col: &str) -> bool {
        // The constant singleton partition key is never digested — the
        // digest already filters `WHERE project_id = 1`.
        if col == "project_id" {
            return true;
        }
        DIGEST_EXEMPT_COLUMNS
            .iter()
            .any(|(t, c, _)| *t == table && *c == col)
    }

    #[test]
    fn content_tables_cover_the_live_schema() {
        // Every live table and column in a fully-migrated DB must be either
        // digested (CONTENT_TABLES) or explicitly exempt with a reason.
        // Drift — a durable table/column added without updating one of the
        // lists — turns this red, so the sync check can never silently miss
        // a new table's divergence (audit finding F2).
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let ctx = db::open_project(temp.path()).expect("open");
        let conn = &ctx.conn;

        let digested: std::collections::HashMap<&str, Vec<&str>> = CONTENT_TABLES
            .iter()
            .map(|(t, cols)| (*t, cols.to_vec()))
            .collect();

        let mut tables_stmt = conn
            .prepare("SELECT name FROM sqlite_master WHERE type = 'table' ORDER BY name")
            .expect("prepare tables");
        let tables: Vec<String> = tables_stmt
            .query_map([], |r| r.get::<_, String>(0))
            .expect("query tables")
            .map(|r| r.expect("table name"))
            .collect();

        let mut problems: Vec<String> = Vec::new();
        for table in &tables {
            let cols = live_columns(conn, table);
            if let Some(digested_cols) = digested.get(table.as_str()) {
                // Every live column must be digested or column-exempt.
                for col in &cols {
                    if !digested_cols.contains(&col.as_str()) && !column_exempt(table, col) {
                        problems.push(format!(
                            "column `{table}.{col}` is neither digested nor exempt — add it to \
                             CONTENT_TABLES or DIGEST_EXEMPT_COLUMNS with a reason"
                        ));
                    }
                }
                // Every digested column must still exist (catch a rename/drop
                // that would make the digest SELECT error at runtime).
                for dc in digested_cols {
                    if !cols.iter().any(|c| c == dc) {
                        problems.push(format!(
                            "digested column `{table}.{dc}` no longer exists in the live schema"
                        ));
                    }
                }
            } else if !table_exempt(table) {
                problems.push(format!(
                    "table `{table}` is neither digested (CONTENT_TABLES) nor exempt — add it to \
                     CONTENT_TABLES or DIGEST_EXEMPT_TABLES/DIGEST_EXEMPT_FTS_BASES with a reason"
                ));
            }
        }

        assert!(
            problems.is_empty(),
            "LogicalVersion digest drift:\n{}",
            problems.join("\n")
        );
    }

    #[test]
    fn digest_distinguishes_null_from_empty_string() {
        // NULL and '' must not collide (the old COALESCE-to-'' scheme did).
        // Same row, same timestamps: the only change is NULL → '' on a
        // nullable digested column, which must still move the digest.
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        fact::add(temp.path(), "k", "v", "user", "cli:user").expect("fact");
        let ctx = db::open_project(temp.path()).expect("open");

        ctx.conn
            .execute("UPDATE facts SET kind = NULL WHERE key = 'k'", [])
            .expect("set null");
        let d_null = LogicalVersion::read(&ctx.conn).expect("read").digest;

        ctx.conn
            .execute("UPDATE facts SET kind = '' WHERE key = 'k'", [])
            .expect("set empty");
        let d_empty = LogicalVersion::read(&ctx.conn).expect("read").digest;

        assert_ne!(
            d_null, d_empty,
            "a NULL column must produce a different digest than an empty string"
        );
    }

    #[test]
    fn digest_reflects_facts_kind_and_superseded_by() {
        // The two columns F2 named as omitted (migrations 0021/0018): a
        // change to either must move the digest.
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        fact::add(temp.path(), "a", "1", "user", "cli:user").expect("fact a");
        fact::add(temp.path(), "b", "2", "user", "cli:user").expect("fact b");
        let ctx = db::open_project(temp.path()).expect("open");

        let base = LogicalVersion::read(&ctx.conn).expect("read").digest;

        ctx.conn
            .execute("UPDATE facts SET kind = 'gotcha' WHERE key = 'a'", [])
            .expect("set kind");
        let with_kind = LogicalVersion::read(&ctx.conn).expect("read").digest;
        assert_ne!(base, with_kind, "facts.kind must be part of the digest");

        let b_id: i64 = ctx
            .conn
            .query_row("SELECT id FROM facts WHERE key = 'b'", [], |r| r.get(0))
            .expect("b id");
        ctx.conn
            .execute("UPDATE facts SET superseded_by = ?1 WHERE key = 'a'", [b_id])
            .expect("set superseded_by");
        let with_supersede = LogicalVersion::read(&ctx.conn).expect("read").digest;
        assert_ne!(
            with_kind, with_supersede,
            "facts.superseded_by must be part of the digest"
        );
    }

    fn write_doc(dir: &Path, name: &str, body: &str) -> PathBuf {
        let path = dir.join(name);
        fs::write(&path, body).expect("write doc file");
        path
    }

    #[test]
    fn digest_diverges_when_sides_ingest_different_documents() {
        // F2's headline case: two fresh repos start with identical (empty)
        // digests; once each ingests a DIFFERENT document their digests
        // must diverge (the pre-F2 digest omitted documents/doc_chunks
        // entirely, so this comparison read EQUAL over real divergence).
        let a = new_synced_repo();
        let b = new_synced_repo();
        assert_eq!(
            local_state(a.path()).0.digest,
            local_state(b.path()).0.digest,
            "two fresh repos have identical (empty) digests"
        );

        let doc_a = write_doc(a.path(), "spec.md", "# A\n\nalpha body paragraph to chunk.\n");
        doc::add(a.path(), &doc_a, None, "cli:user").expect("ingest a");
        let doc_b = write_doc(b.path(), "notes.md", "# B\n\nbeta body, different entirely.\n");
        doc::add(b.path(), &doc_b, None, "cli:user").expect("ingest b");
        assert_ne!(
            local_state(a.path()).0.digest,
            local_state(b.path()).0.digest,
            "different ingested documents must diverge the digest (F2)"
        );
    }

    #[test]
    fn digest_is_stable_across_page_layout_with_documents() {
        // "Identical logical state still compares equal despite page-layout
        // differences": a document ingested, then carried through a
        // `VACUUM INTO` snapshot + adopt (which recopies/reorders every
        // page), must yield the SAME digest on the receiving side — the
        // digest hashes content, never file bytes. Exercised WITH documents
        // in play so the widened digest is covered end to end.
        let a = new_synced_repo();
        let doc_a = write_doc(a.path(), "spec.md", "# Title\n\nbody paragraph to chunk here.\n");
        doc::add(a.path(), &doc_a, None, "cli:user").expect("ingest");
        let a_digest = local_state(a.path()).0.digest;

        let drive = a.path().join("drive");
        snapshot(a.path(), &drive, false).expect("snapshot");

        let b = new_synced_repo();
        adopt(b.path(), &drive, true).expect("adopt");

        assert_eq!(
            a_digest,
            local_state(b.path()).0.digest,
            "identical logical state (documents included) compares equal despite page reordering"
        );
        assert_eq!(
            check(b.path(), &drive).expect("check").verdict,
            SyncVerdict::UpToDate,
            "the adopting side reads up-to-date on the shared post-adopt baseline"
        );
    }

    #[test]
    fn digest_changes_on_reingest_of_different_content_at_same_path() {
        // Re-ingesting different bytes at the SAME doc path must move the
        // digest (documents.content_hash carries it), so a later check
        // reads Diverged rather than up-to-date.
        let temp = new_synced_repo();
        let path = write_doc(temp.path(), "spec.md", "# V1\n\noriginal body text here.\n");
        doc::add(temp.path(), &path, None, "cli:user").expect("ingest v1");
        let before = local_state(temp.path()).0.digest;

        // Overwrite the same path with new content and re-ingest.
        write_doc(temp.path(), "spec.md", "# V2\n\ncompletely rewritten body text.\n");
        doc::add(temp.path(), &path, None, "cli:user").expect("re-ingest v2");
        let after = local_state(temp.path()).0.digest;

        assert_ne!(
            before, after,
            "re-ingesting different content at the same path must change the digest"
        );
    }

    #[test]
    fn check_reports_diverged_when_only_one_side_ingested_a_doc() {
        // End-to-end at the check() seam: a document only one side ingested
        // must read as divergence, not a false up-to-date (the pre-F2 bug).
        let a = new_synced_repo();
        let doc_a = write_doc(a.path(), "spec.md", "# A\n\nbody paragraph to chunk.\n");
        doc::add(a.path(), &doc_a, None, "cli:user").expect("ingest");
        let drive = a.path().join("drive");
        snapshot(a.path(), &drive, false).expect("snapshot");

        // b is an otherwise-identical repo that ingested NO doc. No shared
        // baseline → first-sync compare: unequal logical (a's doc) →
        // Diverged.
        let b = new_synced_repo();
        assert_eq!(
            check(b.path(), &drive).expect("check").verdict,
            SyncVerdict::Diverged,
            "a doc only one side ingested must read as divergence, not up-to-date (F2)"
        );
    }
}
