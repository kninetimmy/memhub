//! Component B of the token-accounting subsystem (decision 74,
//! task #29).
//!
//! Scrapes Claude Code transcript JSONL for the *real*
//! `usage.input_tokens` / `usage.output_tokens` / cache totals an
//! agent burned, and UPSERTs them into `session_metrics`. Unlike
//! Component A (local arithmetic, cannot break), this component reads
//! an external file format owned by Claude Code, so it lives behind
//! its own `metrics.session_accounting` kill switch and is written
//! defensively: a shape mismatch logs and continues, it never fails
//! the host `memhub` command.
//!
//! ## Cadence
//!
//! `scrape_if_enabled` is called once from `db::open_project`, so it
//! runs opportunistically on every `memhub` invocation — no daemon,
//! no cron (decision 74). Repeated calls within one process (e.g. an
//! eval sweep that recalls many times) are cheap: a session whose
//! file length already equals its recorded `last_scanned_offset` is
//! skipped without opening the file.
//!
//! ## Incremental resume
//!
//! Claude Code writes one append-only `<session-id>.jsonl` per
//! session. We seek to the session's `last_scanned_offset`, read only
//! complete newline-terminated lines, accumulate token deltas, and
//! advance the offset to the byte after the last `\n`. A trailing
//! partial line (the session is still being written) is left
//! unconsumed for the next pass, so no turn is double-counted and no
//! half-written JSON line is parsed.
//!
//! Token counters accumulate (`+= excluded`) rather than replace,
//! because each pass only ever reads the bytes appended since the
//! last pass. `started_at` is pinned to the earliest line seen;
//! `ended_at` advances to the latest. `recall_calls` is deliberately
//! left untouched here — the reconciler (task #30) owns it.
//!
//! ## Deviation from the task wording
//!
//! Task #29 step 2 says "skip files whose mtime <= max(last_scan_ts)".
//! Migration 0012 has no `last_scan_ts` column, and an mtime/clock
//! comparison is fragile (a `touch` or clock skew defeats it).
//! Instead we skip when `file_len == last_scanned_offset`: exact,
//! schema-native, and immune to clock issues. A file shorter than its
//! recorded offset is treated as rotated/rewritten and re-scanned
//! from zero after zeroing that session's counters.

use std::fs::{self, File};
use std::io::{BufRead, BufReader, Seek, SeekFrom};
use std::path::Path;

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::config::MetricsConfig;
use crate::db::log_write;
use crate::Result;

const CLAUDE_AGENT: &str = "claude-code";
const SCRAPER_ACTOR: &str = "metrics:claude-scraper";
const CODEX_AGENT: &str = "codex";
const CODEX_SCRAPER_ACTOR: &str = "metrics:codex-scraper";

/// Opportunistic entry point. Gated by the `metrics.enabled` master
/// switch and the `metrics.session_accounting` sub-switch; both off by
/// default, so this is a zero-cost early return on a non-opted-in
/// install. Errors are swallowed (logged, never propagated) — losing a
/// metrics scrape must never fail an otherwise-successful command.
///
/// `repo_root` is used to filter Codex sessions: Codex writes all
/// projects' sessions to `~/.codex/sessions/`, so each file is checked
/// against `session_meta.payload.cwd` before any rows are written
/// (decision 77).
pub fn scrape_if_enabled(conn: &Connection, cfg: &MetricsConfig, repo_root: &Path) {
    if !cfg.enabled || !cfg.session_accounting {
        return;
    }

    if !cfg.claude_transcripts_dir.is_empty() {
        let dir = Path::new(&cfg.claude_transcripts_dir);
        if let Err(err) = scrape_claude_dir(conn, dir) {
            log::warn!(
                "session_metrics scrape of {} failed: {err}",
                dir.display()
            );
        }
    }

    if !cfg.codex_transcripts_dir.is_empty() {
        let dir = Path::new(&cfg.codex_transcripts_dir);
        if let Err(err) = scrape_codex_dir(conn, dir, repo_root) {
            log::warn!(
                "session_metrics codex scrape of {} failed: {err}",
                dir.display()
            );
        }
    }
}

/// Scrape every `*.jsonl` directly under `dir`. A missing directory is
/// not an error (the user enabled metrics but the path hasn't produced
/// transcripts yet); other directory-read failures propagate.
fn scrape_claude_dir(conn: &Connection, dir: &Path) -> Result<()> {
    let entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };

    for entry in entries {
        let path = match entry {
            Ok(e) => e.path(),
            Err(err) => {
                log::debug!("metrics: skipping unreadable dir entry: {err}");
                continue;
            }
        };
        if path.extension().and_then(|e| e.to_str()) != Some("jsonl") {
            continue;
        }
        // One bad file must not stop the others.
        if let Err(err) = scrape_claude_file(conn, &path) {
            log::warn!(
                "metrics: skipping session file {}: {err}",
                path.display()
            );
        }
    }
    Ok(())
}

/// Scrape the Codex sessions tree: `~/.codex/sessions/YYYY/MM/DD/*.jsonl`.
/// The tree is global (all projects share it), so each file is filtered by
/// `session_meta.payload.cwd` before rows are written (decision 77).
fn scrape_codex_dir(conn: &Connection, dir: &Path, repo_root: &Path) -> Result<()> {
    let l1_entries = match fs::read_dir(dir) {
        Ok(e) => e,
        Err(err) if err.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(err) => return Err(err.into()),
    };
    let canonical_root = repo_root.canonicalize().unwrap_or_else(|_| repo_root.to_path_buf());
    for l1 in l1_entries {
        let l1 = match l1 { Ok(e) => e, Err(_) => continue };
        if !l1.file_type().map(|t| t.is_dir()).unwrap_or(false) { continue; }
        let l2_entries = match fs::read_dir(l1.path()) { Ok(e) => e, Err(_) => continue };
        for l2 in l2_entries {
            let l2 = match l2 { Ok(e) => e, Err(_) => continue };
            if !l2.file_type().map(|t| t.is_dir()).unwrap_or(false) { continue; }
            let l3_entries = match fs::read_dir(l2.path()) { Ok(e) => e, Err(_) => continue };
            for l3 in l3_entries {
                let l3 = match l3 { Ok(e) => e, Err(_) => continue };
                if !l3.file_type().map(|t| t.is_dir()).unwrap_or(false) { continue; }
                let file_entries = match fs::read_dir(l3.path()) { Ok(e) => e, Err(_) => continue };
                for entry in file_entries {
                    let path = match entry { Ok(e) => e.path(), Err(_) => continue };
                    if path.extension().and_then(|e| e.to_str()) != Some("jsonl") { continue; }
                    if let Err(err) = scrape_codex_file(conn, &path, &canonical_root) {
                        log::warn!("metrics: skipping codex session file {}: {err}", path.display());
                    }
                }
            }
        }
    }
    Ok(())
}

/// Scrape one Codex session file.
///
/// Codex session IDs are prefixed with `codex:` to prevent any PK
/// collision with Claude session UUIDs (decision 77).
///
/// Token mapping (decision 77):
///   `last_token_usage.input_tokens`            → input_tokens
///   `last_token_usage.cached_input_tokens`      → cache_read_tokens
///   `last_token_usage.output_tokens`            → output_tokens (base)
///   `last_token_usage.reasoning_output_tokens`  → rolled into output_tokens
///   cache_creation_tokens                       → always 0 (no Codex equivalent)
fn scrape_codex_file(conn: &Connection, path: &Path, canonical_root: &Path) -> Result<()> {
    // Extract UUID from stem: "rollout-YYYY-MM-DDTHH-MM-SS-<UUID>" where
    // UUID is always the last 5 hyphen-delimited groups.
    let stem = match path.file_stem().and_then(|s| s.to_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => return Ok(()),
    };
    let parts: Vec<&str> = stem.split('-').collect();
    if parts.len() < 5 {
        log::debug!("metrics: codex stem has too few hyphen parts, skipping: {stem}");
        return Ok(());
    }
    let uuid = parts[parts.len() - 5..].join("-");
    let session_id = format!("codex:{uuid}");

    let file_len = fs::metadata(path)?.len();

    let existing_offset: Option<i64> = conn
        .query_row(
            "SELECT last_scanned_offset FROM session_metrics WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();

    let mut offset = existing_offset.unwrap_or(0).max(0) as u64;

    if existing_offset.is_some() && file_len == offset {
        return Ok(());
    }

    if file_len < offset {
        conn.execute(
            "UPDATE session_metrics SET \
                input_tokens = 0, output_tokens = 0, \
                cache_read_tokens = 0, cache_creation_tokens = 0, \
                last_scanned_offset = 0 \
             WHERE session_id = ?1",
            params![session_id],
        )?;
        conn.execute(
            "DELETE FROM session_turn_metrics WHERE session_id = ?1",
            params![session_id],
        )?;
        let _ = log_write(
            conn, CODEX_SCRAPER_ACTOR, "session_metrics", None, "scrape_reset",
            &format!("{} shrank ({} < {}); rescanning from 0", path.display(), file_len, offset),
        );
        offset = 0;
    }

    // CWD filter: only needed for sessions we haven't seen before.
    // On a resume (existing_offset.is_some()) the project already matched.
    if existing_offset.is_none() {
        if !codex_session_matches_project(path, canonical_root) {
            return Ok(());
        }
    }

    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut reader = BufReader::new(file);

    let mut d = Delta::default();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 { break; }
        if buf.last() != Some(&b'\n') { break; }
        d.consumed += n as u64;
        ingest_codex_line(&String::from_utf8_lossy(&buf), &mut d);
    }

    if d.consumed == 0 {
        return Ok(());
    }

    let new_offset = (offset + d.consumed) as i64;

    conn.execute(
        "INSERT INTO session_metrics \
            (session_id, agent, started_at, ended_at, \
             input_tokens, output_tokens, cache_read_tokens, \
             cache_creation_tokens, recall_calls, last_scanned_offset) \
         VALUES (?1, ?2, COALESCE(?3, CURRENT_TIMESTAMP), ?4, \
                 ?5, ?6, ?7, 0, 0, ?8) \
         ON CONFLICT(session_id) DO UPDATE SET \
            input_tokens          = input_tokens          + excluded.input_tokens, \
            output_tokens         = output_tokens         + excluded.output_tokens, \
            cache_read_tokens     = cache_read_tokens     + excluded.cache_read_tokens, \
            cache_creation_tokens = cache_creation_tokens + excluded.cache_creation_tokens, \
            ended_at              = COALESCE(excluded.ended_at, session_metrics.ended_at), \
            last_scanned_offset   = excluded.last_scanned_offset",
        params![
            session_id,
            CODEX_AGENT,
            d.min_ts,
            d.max_ts,
            d.input,
            d.output,
            d.cache_read,
            new_offset,
        ],
    )?;

    if !d.turns.is_empty() {
        let mut stmt = conn.prepare(
            "INSERT INTO session_turn_metrics \
                (session_id, ts, input_tokens, output_tokens, \
                 cache_read_tokens, cache_creation_tokens) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for turn in &d.turns {
            stmt.execute(params![
                session_id,
                turn.ts,
                turn.input,
                turn.output,
                turn.cache_read,
                turn.cache_creation,
            ])?;
        }
    }

    if d.parse_skips > 0 {
        let _ = log_write(
            conn, CODEX_SCRAPER_ACTOR, "session_metrics", None, "scrape_skip",
            &format!(
                "{}: {} unparseable/foreign line(s) skipped, {} token_count line(s) counted",
                path.display(), d.parse_skips, d.lines_with_usage
            ),
        );
    }

    Ok(())
}

/// Read the first line of a Codex session file and return true when
/// `session_meta.payload.cwd` matches `canonical_root`.
fn codex_session_matches_project(path: &Path, canonical_root: &Path) -> bool {
    let file = match File::open(path) {
        Ok(f) => f,
        Err(_) => return false,
    };
    let mut reader = BufReader::new(file);
    let mut line = String::new();
    if reader.read_line(&mut line).is_err() {
        return false;
    }
    let v: Value = match serde_json::from_str(line.trim()) {
        Ok(v) => v,
        Err(_) => return false,
    };
    if v.get("type").and_then(Value::as_str) != Some("session_meta") {
        return false;
    }
    let cwd = match v.get("payload").and_then(|p| p.get("cwd")).and_then(Value::as_str) {
        Some(c) => c,
        None => return false,
    };
    let cwd_path = std::path::Path::new(cwd);
    let canonical_cwd = cwd_path.canonicalize().unwrap_or_else(|_| cwd_path.to_path_buf());
    canonical_cwd == canonical_root
}

/// Parse one Codex `event_msg / token_count` line and fold its
/// `last_token_usage` into `d`. Any other event type is silently
/// ignored (not a skip). `reasoning_output_tokens` is rolled into
/// `output_tokens`; `cache_creation_tokens` is always 0.
fn ingest_codex_line(line: &str, d: &mut Delta) {
    let line = line.trim();
    if line.is_empty() { return; }

    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => { d.parse_skips += 1; return; }
    };

    let line_ts: Option<String> = v
        .get("timestamp")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(ts) = line_ts.clone() {
        if d.min_ts.as_ref().is_none_or(|m| ts < *m) { d.min_ts = Some(ts.clone()); }
        if d.max_ts.as_ref().is_none_or(|m| ts > *m) { d.max_ts = Some(ts); }
    }

    if v.get("type").and_then(Value::as_str) != Some("event_msg") { return; }
    let payload = match v.get("payload") { Some(p) => p, None => return };
    if payload.get("type").and_then(Value::as_str) != Some("token_count") { return; }
    let info = match payload.get("info") {
        Some(i) if !i.is_null() => i,
        _ => return,
    };
    let last = match info.get("last_token_usage") {
        Some(l) if !l.is_null() => l,
        _ => return,
    };

    let n = |key: &str| -> i64 { last.get(key).and_then(Value::as_i64).unwrap_or(0) };

    let input = n("input_tokens");
    let cache_read = n("cached_input_tokens");
    let output = n("output_tokens") + n("reasoning_output_tokens");

    d.input += input;
    d.output += output;
    d.cache_read += cache_read;
    d.lines_with_usage += 1;
    d.turns.push(TurnRecord {
        ts: line_ts,
        input,
        output,
        cache_read,
        cache_creation: 0,
    });
}

/// One usage-bearing assistant line, captured verbatim for the
/// per-turn (`session_turn_metrics`) curve. The session-level
/// aggregate still comes from the summed `Delta` fields below; this is
/// purely additional granularity, not a replacement.
#[derive(Default)]
struct TurnRecord {
    ts: Option<String>,
    input: i64,
    output: i64,
    cache_read: i64,
    cache_creation: i64,
}

/// Accumulated token deltas + timestamp bounds for a single scrape
/// pass over one file's newly appended bytes.
#[derive(Default)]
struct Delta {
    input: i64,
    output: i64,
    cache_read: i64,
    cache_creation: i64,
    min_ts: Option<String>,
    max_ts: Option<String>,
    consumed: u64,
    parse_skips: u64,
    lines_with_usage: u64,
    /// One entry per usage-bearing line, in transcript (consume) order.
    turns: Vec<TurnRecord>,
}

fn scrape_claude_file(conn: &Connection, path: &Path) -> Result<()> {
    // Claude Code names the file `<session-id>.jsonl`; the stem is the
    // stable per-session key (the in-line `sessionId` is expected to
    // match and is treated as a cross-check, not the key, so we can
    // resolve the resume offset before reading a single byte).
    let session_id = match path.file_stem().and_then(|s| s.to_str()) {
        Some(s) if !s.is_empty() => s.to_string(),
        _ => {
            log::debug!(
                "metrics: session file with no usable stem: {}",
                path.display()
            );
            return Ok(());
        }
    };

    let file_len = fs::metadata(path)?.len();

    let existing_offset: Option<i64> = conn
        .query_row(
            "SELECT last_scanned_offset FROM session_metrics WHERE session_id = ?1",
            params![session_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();

    let mut offset = existing_offset.unwrap_or(0).max(0) as u64;

    if existing_offset.is_some() && file_len == offset {
        // No bytes appended since the last pass — the cheap skip.
        return Ok(());
    }

    if file_len < offset {
        // Shorter than what we already consumed: the session file was
        // rotated or rewritten. Re-scan from zero, but first zero the
        // accumulators so the additive UPSERT doesn't double-count the
        // surviving prefix. recall_calls is left alone (task #30).
        conn.execute(
            "UPDATE session_metrics SET \
                input_tokens = 0, output_tokens = 0, \
                cache_read_tokens = 0, cache_creation_tokens = 0, \
                last_scanned_offset = 0 \
             WHERE session_id = ?1",
            params![session_id],
        )?;
        // The per-turn rows mirror the consumed prefix; a from-zero
        // re-scan would re-insert all of them, so drop this session's
        // turn history too. The session_metrics row is kept (zeroed)
        // because its recall_calls is owned by the reconciler.
        conn.execute(
            "DELETE FROM session_turn_metrics WHERE session_id = ?1",
            params![session_id],
        )?;
        let _ = log_write(
            conn,
            SCRAPER_ACTOR,
            "session_metrics",
            None,
            "scrape_reset",
            &format!(
                "{} shrank ({} < {}); rescanning from 0",
                path.display(),
                file_len,
                offset
            ),
        );
        offset = 0;
    }

    let mut file = File::open(path)?;
    file.seek(SeekFrom::Start(offset))?;
    let mut reader = BufReader::new(file);

    let mut d = Delta::default();
    let mut buf = Vec::new();
    loop {
        buf.clear();
        let n = reader.read_until(b'\n', &mut buf)?;
        if n == 0 {
            break; // EOF
        }
        if buf.last() != Some(&b'\n') {
            // Trailing partial line: the session is still being
            // written. Leave it unconsumed for the next pass.
            break;
        }
        d.consumed += n as u64;
        ingest_line(&String::from_utf8_lossy(&buf), &mut d);
    }

    if d.consumed == 0 {
        // Only a partial line (or nothing) past the offset. Don't
        // create or touch a row; don't advance the offset.
        return Ok(());
    }

    let new_offset = (offset + d.consumed) as i64;

    // Additive UPSERT. started_at is NOT NULL: fall back to
    // CURRENT_TIMESTAMP when the consumed lines carried no parseable
    // timestamp. On conflict, started_at is intentionally not updated
    // (keep the earliest) and ended_at only moves forward.
    conn.execute(
        "INSERT INTO session_metrics \
            (session_id, agent, started_at, ended_at, \
             input_tokens, output_tokens, cache_read_tokens, \
             cache_creation_tokens, recall_calls, last_scanned_offset) \
         VALUES (?1, ?2, COALESCE(?3, CURRENT_TIMESTAMP), ?4, \
                 ?5, ?6, ?7, ?8, 0, ?9) \
         ON CONFLICT(session_id) DO UPDATE SET \
            input_tokens          = input_tokens          + excluded.input_tokens, \
            output_tokens         = output_tokens         + excluded.output_tokens, \
            cache_read_tokens     = cache_read_tokens     + excluded.cache_read_tokens, \
            cache_creation_tokens = cache_creation_tokens + excluded.cache_creation_tokens, \
            ended_at              = COALESCE(excluded.ended_at, session_metrics.ended_at), \
            last_scanned_offset   = excluded.last_scanned_offset",
        params![
            session_id,
            CLAUDE_AGENT,
            d.min_ts,
            d.max_ts,
            d.input,
            d.output,
            d.cache_read,
            d.cache_creation,
            new_offset,
        ],
    )?;

    // Per-turn granularity (migration 0013). Done AFTER the
    // offset-advancing UPSERT on purpose: if these inserts fail the
    // offset has already moved, so the pass under-counts the curve by
    // a few turns rather than re-reading the bytes next pass and
    // double-inserting. Under-count is the conservative direction for
    // an advisory subsystem, matching this module's "never fatal".
    // Each consumed usage line maps to exactly one row (rows are never
    // updated), so plain INSERTs stay idempotent across resumes.
    if !d.turns.is_empty() {
        let mut stmt = conn.prepare(
            "INSERT INTO session_turn_metrics \
                (session_id, ts, input_tokens, output_tokens, \
                 cache_read_tokens, cache_creation_tokens) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
        )?;
        for turn in &d.turns {
            stmt.execute(params![
                session_id,
                turn.ts,
                turn.input,
                turn.output,
                turn.cache_read,
                turn.cache_creation,
            ])?;
        }
    }

    if d.parse_skips > 0 {
        // One summary row per file per pass — never one per bad line,
        // which could flood writes_log on a foreign-format file.
        let _ = log_write(
            conn,
            SCRAPER_ACTOR,
            "session_metrics",
            None,
            "scrape_skip",
            &format!(
                "{}: {} unparseable/foreign line(s) skipped, {} usage line(s) counted",
                path.display(),
                d.parse_skips,
                d.lines_with_usage
            ),
        );
    }

    Ok(())
}

/// Parse one JSONL line and fold its usage into `d`. Defensive: any
/// line that isn't a JSON object, isn't an assistant turn, or lacks a
/// `usage` block is counted as a skip and ignored. A line is never
/// allowed to abort the scrape.
fn ingest_line(line: &str, d: &mut Delta) {
    let line = line.trim();
    if line.is_empty() {
        return; // blank line: not a skip, just nothing
    }

    let v: Value = match serde_json::from_str(line) {
        Ok(v) => v,
        Err(_) => {
            d.parse_skips += 1;
            return;
        }
    };

    // Timestamp bounds track *every* line (user + assistant), so a
    // session with only user turns so far still gets a sane window.
    // The same value is later pinned onto this line's TurnRecord.
    let line_ts: Option<String> = v
        .get("timestamp")
        .and_then(Value::as_str)
        .map(str::to_string);
    if let Some(ts) = line_ts.clone() {
        if d.min_ts.as_ref().is_none_or(|m| ts < *m) {
            d.min_ts = Some(ts.clone());
        }
        if d.max_ts.as_ref().is_none_or(|m| ts > *m) {
            d.max_ts = Some(ts);
        }
    }

    // Usage lives on assistant turns under message.usage in the Claude
    // Code shape; tolerate a top-level `usage` too in case the shape
    // shifts. A line without usage isn't an error (user turns, tool
    // results, summaries) — only count it as a skip if it also failed
    // to look like a known event at all.
    let usage = v
        .get("message")
        .and_then(|m| m.get("usage"))
        .or_else(|| v.get("usage"));

    let usage = match usage {
        Some(u) => u,
        None => return, // non-usage event: silently ignored
    };

    let n = |key: &str| -> i64 {
        usage.get(key).and_then(Value::as_i64).unwrap_or(0)
    };

    let input = n("input_tokens");
    let output = n("output_tokens");
    let cache_read = n("cache_read_input_tokens");
    let cache_creation = n("cache_creation_input_tokens");

    d.input += input;
    d.output += output;
    d.cache_read += cache_read;
    d.cache_creation += cache_creation;
    d.lines_with_usage += 1;
    d.turns.push(TurnRecord {
        ts: line_ts,
        input,
        output,
        cache_read,
        cache_creation,
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    fn d_after(lines: &[&str]) -> Delta {
        let mut d = Delta::default();
        for l in lines {
            ingest_line(l, &mut d);
        }
        d
    }

    #[test]
    fn sums_usage_across_assistant_turns_in_claude_shape() {
        let d = d_after(&[
            r#"{"type":"user","timestamp":"2026-05-15T09:00:00.000Z","message":{"role":"user"}}"#,
            r#"{"type":"assistant","timestamp":"2026-05-15T09:00:05.000Z","message":{"role":"assistant","usage":{"input_tokens":120,"output_tokens":40,"cache_read_input_tokens":900,"cache_creation_input_tokens":50}}}"#,
            r#"{"type":"assistant","timestamp":"2026-05-15T09:01:00.000Z","message":{"role":"assistant","usage":{"input_tokens":200,"output_tokens":60}}}"#,
        ]);
        assert_eq!(d.input, 320);
        assert_eq!(d.output, 100);
        assert_eq!(d.cache_read, 900);
        assert_eq!(d.cache_creation, 50);
        assert_eq!(d.lines_with_usage, 2);
        assert_eq!(d.parse_skips, 0);
        assert_eq!(d.min_ts.as_deref(), Some("2026-05-15T09:00:00.000Z"));
        assert_eq!(d.max_ts.as_deref(), Some("2026-05-15T09:01:00.000Z"));
    }

    #[test]
    fn malformed_json_is_a_counted_skip_not_a_failure() {
        let d = d_after(&[
            r#"{not json"#,
            r#"{"type":"assistant","message":{"usage":{"input_tokens":10}}}"#,
            "",
        ]);
        assert_eq!(d.parse_skips, 1);
        assert_eq!(d.input, 10);
        assert_eq!(d.lines_with_usage, 1);
    }

    #[test]
    fn non_usage_events_are_ignored_without_skip_inflation() {
        let d = d_after(&[
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
            r#"{"type":"summary","summary":"..."}"#,
        ]);
        assert_eq!(d.parse_skips, 0);
        assert_eq!(d.lines_with_usage, 0);
        assert_eq!(d.input, 0);
    }

    #[test]
    fn tolerates_top_level_usage_shape_drift() {
        let d = d_after(&[
            r#"{"type":"assistant","usage":{"input_tokens":7,"output_tokens":3}}"#,
        ]);
        assert_eq!(d.input, 7);
        assert_eq!(d.output, 3);
    }

    #[test]
    fn captures_one_turn_record_per_usage_line_with_its_own_ts() {
        let d = d_after(&[
            r#"{"type":"user","timestamp":"2026-05-15T09:00:00.000Z","message":{"role":"user"}}"#,
            r#"{"type":"assistant","timestamp":"2026-05-15T09:00:05.000Z","message":{"role":"assistant","usage":{"input_tokens":120,"output_tokens":40}}}"#,
            r#"{"type":"assistant","timestamp":"2026-05-15T09:01:00.000Z","message":{"role":"assistant","usage":{"input_tokens":200,"output_tokens":60,"cache_read_input_tokens":10}}}"#,
        ]);
        // Per-turn rows: one per usage line, NOT one per transcript line.
        assert_eq!(d.turns.len(), 2);
        assert_eq!(d.turns[0].ts.as_deref(), Some("2026-05-15T09:00:05.000Z"));
        assert_eq!(d.turns[0].input, 120);
        assert_eq!(d.turns[0].output, 40);
        assert_eq!(d.turns[1].ts.as_deref(), Some("2026-05-15T09:01:00.000Z"));
        assert_eq!(d.turns[1].cache_read, 10);
        // Aggregate is still the sum — per-turn is purely additive.
        assert_eq!(d.input, 320);
        assert_eq!(d.output, 100);
    }

    #[test]
    fn non_usage_and_malformed_lines_produce_no_turn_records() {
        let d = d_after(&[
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#,
            r#"{not json"#,
            r#"{"type":"summary","summary":"..."}"#,
        ]);
        assert!(d.turns.is_empty());
    }

    fn write(path: &std::path::Path, lines: &[&str]) {
        let mut body = lines.join("\n");
        body.push('\n');
        std::fs::write(path, body).expect("write transcript");
    }

    #[test]
    fn scrape_persists_per_turn_rows_alongside_session_aggregate() {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        let ctx = crate::db::open_project(temp.path()).expect("open");
        let conn = &ctx.conn;

        let file = temp.path().join("sess-A.jsonl");
        write(
            &file,
            &[
                r#"{"type":"user","timestamp":"2026-05-15T10:00:00.000Z","message":{"role":"user"}}"#,
                r#"{"type":"assistant","timestamp":"2026-05-15T10:00:01.000Z","message":{"usage":{"input_tokens":100,"output_tokens":20}}}"#,
                r#"{"type":"assistant","timestamp":"2026-05-15T10:00:09.000Z","message":{"usage":{"input_tokens":300,"output_tokens":40}}}"#,
            ],
        );
        scrape_claude_file(conn, &file).expect("scrape");

        let (turns, sum_in, sum_out): (i64, i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(input_tokens),0), \
                        COALESCE(SUM(output_tokens),0) \
                 FROM session_turn_metrics WHERE session_id = 'sess-A'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("turn query");
        assert_eq!(turns, 2, "one row per usage line");
        assert_eq!(sum_in, 400);
        assert_eq!(sum_out, 60);

        // The aggregate session row must match the per-turn sum.
        let (agg_in, agg_out): (i64, i64) = conn
            .query_row(
                "SELECT input_tokens, output_tokens FROM session_metrics \
                 WHERE session_id = 'sess-A'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("session query");
        assert_eq!(agg_in, 400);
        assert_eq!(agg_out, 60);
    }

    #[test]
    fn rescanning_appended_bytes_does_not_double_insert_turns() {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        let ctx = crate::db::open_project(temp.path()).expect("open");
        let conn = &ctx.conn;
        let file = temp.path().join("sess-B.jsonl");

        write(
            &file,
            &[r#"{"type":"assistant","timestamp":"2026-05-15T11:00:00.000Z","message":{"usage":{"input_tokens":10,"output_tokens":5}}}"#],
        );
        scrape_claude_file(conn, &file).expect("scrape 1");

        // Append a second turn; the resume should only read the new line.
        write(
            &file,
            &[
                r#"{"type":"assistant","timestamp":"2026-05-15T11:00:00.000Z","message":{"usage":{"input_tokens":10,"output_tokens":5}}}"#,
                r#"{"type":"assistant","timestamp":"2026-05-15T11:05:00.000Z","message":{"usage":{"input_tokens":20,"output_tokens":7}}}"#,
            ],
        );
        scrape_claude_file(conn, &file).expect("scrape 2");

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM session_turn_metrics WHERE session_id = 'sess-B'",
                [],
                |r| r.get(0),
            )
            .expect("count");
        assert_eq!(count, 2, "incremental resume must not re-insert turn 1");
    }

    #[test]
    fn file_shrink_resets_turn_history_so_rescan_cannot_double_count() {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        let ctx = crate::db::open_project(temp.path()).expect("open");
        let conn = &ctx.conn;
        let file = temp.path().join("sess-C.jsonl");

        write(
            &file,
            &[
                r#"{"type":"assistant","timestamp":"2026-05-15T12:00:00.000Z","message":{"usage":{"input_tokens":111,"output_tokens":22}}}"#,
                r#"{"type":"assistant","timestamp":"2026-05-15T12:01:00.000Z","message":{"usage":{"input_tokens":222,"output_tokens":33}}}"#,
            ],
        );
        scrape_claude_file(conn, &file).expect("scrape 1");

        // Rewrite shorter (rotation): one different line, fewer bytes.
        write(
            &file,
            &[r#"{"type":"assistant","timestamp":"2026-05-15T13:00:00.000Z","message":{"usage":{"input_tokens":9,"output_tokens":1}}}"#],
        );
        scrape_claude_file(conn, &file).expect("scrape 2");

        let (count, sum_in): (i64, i64) = conn
            .query_row(
                "SELECT COUNT(*), COALESCE(SUM(input_tokens),0) \
                 FROM session_turn_metrics WHERE session_id = 'sess-C'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("turn query");
        assert_eq!(count, 1, "old turn rows must be cleared on shrink");
        assert_eq!(sum_in, 9, "only the rewritten content counts");
    }
}
