//! Code locator query path (M11 PR3, decision 107).
//!
//! [`locate`] is the read side of the sibling code index. It mirrors the
//! project recall fusion ([`crate::retrieval::recall`]) but over the
//! `code_chunks` / `code_embeddings` / `code_chunks_fts` tables in
//! `.memhub/code_index.sqlite`: FTS5 BM25 blended with brute-force cosine
//! similarity (hybrid mode only), min-max normalized and weighted by the
//! same `[retrieval.scoring]` knobs. There is no stale penalty — a code
//! chunk is either present or not, never "decayed".
//!
//! **Lazy freshness (decision 107).** A locate first runs [`super::refresh`]
//! so the index always reflects the working tree; the PR1 staleness engine
//! makes that stat-only (no read, no embed) when nothing changed, so a warm
//! index pays almost nothing. `memhub code index` is the explicit warm-up /
//! rebuild for callers that want to pay that cost up front.
//!
//! **Reranker off by default (PR3).** The bundled ms-marco cross-encoder is
//! NL-trained; its fit on code is unproven until PR5 measures it. So
//! `--rerank` gates it and there is no min-score floor here — when on, it
//! only reorders the candidate pool. The default fusion path never loads
//! the cross-encoder.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use rusqlite::{Connection, params};

use crate::Result;
use crate::config::{ProjectConfig, RetrievalMode, RetrievalScoringConfig};
use crate::db;
use crate::retrieval::embeddings::{EMBEDDING_DIMENSION, EMBEDDING_MODEL_NAME, embed_one};

use super::{code_index_db_path, open_code_index, refresh};

/// FTS candidates pulled before blending. Generous so the vector side can
/// re-rank a broad keyword pool; the final list is truncated to `limit`.
const FTS_CANDIDATE_LIMIT: i64 = 100;

/// Default result count when the caller passes 0.
pub const DEFAULT_LOCATE_LIMIT: usize = 10;

/// Snippet caps: a locator returns a breadcrumb, never full code. The body
/// is clipped to the first [`SNIPPET_MAX_LINES`] lines of the chunk and at
/// most [`SNIPPET_MAX_CHARS`] characters, with a trailing `…` when clipped.
const SNIPPET_MAX_LINES: usize = 6;
const SNIPPET_MAX_CHARS: usize = 400;

/// Caller-supplied locate request.
#[derive(Debug, Clone)]
pub struct LocateOptions {
    pub query: String,
    /// 0 means "use [`DEFAULT_LOCATE_LIMIT`]".
    pub limit: usize,
    /// Run the bundled cross-encoder over the candidate pool. Off by
    /// default in PR3; ignored in fts mode (no vectors to draw a pool from
    /// — FTS order stands).
    pub use_reranker: bool,
}

/// One ranked locator hit.
#[derive(Debug, Clone)]
pub struct LocateHit {
    pub rank: usize,
    /// Repo-relative, forward-slashed path.
    pub path: String,
    /// 1-indexed inclusive line range of the chunk.
    pub start_line: usize,
    pub end_line: usize,
    /// Symbol name when the chunk is symbol-aware (`None` for line windows).
    pub symbol: Option<String>,
    /// Chunk kind tag (`function`, `struct`, `line-window`, …).
    pub kind: String,
    /// Blended fusion score (or the rerank logit's rank position when
    /// reranking reordered the pool — `score` stays the fusion score so the
    /// fusion signal is still visible; `rerank_score` carries the logit).
    pub score: f64,
    pub fts_score: f64,
    pub vector_score: f64,
    /// Cross-encoder relevance logit when `--rerank` ran, else `None`.
    pub rerank_score: Option<f32>,
    /// A short, clipped excerpt of the chunk body read from disk.
    pub snippet: String,
}

/// Locate response bundle.
#[derive(Debug, Clone)]
pub struct LocateResponse {
    pub query: String,
    pub mode: RetrievalMode,
    pub results: Vec<LocateHit>,
    /// Distinct chunks that matched before truncation to `limit`.
    pub candidate_count: usize,
    pub returned_count: usize,
    /// Whether the cross-encoder actually ran this call.
    pub reranked: bool,
    /// Files / chunks in the index after the pre-query refresh.
    pub files_total: usize,
    pub chunks_total: usize,
    /// Indexed `HEAD` after the refresh, if resolvable.
    pub head: Option<String>,
    pub elapsed_ms: u128,
}

/// Run a code locate: refresh the index to the working tree, then blend
/// FTS + vector candidates and return the ranked breadcrumbs.
pub fn locate(start: &Path, options: LocateOptions) -> Result<LocateResponse> {
    let started = Instant::now();

    // Lazy freshness: bring the index in line with the tree first. Cheap
    // (stat-only, no embed) when nothing changed.
    let summary = refresh(start)?;

    // Resolve repo root + config the same decoupled way refresh does — no
    // open_project, so the code index stays independent of project.sqlite.
    let paths = db::discover_paths(start)?;
    let repo_root = paths.repo_root.clone();
    let config = if paths.config_path.exists() {
        ProjectConfig::load(&paths.config_path)?
    } else {
        let repo_name = repo_root
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("memhub");
        ProjectConfig::default_for_repo_name(repo_name)
    };
    let mode = config.retrieval.mode;
    let scoring = &config.retrieval.scoring;

    let limit = if options.limit == 0 {
        DEFAULT_LOCATE_LIMIT
    } else {
        options.limit
    };

    let db_path = code_index_db_path(&repo_root);
    let conn = open_code_index(&db_path)?;

    let mut candidates = gather_candidates(&conn, &options.query, mode)?;
    let candidate_count = candidates.len();

    // Blend + sort by descending fusion score.
    score_candidates(&mut candidates, scoring);
    candidates.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.chunk_id.cmp(&b.chunk_id))
    });

    // Optional rerank over the top pool. Only meaningful in hybrid mode —
    // in fts mode there is no embed pool worth reordering, so FTS rank
    // stands (mirrors recall's reranker-is-hybrid-only rule).
    let mut reranked = false;
    if options.use_reranker && mode == RetrievalMode::Hybrid && !candidates.is_empty() {
        let pool = config.retrieval.rerank_candidate_pool.max(limit);
        let pool_len = pool.min(candidates.len());
        let pool_texts = fetch_embed_texts(&conn, &candidates[..pool_len])?;
        let doc_refs: Vec<String> = pool_texts.iter().map(|(_, t)| t.clone()).collect();
        let ranked = crate::retrieval::rerank::rerank(&options.query, &doc_refs)?;
        // ranked is (pool_index, logit) sorted descending. Rebuild the pool
        // in that order, stamping the logit; the tail past the pool keeps
        // its fusion order behind the reranked head.
        let mut new_head: Vec<Candidate> = Vec::with_capacity(pool_len);
        for (pool_idx, logit) in &ranked {
            let mut c = candidates[*pool_idx].clone();
            c.rerank_score = Some(*logit);
            new_head.push(c);
        }
        let tail = candidates.split_off(pool_len);
        candidates = new_head;
        candidates.extend(tail);
        reranked = true;
    }

    candidates.truncate(limit);

    let mut line_cache: HashMap<String, Option<Vec<String>>> = HashMap::new();
    let mut results = Vec::with_capacity(candidates.len());
    for (idx, c) in candidates.iter().enumerate() {
        let meta = fetch_chunk_meta(&conn, c.chunk_id)?;
        let Some(meta) = meta else { continue };
        let snippet = read_snippet(
            &repo_root,
            &meta.path,
            meta.start_line,
            meta.end_line,
            &mut line_cache,
        );
        results.push(LocateHit {
            rank: idx + 1,
            path: meta.path,
            start_line: meta.start_line,
            end_line: meta.end_line,
            symbol: meta.symbol,
            kind: meta.kind,
            score: c.score,
            fts_score: c.fts_score,
            vector_score: c.vector_score,
            rerank_score: c.rerank_score,
            snippet,
        });
    }

    let returned_count = results.len();
    Ok(LocateResponse {
        query: options.query,
        mode,
        results,
        candidate_count,
        returned_count,
        reranked,
        files_total: summary.files_total,
        chunks_total: summary.chunks_total,
        head: summary.head,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

#[derive(Debug, Clone)]
struct Candidate {
    chunk_id: i64,
    /// Raw BM25 (lower is better); `None` if the chunk only matched on the
    /// vector side.
    fts_raw: Option<f64>,
    /// Cosine similarity in [-1, 1]; `None` if FTS-only.
    cosine: Option<f64>,
    fts_score: f64,
    vector_score: f64,
    score: f64,
    rerank_score: Option<f32>,
    /// Repo-relative forward-slashed path of the source file this chunk
    /// belongs to. Used by `score_candidates` to apply the test-path penalty.
    path: String,
}

/// Multiplicative penalty applied to the blended fusion score for chunks
/// under top-level test / bench / example directories. Down-weights
/// non-implementation files so they do not out-rank implementation files
/// in `locate` results (task 85). FTS and vector subscores are left
/// untouched — they remain the honest per-component signals.
const TEST_PATH_PENALTY: f64 = 0.90;

/// Returns `true` when `path` is inside a top-level test / bench / example
/// directory (`tests/`, `benches/`, or `examples/`). Paths in the index
/// are already forward-slashed repo-relative strings.
fn is_test_path(path: &str) -> bool {
    path.starts_with("tests/") || path.starts_with("benches/") || path.starts_with("examples/")
}

/// Gather the union of FTS and (hybrid only) vector matches keyed by chunk.
fn gather_candidates(
    conn: &Connection,
    query: &str,
    mode: RetrievalMode,
) -> Result<Vec<Candidate>> {
    let mut map: HashMap<i64, Candidate> = HashMap::new();

    if let Some(match_expr) = build_fts_match(query) {
        let mut stmt = conn.prepare(
            "SELECT c.id, bm25(code_chunks_fts) AS score, f.path \
             FROM code_chunks_fts \
             JOIN code_chunks c ON c.id = code_chunks_fts.rowid \
             JOIN indexed_files f ON f.id = c.file_id \
             WHERE code_chunks_fts MATCH ?1 \
             ORDER BY score ASC \
             LIMIT ?2",
        )?;
        let rows = stmt.query_map(params![match_expr, FTS_CANDIDATE_LIMIT], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?, row.get::<_, String>(2)?))
        })?;
        for row in rows {
            let (chunk_id, bm25, path) = row?;
            map.entry(chunk_id)
                .or_insert_with(|| new_candidate(chunk_id, path))
                .fts_raw = Some(bm25);
        }
    }

    if mode == RetrievalMode::Hybrid {
        let query_vec = embed_one(query)?;
        if query_vec.len() == EMBEDDING_DIMENSION {
            let mut stmt = conn.prepare(
                "SELECT e.chunk_id, e.vector, f.path \
                 FROM code_embeddings e \
                 JOIN code_chunks c ON c.id = e.chunk_id \
                 JOIN indexed_files f ON f.id = c.file_id \
                 WHERE e.model_name = ?1 AND e.dimension = ?2",
            )?;
            let rows = stmt.query_map(
                params![EMBEDDING_MODEL_NAME, EMBEDDING_DIMENSION as i64],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, Vec<u8>>(1)?,
                        row.get::<_, String>(2)?,
                    ))
                },
            )?;
            for row in rows {
                let (chunk_id, blob, path) = row?;
                if blob.len() != EMBEDDING_DIMENSION * 4 {
                    continue;
                }
                let cosine = cosine_similarity(&query_vec, &bytes_to_vector(&blob));
                map.entry(chunk_id)
                    .or_insert_with(|| new_candidate(chunk_id, path))
                    .cosine = Some(cosine);
            }
        }
    }

    Ok(map.into_values().collect())
}

fn new_candidate(chunk_id: i64, path: String) -> Candidate {
    Candidate {
        chunk_id,
        fts_raw: None,
        cosine: None,
        fts_score: 0.0,
        vector_score: 0.0,
        score: 0.0,
        rerank_score: None,
        path,
    }
}

/// Min-max normalize FTS across the candidate set and blend with the
/// clamped cosine, weighted by config. No stale penalty (code chunks don't
/// decay). Mirrors [`crate::retrieval::recall`]'s `score`.
fn score_candidates(candidates: &mut [Candidate], scoring: &RetrievalScoringConfig) {
    let (fts_min, fts_max) =
        candidates
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |acc, c| {
                match c.fts_raw {
                    Some(raw) => {
                        let pos = -raw; // BM25 lower-is-better → invert.
                        (acc.0.min(pos), acc.1.max(pos))
                    }
                    None => acc,
                }
            });

    for c in candidates.iter_mut() {
        c.fts_score = match c.fts_raw {
            Some(raw) => normalize_fts(-raw, fts_min, fts_max),
            None => 0.0,
        };
        c.vector_score = c.cosine.unwrap_or(0.0).clamp(0.0, 1.0);
        c.score = scoring.fts_weight * c.fts_score + scoring.vector_weight * c.vector_score;
        if is_test_path(&c.path) {
            c.score *= TEST_PATH_PENALTY;
        }
    }
}

fn normalize_fts(value: f64, min: f64, max: f64) -> f64 {
    if !value.is_finite() || !min.is_finite() || !max.is_finite() {
        return 0.0;
    }
    if (max - min).abs() < f64::EPSILON {
        return 1.0;
    }
    ((value - min) / (max - min)).clamp(0.0, 1.0)
}

/// Tokenize a free-text query into a quoted FTS5 `AND` of terms. Returns
/// `None` when the query has no usable tokens (so the caller skips FTS).
fn build_fts_match(query: &str) -> Option<String> {
    let tokens: Vec<String> = query
        .split_whitespace()
        .map(|token| token.trim_matches(|c: char| matches!(c, '"' | '\'' | ',' | '.' | ':' | ';')))
        .filter(|t| !t.is_empty())
        .map(|token| format!("\"{}\"", token.replace('"', "\"\"")))
        .collect();
    if tokens.is_empty() {
        None
    } else {
        Some(tokens.join(" AND "))
    }
}

/// Pull `(chunk_id, embed_text)` for a slice of candidates, preserving
/// their order so the reranker's returned indices line up.
fn fetch_embed_texts(conn: &Connection, pool: &[Candidate]) -> Result<Vec<(i64, String)>> {
    let mut stmt = conn.prepare("SELECT embed_text FROM code_chunks WHERE id = ?1")?;
    let mut out = Vec::with_capacity(pool.len());
    for c in pool {
        let text: String = stmt.query_row(params![c.chunk_id], |row| row.get(0))?;
        out.push((c.chunk_id, text));
    }
    Ok(out)
}

struct ChunkMeta {
    path: String,
    start_line: usize,
    end_line: usize,
    symbol: Option<String>,
    kind: String,
}

fn fetch_chunk_meta(conn: &Connection, chunk_id: i64) -> Result<Option<ChunkMeta>> {
    let mut stmt = conn.prepare(
        "SELECT f.path, c.start_line, c.end_line, c.symbol, c.kind \
         FROM code_chunks c JOIN indexed_files f ON f.id = c.file_id \
         WHERE c.id = ?1",
    )?;
    let row = stmt.query_map(params![chunk_id], |row| {
        Ok(ChunkMeta {
            path: row.get(0)?,
            start_line: row.get::<_, i64>(1)? as usize,
            end_line: row.get::<_, i64>(2)? as usize,
            symbol: row.get(3)?,
            kind: row.get(4)?,
        })
    })?;
    Ok(row.into_iter().next().transpose()?)
}

/// Read a clipped excerpt for `path` at the chunk's 1-indexed line range.
/// File contents are cached across hits in the same locate; a `None` cache
/// entry records an unreadable/absent file so it is not re-read.
fn read_snippet(
    repo_root: &Path,
    path: &str,
    start_line: usize,
    end_line: usize,
    cache: &mut HashMap<String, Option<Vec<String>>>,
) -> String {
    let lines = cache.entry(path.to_string()).or_insert_with(|| {
        std::fs::read_to_string(repo_root.join(path))
            .ok()
            .map(|s| s.lines().map(|l| l.to_string()).collect())
    });
    let Some(lines) = lines else {
        return String::new();
    };
    if start_line == 0 || start_line > lines.len() {
        return String::new();
    }
    // start_line/end_line are 1-indexed inclusive; clamp end to the file.
    let from = start_line - 1;
    let to = end_line.min(lines.len());
    let mut out = String::new();
    let mut truncated = false;
    for (i, line) in lines[from..to].iter().enumerate() {
        if i >= SNIPPET_MAX_LINES {
            truncated = true;
            break;
        }
        if !out.is_empty() {
            out.push('\n');
        }
        out.push_str(line);
        if out.len() >= SNIPPET_MAX_CHARS {
            truncated = true;
            break;
        }
    }
    if out.len() > SNIPPET_MAX_CHARS {
        // Clip on a char boundary, not a byte index.
        let mut end = SNIPPET_MAX_CHARS;
        while end > 0 && !out.is_char_boundary(end) {
            end -= 1;
        }
        out.truncate(end);
        truncated = true;
    }
    if truncated {
        out.push('…');
    }
    out
}

fn bytes_to_vector(blob: &[u8]) -> Vec<f32> {
    blob.chunks_exact(4)
        .map(|b| f32::from_le_bytes([b[0], b[1], b[2], b[3]]))
        .collect()
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut dot = 0.0f64;
    let mut na = 0.0f64;
    let mut nb = 0.0f64;
    for (x, y) in a.iter().zip(b.iter()) {
        let (xf, yf) = (*x as f64, *y as f64);
        dot += xf * yf;
        na += xf * xf;
        nb += yf * yf;
    }
    if na <= f64::EPSILON || nb <= f64::EPSILON {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn is_test_path_classifies_top_level_dirs() {
        assert!(is_test_path("tests/foo.rs"));
        assert!(is_test_path("benches/bench.rs"));
        assert!(is_test_path("examples/ex.rs"));
        assert!(!is_test_path("src/foo.rs"));
        assert!(!is_test_path("src/tests/foo.rs"));
        assert!(!is_test_path(""));
    }

    #[test]
    fn build_fts_match_quotes_and_ands_tokens() {
        assert_eq!(
            build_fts_match("parse manifest"),
            Some("\"parse\" AND \"manifest\"".to_string())
        );
        assert_eq!(build_fts_match("   "), None);
        assert_eq!(build_fts_match(",.;"), None);
    }

    #[test]
    fn normalize_fts_single_hit_is_full_strength() {
        assert_eq!(normalize_fts(-5.0, -5.0, -5.0), 1.0);
    }

    #[test]
    fn normalize_fts_scales_within_range() {
        assert_eq!(normalize_fts(0.0, -10.0, 10.0), 0.5);
    }

    #[test]
    fn cosine_identical_vectors_is_one() {
        let v = vec![1.0, 2.0, 3.0];
        assert!((cosine_similarity(&v, &v) - 1.0).abs() < 1e-9);
    }
}
