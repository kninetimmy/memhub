use std::path::Path;

use rusqlite::{Connection, params};

use crate::Result;
use crate::db;
use crate::errors::MemhubError;
use crate::models::{DecisionSearchHit, FileHistoryHit, SearchResponse, SearchResult};

pub fn run(start: &Path, query: &str, limit: usize) -> Result<SearchResponse> {
    let query = query.trim();
    if query.is_empty() {
        return Err(MemhubError::InvalidInput(
            "search query cannot be empty".to_string(),
        ));
    }

    if limit == 0 {
        return Err(MemhubError::InvalidInput(
            "search limit must be greater than zero".to_string(),
        ));
    }

    let ctx = db::open_project(start)?;
    sync_decision_chunks(&ctx.conn)?;

    if let Some(path_query) = strip_file_prefix(query) {
        let normalized = normalize_path(path_query);
        let results = search_file_history(&ctx.conn, &normalized, limit)?;
        return Ok(SearchResponse {
            matcher: "exact:file-history".to_string(),
            query: normalized,
            results: results.into_iter().map(SearchResult::FileHistory).collect(),
        });
    }

    if looks_like_path(query) && file_exists(&ctx.conn, &normalize_path(query))? {
        let normalized = normalize_path(query);
        let results = search_file_history(&ctx.conn, &normalized, limit)?;
        return Ok(SearchResponse {
            matcher: "exact:file-history".to_string(),
            query: normalized,
            results: results.into_iter().map(SearchResult::FileHistory).collect(),
        });
    }

    let decision_query = strip_decision_prefix(query).unwrap_or(query);
    let results = search_decisions(&ctx.conn, decision_query, limit)?;
    Ok(SearchResponse {
        matcher: if strip_decision_prefix(query).is_some() {
            "fts:decision".to_string()
        } else {
            "fts:decision-fallback".to_string()
        },
        query: decision_query.to_string(),
        results: results.into_iter().map(SearchResult::Decision).collect(),
    })
}

pub fn sync_decision_chunks(conn: &Connection) -> Result<()> {
    conn.execute(
        "INSERT INTO chunks(project_id, source_type, source_id, text)
         SELECT
             d.project_id,
             'decision',
             CAST(d.id AS TEXT),
             d.title || char(10) || d.rationale
         FROM decisions d
         WHERE 1 = 1
         ON CONFLICT(project_id, source_type, source_id) DO UPDATE SET
             text = excluded.text",
        [],
    )?;

    Ok(())
}

fn search_file_history(conn: &Connection, path: &str, limit: usize) -> Result<Vec<FileHistoryHit>> {
    let mut stmt = conn.prepare(
        "SELECT
             f.path,
             c.sha,
             c.author,
             c.committed_at,
             c.message,
             cf.change_type
         FROM files f
         JOIN commit_files cf ON cf.file_id = f.id
         JOIN commits c ON c.sha = cf.commit_sha
         WHERE f.project_id = 1 AND f.path = ?1
         ORDER BY c.committed_at DESC
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(params![path, limit as i64], |row| {
        Ok(FileHistoryHit {
            path: row.get(0)?,
            commit_sha: row.get(1)?,
            author: row.get(2)?,
            committed_at: row.get(3)?,
            message: row.get(4)?,
            change_type: row.get(5)?,
        })
    })?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn search_decisions(
    conn: &Connection,
    query: &str,
    limit: usize,
) -> Result<Vec<DecisionSearchHit>> {
    let match_query = build_fts_query(query)?;
    let mut stmt = conn.prepare(
        "SELECT
             d.id,
             d.title,
             d.rationale,
             d.decided_at,
             bm25(chunk_fts) AS score
         FROM chunk_fts
         JOIN chunks ch ON ch.id = chunk_fts.rowid
         JOIN decisions d
             ON d.id = CAST(ch.source_id AS INTEGER)
            AND d.project_id = ch.project_id
         WHERE chunk_fts MATCH ?1
           AND ch.source_type = 'decision'
         ORDER BY score ASC, d.decided_at DESC
         LIMIT ?2",
    )?;

    let rows = stmt.query_map(params![match_query, limit as i64], |row| {
        Ok(DecisionSearchHit {
            decision_id: row.get(0)?,
            title: row.get(1)?,
            rationale: row.get(2)?,
            decided_at: row.get(3)?,
            score: row.get(4)?,
        })
    })?;

    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

fn build_fts_query(query: &str) -> Result<String> {
    let tokens = query
        .split_whitespace()
        .map(|token| token.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | '.' | ':' | ';')))
        .filter(|token| !token.is_empty())
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect::<Vec<_>>();

    if tokens.is_empty() {
        return Err(MemhubError::InvalidInput(
            "search query must include at least one searchable token".to_string(),
        ));
    }

    Ok(tokens.join(" AND "))
}

fn strip_file_prefix(query: &str) -> Option<&str> {
    query.strip_prefix("file:").map(str::trim)
}

fn strip_decision_prefix(query: &str) -> Option<&str> {
    [
        "find decisions about ",
        "decisions about ",
        "decision about ",
        "decision: ",
        "decisions: ",
        "decision ",
        "decisions ",
    ]
    .into_iter()
    .find_map(|prefix| query.strip_prefix(prefix))
    .map(str::trim)
}

fn file_exists(conn: &Connection, path: &str) -> Result<bool> {
    conn.query_row(
        "SELECT EXISTS(
             SELECT 1 FROM files
             WHERE project_id = 1 AND path = ?1
         )",
        [path],
        |row| row.get(0),
    )
    .map_err(Into::into)
}

fn looks_like_path(query: &str) -> bool {
    query.contains('/') || query.contains('\\') || query.contains('.')
}

fn normalize_path(path: &str) -> String {
    path.trim().replace('\\', "/")
}
