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

/// Opportunistic entry point. Gated by the `metrics.enabled` master
/// switch and the `metrics.session_accounting` sub-switch; both off by
/// default, so this is a zero-cost early return on a non-opted-in
/// install. Errors are swallowed (logged, never propagated) — losing a
/// metrics scrape must never fail an otherwise-successful command.
pub fn scrape_if_enabled(conn: &Connection, cfg: &MetricsConfig) {
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

    // Codex transcript format is unconfirmed (decision 74: deferred).
    // The config field exists and the scraper no-ops on it so a user
    // who sets it doesn't silently believe Codex sessions are counted.
    if !cfg.codex_transcripts_dir.is_empty() {
        log::debug!(
            "metrics: codex_transcripts_dir is set but Codex scraping \
             is deferred (decision 74); skipping {}",
            cfg.codex_transcripts_dir
        );
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
    if let Some(ts) = v.get("timestamp").and_then(Value::as_str) {
        let ts = ts.to_string();
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

    d.input += n("input_tokens");
    d.output += n("output_tokens");
    d.cache_read += n("cache_read_input_tokens");
    d.cache_creation += n("cache_creation_input_tokens");
    d.lines_with_usage += 1;
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
}
