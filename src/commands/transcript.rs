//! `memhub transcript archive` (Wave 6 W3, issue #96 / decisions Q7+Q8):
//! the session-transcript archiver behind the `transcript` wrap-up level.
//!
//! When a machine opts into `[wrap_up] verbosity = "transcript"`, wrap-up
//! copies the current session's raw agent JSONL into
//! `.memhub/transcripts/<date>-<session-id>.jsonl.zst` and records one
//! pointer row in `session_transcripts` (migration 0023).
//!
//! ## Security posture (Q8)
//!
//! The archive is stored **UNREDACTED**. v1 secret handling is warn +
//! explicit per-wrap-up approval, NOT content redaction (a deliberate
//! follow-up). Two things make that safe enough for v1: the source
//! transcript already exists unredacted in the agent's own session dir,
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
//! These are enforced by construction and asserted in
//! `tests/lifecycle/transcript_archive.rs`.
//!
//! ## Path + id reuse
//!
//! Directory resolution and the session-id ↔ file mapping come from the
//! metrics-independent `transcript_files` module, so archiving still works
//! while token accounting is hibernated.

use std::fs;
use std::path::{Path, PathBuf};

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
/// point for append-only JSONL that is written once and rarely read.
const ZSTD_LEVEL: i32 = 3;

/// Which agent's transcript directory + session-id convention to use.
/// Mirrors the two agents the metrics scraper already understands.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Agent {
    Claude,
    Codex,
}

impl Agent {
    /// Agent tag stored in `session_transcripts.agent`, identical to the
    /// tags the scraper writes into `session_metrics.agent`.
    pub fn as_str(self) -> &'static str {
        match self {
            Agent::Claude => "claude-code",
            Agent::Codex => "codex",
        }
    }

    /// Short human label for error/warning text.
    fn label(self) -> &'static str {
        match self {
            Agent::Claude => "Claude",
            Agent::Codex => "Codex",
        }
    }

    /// The `[metrics]` config key that names this agent's transcript dir.
    fn config_key(self) -> &'static str {
        match self {
            Agent::Claude => "claude",
            Agent::Codex => "codex",
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
pub const UNREDACTED_WARNING: &str =
    "WARNING: memhub transcript archive stores the RAW, UNREDACTED session \
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
    // read. Applied uniformly to both agents — a real Claude UUID / Codex
    // `codex:<uuid>` never trips it.
    validate_session_id(session_id)?;

    let session_id = normalize_session_id(agent, session_id);
    let ctx = db::open_project(start)?;
    let retention_days = ctx.config.wrap_up.transcript_retention_days;
    let dir = resolve_transcript_dir(&ctx.config, &ctx.paths.repo_root, agent)?;
    let source = resolve_source(agent, &dir, &session_id)?;

    // Belt-and-suspenders: even with a clean id, a symlinked transcript
    // file could point outside the dir. Canonicalize both and require the
    // resolved source to live under the transcripts directory, or refuse.
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
/// pass through untouched.
fn normalize_session_id(agent: Agent, session_id: &str) -> String {
    match agent {
        Agent::Codex if !session_id.starts_with("codex:") => format!("codex:{session_id}"),
        _ => session_id.to_string(),
    }
}

/// Reject a session id that could escape the transcripts directory when
/// path-joined. Fail-closed: a separator (`/` or `\`) or any `..` is
/// refused before the id is ever resolved or read. No legitimate Claude
/// UUID or Codex `codex:<uuid>` contains any of these.
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
    let (configured, detected) = match agent {
        Agent::Claude => (
            config.metrics.claude_transcripts_dir.clone(),
            transcript_files::detect_claude_transcripts_dir(repo_root),
        ),
        Agent::Codex => (
            config.metrics.codex_transcripts_dir.clone(),
            transcript_files::detect_codex_sessions_dir(),
        ),
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
            agent.config_key(),
        ))
    })
}

/// Locate the source JSONL under `dir` for `session_id`, reusing the
/// scraper's file-naming knowledge (do not re-derive paths here).
fn resolve_source(agent: Agent, dir: &Path, session_id: &str) -> Result<PathBuf> {
    let found = match agent {
        Agent::Claude => transcript_files::find_claude_transcript(dir, session_id),
        Agent::Codex => transcript_files::find_codex_transcript(dir, session_id),
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

/// Core archive step, split out so integration tests can drive it against
/// a real project + a seeded source file. Compresses `source` into
/// `.memhub/transcripts/<date>-<session-id>.jsonl.zst`, upserts the
/// pointer row, then prunes past the retention horizon.
pub(crate) fn archive_into(
    conn: &Connection,
    memhub_dir: &Path,
    agent: Agent,
    session_id: &str,
    source: &Path,
    retention_days: u32,
) -> Result<ArchiveReport> {
    let source_bytes = fs::metadata(source)?.len();

    // Compress the whole file into memory. Session JSONL is small (a few
    // MB at most) and written once, so a single-shot encode is simpler
    // and safer than a streaming copy with partial-write cleanup.
    let compressed = zstd::encode_all(fs::File::open(source)?, ZSTD_LEVEL)?;
    let archive_bytes = compressed.len() as u64;

    let archive_dir = memhub_dir.join(ARCHIVE_DIRNAME);
    fs::create_dir_all(&archive_dir)?;

    // `date('now')` is UTC and pairs with the DB's other timestamps.
    let date: String = conn.query_row("SELECT date('now')", [], |r| r.get(0))?;
    let file_name = format!("{date}-{}.jsonl.zst", sanitize_filename(session_id));
    let archive_path = archive_dir.join(&file_name);

    // Remember any prior archive so a re-archive on a different day cleans
    // up the stale file after the new one is safely written.
    let previous: Option<String> = conn
        .query_row(
            "SELECT archive_path FROM session_transcripts WHERE session_id = ?1",
            params![session_id],
            |r| r.get(0),
        )
        .optional()?;

    fs::write(&archive_path, &compressed)?;

    conn.execute(
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
    )?;

    let replaced_existing = match &previous {
        Some(old) if Path::new(old) != archive_path => {
            // Best-effort: a leftover file is harmless, and prune would
            // not catch it (its row is gone).
            let _ = fs::remove_file(old);
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

    // Remove files first (best-effort); a missing/locked file must not
    // block reclaiming the row — the archive dir is under `.memhub/`.
    for (_, path) in &stale {
        let full = archive_file_path(memhub_dir, path);
        let _ = fs::remove_file(&full);
    }

    conn.execute(
        "DELETE FROM session_transcripts WHERE created_at < datetime('now', ?1)",
        params![cutoff],
    )?;

    let _ = log_write(
        conn,
        ACTOR,
        "session_transcripts",
        None,
        "prune",
        &format!(
            "pruned {} transcript archive(s) older than {} day(s)",
            stale.len(),
            retention_days
        ),
    );

    Ok(stale.len())
}

/// `archive_path` is stored as written (absolute for a real archive). If
/// it is somehow relative, resolve it under `.memhub/` so prune still
/// finds the file.
fn archive_file_path(memhub_dir: &Path, stored: &str) -> PathBuf {
    let p = Path::new(stored);
    if p.is_absolute() {
        p.to_path_buf()
    } else {
        memhub_dir.join(p)
    }
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
        assert_eq!(
            normalize_session_id(Agent::Codex, "uuid-1"),
            "codex:uuid-1"
        );
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
    }

    #[test]
    fn validate_session_id_rejects_traversal_and_accepts_real_ids() {
        // Real ids pass.
        validate_session_id("11111111-2222-3333-4444-555555555555").expect("claude uuid");
        validate_session_id("codex:abcd-1234").expect("codex id");
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
        let err = archive(
            Path::new("/nonexistent"),
            Agent::Claude,
            "sess-1",
            false,
        )
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
}
