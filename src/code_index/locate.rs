//! Code locator query path (M11 PR3, decision 107).
//!
//! [`locate`] is the read side of the sibling code index. It mirrors the
//! project recall fusion ([`crate::retrieval::recall`]) but over the
//! `code_chunks` / `code_embeddings` / `code_chunks_fts` tables in
//! `.memhub/code_index.sqlite`: FTS5 BM25 blended with brute-force cosine
//! similarity (hybrid mode only), min-max normalized and weighted by its
//! own `[code_index]` knobs — split from recall's `[retrieval.scoring]`
//! (R11, issue #73) so tuning one no longer silently retunes the other.
//! There is no stale penalty — a code chunk is either present or not,
//! never "decayed".
//!
//! **Lazy freshness (decision 107).** A locate first runs [`super::refresh`]
//! so the index always reflects the working tree; the PR1 staleness engine
//! makes that stat-only (no read, no embed) when nothing changed, so a warm
//! index pays almost nothing. `memhub code index` is the explicit warm-up /
//! rebuild for callers that want to pay that cost up front. `--no-refresh`
//! (issue #67) opts out of that pass entirely — no `git ls-files`, no stat,
//! no `git rev-parse HEAD` — for callers making tight repeat calls against
//! a warm index who accept stale-by-choice results in exchange for the
//! lowest possible latency.
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
use crate::config::{CodeIndexConfig, ProjectConfig, RetrievalMode};
use crate::db;
use crate::retrieval::embeddings::{EMBEDDING_DIMENSION, EMBEDDING_MODEL_NAME, embed_one};
use crate::retrieval::util::{build_fts_match, bytes_to_vector, cosine_similarity, normalize_fts};

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
    /// Skip the pre-query [`super::refresh`] (issue #67): no `git
    /// ls-files`, no per-file stat, no `git rev-parse HEAD`. Queries the
    /// index exactly as it last stood. Stale-by-choice — an explicit
    /// opt-in for tight repeat-locate loops on a warm index; the default
    /// (`false`) keeps the lazy-freshness guarantee described on
    /// [`locate`].
    pub no_refresh: bool,
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
    /// Count of `code_embeddings` rows in scope for this query whose
    /// stored vector blob is the wrong byte length for
    /// `EMBEDDING_DIMENSION` -- a corrupt or truncated write. These chunks
    /// are silently degraded to FTS-only for this call (there's nothing
    /// to compute cosine similarity against); this count is the caller's
    /// only signal that degradation happened, since the code index has
    /// no `stale_embeddings`-style warning list of its own. Always 0 in
    /// `fts` mode (the vector path never runs).
    pub corrupt_embeddings: usize,
    pub elapsed_ms: u128,
}

/// Run a code locate: refresh the index to the working tree, then blend
/// FTS + vector candidates and return the ranked breadcrumbs.
pub fn locate(start: &Path, options: LocateOptions) -> Result<LocateResponse> {
    let started = Instant::now();

    // Lazy freshness: bring the index in line with the tree first. Cheap
    // (stat-only, no embed) when nothing changed. `--no-refresh` skips this
    // entirely — no `git ls-files`, no per-file stat, no `git rev-parse
    // HEAD` — trading freshness for the lowest possible warm latency
    // (issue #67). `files_total`/`chunks_total`/`head` are then read
    // straight off the index below instead of off this summary.
    let summary = if options.no_refresh {
        None
    } else {
        Some(refresh(start)?)
    };

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
    let scoring = &config.code_index;

    let limit = if options.limit == 0 {
        DEFAULT_LOCATE_LIMIT
    } else {
        options.limit
    };

    let db_path = code_index_db_path(&repo_root);
    let conn = open_code_index(&db_path)?;

    // With a refresh, its summary already carries the post-refresh counts
    // + resolved HEAD. Without one, read the same three values straight
    // off the index (mirrors `status()`) rather than trusting a summary
    // that never ran — `head` here is the last-*indexed* HEAD, not a fresh
    // `git rev-parse`, since `--no-refresh` promises no git calls.
    let (files_total, chunks_total, head) = match &summary {
        Some(s) => (s.files_total, s.chunks_total, s.head.clone()),
        None => index_snapshot(&conn)?,
    };

    let (mut candidates, corrupt_embeddings) = gather_candidates(&conn, &options.query, mode)?;
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
        files_total,
        chunks_total,
        head,
        corrupt_embeddings,
        elapsed_ms: started.elapsed().as_millis(),
    })
}

/// Read `files_total` / `chunks_total` / last-indexed `head` straight off
/// the index DB without touching the working tree or spawning `git`. The
/// `--no-refresh` counterpart to the counts a [`super::RefreshSummary`]
/// carries; mirrors [`super::status`]'s snapshot query.
fn index_snapshot(conn: &Connection) -> Result<(usize, usize, Option<String>)> {
    let files_total = conn.query_row("SELECT COUNT(*) FROM indexed_files", [], |r| {
        r.get::<_, i64>(0)
    })? as usize;
    let chunks_total = conn.query_row("SELECT COUNT(*) FROM code_chunks", [], |r| {
        r.get::<_, i64>(0)
    })? as usize;
    let head: Option<String> = conn
        .query_row(
            "SELECT value FROM index_meta WHERE key = 'last_head'",
            [],
            |r| r.get(0),
        )
        .ok();
    Ok((files_total, chunks_total, head))
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

/// Returns `true` when `path` is inside a top-level test / bench / example
/// directory (`tests/`, `benches/`, or `examples/`). Paths in the index
/// are already forward-slashed repo-relative strings.
fn is_test_path(path: &str) -> bool {
    path.starts_with("tests/") || path.starts_with("benches/") || path.starts_with("examples/")
}

/// Gather the union of FTS and (hybrid only) vector matches keyed by chunk.
/// Returns the candidates plus a count of length-mismatched embedding
/// blobs found while scanning `code_embeddings` (see [`LocateResponse::
/// corrupt_embeddings`]).
fn gather_candidates(
    conn: &Connection,
    query: &str,
    mode: RetrievalMode,
) -> Result<(Vec<Candidate>, usize)> {
    let mut map: HashMap<i64, Candidate> = HashMap::new();
    let mut corrupt_embeddings = 0usize;

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
                    // Corrupt or truncated write: nothing to compute
                    // cosine similarity against. Skipped for scoring but
                    // counted so the caller isn't told the pool is clean
                    // when it silently degraded to FTS-only for this
                    // chunk.
                    corrupt_embeddings += 1;
                    continue;
                }
                let cosine = cosine_similarity(&query_vec, &bytes_to_vector(&blob));
                map.entry(chunk_id)
                    .or_insert_with(|| new_candidate(chunk_id, path))
                    .cosine = Some(cosine);
            }
        }
    }

    Ok((map.into_values().collect(), corrupt_embeddings))
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
/// clamped cosine, weighted by `[code_index]` config. No stale penalty
/// (code chunks don't decay). Mirrors [`crate::retrieval::recall`]'s
/// `score`, but reads its own `CodeIndexConfig` rather than recall's
/// `RetrievalScoringConfig` (R11, issue #73) — see the module doc.
fn score_candidates(candidates: &mut [Candidate], scoring: &CodeIndexConfig) {
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
            c.score *= scoring.test_path_penalty;
        }
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Regression for the bug this fix addresses: a `code_embeddings` row
    /// whose vector blob is the wrong byte length (corrupt/truncated) was
    /// silently skipped with no signal at all -- unlike recall's
    /// `stale_embeddings` warning, locate had no staleness surface
    /// whatsoever. `gather_candidates` must now count it.
    #[test]
    fn gather_candidates_counts_corrupt_length_embeddings() {
        let conn = Connection::open_in_memory().expect("open");
        crate::code_index::schema::bootstrap(&conn).expect("bootstrap");

        conn.execute(
            "INSERT INTO indexed_files(path, mtime, size, content_hash, language) \
             VALUES ('src/lib.rs', 0, 0, 'h', 'rust')",
            [],
        )
        .expect("insert file");
        conn.execute(
            "INSERT INTO code_chunks(\
                file_id, start_line, end_line, symbol, kind, content_hash, embed_text\
             ) VALUES (1, 1, 5, 'foo', 'function', 'h', 'fn foo() {}')",
            [],
        )
        .expect("insert chunk");
        // Corrupt-length vector: 4 bytes instead of EMBEDDING_DIMENSION * 4.
        conn.execute(
            "INSERT INTO code_embeddings(chunk_id, model_name, dimension, vector, content_hash) \
             VALUES (1, ?1, ?2, ?3, 'h')",
            params![EMBEDDING_MODEL_NAME, EMBEDDING_DIMENSION as i64, vec![0u8; 4]],
        )
        .expect("insert embedding");

        let (candidates, corrupt) =
            gather_candidates(&conn, "foo", RetrievalMode::Hybrid).expect("gather");

        assert_eq!(corrupt, 1, "corrupt-length blob must be counted");
        assert!(
            candidates.iter().all(|c| c.cosine.is_none()),
            "corrupt blob must not contribute a vector score"
        );
    }

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

    // -- R11 (issue #73): [code_index] split off [retrieval.scoring] ---

    #[test]
    fn code_index_config_defaults_match_pre_split_shared_values() {
        // The old shared values were DEFAULT_FTS_WEIGHT/DEFAULT_VECTOR_WEIGHT
        // (0.5/0.5, still recall's own defaults post-split) and the
        // hardcoded TEST_PATH_PENALTY const (0.90, task 85). An untouched
        // install's locate ranking must not move.
        let cfg = CodeIndexConfig::default();
        assert_eq!(cfg.fts_weight, crate::config::DEFAULT_FTS_WEIGHT);
        assert_eq!(cfg.vector_weight, crate::config::DEFAULT_VECTOR_WEIGHT);
        assert_eq!(cfg.test_path_penalty, 0.90);
    }

    #[test]
    fn code_index_config_defaults_are_used_when_toml_omits_the_section() {
        // Every config.toml written before this PR has no [code_index]
        // table at all. It must still deserialize — via #[serde(default)]
        // on both the ProjectConfig field and every CodeIndexConfig field
        // — to the byte-identical pre-split defaults, not fail or zero out.
        let raw = r#"
project_name = "x"
auto_sync_md = false
log_level = "info"
"#;
        let cfg: ProjectConfig = toml::from_str(raw).expect("parse");
        assert_eq!(cfg.code_index.fts_weight, crate::config::DEFAULT_CODE_INDEX_FTS_WEIGHT);
        assert_eq!(
            cfg.code_index.vector_weight,
            crate::config::DEFAULT_CODE_INDEX_VECTOR_WEIGHT
        );
        assert_eq!(
            cfg.code_index.test_path_penalty,
            crate::config::DEFAULT_TEST_PATH_PENALTY
        );
    }

    /// The score seam: default-config locate scoring must equal the
    /// pre-split hardcoded formula (`0.5 * fts + 0.5 * vector`, `* 0.90`
    /// for test paths) bit-for-bit. Data is chosen so `normalize_fts`
    /// lands on clean fractions.
    #[test]
    fn score_candidates_default_config_matches_pre_split_formula() {
        let mut candidates = vec![
            new_candidate(1, "src/lib.rs".to_string()),
            new_candidate(2, "tests/foo.rs".to_string()),
        ];
        candidates[0].fts_raw = Some(-10.0); // pos = 10.0 -> fts_score 1.0
        candidates[0].cosine = Some(0.4);
        candidates[1].fts_raw = Some(0.0); // pos = 0.0 -> fts_score 0.0
        candidates[1].cosine = Some(1.0);

        score_candidates(&mut candidates, &CodeIndexConfig::default());

        // src/lib.rs: 0.5*1.0 + 0.5*0.4 = 0.7, no test-path penalty.
        assert!(
            (candidates[0].score - 0.7).abs() < 1e-9,
            "non-test-path score: {}",
            candidates[0].score
        );
        // tests/foo.rs: 0.5*0.0 + 0.5*1.0 = 0.5, times 0.90 penalty = 0.45.
        assert!(
            (candidates[1].score - 0.45).abs() < 1e-9,
            "test-path score: {}",
            candidates[1].score
        );
    }

    /// Regression guard for the bug this issue fixes: locate must read
    /// `[code_index]`, never `[retrieval.scoring]`. Distinctive,
    /// asymmetric weights (1.0/0.0) make it unambiguous which config
    /// path actually drove the score.
    #[test]
    fn score_candidates_uses_code_index_weights_not_retrieval_scoring() {
        let mut candidates = vec![new_candidate(1, "src/lib.rs".to_string())];
        candidates[0].fts_raw = Some(0.0); // single FTS hit -> fts_score 1.0
        candidates[0].cosine = Some(0.0); // vector_score 0.0

        let scoring = CodeIndexConfig {
            fts_weight: 1.0,
            vector_weight: 0.0,
            test_path_penalty: 1.0,
        };
        score_candidates(&mut candidates, &scoring);

        assert!(
            (candidates[0].score - 1.0).abs() < 1e-9,
            "expected fts-only score of 1.0, got {}",
            candidates[0].score
        );
    }
}
