//! Hybrid SQL+RAG recall surface (M8 PR4).
//!
//! Reads from durable facts/decisions/tasks tables, ranks them with a
//! blend of FTS5 BM25 (per-source-type) and brute-force cosine similarity
//! over the per-row embeddings (hybrid mode only), and returns a ranked
//! evidence bundle to the caller. The shape mirrors the addendum's
//! §8 specification of `memhub.recall`.
//!
//! Stays read-only: never writes to durable tables, never writes to
//! pending_writes, never records to `writes_log`.

use std::collections::HashMap;
use std::path::Path;
use std::time::Instant;

use rusqlite::{Connection, Row, params};

use crate::config::{RetrievalMode, RetrievalScoringConfig};
use crate::db;
use crate::models::FACT_STALE_AFTER_DAYS;
use crate::retrieval::embeddings::{EMBEDDING_DIMENSION, EMBEDDING_MODEL_NAME, embed_one};
use crate::retrieval::persist::{
    SourceType, decision_embed_text, doc_chunk_embed_text, fact_embed_text, task_embed_text,
};
use crate::retrieval::rerank;
use crate::{MemhubError, Result};

const PER_SOURCE_FTS_LIMIT: i64 = 50;

/// Caller-supplied recall request.
#[derive(Debug, Clone)]
pub struct RecallOptions {
    pub query: String,
    /// Optional override of `[retrieval] mode`. None = use project config.
    pub mode: Option<RetrievalMode>,
    /// 0 means "use config default".
    pub max_results: usize,
    /// Empty = all source types allowed.
    pub source_types: Vec<SourceType>,
    /// Override of `include_stale_by_default`.
    pub include_stale: Option<bool>,
    /// Override of `accepted_only_by_default`.
    pub accepted_only: Option<bool>,
    /// Override of `[retrieval] use_reranker`. None = use project config.
    /// Ignored when mode resolves to fts (re-ranker only runs on hybrid).
    pub use_reranker: Option<bool>,
    /// Override of `[retrieval.scoring] min_rerank_score`. None = use
    /// project config. Ignored when mode resolves to fts or when the
    /// re-ranker is disabled (the floor only applies to rerank scores).
    /// Exists primarily to support `memhub eval retrieval
    /// --min-rerank-score` calibration sweeps (decisions 70, 71).
    pub min_rerank_score: Option<f32>,
    /// Append a `recall_metrics` row for this call (component A of
    /// decision 74's token-accounting subsystem). The agent-facing
    /// call sites — CLI and MCP server — set this to `true`. Eval
    /// sweeps and the viz dashboard's recall inspector pass `false`
    /// because calibration runs and human inspection are not the
    /// "real usage" the dashboard reports on. The `metrics.enabled`
    /// master switch gates the actual insert separately, so setting
    /// this `true` on a non-opted-in install is still a no-op.
    pub log_metrics: bool,
}

#[derive(Debug, Clone)]
pub struct RecallHit {
    pub rank: usize,
    pub source_type: String,
    pub source_id: i64,
    pub title: String,
    pub body: String,
    pub score: f64,
    pub fts_score: f64,
    pub vector_score: f64,
    pub confidence: f64,
    pub stale: bool,
    pub source: String,
    pub created_at: String,
    /// Cross-encoder relevance score. Some(score) when the re-ranker ran
    /// for this query (hybrid mode + `use_reranker`), None otherwise.
    /// Positive = relevant; nonsense candidates score negative and are
    /// dropped by `[retrieval.scoring] min_rerank_score` before this
    /// hit ever surfaces (decisions 70, 71).
    pub rerank_score: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct RecallWarning {
    pub kind: String,
    pub stale_count: usize,
    pub total_count: usize,
    pub reason: String,
    pub fix: String,
}

#[derive(Debug, Clone)]
pub struct RecallResponse {
    pub query: String,
    pub mode: RetrievalMode,
    pub results: Vec<RecallHit>,
    pub candidate_count: usize,
    pub returned_count: usize,
    pub warnings: Vec<RecallWarning>,
    pub matcher: String,
    pub elapsed_ms: u128,
    /// Count of ingested doc chunks that exist in this repo but were NOT
    /// searched because `doc_chunk` was not in the requested source
    /// types. Zero when docs were queried, when none are ingested, or
    /// when the caller explicitly scoped to docs. A non-zero value is a
    /// cheap cue for the agent to decide whether a follow-up doc-scoped
    /// recall is worthwhile — docs are deliberately opt-in and never in
    /// the default bundle (see migration 0014 / the doc-scope decision).
    pub available_docs: usize,
}

/// Top-level entry: resolves config, then dispatches to the
/// in-connection recall implementation.
pub fn recall(start: &Path, options: RecallOptions) -> Result<RecallResponse> {
    let ctx = db::open_project(start)?;
    let resolved = ResolvedOptions::from(&options, &ctx.config.retrieval)?;
    let response = run(&ctx.conn, &resolved, &ctx.config.retrieval.scoring)?;
    if options.log_metrics {
        crate::metrics::recall_proxy::log_recall(
            &ctx.conn,
            &ctx.config.metrics,
            &ctx.paths.repo_root,
            &ctx.config.render.output_dir,
            &resolved.query,
            &response,
        );
    }
    Ok(response)
}

#[derive(Debug)]
struct ResolvedOptions {
    query: String,
    mode: RetrievalMode,
    max_results: usize,
    source_types: Vec<SourceType>,
    include_stale: bool,
    accepted_only: bool,
    /// Effective rerank toggle; honored only when mode = hybrid.
    use_reranker: bool,
    /// Effective candidate pool size for the cross-encoder when active.
    rerank_candidate_pool: usize,
    /// Effective cross-encoder score floor; honored only when mode =
    /// hybrid and the re-ranker actually runs. Replaces the prior
    /// `min_vector_score` floor (D70, D71): MiniLM gives positive logits
    /// to relevant docs and negative logits to nonsense, so a single
    /// rerank-score cutoff cleanly separates the two without the
    /// cosine-band overlap that doomed the vector-path floor.
    min_rerank_score: f32,
}

impl ResolvedOptions {
    fn from(opts: &RecallOptions, cfg: &crate::config::RetrievalConfig) -> Result<Self> {
        let query = opts.query.trim();
        if query.is_empty() {
            return Err(MemhubError::InvalidInput(
                "recall query cannot be empty".to_string(),
            ));
        }
        let max_results = if opts.max_results == 0 {
            cfg.default_max_results
        } else {
            opts.max_results
        };
        if max_results == 0 {
            return Err(MemhubError::InvalidInput(
                "max_results must be greater than zero".to_string(),
            ));
        }
        let source_types = if opts.source_types.is_empty() {
            vec![SourceType::Fact, SourceType::Decision, SourceType::Task]
        } else {
            let mut deduped = Vec::with_capacity(opts.source_types.len());
            for st in &opts.source_types {
                if !deduped.contains(st) {
                    deduped.push(*st);
                }
            }
            deduped
        };
        Ok(Self {
            query: query.to_string(),
            mode: opts.mode.unwrap_or(cfg.mode),
            max_results,
            source_types,
            include_stale: opts.include_stale.unwrap_or(cfg.include_stale_by_default),
            accepted_only: opts.accepted_only.unwrap_or(cfg.accepted_only_by_default),
            use_reranker: opts.use_reranker.unwrap_or(cfg.use_reranker),
            rerank_candidate_pool: cfg.rerank_candidate_pool.max(max_results),
            min_rerank_score: opts
                .min_rerank_score
                .unwrap_or(cfg.scoring.min_rerank_score),
        })
    }
}

fn run(
    conn: &Connection,
    opts: &ResolvedOptions,
    scoring: &RetrievalScoringConfig,
) -> Result<RecallResponse> {
    let started = Instant::now();
    let mut candidates: HashMap<(SourceType, i64), CandidateRow> = HashMap::new();

    let fts_match = build_fts_match(&opts.query);
    let fts_results: Vec<(SourceType, i64, f64)> = if let Some(match_expr) = fts_match.as_ref() {
        let mut acc = Vec::new();
        for st in &opts.source_types {
            let hits = fts_lookup(conn, *st, match_expr)?;
            acc.extend(hits.into_iter().map(|(id, score)| (*st, id, score)));
        }
        acc
    } else {
        Vec::new()
    };

    // Hydrate source rows for every FTS hit so we can apply filters and
    // assemble the response.
    for (st, id, fts_raw) in &fts_results {
        let entry = candidates
            .entry((*st, *id))
            .or_insert_with(|| CandidateRow::empty(*st, *id));
        entry.fts_raw = Some(*fts_raw);
    }

    // Vector path (hybrid only).
    let mut warnings: Vec<RecallWarning> = Vec::new();
    if opts.mode == RetrievalMode::Hybrid {
        let query_vec = embed_one(&opts.query)?;
        if query_vec.len() != EMBEDDING_DIMENSION {
            return Err(MemhubError::Embedding(format!(
                "query embedding produced {}-dim vector, expected {EMBEDDING_DIMENSION}",
                query_vec.len()
            )));
        }
        // No vector-path floor — D70/D71 retired `min_vector_score` after
        // the MiniLM bundle landed. Nonsense rejection now lives in the
        // rerank-score filter applied below.
        let vector_hits = vector_lookup(conn, &opts.source_types, &query_vec, 0.0)?;
        for hit in &vector_hits {
            let entry = candidates
                .entry((hit.source_type, hit.source_id))
                .or_insert_with(|| CandidateRow::empty(hit.source_type, hit.source_id));
            entry.vector_score = Some(hit.cosine);
        }

        // Stale-embedding detection: across the candidate set, count rows
        // that are missing an embedding or whose content_hash doesn't
        // match the current body.
        let stale = detect_stale_candidates(conn, &candidates)?;
        if stale.stale_count > 0 {
            warnings.push(RecallWarning {
                kind: "stale_embeddings".to_string(),
                stale_count: stale.stale_count,
                total_count: stale.total_count,
                reason: stale.reason,
                fix: "Run /reindex (memhub index rebuild) to refresh.".to_string(),
            });
        }
    }

    // Hydrate source rows for every candidate.
    hydrate_sources(conn, &mut candidates)?;

    // Apply filters (per §4 of the addendum, filters apply before scoring).
    let surviving: Vec<CandidateRow> = candidates
        .into_values()
        .filter(|c| c.has_source_row())
        .filter(|c| {
            if opts.include_stale {
                true
            } else {
                !c.is_stale
            }
        })
        .filter(|c| {
            if opts.accepted_only {
                is_accepted_source(&c.source)
            } else {
                true
            }
        })
        .collect();

    let candidate_count = surviving.len();

    // Score and rank.
    let mut scored: Vec<ScoredHit> = score(&surviving, scoring);
    scored.sort_by(|a, b| {
        b.score
            .partial_cmp(&a.score)
            .unwrap_or(std::cmp::Ordering::Equal)
            .then_with(|| a.source_type.as_str().cmp(b.source_type.as_str()))
            .then_with(|| a.source_id.cmp(&b.source_id))
    });

    // Optional cross-encoder re-rank pass (decision 68). Only runs on
    // hybrid mode and only when the operator hasn't opted out. Takes the
    // top `rerank_candidate_pool` by blended score, reorders them by the
    // cross-encoder, drops anything below `min_rerank_score` (D71 — the
    // nonsense-rejection floor that replaced `min_vector_score`), then
    // truncates to `max_results`. fts-only callers and trivially short
    // candidate sets bypass entirely.
    let reranked =
        opts.mode == RetrievalMode::Hybrid && opts.use_reranker && scored.len() > 1;
    if reranked {
        scored.truncate(opts.rerank_candidate_pool);
        // Cross-encoder input mirrors the bi-encoder's embed text shape:
        // prepend the row's natural-language summary when present so the
        // re-ranker can score paraphrase matches, not just title+body
        // surface tokens (decision 72 / task #23).
        let docs: Vec<String> = scored
            .iter()
            .map(|h| match h.summary.as_deref() {
                Some(s) if !s.trim().is_empty() => {
                    format!("{}\n\n{}\n\n{}", s, h.title, h.body)
                }
                _ => format!("{}\n\n{}", h.title, h.body),
            })
            .collect();
        let scored_order = rerank::rerank(&opts.query, &docs)?;
        let mut reshuffled: Vec<ScoredHit> = Vec::with_capacity(scored.len());
        for (idx, score) in scored_order {
            if score < opts.min_rerank_score {
                continue;
            }
            if let Some(hit) = scored.get(idx) {
                let mut hit = hit.clone();
                hit.rerank_score = Some(score);
                reshuffled.push(hit);
            }
        }
        scored = reshuffled;
    }
    scored.truncate(opts.max_results);

    let mut results = Vec::with_capacity(scored.len());
    for (idx, hit) in scored.into_iter().enumerate() {
        results.push(RecallHit {
            rank: idx + 1,
            source_type: hit.source_type.as_str().to_string(),
            source_id: hit.source_id,
            title: hit.title,
            body: hit.body,
            score: hit.score,
            fts_score: hit.fts_score,
            vector_score: hit.vector_score,
            confidence: hit.confidence,
            stale: hit.stale,
            source: hit.source,
            created_at: hit.created_at,
            rerank_score: hit.rerank_score,
        });
    }

    let returned_count = results.len();
    let matcher = match opts.mode {
        RetrievalMode::Fts => "recall:fts".to_string(),
        RetrievalMode::Hybrid => {
            if reranked {
                "recall:hybrid+rerank".to_string()
            } else {
                "recall:hybrid".to_string()
            }
        }
    };

    // Cheap awareness signal: if the caller did not scope to docs, tell
    // them how many ingested doc chunks they skipped so the agent can
    // decide whether a follow-up doc-scoped recall is worthwhile.
    let available_docs: usize = if opts.source_types.contains(&SourceType::DocChunk) {
        0
    } else {
        conn.query_row(
            "SELECT COUNT(*) FROM doc_chunks WHERE project_id = 1",
            [],
            |r| r.get::<_, i64>(0),
        )
        .optional_row()?
        .unwrap_or(0)
        .max(0) as usize
    };

    Ok(RecallResponse {
        query: opts.query.clone(),
        mode: opts.mode,
        results,
        candidate_count,
        returned_count,
        warnings,
        matcher,
        elapsed_ms: started.elapsed().as_millis(),
        available_docs,
    })
}

#[derive(Debug)]
struct CandidateRow {
    source_type: SourceType,
    source_id: i64,
    fts_raw: Option<f64>,
    vector_score: Option<f64>,
    title: String,
    body: String,
    /// Optional augmenting paraphrase. When `Some`, the cross-encoder
    /// rerank input is built as `summary\n\ntitle\n\nbody`; otherwise
    /// the existing `title\n\nbody` shape is preserved. Today only
    /// populated for decisions (migration 0011 / decision 72).
    summary: Option<String>,
    source: String,
    confidence: f64,
    is_stale: bool,
    created_at: String,
    hydrated: bool,
}

impl CandidateRow {
    fn empty(source_type: SourceType, source_id: i64) -> Self {
        Self {
            source_type,
            source_id,
            fts_raw: None,
            vector_score: None,
            title: String::new(),
            body: String::new(),
            summary: None,
            source: String::new(),
            confidence: 1.0,
            is_stale: false,
            created_at: String::new(),
            hydrated: false,
        }
    }

    fn has_source_row(&self) -> bool {
        self.hydrated
    }
}

#[derive(Clone)]
struct ScoredHit {
    source_type: SourceType,
    source_id: i64,
    title: String,
    body: String,
    /// Carried forward from the candidate row so the cross-encoder
    /// rerank input can be built as `summary\n\ntitle\n\nbody` when
    /// present (decision 72). Not exposed on the public RecallHit
    /// shape — callers see only title/body/score/etc.
    summary: Option<String>,
    score: f64,
    fts_score: f64,
    vector_score: f64,
    confidence: f64,
    stale: bool,
    source: String,
    created_at: String,
    /// Cross-encoder relevance score, populated only when the re-ranker
    /// ran (hybrid mode + use_reranker + non-trivial candidate set).
    /// Used both as the final ordering key and as the nonsense-rejection
    /// floor (`min_rerank_score`). See decisions 68, 70, 71.
    rerank_score: Option<f32>,
}

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

fn fts_lookup(
    conn: &Connection,
    source_type: SourceType,
    match_expr: &str,
) -> Result<Vec<(i64, f64)>> {
    let sql = match source_type {
        SourceType::Fact => {
            "SELECT facts_fts.rowid, bm25(facts_fts) AS score \
             FROM facts_fts \
             WHERE facts_fts MATCH ?1 \
             ORDER BY score ASC \
             LIMIT ?2"
        }
        SourceType::Decision => {
            "SELECT decisions_fts.rowid, bm25(decisions_fts) AS score \
             FROM decisions_fts \
             WHERE decisions_fts MATCH ?1 \
             ORDER BY score ASC \
             LIMIT ?2"
        }
        SourceType::Task => {
            "SELECT tasks_fts.rowid, bm25(tasks_fts) AS score \
             FROM tasks_fts \
             WHERE tasks_fts MATCH ?1 \
             ORDER BY score ASC \
             LIMIT ?2"
        }
        SourceType::DocChunk => {
            "SELECT doc_chunks_fts.rowid, bm25(doc_chunks_fts) AS score \
             FROM doc_chunks_fts \
             WHERE doc_chunks_fts MATCH ?1 \
             ORDER BY score ASC \
             LIMIT ?2"
        }
    };
    let mut stmt = conn.prepare(sql)?;
    let rows = stmt.query_map(params![match_expr, PER_SOURCE_FTS_LIMIT], |row| {
        Ok((row.get::<_, i64>(0)?, row.get::<_, f64>(1)?))
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

#[derive(Debug)]
struct VectorHit {
    source_type: SourceType,
    source_id: i64,
    cosine: f64,
}

fn vector_lookup(
    conn: &Connection,
    source_types: &[SourceType],
    query_vec: &[f32],
    min_vector_score: f64,
) -> Result<Vec<VectorHit>> {
    let mut stmt = conn.prepare(
        "SELECT source_type, source_id, vector \
         FROM embeddings \
         WHERE model_name = ?1 AND dimension = ?2",
    )?;
    let allowed: Vec<&'static str> = source_types.iter().map(|st| st.as_str()).collect();
    let rows = stmt.query_map(
        params![EMBEDDING_MODEL_NAME, EMBEDDING_DIMENSION as i64],
        |row| {
            let st: String = row.get(0)?;
            let id: i64 = row.get(1)?;
            let blob: Vec<u8> = row.get(2)?;
            Ok((st, id, blob))
        },
    )?;

    let mut out = Vec::new();
    for row in rows {
        let (st, id, blob) = row?;
        if !allowed.contains(&st.as_str()) {
            continue;
        }
        let source_type = match parse_source_type(&st) {
            Some(t) => t,
            None => continue,
        };
        if blob.len() != EMBEDDING_DIMENSION * 4 {
            continue;
        }
        let candidate_vec = bytes_to_vector(&blob);
        let cosine = cosine_similarity(query_vec, &candidate_vec);
        if cosine < min_vector_score {
            continue;
        }
        out.push(VectorHit {
            source_type,
            source_id: id,
            cosine,
        });
    }
    Ok(out)
}

struct StaleSummary {
    stale_count: usize,
    total_count: usize,
    reason: String,
}

fn detect_stale_candidates(
    conn: &Connection,
    candidates: &HashMap<(SourceType, i64), CandidateRow>,
) -> Result<StaleSummary> {
    if candidates.is_empty() {
        return Ok(StaleSummary {
            stale_count: 0,
            total_count: 0,
            reason: String::new(),
        });
    }

    let mut stale_count = 0;
    let total_count = candidates.len();
    let mut reason_missing = false;
    let mut reason_drift = false;

    let mut stmt = conn.prepare(
        "SELECT content_hash FROM embeddings \
         WHERE source_type = ?1 AND source_id = ?2 AND model_name = ?3",
    )?;

    for (key, _) in candidates {
        let current_text = match current_embed_text(conn, key.0, key.1)? {
            Some(t) => t,
            None => continue,
        };
        let current_hash = sha256_hex(&current_text);
        let existing: Option<String> = stmt
            .query_row(
                params![key.0.as_str(), key.1, EMBEDDING_MODEL_NAME],
                |row| row.get(0),
            )
            .optional_row()?;
        match existing {
            None => {
                stale_count += 1;
                reason_missing = true;
            }
            Some(hash) if hash != current_hash => {
                stale_count += 1;
                reason_drift = true;
            }
            _ => {}
        }
    }

    let reason = match (reason_missing, reason_drift) {
        (true, true) => "missing_or_drift".to_string(),
        (true, false) => "missing_embeddings".to_string(),
        (false, true) => "content_drift".to_string(),
        (false, false) => String::new(),
    };

    Ok(StaleSummary {
        stale_count,
        total_count,
        reason,
    })
}

trait OptionalRowExt<T> {
    fn optional_row(self) -> std::result::Result<Option<T>, rusqlite::Error>;
}

impl<T> OptionalRowExt<T> for std::result::Result<T, rusqlite::Error> {
    fn optional_row(self) -> std::result::Result<Option<T>, rusqlite::Error> {
        match self {
            Ok(v) => Ok(Some(v)),
            Err(rusqlite::Error::QueryReturnedNoRows) => Ok(None),
            Err(e) => Err(e),
        }
    }
}

fn current_embed_text(
    conn: &Connection,
    source_type: SourceType,
    source_id: i64,
) -> Result<Option<String>> {
    match source_type {
        SourceType::Fact => {
            let row: std::result::Result<(String, String), rusqlite::Error> = conn.query_row(
                "SELECT key, value FROM facts WHERE id = ?1",
                params![source_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            );
            match row.optional_row()? {
                Some((k, v)) => Ok(Some(fact_embed_text(&k, &v))),
                None => Ok(None),
            }
        }
        SourceType::Decision => {
            let row: std::result::Result<(String, String, Option<String>), rusqlite::Error> = conn
                .query_row(
                    "SELECT title, rationale, summary FROM decisions WHERE id = ?1",
                    params![source_id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                );
            match row.optional_row()? {
                Some((t, r, s)) => Ok(Some(decision_embed_text(&t, &r, s.as_deref()))),
                None => Ok(None),
            }
        }
        SourceType::Task => {
            let row: std::result::Result<(String, Option<String>), rusqlite::Error> = conn
                .query_row(
                    "SELECT title, notes FROM tasks WHERE id = ?1",
                    params![source_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                );
            match row.optional_row()? {
                Some((t, n)) => Ok(Some(task_embed_text(&t, n.as_deref()))),
                None => Ok(None),
            }
        }
        SourceType::DocChunk => {
            let row: std::result::Result<(String, String), rusqlite::Error> = conn.query_row(
                "SELECT heading_path, body FROM doc_chunks WHERE id = ?1",
                params![source_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            );
            match row.optional_row()? {
                Some((h, b)) => Ok(Some(doc_chunk_embed_text(&h, &b))),
                None => Ok(None),
            }
        }
    }
}

fn hydrate_sources(
    conn: &Connection,
    candidates: &mut HashMap<(SourceType, i64), CandidateRow>,
) -> Result<()> {
    // Group by source type to minimize prepared statements.
    let mut by_type: HashMap<SourceType, Vec<i64>> = HashMap::new();
    for key in candidates.keys() {
        by_type.entry(key.0).or_default().push(key.1);
    }

    for (st, ids) in by_type {
        for id in ids {
            if let Some(row) = load_source_row(conn, st, id)? {
                if let Some(entry) = candidates.get_mut(&(st, id)) {
                    entry.title = row.title;
                    entry.body = row.body;
                    entry.summary = row.summary;
                    entry.source = row.source;
                    entry.confidence = row.confidence;
                    entry.is_stale = row.is_stale;
                    entry.created_at = row.created_at;
                    entry.hydrated = true;
                }
            }
        }
    }
    Ok(())
}

struct HydratedSource {
    title: String,
    body: String,
    /// Only populated for decisions (migration 0011 / decision 72).
    summary: Option<String>,
    source: String,
    confidence: f64,
    is_stale: bool,
    created_at: String,
}

fn load_source_row(
    conn: &Connection,
    source_type: SourceType,
    source_id: i64,
) -> Result<Option<HydratedSource>> {
    match source_type {
        SourceType::Fact => {
            let mut stmt = conn.prepare(
                "SELECT key, value, source, confidence, verified_at, created_at, \
                    CASE \
                        WHEN verified_at IS NULL THEN 1 \
                        WHEN (julianday('now') - julianday(verified_at)) > ?2 THEN 1 \
                        ELSE 0 \
                    END AS is_stale \
                FROM facts WHERE id = ?1",
            )?;
            let row: std::result::Result<HydratedSource, rusqlite::Error> =
                stmt.query_row(params![source_id, FACT_STALE_AFTER_DAYS], |r: &Row<'_>| {
                    let key: String = r.get(0)?;
                    let value: String = r.get(1)?;
                    let source: String = r.get(2)?;
                    let confidence: f64 = r.get(3)?;
                    let created_at: String = r.get(5)?;
                    let stale_int: i64 = r.get(6)?;
                    Ok(HydratedSource {
                        title: key,
                        body: value,
                        summary: None,
                        source,
                        confidence,
                        is_stale: stale_int != 0,
                        created_at,
                    })
                });
            row.optional_row().map_err(Into::into)
        }
        SourceType::Decision => {
            let mut stmt = conn.prepare(
                "SELECT title, rationale, source, decided_at, summary \
                 FROM decisions WHERE id = ?1",
            )?;
            let row: std::result::Result<HydratedSource, rusqlite::Error> =
                stmt.query_row(params![source_id], |r: &Row<'_>| {
                    let title: String = r.get(0)?;
                    let rationale: String = r.get(1)?;
                    let source: String = r.get(2)?;
                    let decided_at: String = r.get(3)?;
                    let summary: Option<String> = r.get(4)?;
                    Ok(HydratedSource {
                        title,
                        body: rationale,
                        summary,
                        source,
                        confidence: 1.0,
                        is_stale: false,
                        created_at: decided_at,
                    })
                });
            row.optional_row().map_err(Into::into)
        }
        SourceType::Task => {
            let mut stmt =
                conn.prepare("SELECT title, notes, created_at FROM tasks WHERE id = ?1")?;
            let row: std::result::Result<HydratedSource, rusqlite::Error> =
                stmt.query_row(params![source_id], |r: &Row<'_>| {
                    let title: String = r.get(0)?;
                    let notes: Option<String> = r.get(1)?;
                    let created_at: String = r.get(2)?;
                    Ok(HydratedSource {
                        title,
                        body: notes.unwrap_or_default(),
                        summary: None,
                        source: String::new(),
                        confidence: 1.0,
                        is_stale: false,
                        created_at,
                    })
                });
            row.optional_row().map_err(Into::into)
        }
        SourceType::DocChunk => {
            // Title shows the document title plus the section breadcrumb
            // so a recalled chunk is self-describing about its origin.
            let mut stmt = conn.prepare(
                "SELECT d.title, c.heading_path, c.body, d.source, c.created_at \
                 FROM doc_chunks c JOIN documents d ON d.id = c.doc_id \
                 WHERE c.id = ?1",
            )?;
            let row: std::result::Result<HydratedSource, rusqlite::Error> =
                stmt.query_row(params![source_id], |r: &Row<'_>| {
                    let doc_title: String = r.get(0)?;
                    let heading_path: String = r.get(1)?;
                    let body: String = r.get(2)?;
                    let source: String = r.get(3)?;
                    let created_at: String = r.get(4)?;
                    let title = if heading_path.trim().is_empty() {
                        doc_title
                    } else {
                        format!("{doc_title} — {heading_path}")
                    };
                    Ok(HydratedSource {
                        title,
                        body,
                        summary: None,
                        source,
                        confidence: 1.0,
                        is_stale: false,
                        created_at,
                    })
                });
            row.optional_row().map_err(Into::into)
        }
    }
}

fn is_accepted_source(source: &str) -> bool {
    source == "user" || source.starts_with("user+agent:")
}

fn parse_source_type(raw: &str) -> Option<SourceType> {
    match raw {
        "fact" => Some(SourceType::Fact),
        "decision" => Some(SourceType::Decision),
        "task" => Some(SourceType::Task),
        "doc_chunk" => Some(SourceType::DocChunk),
        _ => None,
    }
}

fn score(rows: &[CandidateRow], scoring: &RetrievalScoringConfig) -> Vec<ScoredHit> {
    let (fts_min, fts_max) = rows
        .iter()
        .fold((f64::INFINITY, f64::NEG_INFINITY), |acc, c| {
            match c.fts_raw {
                Some(raw) => {
                    let pos = -raw; // BM25 is "lower is better"; invert so higher is better.
                    (acc.0.min(pos), acc.1.max(pos))
                }
                None => acc,
            }
        });

    rows.iter()
        .map(|c| {
            let fts_score = match c.fts_raw {
                Some(raw) => normalize_fts(-raw, fts_min, fts_max),
                None => 0.0,
            };
            let vector_score = c.vector_score.unwrap_or(0.0).clamp(0.0, 1.0);
            let penalty = if c.is_stale {
                scoring.stale_penalty
            } else {
                0.0
            };
            let score =
                scoring.fts_weight * fts_score + scoring.vector_weight * vector_score - penalty;
            ScoredHit {
                source_type: c.source_type,
                source_id: c.source_id,
                title: c.title.clone(),
                body: c.body.clone(),
                summary: c.summary.clone(),
                score,
                fts_score,
                vector_score,
                confidence: c.confidence,
                stale: c.is_stale,
                source: c.source.clone(),
                created_at: c.created_at.clone(),
                rerank_score: None,
            }
        })
        .collect()
}

fn normalize_fts(value: f64, min: f64, max: f64) -> f64 {
    if !value.is_finite() || !min.is_finite() || !max.is_finite() {
        return 0.0;
    }
    if (max - min).abs() < f64::EPSILON {
        // Single FTS hit (or ties): treat as full strength.
        return 1.0;
    }
    ((value - min) / (max - min)).clamp(0.0, 1.0)
}

fn bytes_to_vector(blob: &[u8]) -> Vec<f32> {
    let mut out = Vec::with_capacity(blob.len() / 4);
    for chunk in blob.chunks_exact(4) {
        let bytes: [u8; 4] = chunk.try_into().expect("chunk is exactly 4 bytes");
        out.push(f32::from_le_bytes(bytes));
    }
    out
}

fn cosine_similarity(a: &[f32], b: &[f32]) -> f64 {
    if a.len() != b.len() {
        return 0.0;
    }
    let mut dot: f64 = 0.0;
    let mut na: f64 = 0.0;
    let mut nb: f64 = 0.0;
    for (x, y) in a.iter().zip(b.iter()) {
        let xf = *x as f64;
        let yf = *y as f64;
        dot += xf * yf;
        na += xf * xf;
        nb += yf * yf;
    }
    if na <= f64::EPSILON || nb <= f64::EPSILON {
        return 0.0;
    }
    dot / (na.sqrt() * nb.sqrt())
}

fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{decision, doc, fact, init, task};
    use crate::config::{ProjectConfig, RetrievalMode};
    use rusqlite::params;
    use tempfile::tempdir;

    fn seed(temp: &std::path::Path) {
        init::run(temp).expect("init");
        fact::add(temp, "build-command", "cargo build", "user", "cli:user").expect("fact 1");
        fact::add(
            temp,
            "lint-command",
            "cargo clippy --all-targets",
            "agent:codex",
            "cli:user",
        )
        .expect("fact 2");
        decision::add(
            temp,
            "Stage agent-originated writes before promotion",
            "Agents may propose facts and decisions but durable rows require human review.",
            "user+agent:claude-code",
            "cli:user",
        )
        .expect("decision 1");
        task::add(
            temp,
            "Ship recall surface",
            Some("PR4 of M8 rolls out recall CLI plus MCP tool."),
            "cli:user",
        )
        .expect("task 1");
    }

    #[test]
    fn recall_returns_decision_match_for_topic_query() {
        let temp = tempdir().expect("tempdir");
        seed(temp.path());

        let response = recall(
            temp.path(),
            RecallOptions {
                query: "agent originated writes review".to_string(),
                mode: Some(RetrievalMode::Fts),
                max_results: 5,
                source_types: vec![],
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                min_rerank_score: None,
                log_metrics: false,
            },
        )
        .expect("recall");

        assert_eq!(response.mode, RetrievalMode::Fts);
        assert!(!response.results.is_empty());
        let top = &response.results[0];
        assert_eq!(top.source_type, "decision");
        assert_eq!(top.rank, 1);
        assert!(top.fts_score > 0.0);
        assert_eq!(top.vector_score, 0.0);
        assert_eq!(response.matcher, "recall:fts");
    }

    #[test]
    fn recall_filters_by_source_type_allowlist() {
        let temp = tempdir().expect("tempdir");
        seed(temp.path());

        let response = recall(
            temp.path(),
            RecallOptions {
                query: "cargo build".to_string(),
                mode: Some(RetrievalMode::Fts),
                max_results: 5,
                source_types: vec![SourceType::Decision],
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                min_rerank_score: None,
                log_metrics: false,
            },
        )
        .expect("recall");

        assert!(response.results.iter().all(|h| h.source_type == "decision"));
    }

    #[test]
    fn recall_accepted_only_excludes_agent_origin() {
        let temp = tempdir().expect("tempdir");
        seed(temp.path());

        let response = recall(
            temp.path(),
            RecallOptions {
                query: "command".to_string(),
                mode: Some(RetrievalMode::Fts),
                max_results: 10,
                source_types: vec![SourceType::Fact],
                include_stale: None,
                accepted_only: Some(true),
                use_reranker: None,
                min_rerank_score: None,
                log_metrics: false,
            },
        )
        .expect("recall");

        assert!(
            response
                .results
                .iter()
                .all(|h| is_accepted_source(&h.source)),
            "accepted_only must exclude agent:* rows, got {:?}",
            response
                .results
                .iter()
                .map(|h| h.source.as_str())
                .collect::<Vec<_>>(),
        );
        let has_build = response.results.iter().any(|h| h.title == "build-command");
        assert!(has_build, "user-authored fact must remain visible");
    }

    #[test]
    fn recall_empty_query_is_rejected() {
        let temp = tempdir().expect("tempdir");
        seed(temp.path());
        let err = recall(
            temp.path(),
            RecallOptions {
                query: "   ".to_string(),
                mode: Some(RetrievalMode::Fts),
                max_results: 0,
                source_types: vec![],
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                min_rerank_score: None,
                log_metrics: false,
            },
        )
        .expect_err("empty query");
        assert!(matches!(err, MemhubError::InvalidInput(_)));
    }

    #[test]
    fn recall_zero_results_returns_empty_bundle() {
        let temp = tempdir().expect("tempdir");
        seed(temp.path());

        let response = recall(
            temp.path(),
            RecallOptions {
                query: "zxqv-no-such-token-anywhere".to_string(),
                mode: Some(RetrievalMode::Fts),
                max_results: 5,
                source_types: vec![],
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                min_rerank_score: None,
                log_metrics: false,
            },
        )
        .expect("recall");

        assert_eq!(response.results.len(), 0);
        assert_eq!(response.candidate_count, 0);
        assert_eq!(response.returned_count, 0);
        assert!(response.warnings.is_empty());
        assert_eq!(response.matcher, "recall:fts");
    }

    #[test]
    fn recall_respects_max_results_override_of_config_default() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        for i in 0..10 {
            fact::add(
                temp.path(),
                &format!("cmd-{i}"),
                &format!("cargo build target {i}"),
                "user",
                "cli:user",
            )
            .expect("seed fact");
        }

        let response = recall(
            temp.path(),
            RecallOptions {
                query: "cargo".to_string(),
                mode: Some(RetrievalMode::Fts),
                max_results: 3,
                source_types: vec![SourceType::Fact],
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                min_rerank_score: None,
                log_metrics: false,
            },
        )
        .expect("recall");

        assert_eq!(response.results.len(), 3);
        assert!(response.candidate_count >= 3);
    }

    #[test]
    fn recall_does_not_mutate_legacy_decision_chunks() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let decision_id = decision::add(
            temp.path(),
            "Original decision",
            "Original rationale",
            "user",
            "cli:user",
        )
        .expect("decision");

        {
            let ctx = crate::db::open_project(temp.path()).expect("open");
            ctx.conn
                .execute(
                    "UPDATE decisions
                     SET rationale = 'freshrecalltoken is only in the source table'
                     WHERE id = ?1",
                    params![decision_id],
                )
                .expect("direct update");
        }

        let before: i64 = {
            let ctx = crate::db::open_project(temp.path()).expect("open");
            ctx.conn
                .query_row(
                    "SELECT COUNT(*) FROM chunks WHERE text LIKE '%freshrecalltoken%'",
                    params![],
                    |r| r.get(0),
                )
                .expect("count chunks")
        };
        assert_eq!(before, 0, "test setup should leave legacy chunks stale");

        let response = recall(
            temp.path(),
            RecallOptions {
                query: "freshrecalltoken".to_string(),
                mode: Some(RetrievalMode::Fts),
                max_results: 5,
                source_types: vec![SourceType::Decision],
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                min_rerank_score: None,
                log_metrics: false,
            },
        )
        .expect("recall");
        assert_eq!(response.results.len(), 1);

        let after: i64 = {
            let ctx = crate::db::open_project(temp.path()).expect("open");
            ctx.conn
                .query_row(
                    "SELECT COUNT(*) FROM chunks WHERE text LIKE '%freshrecalltoken%'",
                    params![],
                    |r| r.get(0),
                )
                .expect("count chunks")
        };
        assert_eq!(after, 0, "recall must not sync or rewrite chunks");
    }

    #[test]
    fn recall_keeps_done_tasks_visible_without_include_stale() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let task_id = task::add(
            temp.path(),
            "Ship eval harness",
            Some("golden queries for recall"),
            "cli:user",
        )
        .expect("task");
        task::done(temp.path(), task_id, "cli:user").expect("done");

        let response = recall(
            temp.path(),
            RecallOptions {
                query: "eval harness".to_string(),
                mode: Some(RetrievalMode::Fts),
                max_results: 5,
                source_types: vec![SourceType::Task],
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                min_rerank_score: None,
                log_metrics: false,
            },
        )
        .expect("recall");

        assert!(
            response.results.iter().any(|hit| hit.source_id == task_id),
            "done task should remain recallable by default"
        );
    }

    #[test]
    fn fts_normalization_collapses_to_one_for_single_hit() {
        let v = normalize_fts(-5.0, -5.0, -5.0);
        assert_eq!(v, 1.0);
    }

    #[test]
    fn cosine_of_orthogonal_vectors_is_zero() {
        let a = vec![1.0f32, 0.0];
        let b = vec![0.0f32, 1.0];
        assert!(cosine_similarity(&a, &b).abs() < 1e-9);
    }

    #[test]
    fn cosine_of_identical_vectors_is_one() {
        let a = vec![0.5f32, 0.5, 0.5];
        let b = a.clone();
        assert!((cosine_similarity(&a, &b) - 1.0).abs() < 1e-9);
    }

    #[test]
    fn hybrid_mode_returns_warning_for_pre_existing_unindexed_facts() {
        // Insert under fts mode → no embeddings written; then recall in
        // hybrid mode should still rank the row via FTS and surface a
        // stale_embeddings warning for the missing vector.
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        fact::add(
            temp.path(),
            "build-command",
            "cargo build",
            "user",
            "cli:user",
        )
        .expect("seed fact");

        // Sanity: no embedding row exists.
        let ctx = crate::db::open_project(temp.path()).expect("open");
        let count: i64 = ctx
            .conn
            .query_row("SELECT COUNT(*) FROM embeddings", params![], |r| r.get(0))
            .expect("count");
        assert_eq!(count, 0);
        drop(ctx);

        let response = recall(
            temp.path(),
            RecallOptions {
                query: "cargo build".to_string(),
                mode: Some(RetrievalMode::Hybrid),
                max_results: 5,
                source_types: vec![SourceType::Fact],
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                min_rerank_score: None,
                log_metrics: false,
            },
        )
        .expect("recall");

        assert!(!response.results.is_empty());
        let warn = response
            .warnings
            .iter()
            .find(|w| w.kind == "stale_embeddings")
            .expect("warning present");
        assert!(warn.stale_count >= 1);
    }

    #[test]
    fn config_defaults_round_trip() {
        // Sanity: a saved/loaded config preserves the new retrieval knobs.
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let cfg_path = temp.path().join(".memhub/config.toml");
        let cfg = ProjectConfig::load(&cfg_path).expect("load");
        assert_eq!(
            cfg.retrieval.default_max_results,
            super::super::super::config::DEFAULT_RECALL_MAX_RESULTS
        );
        assert!(
            (cfg.retrieval.scoring.fts_weight - super::super::super::config::DEFAULT_FTS_WEIGHT)
                .abs()
                < 1e-9
        );
        assert!(
            (cfg.retrieval.scoring.vector_weight
                - super::super::super::config::DEFAULT_VECTOR_WEIGHT)
                .abs()
                < 1e-9
        );
        assert!(
            (cfg.retrieval.scoring.stale_penalty
                - super::super::super::config::DEFAULT_STALE_PENALTY)
                .abs()
                < 1e-9
        );
        assert!(
            (cfg.retrieval.scoring.min_rerank_score
                - super::super::super::config::DEFAULT_MIN_RERANK_SCORE)
                .abs()
                < 1e-6
        );
    }

    #[test]
    fn hybrid_min_rerank_score_drops_nonsense_when_reranker_runs() {
        // With hybrid + use_reranker on (project defaults) and the
        // min_rerank_score floor at its default near 0, MiniLM's negative
        // logits on a pure-nonsense query drop every candidate, so the
        // bundle must be empty.
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let cfg_path = temp.path().join(".memhub/config.toml");
        let mut cfg = ProjectConfig::load(&cfg_path).expect("load");
        cfg.retrieval.mode = RetrievalMode::Hybrid;
        cfg.save(&cfg_path).expect("save");

        seed(temp.path());

        let response = recall(
            temp.path(),
            RecallOptions {
                query: "zxqv-pure-nonsense-no-real-token-anywhere-in-this-repo".to_string(),
                mode: Some(RetrievalMode::Hybrid),
                max_results: 5,
                source_types: vec![],
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                min_rerank_score: None,
                log_metrics: false,
            },
        )
        .expect("recall");

        assert_eq!(
            response.results.len(),
            0,
            "nonsense query must return empty bundle once the rerank-score floor drops negative-logit candidates; got {:?}",
            response
                .results
                .iter()
                .map(|h| (h.source_type.clone(), h.rerank_score))
                .collect::<Vec<_>>(),
        );
        assert_eq!(response.matcher, "recall:hybrid+rerank");
    }

    #[test]
    fn hybrid_negative_min_rerank_score_keeps_every_candidate() {
        // Inverse check: when the operator opts out of the rerank floor
        // by setting it to a value below any cross-encoder logit (-1000.0),
        // the bundle re-fills with nonsense hits. Guards against a future
        // refactor that hard-codes a floor.
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let cfg_path = temp.path().join(".memhub/config.toml");
        let mut cfg = ProjectConfig::load(&cfg_path).expect("load");
        cfg.retrieval.mode = RetrievalMode::Hybrid;
        cfg.save(&cfg_path).expect("save");

        seed(temp.path());

        let response = recall(
            temp.path(),
            RecallOptions {
                query: "zxqv-pure-nonsense-no-real-token-anywhere-in-this-repo".to_string(),
                mode: Some(RetrievalMode::Hybrid),
                max_results: 5,
                source_types: vec![],
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                min_rerank_score: Some(-1000.0),
                log_metrics: false,
            },
        )
        .expect("recall");

        assert!(
            !response.results.is_empty(),
            "with the rerank floor pinned below every possible logit, hybrid recall should still surface low-confidence vector hits",
        );
    }

    fn count_recall_metrics(temp_path: &std::path::Path) -> i64 {
        let ctx = crate::db::open_project(temp_path).expect("open");
        ctx.conn
            .query_row("SELECT COUNT(*) FROM recall_metrics", params![], |r| {
                r.get(0)
            })
            .expect("count recall_metrics")
    }

    #[test]
    fn docs_are_opt_in_and_signal_availability() {
        // Docs must never enter the default bundle, but recall must
        // advertise that they exist via `available_docs` so the agent
        // can decide to follow up. Scoping to docs returns the chunk
        // and zeroes the signal.
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        fact::add(temp.path(), "build-command", "cargo build", "user", "cli:user")
            .expect("fact");
        let doc_file = temp.path().join("spec.md");
        std::fs::write(
            &doc_file,
            "# Design Spec\n\n## Shapes\n\nButtons use a 4px corner radius for a crisp engineered look.\n",
        )
        .expect("write doc");
        doc::add(temp.path(), &doc_file, None, "cli:user").expect("ingest doc");

        // Default scope: no doc hits, but availability is signalled.
        let default = recall(
            temp.path(),
            RecallOptions {
                query: "corner radius for buttons".to_string(),
                mode: Some(RetrievalMode::Fts),
                max_results: 10,
                source_types: vec![],
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                min_rerank_score: None,
                log_metrics: false,
            },
        )
        .expect("default recall");
        assert!(
            default.results.iter().all(|h| h.source_type != "doc_chunk"),
            "docs must not appear in the default bundle"
        );
        assert!(
            default.available_docs >= 1,
            "available_docs must flag ingested-but-unsearched docs, got {}",
            default.available_docs
        );

        // Opt-in scope: the chunk surfaces and the signal zeroes out.
        let scoped = recall(
            temp.path(),
            RecallOptions {
                query: "corner radius for buttons".to_string(),
                mode: Some(RetrievalMode::Fts),
                max_results: 10,
                source_types: vec![SourceType::DocChunk],
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                min_rerank_score: None,
                log_metrics: false,
            },
        )
        .expect("doc-scoped recall");
        assert!(
            scoped.results.iter().any(|h| h.source_type == "doc_chunk"),
            "doc-scoped recall must return the ingested chunk"
        );
        assert_eq!(
            scoped.available_docs, 0,
            "available_docs is 0 when the caller already scoped to docs"
        );
    }

    #[test]
    fn metrics_disabled_writes_no_rows() {
        // Default config ships `metrics.enabled = false`, so even when
        // the caller asks for logging the proxy must stay silent.
        // Guards against accidentally enabling the master switch on
        // every install — the regression would silently flood every
        // unrelated repo's DB with recall_metrics rows.
        let temp = tempdir().expect("tempdir");
        seed(temp.path());

        let response = recall(
            temp.path(),
            RecallOptions {
                query: "agent originated writes review".to_string(),
                mode: Some(RetrievalMode::Fts),
                max_results: 5,
                source_types: vec![],
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                min_rerank_score: None,
                log_metrics: true,
            },
        )
        .expect("recall");

        assert!(!response.results.is_empty());
        assert_eq!(count_recall_metrics(temp.path()), 0);
    }

    #[test]
    fn metrics_enabled_writes_one_row_per_recall() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let cfg_path = temp.path().join(".memhub/config.toml");
        let mut cfg = ProjectConfig::load(&cfg_path).expect("load");
        cfg.metrics.enabled = true;
        cfg.metrics.recall_proxy = true;
        cfg.save(&cfg_path).expect("save");

        seed(temp.path());

        let response = recall(
            temp.path(),
            RecallOptions {
                query: "agent originated writes review".to_string(),
                mode: Some(RetrievalMode::Fts),
                max_results: 5,
                source_types: vec![],
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                min_rerank_score: None,
                log_metrics: true,
            },
        )
        .expect("recall");
        assert!(!response.results.is_empty());

        let ctx = crate::db::open_project(temp.path()).expect("open");
        let mut stmt = ctx
            .conn
            .prepare(
                "SELECT query_hash, bundle_tokens, ledger_tokens, rerank_used, result_count \
                 FROM recall_metrics",
            )
            .expect("prepare");
        let rows: Vec<(String, i64, i64, i64, i64)> = stmt
            .query_map(params![], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect");

        assert_eq!(rows.len(), 1, "exactly one row per recall");
        let (query_hash, bundle_tokens, ledger_tokens, rerank_used, result_count) = &rows[0];
        assert_eq!(query_hash.len(), 64);
        assert!(*bundle_tokens > 0, "bundle_tokens must reflect returned hits");
        assert_eq!(
            *ledger_tokens, 0,
            "no rendered ledger in this tempdir, so ledger_tokens = 0"
        );
        assert_eq!(*rerank_used, 0, "fts mode never invokes the re-ranker");
        assert_eq!(*result_count, response.results.len() as i64);
    }

    #[test]
    fn metrics_log_opt_out_skips_insert_even_when_enabled() {
        // Eval and dashboard set log_metrics = false. Pin that
        // behavior so a future refactor can't silently start logging
        // calibration sweeps to the dashboard.
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let cfg_path = temp.path().join(".memhub/config.toml");
        let mut cfg = ProjectConfig::load(&cfg_path).expect("load");
        cfg.metrics.enabled = true;
        cfg.metrics.recall_proxy = true;
        cfg.save(&cfg_path).expect("save");

        seed(temp.path());

        recall(
            temp.path(),
            RecallOptions {
                query: "agent originated writes review".to_string(),
                mode: Some(RetrievalMode::Fts),
                max_results: 5,
                source_types: vec![],
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                min_rerank_score: None,
                log_metrics: false,
            },
        )
        .expect("recall");

        assert_eq!(count_recall_metrics(temp.path()), 0);
    }
}
