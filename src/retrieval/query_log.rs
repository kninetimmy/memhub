//! Opt-in machine-local JSONL log of agent-facing recalls (`[retrieval]
//! log_queries`, off by default). One JSON line per CLI/MCP recall, appended
//! to a plain file in the project's memhub directory -- never the database,
//! so Drive sync, export and the machine-global store never carry it. Its
//! purpose is mining real agent queries for golden-set probes.

use std::fs::OpenOptions;
use std::io::Write;
use std::path::Path;

use rusqlite::Connection;
use serde_json::json;

use super::recall::{RecallResponse, RecallSurface};
use crate::Result;

/// File name inside the memhub directory (next to `project.sqlite`).
pub const QUERY_LOG_FILENAME: &str = "recall_queries.jsonl";

/// What the caller asked for, as resolved by `recall()`.
pub struct LoggedRequest<'a> {
    pub surface: RecallSurface,
    pub variants: &'a [String],
    pub source_types: &'a [&'a str],
    pub max_results: usize,
}

/// Append one line for this recall. A failure never reaches the caller: it
/// is reported on stderr only (stdout carries the recall output, and MCP's
/// JSON-RPC stream).
pub fn log_recall(
    conn: &Connection,
    memhub_dir: &Path,
    request: &LoggedRequest,
    response: &RecallResponse,
) {
    if let Err(err) = try_log_recall(conn, memhub_dir, request, response) {
        eprintln!("memhub: recall query log not written: {err}");
    }
}

fn try_log_recall(
    conn: &Connection,
    memhub_dir: &Path,
    request: &LoggedRequest,
    response: &RecallResponse,
) -> Result<()> {
    let ts: String = conn.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now')", [], |row| {
        row.get(0)
    })?;
    let results: Vec<_> = response
        .results
        .iter()
        .map(|h| {
            json!({
                "source_type": h.source_type,
                "source_id": h.source_id,
                "scope": h.scope,
                "title": h.title,
                "rerank_score": h.rerank_score,
            })
        })
        .collect();
    let warnings: Vec<_> = response.warnings.iter().map(|w| &w.kind).collect();
    let mut line = json!({
        "ts": ts,
        "surface": request.surface.as_str(),
        "query": response.query,
        "variants": request.variants,
        "source_types": request.source_types,
        "max_results": request.max_results,
        "results": results,
        "warnings": warnings,
    })
    .to_string();
    line.push('\n');

    // shortcut: the log is never rotated or size-capped, so it grows with
    // every agent recall; add rotation if a real install's file gets large.
    // One write_all of the whole line keeps O_APPEND writers from
    // interleaving partial lines.
    let mut file = OpenOptions::new()
        .create(true)
        .append(true)
        .open(memhub_dir.join(QUERY_LOG_FILENAME))?;
    file.write_all(line.as_bytes())?;
    Ok(())
}
