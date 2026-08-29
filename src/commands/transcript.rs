//! `memhub transcript archive` (Wave 6 W3, issue #96 / decisions Q7+Q8):
//! the session-transcript archiver behind the `transcript` wrap-up level.
//!
//! When a machine opts into `[wrap_up] verbosity = "transcript"`, wrap-up
//! archives the current session. Before issue #214, this copied
//! Claude/Codex JSONL into
//! `.memhub/transcripts/<date>-<session-id>.jsonl.zst`. Issue #214 keeps
//! that behavior and adds complete OpenCode 2 session exports as
//! `.json.zst`; both forms record one pointer row in `session_transcripts`
//! (migration 0023).
//!
//! ## Security posture (Q8)
//!
//! The archive is stored **UNREDACTED**. v1 secret handling is warn +
//! explicit per-wrap-up approval, NOT content redaction (a deliberate
//! follow-up). Two things make that safe enough for v1: the source session
//! already exists unredacted in the agent's own local storage,
//! and the copy lands under gitignored, export-excluded `.memhub/`. So
//! every archive surface **fails closed**: this module refuses outright
//! unless the caller passes `approved = true`, and the CLI/MCP gates only
//! set that after an explicit `--yes` / `confirm=true` (the CLI also
//! refuses on a non-TTY without `--yes`). A loud "may contain secrets"
//! warning is emitted at every surface.
//!
//! ## Isolation invariants (the tier:opus core)
//!
//! A transcript is a compressed file on disk plus a pointer row. It is
//! deliberately NOT a retrieval object:
//!   - **never embedded** — there is no `retrieval::SourceType::Transcript`
//!     and this path never calls the eager-embed writers, so the
//!     `embeddings` table never gains a transcript row;
//!   - **never in recall** — recall only ever queries
//!     fact/decision/task/doc_chunk sources;
//!   - **excluded from `memhub export` / import** — the `Export` shape is a
//!     fixed field list with no `session_transcripts`, so an archive can
//!     never leave the machine via a memhub export.
//!
//! These restrictions apply to every [`Agent`] variant, not only the
//! filesystem-backed Claude/Codex variants.
//!
//! These are enforced by construction and asserted in
//! `tests/lifecycle/transcript_archive.rs`.
//!
//! ## Path + id reuse
//!
//! Claude/Codex directory resolution and session-id ↔ file mapping come
//! from the metrics-independent `transcript_files` module. OpenCode's API
//! export path is likewise outside `metrics`, so all three keep archiving
//! while token accounting is hibernated.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use rusqlite::{Connection, OptionalExtension, params};

use crate::config::ProjectConfig;
use crate::db::{self, log_write};
use crate::transcript_files;
use crate::{MemhubError, Result};

/// Directory under `.memhub/` that holds compressed transcript archives.
const ARCHIVE_DIRNAME: &str = "transcripts";
/// `writes_log` actor for archive + prune events.
const ACTOR: &str = "cli:transcript-archive";
/// zstd compression level. 3 is the library default — a good size/speed
/// point for session JSON/JSONL that is written once and rarely read.
const ZSTD_LEVEL: i32 = 3;
/// Non-filesystem source recorded for OpenCode's local session-export API.
const OPENCODE_EXPORT_SOURCE: &str = "opencode2 api v2.session.export";

#[cfg(windows)]
const OPENCODE_PROGRAMS: &[&str] = &["opencode2", "opencode2.cmd"];
#[cfg(not(windows))]
const OPENCODE_PROGRAMS: &[&str] = &["opencode2"];

/// Which agent's transcript source + session-id convention to use. Claude
/// and Codex mirror the metrics scraper; OpenCode is archive-only and uses
/// its local complete-session export API (it is not a metrics source).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
    OpenCode,
}

impl Agent {
    /// Agent tag stored in `session_transcripts.agent`. Claude/Codex match
    /// their optional metrics tags; OpenCode has no metrics ingestion.
    pub fn as_str(self) -> &'static str {
        match self {
            Agent::Claude => "claude-code",
            Agent::Codex => "codex",
            Agent::OpenCode => "opencode",
        }
    }

    /// Short human label for error/warning text.
    fn label(self) -> &'static str {
        match self {
            Agent::Claude => "Claude",
            Agent::Codex => "Codex",
            Agent::OpenCode => "OpenCode",
        }
    }

    fn archive_extension(self) -> &'static str {
        match self {
            Agent::Claude | Agent::Codex => "jsonl.zst",
            Agent::OpenCode => "json.zst",
        }
    }
}

/// Outcome of one archive run.
#[derive(Debug, Clone)]
pub struct ArchiveReport {
    pub session_id: String,
    pub agent: &'static str,
    pub source_path: PathBuf,
    pub archive_path: PathBuf,
    pub source_bytes: u64,
    pub archive_bytes: u64,
    /// True when a prior archive for this session was replaced.
    pub replaced_existing: bool,
    /// Number of archives pruned past the retention horizon this run.
    pub pruned: usize,
}

/// The one-line, unmissable secret warning every archive surface prints.
pub const UNREDACTED_WARNING: &str = "WARNING: memhub transcript archive stores the RAW, UNREDACTED session \
     transcript. It may contain secrets, tokens, or private data pasted or \
     printed during the session. The archive lands in gitignored, \
     export-excluded .memhub/transcripts/ and is never embedded, recalled, \
     or exported — but it is NOT redacted.";

/// Archive the transcript for `session_id`. High-level entry used by the
/// CLI and MCP surfaces.
///
/// `approved` MUST be `true`: the interactive/`--yes`/`confirm=true` gate
/// lives in the caller, and this is the fail-closed defense-in-depth check
/// so no future caller can archive an unredacted transcript without an
/// explicit approval flowing all the way down.
pub fn archive(
    start: &Path,
    agent: Agent,
    session_id: &str,
    approved: bool,
) -> Result<ArchiveReport> {
    archive_with_opencode_exporter(start, agent, session_id, approved, export_opencode_session)
}

fn archive_with_opencode_exporter<F>(
    start: &Path,
    agent: Agent,
    session_id: &str,
    approved: bool,
    export_opencode: F,
) -> Result<ArchiveReport>
where
    F: FnOnce(&str) -> Result<Vec<u8>>,
{
    if !approved {
        return Err(MemhubError::InvalidInput(
            "transcript archive requires explicit approval (--yes / confirm=true); \
             refusing to copy an unredacted transcript"
                .to_string(),
        ));
    }

    // Fail-closed path-traversal guard. The Claude resolver path-joins the
    // session id (`<dir>/<session_id>.jsonl`), so an id carrying a
    // separator or a `..` component could read a file OUTSIDE the
    // transcripts dir. Reject that external input before any resolution or
    // read. Applied uniformly to all archive agents — real Claude, Codex,
    // and OpenCode ids never trip it.
    validate_session_id(session_id)?;

    if agent == Agent::OpenCode {
        validate_opencode_session_id(session_id)?;
    }

    let session_id = normalize_session_id(agent, session_id);

    if agent == Agent::OpenCode {
        // Export and validate before opening the project or creating the
        // archive directory. A missing CLI, failed request, malformed
        // response, or mismatched session id therefore cannot leave an
        // archive or pointer row behind.
        let exported = export_opencode(&session_id)?;
        let ctx = db::open_project(start)?;
        return archive_bytes_into(
            &ctx.conn,
            &ctx.paths.memhub_dir,
            agent,
            &session_id,
            Path::new(OPENCODE_EXPORT_SOURCE),
            &exported,
            ctx.config.wrap_up.transcript_retention_days,
        );
    }

    let ctx = db::open_project(start)?;
    let retention_days = ctx.config.wrap_up.transcript_retention_days;
    let dir = resolve_transcript_dir(&ctx.config, &ctx.paths.repo_root, agent)?;
    let source = resolve_source(agent, &dir, &session_id)?;

    // Belt-and-suspenders for every filesystem-backed agent (Claude and
    // Codex): even with a clean id, a symlinked transcript file could point
    // outside the dir. OpenCode has no source path to contain; its complete
    // export is validated in memory before this branch.
    assert_source_contained(&dir, &source)?;

    archive_into(
        &ctx.conn,
        &ctx.paths.memhub_dir,
        agent,
        &session_id,
        &source,
        retention_days,
    )
}

/// Codex sessions are keyed `codex:<uuid>` everywhere (scraper +
/// `session_transcripts`). Accept a bare uuid from a caller and normalize
/// it so the pointer row and metrics row share the same key. Claude ids
/// and OpenCode ids pass through untouched.
fn normalize_session_id(agent: Agent, session_id: &str) -> String {
    match agent {
        Agent::Codex if !session_id.starts_with("codex:") => format!("codex:{session_id}"),
        _ => session_id.to_string(),
    }
}

struct ProcessOutput {
    success: bool,
    status: String,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// Request OpenCode's documented complete projected session export with
/// sanitization disabled. Arguments are passed directly to the process — no
/// shell interpolation — and Windows also tries the npm-installed `.cmd`
/// shim after a bare-program not-found result.
fn export_opencode_session(session_id: &str) -> Result<Vec<u8>> {
    request_opencode_export_with(session_id, |program, args| {
        let output = Command::new(program).args(args).output()?;
        Ok(ProcessOutput {
            success: output.status.success(),
            status: output.status.to_string(),
            stdout: output.stdout,
            stderr: output.stderr,
        })
    })
}

fn request_opencode_export_with<F>(session_id: &str, mut run: F) -> Result<Vec<u8>>
where
    F: FnMut(&str, &[String]) -> std::io::Result<ProcessOutput>,
{
    let args = vec![
        "api".to_string(),
        "v2.session.export".to_string(),
        "--param".to_string(),
        format!("sessionID={session_id}"),
        "--param".to_string(),
        "sanitize=false".to_string(),
    ];

    for program in OPENCODE_PROGRAMS {
        let output = match run(program, &args) {
            Ok(output) => output,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => {
                return Err(MemhubError::ExternalCommand {
                    command: format!("{program} api v2.session.export"),
                    stderr: error.to_string(),
                });
            }
        };

        if !output.success {
            let stderr = String::from_utf8_lossy(&output.stderr).trim().to_string();
            return Err(MemhubError::ExternalCommand {
                command: format!("{program} api v2.session.export"),
                stderr: if stderr.is_empty() {
                    format!("process exited with {}", output.status)
                } else {
                    stderr
                },
            });
        }

        validate_opencode_export(&output.stdout, session_id)?;
        return Ok(output.stdout);
    }

    Err(MemhubError::ExternalCommand {
        command: "opencode2 api v2.session.export".to_string(),
        stderr: "OpenCode 2 CLI not found on PATH (tried opencode2 and, on Windows, opencode2.cmd)"
            .to_string(),
    })
}

fn validate_opencode_export(raw: &[u8], expected_session_id: &str) -> Result<()> {
    let response: serde_json::Value = serde_json::from_slice(raw).map_err(|error| {
        MemhubError::InvalidInput(format!(
            "OpenCode session export returned malformed JSON: {error}"
        ))
    })?;
    let data = response
        .get("data")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            MemhubError::InvalidInput(
                "OpenCode session export response is missing object data".to_string(),
            )
        })?;
    let info = data
        .get("info")
        .and_then(serde_json::Value::as_object)
        .ok_or_else(|| {
            MemhubError::InvalidInput(
                "OpenCode session export response is missing data.info".to_string(),
            )
        })?;
    let exported_session_id = info
        .get("id")
        .and_then(serde_json::Value::as_str)
        .ok_or_else(|| {
            MemhubError::InvalidInput(
                "OpenCode session export response is missing data.info.id".to_string(),
            )
        })?;
    if exported_session_id != expected_session_id {
        return Err(MemhubError::InvalidInput(format!(
            "OpenCode session export id mismatch: requested '{expected_session_id}', got '{exported_session_id}'"
        )));
    }
    if !data
        .get("messages")
        .is_some_and(serde_json::Value::is_array)
    {
        return Err(MemhubError::InvalidInput(
            "OpenCode session export response is missing data.messages array".to_string(),
        ));
    }
    Ok(())
}

/// Reject a session id that could escape the transcripts directory when
/// path-joined. Fail-closed: a separator (`/` or `\`) or any `..` is
/// refused before the id is ever resolved or read. No legitimate Claude,
/// Codex, or OpenCode id contains any of these.
fn validate_session_id(session_id: &str) -> Result<()> {
    if session_id.trim().is_empty() {
        return Err(MemhubError::InvalidInput(
            "session id must not be empty".to_string(),
        ));
    }
    if session_id.contains('/') || session_id.contains('\\') || session_id.contains("..") {
        return Err(MemhubError::InvalidInput(format!(
            "session id '{session_id}' is invalid: it must not contain a path separator \
             ('/' or '\\') or '..' — refusing to resolve a transcript outside the \
             transcripts directory"
        )));
    }
    Ok(())
}

/// OpenCode's Windows npm shim is a `.cmd` file, so its arguments cross a
/// command-interpreter boundary. Accept only the documented `ses` id prefix
/// plus the ASCII characters observed in V2 ids before invoking the shim.
fn validate_opencode_session_id(session_id: &str) -> Result<()> {
    let valid = session_id.strip_prefix("ses").is_some_and(|remainder| {
        !remainder.is_empty()
            && remainder
                .bytes()
                .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'_' | b'-'))
    });
    if !valid {
        return Err(MemhubError::InvalidInput(
            "OpenCode session id must start with 'ses' and contain only ASCII letters, digits, '_' or '-' after the prefix"
                .to_string(),
        ));
    }
    Ok(())
}

/// Belt-and-suspenders containment check: the resolved source file, once
/// canonicalized (following any symlink), must live under the transcripts
/// directory. Fails closed on a path that escapes, or if either path
/// cannot be canonicalized.
fn assert_source_contained(dir: &Path, source: &Path) -> Result<()> {
    let dir_canon = dir.canonicalize().map_err(|e| {
        MemhubError::InvalidInput(format!(
            "cannot canonicalize the transcripts directory {}: {e}",
            dir.display()
        ))
    })?;
    let source_canon = source.canonicalize().map_err(|e| {
        MemhubError::InvalidInput(format!(
            "cannot canonicalize the resolved transcript {}: {e}",
            source.display()
        ))
    })?;
    if !source_canon.starts_with(&dir_canon) {
        return Err(MemhubError::InvalidInput(format!(
            "resolved transcript {} is outside the transcripts directory {} — refusing to archive",
            source_canon.display(),
            dir_canon.display()
        )));
    }
    Ok(())
}

/// Resolve the transcript directory for `agent`: the configured `[metrics]
/// *_transcripts_dir` when set, else the same auto-detection
/// `memhub metrics enable` performs. Errors (rather than silently doing
/// nothing) when neither yields a directory.
fn resolve_transcript_dir(
    config: &ProjectConfig,
    repo_root: &Path,
    agent: Agent,
) -> Result<PathBuf> {
    let (configured, detected, config_key) = match agent {
        Agent::Claude => (
            config.metrics.claude_transcripts_dir.clone(),
            transcript_files::detect_claude_transcripts_dir(repo_root),
            "claude",
        ),
        Agent::Codex => (
            config.metrics.codex_transcripts_dir.clone(),
            transcript_files::detect_codex_sessions_dir(),
            "codex",
        ),
        Agent::OpenCode => {
            return Err(MemhubError::InvalidInput(
                "OpenCode transcripts come from the local session-export API, not a transcript directory"
                    .to_string(),
            ));
        }
    };

    if !configured.is_empty() {
        return Ok(PathBuf::from(configured));
    }
    detected.ok_or_else(|| {
        MemhubError::InvalidInput(format!(
            "cannot resolve the {} transcript directory: it is not set in \
             [metrics] {}_transcripts_dir and auto-detection found nothing. \
             Set it directly or create the agent's \
             transcript directory first.",
            agent.label(),
            config_key,
        ))
    })
}

/// Locate the source JSONL under `dir` for `session_id`, reusing the
/// scraper's file-naming knowledge (do not re-derive paths here).
fn resolve_source(agent: Agent, dir: &Path, session_id: &str) -> Result<PathBuf> {
    let found = match agent {
        Agent::Claude => transcript_files::find_claude_transcript(dir, session_id),
        Agent::Codex => transcript_files::find_codex_transcript(dir, session_id),
        Agent::OpenCode => None,
    };
    found.ok_or_else(|| {
        MemhubError::InvalidInput(format!(
            "no {} transcript found for session '{}' under {}",
            agent.label(),
            session_id,
            dir.display(),
        ))
    })
}

/// Filesystem-backed archive step, split out so integration tests can drive
/// it against a real project + a seeded source file. Claude/Codex use
/// `.jsonl.zst`; OpenCode reaches the shared byte archive below and uses
/// `.json.zst`. Both upsert a pointer row and prune past the horizon.
pub(crate) fn archive_into(
    conn: &Connection,
    memhub_dir: &Path,
    agent: Agent,
    session_id: &str,
    source: &Path,
    retention_days: u32,
) -> Result<ArchiveReport> {
    if agent == Agent::OpenCode {
        return Err(MemhubError::InvalidInput(
            "OpenCode archives must come from the validated local session-export API".to_string(),
        ));
    }

    // Preserve the established Claude/Codex path exactly: size from source
    // metadata, then stream the JSONL file into zstd.
    let source_bytes = fs::metadata(source)?.len();
    let compressed = zstd::encode_all(fs::File::open(source)?, ZSTD_LEVEL)?;
    archive_compressed_into(
        conn,
        memhub_dir,
        agent,
        session_id,
        source,
        source_bytes,
        &compressed,
        retention_days,
    )
}

fn archive_bytes_into(
    conn: &Connection,
    memhub_dir: &Path,
    agent: Agent,
    session_id: &str,
    source: &Path,
    body: &[u8],
    retention_days: u32,
) -> Result<ArchiveReport> {
    let source_bytes = body.len() as u64;
    let compressed = zstd::encode_all(body, ZSTD_LEVEL)?;
    archive_compressed_into(
        conn,
        memhub_dir,
        agent,
        session_id,
        source,
        source_bytes,
        &compressed,
        retention_days,
    )
}

#[allow(clippy::too_many_arguments)]
fn archive_compressed_into(
    conn: &Connection,
    memhub_dir: &Path,
    agent: Agent,
    session_id: &str,
    source: &Path,
    source_bytes: u64,
    compressed: &[u8],
    retention_days: u32,
) -> Result<ArchiveReport> {
    let archive_bytes = compressed.len() as u64;

    let archive_dir = memhub_dir.join(ARCHIVE_DIRNAME);
    fs::create_dir_all(&archive_dir)?;
    // The canonical archive root is the ONLY tree the previous-archive
    // cleanup below is ever allowed to delete inside. Resolve it now (the
    // dir was just created), so a poisoned pointer row can never steer a
    // delete outside it.
    let archive_root = archive_dir.canonicalize()?;

    // `date('now')` is UTC and pairs with the DB's other timestamps.
    let date: String = conn.query_row("SELECT date('now')", [], |r| r.get(0))?;
    let file_name = format!(
        "{date}-{}.{}",
        sanitize_filename(session_id),
        agent.archive_extension()
    );
    let archive_path = archive_dir.join(&file_name);

    // Decision 161 — REJECT LOUDLY on a sanitize_filename collision. Two
    // distinct session ids can sanitize to the same on-disk filename (the
    // `codex:` colon and every other non-`[A-Za-z0-9._-]` char all fold to
    // `_`). If the archive path we are about to write already belongs to a
    // DIFFERENT session, refuse rather than silently clobber that session's
    // archive and cross-link the rows — which would let one session's prune
    // delete the other's archive. Same-session re-archive is excluded and
    // still overwrites in place.
    let colliding: Option<String> = conn
        .query_row(
            "SELECT session_id FROM session_transcripts \
             WHERE archive_path = ?1 AND session_id != ?2",
            params![archive_path.to_string_lossy(), session_id],
            |r| r.get(0),
        )
        .optional()?;
    if let Some(other) = colliding {
        return Err(MemhubError::InvalidInput(format!(
            "transcript archive filename {file_name:?} for session {session_id:?} \
             collides with a different session {other:?} already archived under the \
             same name; refusing to overwrite it. Distinct session ids must not \
             sanitize to the same archive filename."
        )));
    }

    // Remember any prior archive so a re-archive on a different day cleans
    // up the stale file after the new one is safely written.
    let previous: Option<String> = conn
        .query_row(
            "SELECT archive_path FROM session_transcripts WHERE session_id = ?1",
            params![session_id],
            |r| r.get(0),
        )
        .optional()?;

    // Temp-then-publish (F7 (a)): write to a sibling temp file and rename it
    // atomically into place, so a crash mid-write never leaves a
    // half-written archive at the real path. Track whether the publish
    // overwrites an existing file for THIS session (a same-day re-archive):
    // if so, a later upsert failure must NOT delete a file a surviving row
    // still points at.
    let published_over_existing = archive_path.exists();
    let tmp_path = archive_dir.join(format!(".{file_name}.tmp-{}", std::process::id()));
    fs::write(&tmp_path, compressed)?;
    if let Err(e) = fs::rename(&tmp_path, &archive_path) {
        let _ = fs::remove_file(&tmp_path);
        return Err(e.into());
    }

    // Upsert the pointer row. If it fails, clean up the archive we just
    // published so an unredacted .zst is never orphaned on disk without a
    // row (F7 (a)) — unless we overwrote a file a prior row still references.
    if let Err(e) = conn.execute(
        "INSERT INTO session_transcripts \
            (session_id, agent, source_path, archive_path, source_bytes, \
             archive_bytes, created_at) \
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, CURRENT_TIMESTAMP) \
         ON CONFLICT(session_id) DO UPDATE SET \
            agent         = excluded.agent, \
            source_path   = excluded.source_path, \
            archive_path  = excluded.archive_path, \
            source_bytes  = excluded.source_bytes, \
            archive_bytes = excluded.archive_bytes, \
            created_at    = CURRENT_TIMESTAMP",
        params![
            session_id,
            agent.as_str(),
            source.to_string_lossy(),
            archive_path.to_string_lossy(),
            source_bytes as i64,
            archive_bytes as i64,
        ],
    ) {
        if !published_over_existing {
            let _ = fs::remove_file(&archive_path);
        }
        return Err(e.into());
    }

    let replaced_existing = match &previous {
        Some(old) if Path::new(old) != archive_path => {
            // Guarded best-effort cleanup of the prior archive. A
            // poisoned/adopted row could store an ARBITRARY absolute path
            // here, so only delete a prior archive we can prove lives inside
            // the canonical archive root — never an outside target.
            match classify_archive_target(&archive_root, memhub_dir, old) {
                ArchiveTarget::Contained(path) => {
                    let _ = fs::remove_file(path);
                }
                ArchiveTarget::Escapes => {
                    log::warn!(
                        "transcript archive: refusing to delete prior out-of-root \
                         archive path {old:?} for session {session_id:?}"
                    );
                }
            }
            true
        }
        Some(_) => true,
        None => false,
    };

    let _ = log_write(
        conn,
        ACTOR,
        "session_transcripts",
        None,
        "archive",
        &format!(
            "archived {} ({} -> {} bytes) for session {}",
            source.display(),
            source_bytes,
            archive_bytes,
            session_id
        ),
    );

    let pruned = prune(conn, memhub_dir, retention_days)?;

    Ok(ArchiveReport {
        session_id: session_id.to_string(),
        agent: agent.as_str(),
        source_path: source.to_path_buf(),
        archive_path,
        source_bytes,
        archive_bytes,
        replaced_existing,
        pruned,
    })
}

/// Delete archive files and their pointer rows older than
/// `retention_days`. `0` disables pruning (keep forever). Returns the
/// number of archives removed.
pub(crate) fn prune(conn: &Connection, memhub_dir: &Path, retention_days: u32) -> Result<usize> {
    if retention_days == 0 {
        return Ok(0);
    }

    let cutoff = format!("-{retention_days} days");
    let stale: Vec<(i64, String)> = {
        let mut stmt = conn.prepare(
            "SELECT id, archive_path FROM session_transcripts \
             WHERE created_at < datetime('now', ?1)",
        )?;
        let rows = stmt.query_map(params![cutoff], |r| Ok((r.get(0)?, r.get(1)?)))?;
        rows.collect::<std::result::Result<Vec<_>, _>>()?
    };

    if stale.is_empty() {
        return Ok(0);
    }

    // The canonical archive root: the ONLY tree prune may delete inside. If
    // it cannot be resolved we cannot prove ANY path is safe to delete, so
    // keep every row rather than silently orphan an unredacted archive.
    let archive_dir = memhub_dir.join(ARCHIVE_DIRNAME);
    let root = match archive_dir.canonicalize() {
        Ok(r) => r,
        Err(e) => {
            log::warn!(
                "transcript prune: cannot canonicalize archive root {} ({e}) — \
                 keeping {} stale pointer row(s) untouched",
                archive_dir.display(),
                stale.len()
            );
            return Ok(0);
        }
    };

    let mut removed = 0usize;
    // Row ids that are safe to drop: the file was removed, the file was
    // already gone, or the row is a poisoned/out-of-root pointer we must
    // never act on (its target, if any, is not ours to keep tracking).
    let mut reclaim: Vec<i64> = Vec::new();

    for (id, stored) in &stale {
        match classify_archive_target(&root, memhub_dir, stored) {
            ArchiveTarget::Contained(path) => match fs::remove_file(&path) {
                Ok(()) => {
                    removed += 1;
                    reclaim.push(*id);
                }
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                    // Nothing on disk to orphan; reclaim the pointer row.
                    reclaim.push(*id);
                }
                Err(e) => {
                    // A real delete failure (a Windows open handle, a
                    // permission error, ...). Do NOT drop the row: the
                    // unredacted archive is still on disk and retention must
                    // retry, not silently forget it (F7 (b), criterion 3).
                    log::warn!(
                        "transcript prune: failed to delete archive {} — keeping \
                         its pointer row for a later retry: {e}",
                        path.display()
                    );
                }
            },
            ArchiveTarget::Escapes => {
                // Poisoned/corrupt/adopted row pointing outside the archive
                // root. NEVER delete the target. Drop only the pointer row
                // and warn loudly so it is not silent (F1/F7, criterion 2).
                log::warn!(
                    "transcript prune: refusing to delete out-of-root archive path \
                     {stored:?} from stale pointer row id {id}; dropping the pointer \
                     row only"
                );
                reclaim.push(*id);
            }
        }
    }

    if !reclaim.is_empty() {
        // Delete exactly the rows we handled, by id — never a blanket cutoff
        // DELETE that could drop a row whose unredacted file still survives.
        let mut stmt = conn.prepare("DELETE FROM session_transcripts WHERE id = ?1")?;
        for id in &reclaim {
            stmt.execute(params![id])?;
        }
    }

    let _ = log_write(
        conn,
        ACTOR,
        "session_transcripts",
        None,
        "prune",
        &format!("pruned {removed} transcript archive(s) older than {retention_days} day(s)"),
    );

    Ok(removed)
}

/// Classification of a stored `archive_path` for the two deletion sinks
/// (prune + the previous-archive cleanup in `archive_into`).
enum ArchiveTarget {
    /// Provably inside the canonical transcripts root, so safe to remove.
    /// The file itself may or may not still exist.
    Contained(PathBuf),
    /// Escapes the root, or cannot be proven inside it. NEVER delete it.
    Escapes,
}

/// Decide whether `stored` (an `archive_path` value from
/// `session_transcripts`) may be deleted. Fail-closed: only a path proven
/// to live under the canonicalized transcripts `root` is `Contained`.
///
/// Back-compat: real archives store an absolute path under the root, so
/// those stay deletable. A poisoned/adopted row with an absolute path
/// outside the root, a `..` escape, or a symlink whose canonical target
/// leaves the root is classified `Escapes` and never touched. `root` must
/// already be canonicalized by the caller.
fn classify_archive_target(root: &Path, memhub_dir: &Path, stored: &str) -> ArchiveTarget {
    if stored.trim().is_empty() {
        return ArchiveTarget::Escapes;
    }

    // Resolve a relative stored path under `.memhub/` (legacy behaviour), an
    // absolute one as-is.
    let candidate = {
        let p = Path::new(stored);
        if p.is_absolute() {
            p.to_path_buf()
        } else {
            memhub_dir.join(p)
        }
    };

    let root_comps = split_path_components(&root.to_string_lossy());
    let cand_comps = split_path_components(&candidate.to_string_lossy());

    // A `..` component is always an escape attempt. This is also the only
    // defence for a MISSING target, which cannot be canonicalized.
    if has_parent_component(&cand_comps) {
        return ArchiveTarget::Escapes;
    }

    let case_insensitive = cfg!(windows);

    // Canonicalization is the authoritative, symlink-safe containment proof.
    // `canonicalize_lexical` follows symlinks for an existing target and
    // falls back to the nearest existing ancestor for a missing one, so a
    // symlinked path prefix never yields a false negative and a symlink
    // inside the root pointing OUT is caught (its resolved target escapes).
    match canonicalize_lexical(&candidate) {
        Some(real) => {
            let real_comps = split_path_components(&real.to_string_lossy());
            if components_contained(&root_comps, &real_comps, case_insensitive) {
                ArchiveTarget::Contained(candidate)
            } else {
                ArchiveTarget::Escapes
            }
        }
        // No resolvable ancestor at all: we cannot prove containment, so
        // refuse. (remove_file would no-op on a truly absent path anyway.)
        None => ArchiveTarget::Escapes,
    }
}

/// Canonicalize `path`, tolerating a missing leaf: if the full path does
/// not exist, canonicalize the nearest existing ancestor and re-append the
/// missing tail. Returns `None` when no ancestor resolves. Callers must
/// have already rejected any `..` component so re-appending is safe.
fn canonicalize_lexical(path: &Path) -> Option<PathBuf> {
    if let Ok(real) = path.canonicalize() {
        return Some(real);
    }
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut cur = path;
    loop {
        let parent = cur.parent()?;
        let name = cur.file_name()?;
        tail.push(name.to_os_string());
        if let Ok(real_parent) = parent.canonicalize() {
            let mut result = real_parent;
            for seg in tail.iter().rev() {
                result.push(seg);
            }
            return Some(result);
        }
        cur = parent;
    }
}

/// Split a path string into components, treating BOTH `/` and `\` as
/// separators and stripping a Windows `\\?\` / `\\?\UNC\` verbatim prefix.
/// This is pure string logic so the containment check is unit-testable
/// cross-platform — a Windows-style string never parses into Windows
/// `Path` components on a Unix host. `.` segments are dropped; `..` is
/// preserved so callers can reject it.
fn split_path_components(raw: &str) -> Vec<String> {
    let stripped = raw
        .strip_prefix(r"\\?\UNC\")
        .or_else(|| raw.strip_prefix(r"\\?\"))
        .unwrap_or(raw);
    stripped
        .split(['/', '\\'])
        .filter(|seg| !seg.is_empty() && *seg != ".")
        .map(|seg| seg.to_string())
        .collect()
}

/// True when any component is a `..` parent reference.
fn has_parent_component(comps: &[String]) -> bool {
    comps.iter().any(|c| c == "..")
}

/// Component-wise prefix containment: is `candidate` inside `root`? When
/// `case_insensitive` (Windows/NTFS), components are compared case-folded.
fn components_contained(root: &[String], candidate: &[String], case_insensitive: bool) -> bool {
    if candidate.len() < root.len() {
        return false;
    }
    root.iter().zip(candidate.iter()).all(|(r, c)| {
        if case_insensitive {
            r.eq_ignore_ascii_case(c)
        } else {
            r == c
        }
    })
}

/// Make a session id safe as a single filename component. Colons (the
/// `codex:` prefix carries one) are invalid on Windows, so map anything
/// outside `[A-Za-z0-9._-]` to `_`. The raw id is still stored verbatim
/// in the pointer row; only the on-disk filename is sanitized.
fn sanitize_filename(session_id: &str) -> String {
    session_id
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn opencode_export(session_id: &str, marker: &str) -> Vec<u8> {
        serde_json::to_vec(&serde_json::json!({
            "data": {
                "info": { "id": session_id, "title": "complete export" },
                "messages": [{ "info": { "id": "msg-1" }, "parts": [{ "text": marker }] }]
            }
        }))
        .expect("serialize fixture")
    }

    fn successful_process(stdout: Vec<u8>) -> ProcessOutput {
        ProcessOutput {
            success: true,
            status: "exit code: 0".to_string(),
            stdout,
            stderr: Vec::new(),
        }
    }

    #[test]
    fn opencode_export_uses_complete_v2_api_with_sanitization_disabled() {
        let body = opencode_export("ses_current", "all messages");
        let mut calls = Vec::new();
        let returned = request_opencode_export_with("ses_current", |program, args| {
            calls.push((program.to_string(), args.to_vec()));
            Ok(successful_process(body.clone()))
        })
        .expect("valid export");

        assert_eq!(returned, body, "the complete response bytes are retained");
        assert_eq!(calls.len(), 1);
        assert_eq!(calls[0].0, "opencode2");
        assert_eq!(
            calls[0].1,
            [
                "api",
                "v2.session.export",
                "--param",
                "sessionID=ses_current",
                "--param",
                "sanitize=false",
            ]
        );
    }

    #[cfg(windows)]
    #[test]
    fn opencode_export_falls_back_to_the_windows_cmd_shim() {
        let body = opencode_export("ses_current", "all messages");
        let mut programs = Vec::new();
        let returned = request_opencode_export_with("ses_current", |program, _| {
            programs.push(program.to_string());
            if program == "opencode2" {
                Err(std::io::Error::new(
                    std::io::ErrorKind::NotFound,
                    "bare npm command is not directly executable",
                ))
            } else {
                Ok(successful_process(body.clone()))
            }
        })
        .expect(".cmd fallback");

        assert_eq!(returned, body);
        assert_eq!(programs, ["opencode2", "opencode2.cmd"]);
    }

    fn assert_opencode_failure_before_write<F>(exporter: F, expected: &str)
    where
        F: FnOnce(&str) -> Result<Vec<u8>>,
    {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");

        let error = archive_with_opencode_exporter(
            temp.path(),
            Agent::OpenCode,
            "ses_current",
            true,
            exporter,
        )
        .expect_err("export failure must stop archival");
        assert!(
            error.to_string().contains(expected),
            "expected {expected:?}, got {error}"
        );

        let ctx = crate::db::open_project(temp.path()).expect("open");
        let rows: i64 = ctx
            .conn
            .query_row("SELECT COUNT(*) FROM session_transcripts", [], |row| {
                row.get(0)
            })
            .expect("count rows");
        assert_eq!(rows, 0, "failure must not create a pointer row");
        assert!(
            !ctx.paths.memhub_dir.join(ARCHIVE_DIRNAME).exists(),
            "failure must not create an archive directory"
        );
    }

    #[test]
    fn opencode_export_failures_all_happen_before_archive_writes() {
        assert_opencode_failure_before_write(
            |session_id| {
                request_opencode_export_with(session_id, |_, _| {
                    Err(std::io::Error::new(std::io::ErrorKind::NotFound, "missing"))
                })
            },
            "CLI not found",
        );

        assert_opencode_failure_before_write(
            |session_id| {
                request_opencode_export_with(session_id, |_, _| {
                    Ok(ProcessOutput {
                        success: false,
                        status: "exit code: 7".to_string(),
                        stdout: Vec::new(),
                        stderr: b"local API unavailable".to_vec(),
                    })
                })
            },
            "local API unavailable",
        );

        assert_opencode_failure_before_write(
            |session_id| {
                request_opencode_export_with(session_id, |_, _| {
                    Ok(successful_process(b"not json".to_vec()))
                })
            },
            "malformed JSON",
        );

        assert_opencode_failure_before_write(
            |session_id| {
                request_opencode_export_with(session_id, |_, _| {
                    Ok(successful_process(
                        br#"{"data":{"info":{"id":"ses_current"}}}"#.to_vec(),
                    ))
                })
            },
            "data.messages array",
        );

        assert_opencode_failure_before_write(
            |session_id| {
                request_opencode_export_with(session_id, |_, _| {
                    Ok(successful_process(opencode_export("ses_other", "body")))
                })
            },
            "id mismatch",
        );
    }

    #[test]
    fn opencode_metacharacter_id_never_reaches_process_or_archive_writes() {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        let mut exporter_called = false;

        let error = archive_with_opencode_exporter(
            temp.path(),
            Agent::OpenCode,
            "ses_safe&whoami",
            true,
            |_| {
                exporter_called = true;
                Ok(Vec::new())
            },
        )
        .expect_err("unsafe OpenCode id must be rejected");

        assert!(!exporter_called, "unsafe id reached the process seam");
        assert!(error.to_string().contains("only ASCII"), "{error}");
        let ctx = crate::db::open_project(temp.path()).expect("open");
        let rows: i64 = ctx
            .conn
            .query_row("SELECT COUNT(*) FROM session_transcripts", [], |row| {
                row.get(0)
            })
            .expect("count rows");
        assert_eq!(rows, 0, "unsafe id created a pointer row");
        assert!(
            !ctx.paths.memhub_dir.join(ARCHIVE_DIRNAME).exists(),
            "unsafe id created an archive directory"
        );
    }

    #[test]
    fn opencode_archive_round_trips_complete_json_and_keeps_isolation_invariants() {
        const MARKER: &str = "OPENCODE_TRANSCRIPT_SECRET_MARKER";
        let temp = tempfile::tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        let body = opencode_export("ses_current", MARKER);

        let report = archive_with_opencode_exporter(
            temp.path(),
            Agent::OpenCode,
            "ses_current",
            true,
            |_| Ok(body.clone()),
        )
        .expect("archive");

        assert_eq!(report.agent, "opencode");
        assert!(report.archive_path.to_string_lossy().ends_with(".json.zst"));
        assert_eq!(report.source_bytes, body.len() as u64);
        let compressed = fs::read(&report.archive_path).expect("read archive");
        let decoded = zstd::decode_all(&compressed[..]).expect("decode archive");
        assert_eq!(decoded, body, "complete exported JSON must round-trip");

        let ctx = crate::db::open_project(temp.path()).expect("open");
        let (agent, source_path): (String, String) = ctx
            .conn
            .query_row(
                "SELECT agent, source_path FROM session_transcripts WHERE session_id = 'ses_current'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("pointer row");
        assert_eq!(agent, "opencode");
        assert_eq!(source_path, OPENCODE_EXPORT_SOURCE);
        let embeddings: i64 = ctx
            .conn
            .query_row("SELECT COUNT(*) FROM embeddings", [], |row| row.get(0))
            .expect("embeddings count");
        assert_eq!(embeddings, 0, "OpenCode archives are never embedded");
        drop(ctx);

        let search = crate::commands::search::run(temp.path(), MARKER, 10).expect("search");
        assert!(
            search.results.is_empty(),
            "archive must not enter recall/search"
        );

        let export_path = temp.path().join("memhub-export.json");
        crate::commands::export::run(temp.path(), &export_path).expect("memhub export");
        let exported = fs::read_to_string(export_path).expect("read memhub export");
        assert!(!exported.contains(MARKER));
        let exported: serde_json::Value = serde_json::from_str(&exported).expect("parse export");
        assert!(
            exported
                .as_object()
                .is_some_and(|object| !object.contains_key("session_transcripts")),
            "memhub export must not carry the transcript pointer table"
        );
    }

    #[test]
    fn sanitize_filename_neutralizes_codex_colon_and_other_unsafe_chars() {
        assert_eq!(
            sanitize_filename("codex:abcd-1234"),
            "codex_abcd-1234",
            "the codex: colon is invalid in a Windows filename"
        );
        // A Claude UUID is already filename-safe and passes through.
        assert_eq!(
            sanitize_filename("11111111-2222-3333-4444-555555555555"),
            "11111111-2222-3333-4444-555555555555"
        );
        // Path separators and spaces are neutralized, never leaked.
        assert_eq!(sanitize_filename("a/b\\c d"), "a_b_c_d");
    }

    #[test]
    fn normalize_session_id_prefixes_bare_codex_ids_only() {
        assert_eq!(normalize_session_id(Agent::Codex, "uuid-1"), "codex:uuid-1");
        assert_eq!(
            normalize_session_id(Agent::Codex, "codex:uuid-1"),
            "codex:uuid-1",
            "an already-prefixed id is left alone"
        );
        assert_eq!(
            normalize_session_id(Agent::Claude, "uuid-1"),
            "uuid-1",
            "claude ids are never prefixed"
        );
        assert_eq!(
            normalize_session_id(Agent::OpenCode, "ses_current"),
            "ses_current",
            "OpenCode ids are never prefixed"
        );
    }

    #[test]
    fn validate_session_id_rejects_traversal_and_accepts_real_ids() {
        // Real ids pass.
        validate_session_id("11111111-2222-3333-4444-555555555555").expect("claude uuid");
        validate_session_id("codex:abcd-1234").expect("codex id");
        validate_session_id("ses_current").expect("OpenCode id");
        // Traversal vectors are all refused.
        for evil in [
            "../../etc/passwd",
            "..\\..\\secret",
            "a/b",
            "a\\b",
            "..",
            "foo/../bar",
            "",
            "   ",
        ] {
            assert!(
                validate_session_id(evil).is_err(),
                "must reject traversal-style id {evil:?}"
            );
        }
    }

    #[test]
    fn archive_refuses_without_explicit_approval() {
        // The fail-closed guard fires before any project is opened, so a
        // bogus path is fine — we only assert the refusal.
        let err = archive(Path::new("/nonexistent"), Agent::Claude, "sess-1", false)
            .expect_err("must refuse without approval");
        let msg = err.to_string();
        assert!(msg.contains("explicit approval"), "unexpected: {msg}");
    }

    // -- crate-internal end-to-end over the pub(crate) core ----------------

    fn open_ctx(dir: &Path) -> crate::db::ProjectContext {
        crate::commands::init::run(dir).expect("init");
        crate::db::open_project(dir).expect("open")
    }

    #[test]
    fn archive_into_writes_zst_and_pointer_row_that_round_trips() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ctx = open_ctx(temp.path());

        let source = temp.path().join("sess-1.jsonl");
        let body = "{\"type\":\"user\"}\n{\"type\":\"assistant\"}\n";
        fs::write(&source, body).expect("write source");

        let report = archive_into(
            &ctx.conn,
            &ctx.paths.memhub_dir,
            Agent::Claude,
            "sess-1",
            &source,
            90,
        )
        .expect("archive");

        assert!(report.archive_path.exists());
        assert!(
            report
                .archive_path
                .to_string_lossy()
                .ends_with(".jsonl.zst")
        );
        assert_eq!(report.source_bytes, body.len() as u64);
        assert!(!report.replaced_existing);
        assert_eq!(report.pruned, 0);

        // zstd round-trips back to the exact original bytes.
        let compressed = fs::read(&report.archive_path).expect("read archive");
        let decoded = zstd::decode_all(&compressed[..]).expect("decode");
        assert_eq!(decoded, body.as_bytes(), "zstd must round-trip losslessly");

        // Exactly one pointer row carrying the durable fields.
        let (sid, agent, sbytes, abytes): (String, String, i64, i64) = ctx
            .conn
            .query_row(
                "SELECT session_id, agent, source_bytes, archive_bytes \
                 FROM session_transcripts WHERE session_id = 'sess-1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .expect("row");
        assert_eq!(sid, "sess-1");
        assert_eq!(agent, "claude-code");
        assert_eq!(sbytes, body.len() as i64);
        assert_eq!(abytes as u64, report.archive_bytes);
    }

    #[test]
    fn re_archiving_a_session_replaces_in_place_keeping_one_row() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ctx = open_ctx(temp.path());
        let source = temp.path().join("sess-2.jsonl");
        fs::write(&source, "{\"a\":1}\n").expect("write");

        let first = archive_into(
            &ctx.conn,
            &ctx.paths.memhub_dir,
            Agent::Claude,
            "sess-2",
            &source,
            90,
        )
        .expect("first");
        assert!(!first.replaced_existing);

        let second = archive_into(
            &ctx.conn,
            &ctx.paths.memhub_dir,
            Agent::Claude,
            "sess-2",
            &source,
            90,
        )
        .expect("second");
        assert!(second.replaced_existing);

        let count: i64 = ctx
            .conn
            .query_row(
                "SELECT COUNT(*) FROM session_transcripts WHERE session_id = 'sess-2'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(count, 1, "re-archive upserts, never duplicates");
    }

    #[test]
    fn prune_removes_stale_archives_and_rows_but_keeps_fresh() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ctx = open_ctx(temp.path());

        // Archive with retention disabled so the archive step doesn't prune.
        let source = temp.path().join("sess-old.jsonl");
        fs::write(&source, "{\"x\":1}\n").expect("write");
        let report = archive_into(
            &ctx.conn,
            &ctx.paths.memhub_dir,
            Agent::Claude,
            "sess-old",
            &source,
            0,
        )
        .expect("archive");
        let stale_file = report.archive_path.clone();
        assert!(stale_file.exists());

        // Backdate the row well past the horizon, then prune at 30 days.
        ctx.conn
            .execute(
                "UPDATE session_transcripts SET created_at = datetime('now', '-100 days') \
                 WHERE session_id = 'sess-old'",
                [],
            )
            .expect("backdate");

        let pruned = prune(&ctx.conn, &ctx.paths.memhub_dir, 30).expect("prune");
        assert_eq!(pruned, 1);
        let rows: i64 = ctx
            .conn
            .query_row("SELECT COUNT(*) FROM session_transcripts", [], |r| r.get(0))
            .expect("count");
        assert_eq!(rows, 0, "stale pointer row pruned");
        assert!(!stale_file.exists(), "stale archive file removed");
    }

    #[test]
    fn prune_is_a_noop_when_retention_is_zero() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ctx = open_ctx(temp.path());
        let source = temp.path().join("sess-z.jsonl");
        fs::write(&source, "{\"x\":1}\n").expect("write");
        archive_into(
            &ctx.conn,
            &ctx.paths.memhub_dir,
            Agent::Claude,
            "sess-z",
            &source,
            0,
        )
        .expect("archive");
        ctx.conn
            .execute(
                "UPDATE session_transcripts SET created_at = datetime('now', '-3650 days')",
                [],
            )
            .expect("backdate");

        let pruned = prune(&ctx.conn, &ctx.paths.memhub_dir, 0).expect("prune");
        assert_eq!(pruned, 0, "retention 0 keeps everything");
        let rows: i64 = ctx
            .conn
            .query_row("SELECT COUNT(*) FROM session_transcripts", [], |r| r.get(0))
            .expect("count");
        assert_eq!(rows, 1);
    }

    // -- containment + lifecycle hardening (F1 + F7) -----------------------

    /// Seed a stale (backdated) pointer row directly, without going through
    /// the archive path — so tests can plant unsafe `archive_path` values.
    fn seed_stale_row(conn: &Connection, session_id: &str, archive_path: &str) {
        conn.execute(
            "INSERT INTO session_transcripts \
                (session_id, agent, source_path, archive_path, source_bytes, \
                 archive_bytes, created_at) \
             VALUES (?1, 'claude-code', '/src', ?2, 1, 1, datetime('now', '-100 days'))",
            params![session_id, archive_path],
        )
        .expect("seed stale row");
    }

    fn row_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM session_transcripts", [], |r| r.get(0))
            .expect("count")
    }

    #[test]
    fn prune_never_deletes_targets_outside_the_archive_root() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ctx = open_ctx(temp.path());
        let archive_dir = ctx.paths.memhub_dir.join("transcripts");
        fs::create_dir_all(&archive_dir).expect("archive dir");

        // (1) Absolute path escaping the root: a sentinel elsewhere in the
        // repo tree that a poisoned/adopted row points at.
        let victim_abs = temp.path().join("victim-abs.txt");
        fs::write(&victim_abs, "keep me").expect("victim abs");
        seed_stale_row(&ctx.conn, "evil-abs", &victim_abs.to_string_lossy());

        // (2) `..` relative escape: resolves to <temp>/victim-rel.txt.
        let victim_rel = temp.path().join("victim-rel.txt");
        fs::write(&victim_rel, "keep me too").expect("victim rel");
        seed_stale_row(&ctx.conn, "evil-rel", "../victim-rel.txt");

        let pruned = prune(&ctx.conn, &ctx.paths.memhub_dir, 30).expect("prune");

        assert!(victim_abs.exists(), "absolute-escape target must survive");
        assert!(
            victim_rel.exists(),
            "..-relative-escape target must survive"
        );
        assert_eq!(pruned, 0, "no in-root archive was removed");
        assert_eq!(
            row_count(&ctx.conn),
            0,
            "poisoned pointer rows are dropped, targets untouched"
        );
    }

    #[cfg(unix)]
    #[test]
    fn prune_never_deletes_through_an_in_root_symlink_to_outside() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ctx = open_ctx(temp.path());
        let archive_dir = ctx.paths.memhub_dir.join("transcripts");
        fs::create_dir_all(&archive_dir).expect("archive dir");

        // A symlink that LIVES inside the root but points OUT of it. Its
        // canonical target escapes, so the row must be classified unsafe.
        let outside = temp.path().join("outside-secret.txt");
        fs::write(&outside, "do not delete").expect("outside");
        let link = archive_dir.join("2020-01-01-linked.jsonl.zst");
        std::os::unix::fs::symlink(&outside, &link).expect("symlink");
        seed_stale_row(&ctx.conn, "evil-symlink", &link.to_string_lossy());

        let pruned = prune(&ctx.conn, &ctx.paths.memhub_dir, 30).expect("prune");

        assert!(
            outside.exists(),
            "the symlink target outside the root must survive"
        );
        assert_eq!(pruned, 0);
        assert_eq!(row_count(&ctx.conn), 0, "the poisoned row is reclaimed");
    }

    #[test]
    fn prune_reclaims_a_row_whose_contained_archive_is_missing() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ctx = open_ctx(temp.path());
        let archive_dir = ctx.paths.memhub_dir.join("transcripts");
        fs::create_dir_all(&archive_dir).expect("archive dir");

        // A path INSIDE the root whose file was already removed out of band.
        let missing = archive_dir.join("2020-01-01-gone.jsonl.zst");
        seed_stale_row(&ctx.conn, "gone", &missing.to_string_lossy());

        let pruned = prune(&ctx.conn, &ctx.paths.memhub_dir, 30).expect("prune");
        assert_eq!(pruned, 0, "no file existed to remove");
        assert_eq!(
            row_count(&ctx.conn),
            0,
            "an absent-but-contained archive reclaims its row"
        );
    }

    #[test]
    fn prune_keeps_the_row_when_the_file_delete_fails() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ctx = open_ctx(temp.path());
        let archive_dir = ctx.paths.memhub_dir.join("transcripts");
        fs::create_dir_all(&archive_dir).expect("archive dir");

        // A DIRECTORY sitting where the archive file's path points: it is
        // contained in the root and exists, so classification is Contained,
        // but `remove_file` on a directory fails with a non-NotFound error —
        // a portable stand-in for a locked/undeletable archive (F7 (b)).
        let blocked = archive_dir.join("2020-01-01-blocked.jsonl.zst");
        fs::create_dir(&blocked).expect("dir at archive path");
        seed_stale_row(&ctx.conn, "blocked", &blocked.to_string_lossy());

        let pruned = prune(&ctx.conn, &ctx.paths.memhub_dir, 30).expect("prune");
        assert_eq!(
            pruned, 0,
            "the undeletable archive was not counted as removed"
        );
        assert!(blocked.exists(), "the undeletable file survives");
        assert_eq!(
            row_count(&ctx.conn),
            1,
            "the pointer row is kept for a retry, never silently dropped"
        );
    }

    #[test]
    fn archive_into_cleans_up_the_zst_when_the_upsert_fails() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ctx = open_ctx(temp.path());

        // Force the pointer-row INSERT to fail deterministically, AFTER the
        // archive file is published, so the cleanup path is exercised.
        ctx.conn
            .execute_batch(
                "CREATE TRIGGER t_fail_upsert BEFORE INSERT ON session_transcripts \
                 BEGIN SELECT RAISE(FAIL, 'forced upsert failure'); END;",
            )
            .expect("trigger");

        let source = temp.path().join("sess-orphan.jsonl");
        fs::write(&source, "{\"x\":1}\n").expect("write source");

        let err = archive_into(
            &ctx.conn,
            &ctx.paths.memhub_dir,
            Agent::Claude,
            "sess-orphan",
            &source,
            0,
        )
        .expect_err("upsert must fail");
        assert!(
            err.to_string().contains("forced upsert failure"),
            "unexpected error: {err}"
        );

        // No orphaned archive (and no leftover temp file) remains on disk.
        let archive_dir = ctx.paths.memhub_dir.join("transcripts");
        let entries: Vec<String> = fs::read_dir(&archive_dir)
            .expect("read dir")
            .filter_map(|e| e.ok())
            .map(|e| e.file_name().to_string_lossy().into_owned())
            .collect();
        assert!(
            entries.is_empty(),
            "a failed upsert must leave no archive/temp file: {entries:?}"
        );
        assert_eq!(row_count(&ctx.conn), 0, "no pointer row was written");
    }

    #[test]
    fn archive_into_rejects_a_sanitize_filename_collision_across_sessions() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ctx = open_ctx(temp.path());
        let source = temp.path().join("sess.jsonl");
        fs::write(&source, "{\"x\":1}\n").expect("write");

        // Two DISTINCT session ids that sanitize to the same filename: `a:b`
        // and `a_b` both map to `a_b` (decision 161: reject loudly).
        archive_into(
            &ctx.conn,
            &ctx.paths.memhub_dir,
            Agent::Claude,
            "a_b",
            &source,
            0,
        )
        .expect("first archive");
        let err = archive_into(
            &ctx.conn,
            &ctx.paths.memhub_dir,
            Agent::Claude,
            "a:b",
            &source,
            0,
        )
        .expect_err("colliding filename must be refused");
        let msg = err.to_string();
        assert!(
            msg.contains("collides") && msg.contains("a_b"),
            "expected a loud collision refusal, got: {msg}"
        );

        assert_eq!(row_count(&ctx.conn), 1, "only the first session has a row");
        let archive_dir = ctx.paths.memhub_dir.join("transcripts");
        let zst = fs::read_dir(&archive_dir)
            .expect("read dir")
            .filter_map(|e| e.ok())
            .filter(|e| e.file_name().to_string_lossy().ends_with(".jsonl.zst"))
            .count();
        assert_eq!(zst, 1, "the colliding second archive must not be written");
    }

    #[test]
    fn re_archive_previous_cleanup_never_deletes_an_out_of_root_prior_path() {
        let temp = tempfile::tempdir().expect("tempdir");
        let ctx = open_ctx(temp.path());
        fs::create_dir_all(ctx.paths.memhub_dir.join("transcripts")).expect("archive dir");

        // Sentinel OUTSIDE the archive root that a poisoned prior row points
        // at. Dated fresh so prune won't touch it — this exercises the
        // SECOND deletion sink: archive_into's previous-archive cleanup.
        let victim = temp.path().join("prior-victim.txt");
        fs::write(&victim, "keep me").expect("victim");
        ctx.conn
            .execute(
                "INSERT INTO session_transcripts \
                    (session_id, agent, source_path, archive_path, source_bytes, \
                     archive_bytes, created_at) \
                 VALUES ('sess-p', 'claude-code', '/src', ?1, 1, 1, CURRENT_TIMESTAMP)",
                params![victim.to_string_lossy()],
            )
            .expect("seed poisoned row");

        let source = temp.path().join("sess-p.jsonl");
        fs::write(&source, "{\"x\":1}\n").expect("write source");
        let report = archive_into(
            &ctx.conn,
            &ctx.paths.memhub_dir,
            Agent::Claude,
            "sess-p",
            &source,
            0,
        )
        .expect("re-archive");

        assert!(report.replaced_existing, "the prior row was replaced");
        assert!(
            victim.exists(),
            "the out-of-root prior target must survive the cleanup"
        );
    }

    // -- pure Windows-normalization / containment units (criterion 7) ------

    #[test]
    fn split_path_components_strips_verbatim_prefix_and_both_separators() {
        assert_eq!(split_path_components("/a/b/c"), vec!["a", "b", "c"]);
        assert_eq!(split_path_components(r"C:\a\b"), vec!["C:", "a", "b"]);
        assert_eq!(split_path_components(r"\\?\C:\a\b"), vec!["C:", "a", "b"]);
        assert_eq!(
            split_path_components(r"\\?\UNC\server\share\x"),
            vec!["server", "share", "x"]
        );
        // redundant separators and `.` segments are dropped
        assert_eq!(split_path_components("/a//./b/"), vec!["a", "b"]);
        // `..` is preserved so the caller can reject it
        assert_eq!(split_path_components("a/../b"), vec!["a", "..", "b"]);
    }

    #[test]
    fn components_contained_prefix_and_case_semantics() {
        let root = vec!["a".to_string(), "b".to_string()];
        let inside = vec!["a".to_string(), "b".to_string(), "c".to_string()];
        let sibling = vec!["a".to_string(), "bb".to_string()];
        let shorter = vec!["a".to_string()];
        assert!(components_contained(&root, &inside, false));
        assert!(components_contained(&root, &root, false));
        assert!(
            !components_contained(&root, &sibling, false),
            "component prefix, not string prefix"
        );
        assert!(!components_contained(&root, &shorter, false));

        // Case-insensitive (Windows/NTFS) folds; case-sensitive does not.
        let upper = vec!["A".to_string(), "B".to_string(), "c".to_string()];
        assert!(components_contained(&root, &upper, true));
        assert!(!components_contained(&root, &upper, false));
    }

    #[test]
    fn windows_verbatim_paths_normalize_for_containment() {
        // Exercises the Windows path normalization on any host: `\\?\`
        // verbatim absolute paths compared case-insensitively.
        let root = split_path_components(r"\\?\C:\repo\.memhub\transcripts");
        let escape = split_path_components(r"\\?\C:\Windows\System32\evil.txt");
        let inside = split_path_components(r"\\?\C:\repo\.memhub\transcripts\2026-a.jsonl.zst");
        let inside_ci = split_path_components(r"\\?\c:\REPO\.memhub\TRANSCRIPTS\x.zst");
        assert!(
            !components_contained(&root, &escape, true),
            "escape rejected"
        );
        assert!(
            components_contained(&root, &inside, true),
            "in-root accepted"
        );
        assert!(
            components_contained(&root, &inside_ci, true),
            "case-folded in-root path is still contained on Windows"
        );
        assert!(
            !components_contained(&root, &inside_ci, false),
            "the same differ-by-case path is NOT contained under case-sensitive rules"
        );
    }
}
