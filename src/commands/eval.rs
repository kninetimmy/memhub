//! `memhub eval retrieval` — Recall@K harness for the M8 retrieval surface (PR6).
//!
//! Loads a golden-query JSON file, runs each query through the same
//! `retrieval::recall` engine the CLI and MCP tools use, and reports
//! how many `match` queries placed the expected row in the top K and
//! how many `empty` queries kept their result bundle empty.
//!
//! Read-only: never mutates durable state or `writes_log`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::code_index::locate::{self, LocateHit, LocateOptions, LocateResponse};
use crate::config::{ProjectConfig, RetrievalMode};
use crate::db;
use crate::retrieval::{self, RecallHit, RecallOptions, RecallResponse};
use crate::{MemhubError, Result};

pub const DEFAULT_GOLDEN_PATH: &str = "tests/retrieval_golden.json";
pub const DEFAULT_CODE_GOLDEN_PATH: &str = "tests/code_locate_golden.json";
pub const DEFAULT_K: usize = 3;

#[derive(Debug, Deserialize, Serialize)]
pub struct GoldenFile {
    pub version: u32,
    #[serde(default)]
    pub description: String,
    pub queries: Vec<GoldenQuery>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct GoldenQuery {
    pub id: String,
    pub query: String,
    pub kind: GoldenKind,
    #[serde(default)]
    pub source_type: Option<String>,
    #[serde(default)]
    pub title_contains: Vec<String>,
    #[serde(default)]
    pub body_contains: Vec<String>,
    /// Further acceptable answers beyond the one described by the fields
    /// above. A `match` query passes when any hit in the top K satisfies
    /// any single answer. Omitted = single-answer query.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub also_accept: Vec<GoldenAnswer>,
    #[serde(default)]
    pub notes: String,
}

/// One extra acceptable answer for a `match` query; same matcher semantics
/// as the query's own `source_type` / `title_contains` / `body_contains`.
#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct GoldenAnswer {
    #[serde(default)]
    pub source_type: Option<String>,
    #[serde(default)]
    pub title_contains: Vec<String>,
    #[serde(default)]
    pub body_contains: Vec<String>,
}

#[derive(Debug, Deserialize, Serialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
pub enum GoldenKind {
    Match,
    Empty,
}

#[derive(Debug, Clone)]
pub struct EvalOptions {
    pub golden_path: PathBuf,
    pub k: usize,
    pub mode: Option<RetrievalMode>,
    /// Optional override of `[retrieval] use_reranker`. None = use project
    /// config; Some(false) forces the re-ranker off for the run; Some(true)
    /// forces it on regardless of config. Used for A/B harness runs.
    pub use_reranker: Option<bool>,
    /// Optional override of `[retrieval.scoring] min_rerank_score`. None
    /// = use project config. Ignored when mode resolves to fts or when
    /// the re-ranker is disabled. Used for cross-encoder floor
    /// calibration sweeps (decisions 70, 71).
    pub min_rerank_score: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct QueryOutcome {
    pub id: String,
    pub query: String,
    pub kind: GoldenKind,
    pub passed: bool,
    /// 1-indexed rank of the matching hit (Some) or None when no hit qualified.
    pub matched_rank: Option<usize>,
    /// Score of the matched hit, if any.
    pub matched_score: Option<f64>,
    /// The matched hit was a low-confidence fallback (every candidate fell
    /// below the relevance floor), not a hit that cleared its floor.
    pub matched_low_confidence: bool,
    /// Total returned by recall for this query.
    pub returned_count: usize,
    /// Optional reason string for failed outcomes (debug aid in markdown output).
    pub failure_reason: Option<String>,
}

/// The retrieval settings a run actually used (CLI overrides applied over
/// project config), recorded so `eval compare` can show what differed.
#[derive(Debug, Clone, Copy, Deserialize, Serialize, PartialEq)]
pub struct RunSettings {
    pub mode: RetrievalMode,
    /// The cross-encoder re-ranker was enabled for the run (hybrid mode
    /// and `use_reranker`); a trivially short candidate set can still skip
    /// it per query.
    pub reranker: bool,
    pub min_rerank_score: f32,
    pub rerank_candidate_pool: usize,
}

#[derive(Debug, Clone)]
pub struct EvalSummary {
    pub golden_path: PathBuf,
    pub mode: RetrievalMode,
    pub settings: RunSettings,
    pub k: usize,
    pub total_queries: usize,
    pub match_queries: usize,
    pub empty_queries: usize,
    pub match_passes: usize,
    /// Match queries whose first matching hit was ranked first.
    pub match_passes_at_1: usize,
    pub empty_passes: usize,
    pub recall_at_k: f64,
    pub recall_at_1: f64,
    pub safety_failures: usize,
    pub outcomes: Vec<QueryOutcome>,
    pub elapsed_ms: u128,
    /// Median (p50) of each golden query's own `RecallResponse.elapsed_ms`
    /// in this run, in milliseconds (Wave 4 R10, issue #74). "Warm"
    /// because a throwaway warm-up call (below, outside this timer) has
    /// already forced the embed + rerank ONNX models to load — the first
    /// call to either in this process pays a ~1-2s cold-start cost (see
    /// `retrieval::rerank`'s module doc) that would otherwise dominate the
    /// sample as a single outsized outlier. Report-only: unlike
    /// `recall_at_k`, nothing here fails the eval run — it exists so a
    /// 10x latency regression is visible without a hard threshold to tune.
    pub warm_latency_p50_ms: f64,
}

pub fn run_retrieval(start: &Path, opts: EvalOptions) -> Result<EvalSummary> {
    if opts.k == 0 {
        return Err(MemhubError::InvalidInput(
            "--k must be greater than zero".to_string(),
        ));
    }
    let golden = load_golden(&opts.golden_path)?;

    // Warm-up: one throwaway recall call, timed separately (outside
    // `started` below) and never scored, so the one-time ONNX cold-start
    // cost lands outside the per-query latency sample instead of
    // skewing it. Reuses the first `match`-kind query's own text so it
    // exercises the same embed+rerank path real queries do; recall()
    // only touches the ONNX models when the resolved mode is hybrid, so
    // this is a cheap no-op for an fts-mode run. Skipped (harmlessly) if
    // the golden file has no match query — `load_golden` already
    // guarantees at least one query, but not necessarily one of kind
    // match.
    if let Some(warm_query) = golden.queries.iter().find(|q| q.kind == GoldenKind::Match) {
        let _ = retrieval::recall(
            start,
            RecallOptions {
                query: warm_query.query.clone(),
                mode: opts.mode,
                max_results: opts.k,
                source_types: Vec::new(),
                include_stale: None,
                accepted_only: None,
                use_reranker: opts.use_reranker,
                min_rerank_score: opts.min_rerank_score,
                log_metrics: false,
                surface: None,
            },
        );
    }

    let started = Instant::now();
    let mut outcomes = Vec::with_capacity(golden.queries.len());
    let mut resolved_mode = opts.mode;
    let mut latencies_ms: Vec<u128> = Vec::with_capacity(golden.queries.len());

    for query in &golden.queries {
        let recall_opts = RecallOptions {
            query: query.query.clone(),
            mode: opts.mode,
            max_results: opts.k,
            source_types: Vec::new(),
            include_stale: None,
            accepted_only: None,
            use_reranker: opts.use_reranker,
            min_rerank_score: opts.min_rerank_score,
            // Calibration sweeps are not "real usage" — keep them out
            // of recall_metrics so the metrics numbers reflect
            // actual agent + user activity.
            log_metrics: false,
            surface: None,
        };
        let response = retrieval::recall(start, recall_opts)?;
        if resolved_mode.is_none() {
            resolved_mode = Some(response.mode);
        }
        latencies_ms.push(response.elapsed_ms);
        outcomes.push(evaluate_query(query, &response, opts.k));
    }

    let mode = resolved_mode.unwrap_or(RetrievalMode::Fts);
    // Mirrors `ResolvedOptions::from` in retrieval::recall. Read only after
    // the recall calls above, which create a missing config on open.
    let cfg = ProjectConfig::load(&db::discover_paths(start)?.config_path)?.retrieval;
    let settings = RunSettings {
        mode,
        reranker: mode == RetrievalMode::Hybrid && opts.use_reranker.unwrap_or(cfg.use_reranker),
        min_rerank_score: opts
            .min_rerank_score
            .unwrap_or(cfg.scoring.min_rerank_score),
        rerank_candidate_pool: cfg.rerank_candidate_pool.max(opts.k),
    };
    let warm_latency_p50_ms = median_ms(&latencies_ms);
    let summary = summarize(
        &golden,
        opts.k,
        settings,
        opts.golden_path.clone(),
        outcomes,
        started,
        warm_latency_p50_ms,
    );
    Ok(summary)
}

/// Median (p50) of a set of millisecond latencies. `0.0` for an empty
/// slice — never hit by `run_retrieval` in practice (`load_golden`
/// rejects a golden file with zero queries), kept total rather than
/// `Option` so callers don't have to unwrap a value that always exists
/// on the real call path.
fn median_ms(values: &[u128]) -> f64 {
    if values.is_empty() {
        return 0.0;
    }
    let mut sorted = values.to_vec();
    sorted.sort_unstable();
    let mid = sorted.len() / 2;
    if sorted.len() % 2 == 1 {
        sorted[mid] as f64
    } else {
        (sorted[mid - 1] as f64 + sorted[mid] as f64) / 2.0
    }
}

/// Read an eval input JSON file, tolerating a UTF-8 byte-order mark and
/// naming UTF-16 (what Windows PowerShell 5.1's `>` writes) and UTF-32 instead of
/// letting serde report a bare "expected value at line 1 column 1".
fn read_input_json(path: &Path, not_found: String) -> Result<Vec<u8>> {
    let mut bytes = fs::read(path).map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => MemhubError::InvalidInput(not_found),
        _ => MemhubError::from(err),
    })?;
    // UTF-32 first: its little-endian BOM (FF FE 00 00) begins with the UTF-16 one.
    if bytes.starts_with(&[0xFF, 0xFE, 0x00, 0x00]) || bytes.starts_with(&[0x00, 0x00, 0xFE, 0xFF])
    {
        return Err(MemhubError::InvalidInput(format!(
            "{} is UTF-32 encoded; re-save it as UTF-8",
            path.display()
        )));
    }
    if bytes.starts_with(&[0xFF, 0xFE]) || bytes.starts_with(&[0xFE, 0xFF]) {
        return Err(MemhubError::InvalidInput(format!(
            "{} is UTF-16 encoded; re-save it as UTF-8 (PowerShell 5.1 `>` writes UTF-16, use `| Out-File -Encoding utf8`)",
            path.display()
        )));
    }
    if bytes.starts_with(&[0xEF, 0xBB, 0xBF]) {
        bytes.drain(..3);
    }
    Ok(bytes)
}

pub fn load_golden(path: &Path) -> Result<GoldenFile> {
    let bytes = read_input_json(
        path,
        format!(
            "golden file not found at {} — pass --golden <path> or create one",
            path.display()
        ),
    )?;
    let parsed: GoldenFile = serde_json::from_slice(&bytes)?;
    if parsed.version != 1 {
        return Err(MemhubError::InvalidInput(format!(
            "unsupported golden file version {} in {}; expected 1",
            parsed.version,
            path.display(),
        )));
    }
    if parsed.queries.is_empty() {
        return Err(MemhubError::InvalidInput(format!(
            "golden file {} has zero queries",
            path.display(),
        )));
    }
    let mut seen = std::collections::HashSet::new();
    for q in &parsed.queries {
        validate_query(q)?;
        if !seen.insert(q.id.as_str()) {
            return Err(MemhubError::InvalidInput(format!(
                "golden file {} lists query id `{}` more than once",
                path.display(),
                q.id
            )));
        }
    }
    Ok(parsed)
}

fn validate_query(q: &GoldenQuery) -> Result<()> {
    if q.id.trim().is_empty() {
        return Err(MemhubError::InvalidInput(
            "golden query missing `id`".to_string(),
        ));
    }
    if q.query.trim().is_empty() {
        return Err(MemhubError::InvalidInput(format!(
            "golden query `{}` has empty `query`",
            q.id
        )));
    }
    validate_source_type(&q.id, q.source_type.as_deref())?;
    if q.kind == GoldenKind::Match
        && q.title_contains.is_empty()
        && q.body_contains.is_empty()
        && q.source_type.is_none()
    {
        return Err(MemhubError::InvalidInput(format!(
            "golden query `{}` is kind=match but has no matchers; every match query needs at least one of source_type / title_contains / body_contains",
            q.id
        )));
    }
    if q.kind == GoldenKind::Empty && !q.also_accept.is_empty() {
        return Err(MemhubError::InvalidInput(format!(
            "golden query `{}` is kind=empty but lists `also_accept` answers",
            q.id
        )));
    }
    for alt in &q.also_accept {
        validate_source_type(&q.id, alt.source_type.as_deref())?;
        if alt.title_contains.is_empty()
            && alt.body_contains.is_empty()
            && alt.source_type.is_none()
        {
            return Err(MemhubError::InvalidInput(format!(
                "golden query `{}` has an `also_accept` entry with no matchers; each needs at least one of source_type / title_contains / body_contains",
                q.id
            )));
        }
    }
    Ok(())
}

fn validate_source_type(id: &str, source_type: Option<&str>) -> Result<()> {
    match source_type {
        // `doc_chunk` added Wave 4 R10 (issue #74) so the golden set
        // can pin doc-chunk hits to source type, same as the other
        // three durable rows.
        // `arch_section` added with issue #232's default-recall source.
        None | Some("fact" | "decision" | "task" | "doc_chunk" | "arch_section") => Ok(()),
        Some(other) => Err(MemhubError::InvalidInput(format!(
            "golden query `{id}` has unknown source_type `{other}` (expected fact|decision|task|doc_chunk|arch_section)"
        ))),
    }
}

pub fn evaluate_query(query: &GoldenQuery, response: &RecallResponse, k: usize) -> QueryOutcome {
    let returned_count = response.results.len();
    match query.kind {
        GoldenKind::Empty => {
            // Low-confidence fallback hits (issue #225) are allowed: they
            // are flagged, not vouched for. Only a hit that cleared its
            // relevance floor leaks.
            let vouched = response.results.iter().filter(|hit| !hit.low_confidence);
            if vouched.clone().next().is_none() {
                QueryOutcome {
                    id: query.id.clone(),
                    query: query.query.clone(),
                    kind: GoldenKind::Empty,
                    passed: true,
                    matched_rank: None,
                    matched_score: None,
                    matched_low_confidence: false,
                    returned_count,
                    failure_reason: None,
                }
            } else {
                let leaked: Vec<String> = vouched
                    .take(k.min(3))
                    .map(|hit| format!("{}#{}", hit.source_type, hit.source_id))
                    .collect();
                QueryOutcome {
                    id: query.id.clone(),
                    query: query.query.clone(),
                    kind: GoldenKind::Empty,
                    passed: false,
                    matched_rank: None,
                    matched_score: None,
                    matched_low_confidence: false,
                    returned_count,
                    failure_reason: Some(format!(
                        "expected empty bundle (no hit clearing the relevance floor) but recall returned {} hit(s): {}",
                        returned_count,
                        leaked.join(", "),
                    )),
                }
            }
        }
        GoldenKind::Match => {
            let limit = k.min(response.results.len());
            for hit in response.results.iter().take(limit) {
                if hit_matches(query, hit) {
                    return QueryOutcome {
                        id: query.id.clone(),
                        query: query.query.clone(),
                        kind: GoldenKind::Match,
                        passed: true,
                        matched_rank: Some(hit.rank),
                        matched_score: Some(hit.score),
                        matched_low_confidence: hit.low_confidence,
                        returned_count,
                        failure_reason: None,
                    };
                }
            }
            let reason = if returned_count == 0 {
                "recall returned no results".to_string()
            } else {
                let top: Vec<String> = response
                    .results
                    .iter()
                    .take(limit)
                    .map(|hit| {
                        format!(
                            "{}#{}:{}",
                            hit.source_type,
                            hit.source_id,
                            truncate(&hit.title, 40)
                        )
                    })
                    .collect();
                format!(
                    "no top-{} hit matched (returned {}): {}",
                    k,
                    returned_count,
                    top.join(" | "),
                )
            };
            QueryOutcome {
                id: query.id.clone(),
                query: query.query.clone(),
                kind: GoldenKind::Match,
                passed: false,
                matched_rank: None,
                matched_score: None,
                matched_low_confidence: false,
                returned_count,
                failure_reason: Some(reason),
            }
        }
    }
}

fn hit_matches(query: &GoldenQuery, hit: &RecallHit) -> bool {
    answer_matches(
        query.source_type.as_deref(),
        &query.title_contains,
        &query.body_contains,
        hit,
    ) || query.also_accept.iter().any(|alt| {
        answer_matches(
            alt.source_type.as_deref(),
            &alt.title_contains,
            &alt.body_contains,
            hit,
        )
    })
}

fn answer_matches(
    source_type: Option<&str>,
    title_contains: &[String],
    body_contains: &[String],
    hit: &RecallHit,
) -> bool {
    if let Some(expected) = source_type
        && hit.source_type != expected
    {
        return false;
    }
    let title_lower = hit.title.to_lowercase();
    for needle in title_contains {
        if !title_lower.contains(&needle.to_lowercase()) {
            return false;
        }
    }
    let body_lower = hit.body.to_lowercase();
    for needle in body_contains {
        if !body_lower.contains(&needle.to_lowercase()) {
            return false;
        }
    }
    true
}

fn truncate(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max).collect();
        out.push('…');
        out
    }
}

fn summarize(
    golden: &GoldenFile,
    k: usize,
    settings: RunSettings,
    golden_path: PathBuf,
    outcomes: Vec<QueryOutcome>,
    started: Instant,
    warm_latency_p50_ms: f64,
) -> EvalSummary {
    let total_queries = golden.queries.len();
    let mut match_queries = 0usize;
    let mut empty_queries = 0usize;
    let mut match_passes = 0usize;
    let mut match_passes_at_1 = 0usize;
    let mut empty_passes = 0usize;
    let mut safety_failures = 0usize;
    for outcome in &outcomes {
        match outcome.kind {
            GoldenKind::Match => {
                match_queries += 1;
                if outcome.passed {
                    match_passes += 1;
                }
                if outcome.matched_rank == Some(1) {
                    match_passes_at_1 += 1;
                }
            }
            GoldenKind::Empty => {
                empty_queries += 1;
                if outcome.passed {
                    empty_passes += 1;
                } else {
                    safety_failures += 1;
                }
            }
        }
    }
    let recall_at_k = ratio(match_passes, match_queries);
    let recall_at_1 = ratio(match_passes_at_1, match_queries);
    EvalSummary {
        golden_path,
        mode: settings.mode,
        settings,
        k,
        total_queries,
        match_queries,
        empty_queries,
        match_passes,
        match_passes_at_1,
        empty_passes,
        recall_at_k,
        recall_at_1,
        safety_failures,
        outcomes,
        elapsed_ms: started.elapsed().as_millis(),
        warm_latency_p50_ms,
    }
}

// ---------------------------------------------------------------------------
// `memhub eval locate` — code-locator Recall@K harness (M11 PR5, task 65).
//
// Mirrors the retrieval harness above but over the sibling code index: each
// query runs through the same `code_index::locate::locate` the CLI/MCP
// surfaces use, and a hit "matches" when its repo-relative `path` (and
// optional `symbol`) contains the golden substrings. The bar is decision
// 107's deliberately-lossy one — the expected FILE in the top K, precision
// recovered by the agent's real Read — so this reports both Recall@1 and
// Recall@K.
//
// The cross-encoder floor is applied HARNESS-SIDE: `min_rerank_score` filters
// the returned hits by their `rerank_score` before scoring, so a floor can be
// A/B-swept without touching the `locate` runtime (which has no floor by
// design, decision 107). A floor is only meaningful with `--rerank` on; it is
// ignored otherwise, mirroring the retrieval harness's reranker-gated rule.
// ---------------------------------------------------------------------------

#[derive(Debug, Deserialize, Serialize)]
pub struct LocateGoldenFile {
    pub version: u32,
    #[serde(default)]
    pub description: String,
    pub queries: Vec<LocateGoldenQuery>,
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct LocateGoldenQuery {
    pub id: String,
    pub query: String,
    pub kind: GoldenKind,
    /// Substrings that must all appear (case-insensitive) in the hit's
    /// repo-relative, forward-slashed path.
    #[serde(default)]
    pub path_contains: Vec<String>,
    /// Optional substring the hit's symbol must contain. `None` keeps the
    /// bar at file granularity (decision 107's right-file-in-top-K rule).
    #[serde(default)]
    pub symbol_contains: Option<String>,
    #[serde(default)]
    pub notes: String,
}

#[derive(Debug, Clone)]
pub struct LocateEvalOptions {
    pub golden_path: PathBuf,
    pub k: usize,
    /// Run the bundled cross-encoder over the candidate pool. Off by default
    /// (mirrors `memhub locate`); flip on for the A/B against fusion-only.
    pub use_reranker: bool,
    /// Harness-side cross-encoder floor: drop returned hits whose rerank
    /// logit is below this before scoring. Ignored when the reranker is off.
    pub min_rerank_score: Option<f32>,
}

#[derive(Debug, Clone)]
pub struct LocateQueryOutcome {
    pub id: String,
    pub query: String,
    pub kind: GoldenKind,
    /// Passed at Recall@K (top-K contains the expected file).
    pub passed: bool,
    /// Passed at Recall@1 (the very first surviving hit is the expected file).
    pub passed_at_1: bool,
    /// 1-indexed position (within the post-floor list) of the matching hit.
    pub matched_rank: Option<usize>,
    /// Fusion score of the matched hit.
    pub matched_score: Option<f64>,
    /// Cross-encoder logit of the matched hit, when reranking ran.
    pub matched_rerank: Option<f32>,
    /// Hits surviving the floor for this query.
    pub returned_count: usize,
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct LocateEvalSummary {
    pub golden_path: PathBuf,
    pub mode: RetrievalMode,
    pub k: usize,
    pub reranked: bool,
    pub min_rerank_score: Option<f32>,
    pub total_queries: usize,
    pub match_queries: usize,
    pub empty_queries: usize,
    pub match_passes_at_1: usize,
    pub match_passes_at_k: usize,
    pub empty_passes: usize,
    pub recall_at_1: f64,
    pub recall_at_k: f64,
    pub safety_failures: usize,
    pub outcomes: Vec<LocateQueryOutcome>,
    pub elapsed_ms: u128,
}

pub fn run_locate(start: &Path, opts: LocateEvalOptions) -> Result<LocateEvalSummary> {
    if opts.k == 0 {
        return Err(MemhubError::InvalidInput(
            "--k must be greater than zero".to_string(),
        ));
    }
    let golden = load_locate_golden(&opts.golden_path)?;
    let started = Instant::now();
    let mut outcomes = Vec::with_capacity(golden.queries.len());
    let mut resolved_mode: Option<RetrievalMode> = None;

    for query in &golden.queries {
        let locate_opts = LocateOptions {
            query: query.query.clone(),
            limit: opts.k,
            use_reranker: opts.use_reranker,
            // Eval measures retrieval quality against a fully refreshed
            // index, never a stale one — `--no-refresh` is a CLI-only
            // opt-in (issue #67), not something an eval run should touch.
            no_refresh: false,
        };
        let response = locate::locate(start, locate_opts)?;
        if resolved_mode.is_none() {
            resolved_mode = Some(response.mode);
        }
        outcomes.push(evaluate_locate_query(
            query,
            &response,
            opts.k,
            opts.use_reranker,
            opts.min_rerank_score,
        ));
    }

    let mode = resolved_mode.unwrap_or(RetrievalMode::Hybrid);
    Ok(summarize_locate(&golden, mode, opts, outcomes, started))
}

pub fn load_locate_golden(path: &Path) -> Result<LocateGoldenFile> {
    let bytes = read_input_json(
        path,
        format!(
            "code golden file not found at {} — pass --golden <path> or create one",
            path.display()
        ),
    )?;
    let parsed: LocateGoldenFile = serde_json::from_slice(&bytes)?;
    if parsed.version != 1 {
        return Err(MemhubError::InvalidInput(format!(
            "unsupported code golden file version {} in {}; expected 1",
            parsed.version,
            path.display(),
        )));
    }
    if parsed.queries.is_empty() {
        return Err(MemhubError::InvalidInput(format!(
            "code golden file {} has zero queries",
            path.display(),
        )));
    }
    for q in &parsed.queries {
        validate_locate_query(q)?;
    }
    Ok(parsed)
}

fn validate_locate_query(q: &LocateGoldenQuery) -> Result<()> {
    if q.id.trim().is_empty() {
        return Err(MemhubError::InvalidInput(
            "code golden query missing `id`".to_string(),
        ));
    }
    if q.query.trim().is_empty() {
        return Err(MemhubError::InvalidInput(format!(
            "code golden query `{}` has empty `query`",
            q.id
        )));
    }
    if q.kind == GoldenKind::Match
        && q.path_contains.iter().all(|s| s.trim().is_empty())
        && q.symbol_contains.as_deref().is_none_or(str::is_empty)
    {
        return Err(MemhubError::InvalidInput(format!(
            "code golden query `{}` is kind=match but has no matchers; every match query needs at least one of path_contains / symbol_contains",
            q.id
        )));
    }
    Ok(())
}

/// Filter a locate response's hits by the harness-side rerank floor, then
/// score the survivors. The floor only applies when the reranker actually
/// ran (so the hits carry logits) and a threshold was given.
pub fn evaluate_locate_query(
    query: &LocateGoldenQuery,
    response: &LocateResponse,
    k: usize,
    reranker_on: bool,
    min_rerank_score: Option<f32>,
) -> LocateQueryOutcome {
    let floor = if reranker_on { min_rerank_score } else { None };
    let survivors: Vec<&LocateHit> = response
        .results
        .iter()
        .filter(|hit| match floor {
            Some(threshold) => hit.rerank_score.is_some_and(|s| s >= threshold),
            None => true,
        })
        .collect();
    let returned_count = survivors.len();

    match query.kind {
        GoldenKind::Empty => {
            if returned_count == 0 {
                empty_outcome(query, true, returned_count, None)
            } else {
                let leaked: Vec<String> = survivors
                    .iter()
                    .take(k.min(3))
                    .map(|hit| format!("{}:{}", hit.path, hit.start_line))
                    .collect();
                empty_outcome(
                    query,
                    false,
                    returned_count,
                    Some(format!(
                        "expected empty bundle but locate returned {} hit(s): {}",
                        returned_count,
                        leaked.join(", "),
                    )),
                )
            }
        }
        GoldenKind::Match => {
            let limit = k.min(survivors.len());
            for (idx, hit) in survivors.iter().take(limit).enumerate() {
                if locate_hit_matches(query, hit) {
                    let rank = idx + 1;
                    return LocateQueryOutcome {
                        id: query.id.clone(),
                        query: query.query.clone(),
                        kind: GoldenKind::Match,
                        passed: true,
                        passed_at_1: rank == 1,
                        matched_rank: Some(rank),
                        matched_score: Some(hit.score),
                        matched_rerank: hit.rerank_score,
                        returned_count,
                        failure_reason: None,
                    };
                }
            }
            let reason = if returned_count == 0 {
                "locate returned no results".to_string()
            } else {
                let top: Vec<String> = survivors
                    .iter()
                    .take(limit)
                    .map(|hit| {
                        format!(
                            "{}:{}{}",
                            hit.path,
                            hit.start_line,
                            hit.symbol
                                .as_deref()
                                .map(|s| format!(" [{s}]"))
                                .unwrap_or_default(),
                        )
                    })
                    .collect();
                format!(
                    "no top-{} hit matched (returned {}): {}",
                    k,
                    returned_count,
                    top.join(" | "),
                )
            };
            LocateQueryOutcome {
                id: query.id.clone(),
                query: query.query.clone(),
                kind: GoldenKind::Match,
                passed: false,
                passed_at_1: false,
                matched_rank: None,
                matched_score: None,
                matched_rerank: None,
                returned_count,
                failure_reason: Some(reason),
            }
        }
    }
}

fn empty_outcome(
    query: &LocateGoldenQuery,
    passed: bool,
    returned_count: usize,
    failure_reason: Option<String>,
) -> LocateQueryOutcome {
    LocateQueryOutcome {
        id: query.id.clone(),
        query: query.query.clone(),
        kind: GoldenKind::Empty,
        passed,
        passed_at_1: passed,
        matched_rank: None,
        matched_score: None,
        matched_rerank: None,
        returned_count,
        failure_reason,
    }
}

fn locate_hit_matches(query: &LocateGoldenQuery, hit: &LocateHit) -> bool {
    let path_lower = hit.path.to_lowercase();
    for needle in &query.path_contains {
        if !path_lower.contains(&needle.to_lowercase()) {
            return false;
        }
    }
    if let Some(sym_needle) = query.symbol_contains.as_deref().filter(|s| !s.is_empty()) {
        match hit.symbol.as_deref() {
            Some(symbol) if symbol.to_lowercase().contains(&sym_needle.to_lowercase()) => {}
            _ => return false,
        }
    }
    true
}

fn summarize_locate(
    golden: &LocateGoldenFile,
    mode: RetrievalMode,
    opts: LocateEvalOptions,
    outcomes: Vec<LocateQueryOutcome>,
    started: Instant,
) -> LocateEvalSummary {
    let total_queries = golden.queries.len();
    let mut match_queries = 0usize;
    let mut empty_queries = 0usize;
    let mut match_passes_at_1 = 0usize;
    let mut match_passes_at_k = 0usize;
    let mut empty_passes = 0usize;
    let mut safety_failures = 0usize;
    for outcome in &outcomes {
        match outcome.kind {
            GoldenKind::Match => {
                match_queries += 1;
                if outcome.passed {
                    match_passes_at_k += 1;
                }
                if outcome.passed_at_1 {
                    match_passes_at_1 += 1;
                }
            }
            GoldenKind::Empty => {
                empty_queries += 1;
                if outcome.passed {
                    empty_passes += 1;
                } else {
                    safety_failures += 1;
                }
            }
        }
    }
    let recall_at_1 = ratio(match_passes_at_1, match_queries);
    let recall_at_k = ratio(match_passes_at_k, match_queries);
    LocateEvalSummary {
        golden_path: opts.golden_path,
        mode,
        k: opts.k,
        reranked: opts.use_reranker,
        min_rerank_score: opts.min_rerank_score,
        total_queries,
        match_queries,
        empty_queries,
        match_passes_at_1,
        match_passes_at_k,
        empty_passes,
        recall_at_1,
        recall_at_k,
        safety_failures,
        outcomes,
        elapsed_ms: started.elapsed().as_millis(),
    }
}

fn ratio(passes: usize, total: usize) -> f64 {
    if total == 0 {
        0.0
    } else {
        passes as f64 / total as f64
    }
}

// ---------------------------------------------------------------------------
// `memhub eval compare` — paired per-query comparison of two saved
// `memhub eval retrieval --json` runs. Works on saved output (not config
// flags) so it also compares a code change: run A from the old binary, run
// B from the new one. Pure: reads two files, writes nothing (decision 51).
// ---------------------------------------------------------------------------

/// The subset of `eval retrieval --json` that `compare` reads. Runs saved
/// before `settings` existed still load; their settings show as not recorded.
#[derive(Debug, Deserialize)]
struct SavedRun {
    k: usize,
    mode: RetrievalMode,
    #[serde(default)]
    settings: Option<RunSettings>,
    outcomes: Vec<SavedOutcome>,
}

#[derive(Debug, Deserialize)]
struct SavedOutcome {
    id: String,
    query: String,
    kind: GoldenKind,
    passed: bool,
    #[serde(default)]
    matched_rank: Option<usize>,
}

#[derive(Debug, Serialize)]
pub struct RunInfo {
    pub path: String,
    pub mode: RetrievalMode,
    pub settings: Option<RunSettings>,
}

/// Paired result over the match queries for one pass criterion.
#[derive(Debug, Default, Serialize)]
pub struct PairedResult {
    pub passes_a: usize,
    pub passes_b: usize,
    /// Ids that passed only in run A / only in run B (the discordant pairs).
    pub only_a: Vec<String>,
    pub only_b: Vec<String>,
    /// Exact two-sided McNemar p-value over the discordant pairs.
    pub p_value: f64,
}

impl PairedResult {
    fn tally(&mut self, id: &str, in_a: bool, in_b: bool) {
        self.passes_a += usize::from(in_a);
        self.passes_b += usize::from(in_b);
        match (in_a, in_b) {
            (true, false) => self.only_a.push(id.to_string()),
            (false, true) => self.only_b.push(id.to_string()),
            _ => {}
        }
    }
}

#[derive(Debug, Serialize)]
pub struct EmptyProbeDiff {
    pub id: String,
    /// Which run returned a hit for the probe ("a" or "b"); the other kept
    /// its bundle empty.
    pub hit_in: &'static str,
}

#[derive(Debug, Serialize)]
pub struct Comparison {
    pub k: usize,
    pub a: RunInfo,
    pub b: RunInfo,
    pub match_queries: usize,
    pub empty_queries: usize,
    pub rank_1: PairedResult,
    pub found_at_k: PairedResult,
    pub empty_probe_diffs: Vec<EmptyProbeDiff>,
}

fn load_saved_run(path: &Path) -> Result<SavedRun> {
    let bytes = read_input_json(
        path,
        format!("saved eval run not found at {}", path.display()),
    )?;
    let run: SavedRun = serde_json::from_slice(&bytes).map_err(|err| {
        MemhubError::InvalidInput(format!(
            "{} is not a `memhub eval retrieval --json` output: {err}",
            path.display()
        ))
    })?;
    let mut seen = std::collections::HashSet::new();
    for o in &run.outcomes {
        if !seen.insert(o.id.as_str()) {
            return Err(MemhubError::InvalidInput(format!(
                "{} lists query id `{}` more than once",
                path.display(),
                o.id
            )));
        }
    }
    Ok(run)
}

pub fn compare_runs(path_a: &Path, path_b: &Path) -> Result<Comparison> {
    let a = load_saved_run(path_a)?;
    let b = load_saved_run(path_b)?;
    if a.k != b.k {
        return Err(MemhubError::InvalidInput(format!(
            "runs used different K: {} has K={}, {} has K={}",
            path_a.display(),
            a.k,
            path_b.display(),
            b.k
        )));
    }
    let b_by_id: std::collections::HashMap<&str, &SavedOutcome> =
        b.outcomes.iter().map(|o| (o.id.as_str(), o)).collect();
    let a_ids: std::collections::HashSet<&str> = a.outcomes.iter().map(|o| o.id.as_str()).collect();
    let only_in_a: Vec<&str> = a
        .outcomes
        .iter()
        .map(|o| o.id.as_str())
        .filter(|id| !b_by_id.contains_key(id))
        .collect();
    let only_in_b: Vec<&str> = b
        .outcomes
        .iter()
        .map(|o| o.id.as_str())
        .filter(|id| !a_ids.contains(id))
        .collect();
    if !only_in_a.is_empty() || !only_in_b.is_empty() {
        return Err(MemhubError::InvalidInput(format!(
            "runs cover different query ids: only in {}: [{}]; only in {}: [{}]",
            path_a.display(),
            only_in_a.join(", "),
            path_b.display(),
            only_in_b.join(", ")
        )));
    }

    let mut match_queries = 0usize;
    let mut empty_queries = 0usize;
    let mut rank_1 = PairedResult::default();
    let mut found = PairedResult::default();
    let mut empty_probe_diffs = Vec::new();
    for oa in &a.outcomes {
        let ob = b_by_id[oa.id.as_str()];
        if oa.query != ob.query {
            return Err(MemhubError::InvalidInput(format!(
                "query `{}` has different text in the two runs: {:?} vs {:?}",
                oa.id, oa.query, ob.query
            )));
        }
        if oa.kind != ob.kind {
            return Err(MemhubError::InvalidInput(format!(
                "query `{}` has a different kind in the two runs",
                oa.id
            )));
        }
        match oa.kind {
            GoldenKind::Match => {
                match_queries += 1;
                rank_1.tally(
                    &oa.id,
                    oa.matched_rank == Some(1),
                    ob.matched_rank == Some(1),
                );
                found.tally(&oa.id, oa.passed, ob.passed);
            }
            GoldenKind::Empty => {
                empty_queries += 1;
                if oa.passed != ob.passed {
                    empty_probe_diffs.push(EmptyProbeDiff {
                        id: oa.id.clone(),
                        hit_in: if oa.passed { "b" } else { "a" },
                    });
                }
            }
        }
    }
    rank_1.p_value = mcnemar_exact_p(rank_1.only_a.len(), rank_1.only_b.len());
    found.p_value = mcnemar_exact_p(found.only_a.len(), found.only_b.len());
    Ok(Comparison {
        k: a.k,
        a: RunInfo {
            path: path_a.display().to_string(),
            mode: a.mode,
            settings: a.settings,
        },
        b: RunInfo {
            path: path_b.display().to_string(),
            mode: b.mode,
            settings: b.settings,
        },
        match_queries,
        empty_queries,
        rank_1,
        found_at_k: found,
        empty_probe_diffs,
    })
}

/// Exact two-sided McNemar p-value from the discordant counts (`only_a`,
/// `only_b`): `min(1, 2 * P(X <= min) )` for `X ~ Binomial(n, 1/2)`,
/// `n = only_a + only_b`; 1 when there are no discordant pairs. Summed in
/// log space because `2^n` overflows f64 for n > ~1000.
pub fn mcnemar_exact_p(only_a: usize, only_b: usize) -> f64 {
    let n = only_a + only_b;
    if n == 0 {
        return 1.0;
    }
    let k = only_a.min(only_b);
    // ln(C(n, i) / 2^n), advanced term by term; terms that underflow to 0
    // are negligible against the tail they sit beside.
    let mut log_term = -(n as f64) * std::f64::consts::LN_2;
    let mut tail = 0.0;
    for i in 0..=k {
        tail += log_term.exp();
        log_term += (((n - i) as f64) / ((i + 1) as f64)).ln();
    }
    (2.0 * tail).min(1.0)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::retrieval::{RecallHit, RecallResponse, RecallWarning};

    fn fake_response(query: &str, results: Vec<RecallHit>) -> RecallResponse {
        RecallResponse {
            query: query.to_string(),
            mode: RetrievalMode::Fts,
            results,
            candidate_count: 0,
            returned_count: 0,
            warnings: Vec::<RecallWarning>::new(),
            matcher: "recall:fts".to_string(),
            elapsed_ms: 0,
            available_docs: 0,
        }
    }

    fn hit(rank: usize, source_type: &str, source_id: i64, title: &str, body: &str) -> RecallHit {
        RecallHit {
            rank,
            source_type: source_type.to_string(),
            scope: "repo".to_string(),
            source_id,
            title: title.to_string(),
            body: body.to_string(),
            score: 0.9,
            fts_score: 0.9,
            vector_score: 0.0,
            stale: false,
            superseded_by: None,
            source: "user".to_string(),
            created_at: "2026-05-13".to_string(),
            rerank_score: None,
            low_confidence: false,
            kind: None,
        }
    }

    #[test]
    fn match_passes_when_top_k_contains_expected_row() {
        let q = GoldenQuery {
            id: "x".into(),
            query: "test".into(),
            kind: GoldenKind::Match,
            source_type: Some("decision".into()),
            title_contains: vec!["recall".into(), "read-only".into()],
            body_contains: vec!["writes_log".into()],
            also_accept: vec![],
            notes: String::new(),
        };
        let resp = fake_response(
            "test",
            vec![hit(
                1,
                "decision",
                48,
                "memhub recall is read-only and never writes to writes_log",
                "Recall fetches FTS hits and never inserts into writes_log.",
            )],
        );
        let outcome = evaluate_query(&q, &resp, 3);
        assert!(outcome.passed, "{:?}", outcome.failure_reason);
        assert_eq!(outcome.matched_rank, Some(1));
    }

    #[test]
    fn match_fails_when_match_outside_top_k() {
        let q = GoldenQuery {
            id: "x".into(),
            query: "test".into(),
            kind: GoldenKind::Match,
            source_type: Some("decision".into()),
            title_contains: vec!["recall".into()],
            body_contains: vec![],
            also_accept: vec![],
            notes: String::new(),
        };
        let resp = fake_response(
            "test",
            vec![
                hit(1, "fact", 1, "build-command", "cargo build"),
                hit(2, "fact", 2, "test-command", "cargo test"),
                hit(3, "task", 1, "Ship something", "irrelevant"),
                hit(
                    4,
                    "decision",
                    48,
                    "memhub recall is read-only",
                    "irrelevant",
                ),
            ],
        );
        let outcome = evaluate_query(&q, &resp, 3);
        assert!(!outcome.passed);
        assert!(outcome.failure_reason.unwrap().contains("no top-3"));
    }

    #[test]
    fn match_requires_all_substrings_in_same_hit() {
        let q = GoldenQuery {
            id: "x".into(),
            query: "test".into(),
            kind: GoldenKind::Match,
            source_type: Some("decision".into()),
            title_contains: vec!["recall".into(), "ledger".into()],
            body_contains: vec![],
            also_accept: vec![],
            notes: String::new(),
        };
        let resp = fake_response(
            "test",
            vec![
                hit(
                    1,
                    "decision",
                    48,
                    "memhub recall is read-only",
                    "no ledger here",
                ),
                hit(
                    2,
                    "decision",
                    34,
                    "Agents prefer recall over reading the ledger",
                    "ok",
                ),
            ],
        );
        let outcome = evaluate_query(&q, &resp, 3);
        assert!(outcome.passed);
        assert_eq!(outcome.matched_rank, Some(2));
    }

    #[test]
    fn empty_probe_allows_low_confidence_hits_but_not_floor_clearing_ones() {
        let q = GoldenQuery {
            id: "near".into(),
            query: "adjacent topic".into(),
            kind: GoldenKind::Empty,
            source_type: None,
            title_contains: vec![],
            body_contains: vec![],
            also_accept: vec![],
            notes: String::new(),
        };
        let mut low = hit(1, "fact", 1, "build-command", "cargo build");
        low.low_confidence = true;
        let only_low = fake_response("adjacent topic", vec![low.clone()]);
        let outcome = evaluate_query(&q, &only_low, 3);
        assert!(outcome.passed, "{:?}", outcome.failure_reason);

        let cleared = hit(2, "fact", 2, "test-command", "cargo test");
        let mixed = fake_response("adjacent topic", vec![low, cleared]);
        let outcome = evaluate_query(&q, &mixed, 3);
        assert!(!outcome.passed);
        let reason = outcome.failure_reason.unwrap();
        assert!(
            reason.contains("fact#2") && !reason.contains("fact#1"),
            "{reason}"
        );
    }

    #[test]
    fn match_outcome_records_whether_the_matched_hit_was_low_confidence() {
        let q = GoldenQuery {
            id: "kw".into(),
            query: "cargo compile command".into(),
            kind: GoldenKind::Match,
            source_type: Some("fact".into()),
            title_contains: vec!["build".into()],
            body_contains: vec![],
            also_accept: vec![],
            notes: String::new(),
        };
        let mut h = hit(1, "fact", 1, "build-command", "cargo build");
        h.low_confidence = true;
        let outcome = evaluate_query(&q, &fake_response("q", vec![h]), 3);
        assert!(outcome.passed);
        assert!(outcome.matched_low_confidence);

        let h = hit(1, "fact", 1, "build-command", "cargo build");
        let outcome = evaluate_query(&q, &fake_response("q", vec![h]), 3);
        assert!(outcome.passed);
        assert!(!outcome.matched_low_confidence);
    }

    #[test]
    fn empty_passes_when_results_empty() {
        let q = GoldenQuery {
            id: "neg".into(),
            query: "zxqv".into(),
            kind: GoldenKind::Empty,
            source_type: None,
            title_contains: vec![],
            body_contains: vec![],
            also_accept: vec![],
            notes: String::new(),
        };
        let resp = fake_response("zxqv", vec![]);
        let outcome = evaluate_query(&q, &resp, 3);
        assert!(outcome.passed);
        assert!(outcome.failure_reason.is_none());
    }

    #[test]
    fn empty_fails_when_any_result_leaks() {
        let q = GoldenQuery {
            id: "neg".into(),
            query: "zxqv".into(),
            kind: GoldenKind::Empty,
            source_type: None,
            title_contains: vec![],
            body_contains: vec![],
            also_accept: vec![],
            notes: String::new(),
        };
        let resp = fake_response(
            "zxqv",
            vec![hit(1, "fact", 1, "build-command", "cargo build")],
        );
        let outcome = evaluate_query(&q, &resp, 3);
        assert!(!outcome.passed);
        let reason = outcome.failure_reason.expect("reason");
        assert!(reason.contains("expected empty"), "{reason}");
        assert!(reason.contains("fact#1"), "{reason}");
    }

    #[test]
    fn load_golden_rejects_unsupported_version() {
        use std::io::Write;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("golden.json");
        let mut f = std::fs::File::create(&path).expect("create");
        writeln!(f, r#"{{ "version": 99, "queries": [] }}"#).expect("write");
        let err = load_golden(&path).expect_err("version mismatch");
        assert!(matches!(err, MemhubError::InvalidInput(_)));
    }

    #[test]
    fn load_golden_rejects_empty_queries_array() {
        use std::io::Write;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("golden.json");
        let mut f = std::fs::File::create(&path).expect("create");
        writeln!(f, r#"{{ "version": 1, "queries": [] }}"#).expect("write");
        let err = load_golden(&path).expect_err("empty queries");
        assert!(matches!(err, MemhubError::InvalidInput(_)));
    }

    #[test]
    fn load_golden_rejects_match_without_matchers() {
        use std::io::Write;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("golden.json");
        let mut f = std::fs::File::create(&path).expect("create");
        writeln!(
            f,
            r#"{{
                "version": 1,
                "queries": [
                    {{ "id": "x", "query": "q", "kind": "match" }}
                ]
            }}"#
        )
        .expect("write");
        let err = load_golden(&path).expect_err("no matchers");
        assert!(matches!(err, MemhubError::InvalidInput(_)));
    }

    #[test]
    fn summary_computes_recall_and_safety() {
        let outcomes = vec![
            QueryOutcome {
                id: "a".into(),
                query: "q".into(),
                kind: GoldenKind::Match,
                passed: true,
                matched_rank: Some(1),
                matched_score: Some(0.9),
                matched_low_confidence: false,
                returned_count: 1,
                failure_reason: None,
            },
            QueryOutcome {
                id: "b".into(),
                query: "q".into(),
                kind: GoldenKind::Match,
                passed: false,
                matched_rank: None,
                matched_score: None,
                matched_low_confidence: false,
                returned_count: 0,
                failure_reason: Some("no hit".into()),
            },
            QueryOutcome {
                id: "n".into(),
                query: "q".into(),
                kind: GoldenKind::Empty,
                passed: false,
                matched_rank: None,
                matched_score: None,
                matched_low_confidence: false,
                returned_count: 2,
                failure_reason: Some("leaked".into()),
            },
        ];
        let golden = GoldenFile {
            version: 1,
            description: String::new(),
            queries: vec![
                GoldenQuery {
                    id: "a".into(),
                    query: "q".into(),
                    kind: GoldenKind::Match,
                    source_type: None,
                    title_contains: vec!["x".into()],
                    body_contains: vec![],
                    also_accept: vec![],
                    notes: String::new(),
                },
                GoldenQuery {
                    id: "b".into(),
                    query: "q".into(),
                    kind: GoldenKind::Match,
                    source_type: None,
                    title_contains: vec!["x".into()],
                    body_contains: vec![],
                    also_accept: vec![],
                    notes: String::new(),
                },
                GoldenQuery {
                    id: "n".into(),
                    query: "q".into(),
                    kind: GoldenKind::Empty,
                    source_type: None,
                    title_contains: vec![],
                    body_contains: vec![],
                    also_accept: vec![],
                    notes: String::new(),
                },
            ],
        };
        let summary = summarize(
            &golden,
            3,
            RunSettings {
                mode: RetrievalMode::Fts,
                reranker: false,
                min_rerank_score: -1.0,
                rerank_candidate_pool: 20,
            },
            PathBuf::from("x.json"),
            outcomes,
            Instant::now(),
            12.5,
        );
        assert_eq!(summary.total_queries, 3);
        assert_eq!(summary.match_queries, 2);
        assert_eq!(summary.empty_queries, 1);
        assert_eq!(summary.match_passes, 1);
        assert_eq!(summary.empty_passes, 0);
        assert_eq!(summary.safety_failures, 1);
        assert!((summary.recall_at_k - 0.5).abs() < 1e-9);
        assert!((summary.warm_latency_p50_ms - 12.5).abs() < 1e-9);
    }

    #[test]
    fn median_ms_empty_is_zero() {
        assert_eq!(median_ms(&[]), 0.0);
    }

    #[test]
    fn median_ms_odd_count_is_middle_value() {
        assert!((median_ms(&[30, 10, 20]) - 20.0).abs() < 1e-9);
    }

    #[test]
    fn median_ms_even_count_averages_middle_two() {
        assert!((median_ms(&[10, 20, 30, 40]) - 25.0).abs() < 1e-9);
    }

    #[test]
    fn median_ms_is_robust_to_a_single_cold_start_outlier() {
        // The scenario this metric exists for: one huge cold-start sample
        // among many warm ones should barely move the median, unlike a
        // mean which the outlier would dominate.
        let mut latencies = vec![5u128; 17];
        latencies.push(2000);
        assert!((median_ms(&latencies) - 5.0).abs() < 1e-9);
    }

    // --- locate harness (M11 PR5) ---------------------------------------

    fn locate_hit(
        rank: usize,
        path: &str,
        symbol: Option<&str>,
        score: f64,
        rerank_score: Option<f32>,
    ) -> LocateHit {
        LocateHit {
            rank,
            path: path.to_string(),
            start_line: rank,
            end_line: rank + 5,
            symbol: symbol.map(|s| s.to_string()),
            kind: "function".to_string(),
            score,
            fts_score: score,
            vector_score: 0.0,
            rerank_score,
            snippet: String::new(),
        }
    }

    fn fake_locate_response(query: &str, results: Vec<LocateHit>) -> LocateResponse {
        LocateResponse {
            query: query.to_string(),
            mode: RetrievalMode::Hybrid,
            candidate_count: results.len(),
            returned_count: results.len(),
            results,
            reranked: false,
            files_total: 0,
            chunks_total: 0,
            head: None,
            corrupt_embeddings: 0,
            elapsed_ms: 0,
        }
    }

    fn match_query(id: &str, path_contains: &[&str], symbol: Option<&str>) -> LocateGoldenQuery {
        LocateGoldenQuery {
            id: id.into(),
            query: "q".into(),
            kind: GoldenKind::Match,
            path_contains: path_contains.iter().map(|s| s.to_string()).collect(),
            symbol_contains: symbol.map(|s| s.to_string()),
            notes: String::new(),
        }
    }

    #[test]
    fn locate_match_passes_when_path_in_top_k() {
        let q = match_query("x", &["src/code_index/locate.rs"], None);
        let resp = fake_locate_response(
            "q",
            vec![
                locate_hit(
                    1,
                    "src/retrieval/embeddings.rs",
                    Some("embed_one"),
                    0.4,
                    None,
                ),
                locate_hit(
                    2,
                    "src/code_index/locate.rs",
                    Some("cosine_similarity"),
                    0.38,
                    None,
                ),
            ],
        );
        let outcome = evaluate_locate_query(&q, &resp, 3, false, None);
        assert!(outcome.passed, "{:?}", outcome.failure_reason);
        assert_eq!(outcome.matched_rank, Some(2));
        assert!(!outcome.passed_at_1);
    }

    #[test]
    fn locate_match_fails_when_path_outside_top_k() {
        let q = match_query("x", &["src/code_index/locate.rs"], None);
        let resp = fake_locate_response(
            "q",
            vec![
                locate_hit(1, "AGENTS.md", None, 0.4, None),
                locate_hit(2, "docs/reference/spec.md", None, 0.39, None),
                locate_hit(3, "src/mcp/mod.rs", Some("locate"), 0.38, None),
            ],
        );
        let outcome = evaluate_locate_query(&q, &resp, 3, false, None);
        assert!(!outcome.passed);
        assert!(outcome.failure_reason.unwrap().contains("no top-3"));
    }

    #[test]
    fn locate_symbol_contains_gates_the_match() {
        // Right file, wrong symbol → no match.
        let q = match_query("x", &["src/code_index/locate.rs"], Some("cosine"));
        let resp = fake_locate_response(
            "q",
            vec![locate_hit(
                1,
                "src/code_index/locate.rs",
                Some("read_snippet"),
                0.4,
                None,
            )],
        );
        let outcome = evaluate_locate_query(&q, &resp, 3, false, None);
        assert!(!outcome.passed);
    }

    #[test]
    fn locate_passed_at_1_when_first_hit_matches() {
        let q = match_query("x", &["src/code_index/mod.rs"], Some("refresh"));
        let resp = fake_locate_response(
            "q",
            vec![locate_hit(
                1,
                "src/code_index/mod.rs",
                Some("refresh"),
                0.8,
                None,
            )],
        );
        let outcome = evaluate_locate_query(&q, &resp, 3, false, None);
        assert!(outcome.passed);
        assert!(outcome.passed_at_1);
        assert_eq!(outcome.matched_rank, Some(1));
    }

    #[test]
    fn locate_floor_drops_hits_below_threshold_when_reranking() {
        let q = match_query("x", &["src/code_index/locate.rs"], None);
        // The matching hit is rank 1 but its logit is below the floor, so it
        // is filtered out and the query fails.
        let resp = fake_locate_response(
            "q",
            vec![
                locate_hit(
                    1,
                    "src/code_index/locate.rs",
                    Some("cosine"),
                    0.4,
                    Some(-3.0),
                ),
                locate_hit(2, "src/other.rs", None, 0.3, Some(-4.0)),
            ],
        );
        let outcome = evaluate_locate_query(&q, &resp, 3, true, Some(0.0));
        assert!(!outcome.passed);
        assert_eq!(outcome.returned_count, 0);
    }

    #[test]
    fn locate_floor_ignored_when_reranker_off() {
        let q = match_query("x", &["src/code_index/locate.rs"], None);
        let resp = fake_locate_response(
            "q",
            vec![locate_hit(1, "src/code_index/locate.rs", None, 0.4, None)],
        );
        // Floor given, but reranker off → floor is a no-op, hit survives.
        let outcome = evaluate_locate_query(&q, &resp, 3, false, Some(99.0));
        assert!(outcome.passed);
        assert_eq!(outcome.returned_count, 1);
    }

    #[test]
    fn locate_empty_passes_only_when_floor_clears_all_hits() {
        let q = LocateGoldenQuery {
            id: "neg".into(),
            query: "zxqv".into(),
            kind: GoldenKind::Empty,
            path_contains: vec![],
            symbol_contains: None,
            notes: String::new(),
        };
        let resp = fake_locate_response(
            "zxqv",
            vec![locate_hit(1, "Cargo.lock", None, 0.2, Some(-6.0))],
        );
        // Without a floor the nonsense hit leaks → fail.
        let leaked = evaluate_locate_query(&q, &resp, 3, true, None);
        assert!(!leaked.passed);
        // With a floor above the logit the bundle clears → pass.
        let cleared = evaluate_locate_query(&q, &resp, 3, true, Some(0.0));
        assert!(cleared.passed);
        assert_eq!(cleared.returned_count, 0);
    }

    #[test]
    fn load_locate_golden_rejects_match_without_matchers() {
        use std::io::Write;
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("code_golden.json");
        let mut f = std::fs::File::create(&path).expect("create");
        writeln!(
            f,
            r#"{{ "version": 1, "queries": [ {{ "id": "x", "query": "q", "kind": "match" }} ] }}"#
        )
        .expect("write");
        let err = load_locate_golden(&path).expect_err("no matchers");
        assert!(matches!(err, MemhubError::InvalidInput(_)));
    }

    #[test]
    fn summarize_locate_computes_recall_at_1_and_k() {
        let outcomes = vec![
            // passes at 1 and k
            LocateQueryOutcome {
                id: "a".into(),
                query: "q".into(),
                kind: GoldenKind::Match,
                passed: true,
                passed_at_1: true,
                matched_rank: Some(1),
                matched_score: Some(0.9),
                matched_rerank: None,
                returned_count: 3,
                failure_reason: None,
            },
            // passes at k only (rank 3)
            LocateQueryOutcome {
                id: "b".into(),
                query: "q".into(),
                kind: GoldenKind::Match,
                passed: true,
                passed_at_1: false,
                matched_rank: Some(3),
                matched_score: Some(0.5),
                matched_rerank: None,
                returned_count: 3,
                failure_reason: None,
            },
            // empty probe that leaked
            LocateQueryOutcome {
                id: "n".into(),
                query: "q".into(),
                kind: GoldenKind::Empty,
                passed: false,
                passed_at_1: false,
                matched_rank: None,
                matched_score: None,
                matched_rerank: None,
                returned_count: 2,
                failure_reason: Some("leaked".into()),
            },
        ];
        let golden = LocateGoldenFile {
            version: 1,
            description: String::new(),
            queries: vec![
                match_query("a", &["x"], None),
                match_query("b", &["x"], None),
                LocateGoldenQuery {
                    id: "n".into(),
                    query: "q".into(),
                    kind: GoldenKind::Empty,
                    path_contains: vec![],
                    symbol_contains: None,
                    notes: String::new(),
                },
            ],
        };
        let opts = LocateEvalOptions {
            golden_path: PathBuf::from("code.json"),
            k: 3,
            use_reranker: false,
            min_rerank_score: None,
        };
        let summary = summarize_locate(
            &golden,
            RetrievalMode::Hybrid,
            opts,
            outcomes,
            Instant::now(),
        );
        assert_eq!(summary.match_queries, 2);
        assert_eq!(summary.match_passes_at_1, 1);
        assert_eq!(summary.match_passes_at_k, 2);
        assert_eq!(summary.safety_failures, 1);
        assert!((summary.recall_at_1 - 0.5).abs() < 1e-9);
        assert!((summary.recall_at_k - 1.0).abs() < 1e-9);
    }

    // --- multi-answer golden queries + `eval compare` (issue #306) ---------

    #[test]
    fn match_passes_when_any_listed_answer_hits_in_top_k() {
        let q: GoldenQuery = serde_json::from_str(
            r#"{ "id": "x", "query": "q", "kind": "match",
                 "source_type": "decision", "title_contains": ["recall"],
                 "also_accept": [ { "source_type": "fact", "title_contains": ["build"] } ] }"#,
        )
        .expect("parse");
        let resp = fake_response(
            "q",
            vec![
                hit(1, "task", 1, "unrelated", "x"),
                hit(2, "fact", 7, "build-command", "cargo build"),
            ],
        );
        let outcome = evaluate_query(&q, &resp, 3);
        assert!(outcome.passed, "{:?}", outcome.failure_reason);
        assert_eq!(outcome.matched_rank, Some(2));
        // Same hits, but the alternate answer sits outside the top K.
        assert!(!evaluate_query(&q, &resp, 1).passed);
    }

    #[test]
    fn also_accept_is_validated() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("golden.json");
        for (query, needle) in [
            (
                r#"{ "id": "x", "query": "q", "kind": "match", "source_type": "fact",
                     "also_accept": [ {} ] }"#,
                "no matchers",
            ),
            (
                r#"{ "id": "x", "query": "q", "kind": "match", "source_type": "fact",
                     "also_accept": [ { "source_type": "bogus" } ] }"#,
                "unknown source_type",
            ),
            (
                r#"{ "id": "x", "query": "q", "kind": "empty",
                     "also_accept": [ { "source_type": "fact" } ] }"#,
                "kind=empty",
            ),
        ] {
            std::fs::write(
                &path,
                format!(r#"{{ "version": 1, "queries": [ {query} ] }}"#),
            )
            .expect("write");
            let err = load_golden(&path).expect_err("invalid also_accept");
            assert!(err.to_string().contains(needle), "{err}");
        }
    }

    #[test]
    fn summarize_counts_rank_1_passes() {
        let outcome = |id: &str, rank: Option<usize>| QueryOutcome {
            id: id.into(),
            query: "q".into(),
            kind: GoldenKind::Match,
            passed: rank.is_some(),
            matched_rank: rank,
            matched_score: None,
            matched_low_confidence: false,
            returned_count: 3,
            failure_reason: None,
        };
        let golden = GoldenFile {
            version: 1,
            description: String::new(),
            queries: vec![],
        };
        let summary = summarize(
            &golden,
            3,
            RunSettings {
                mode: RetrievalMode::Hybrid,
                reranker: true,
                min_rerank_score: -2.0,
                rerank_candidate_pool: 20,
            },
            PathBuf::from("x.json"),
            vec![
                outcome("a", Some(1)),
                outcome("b", Some(3)),
                outcome("c", None),
            ],
            Instant::now(),
            0.0,
        );
        assert_eq!(summary.match_passes, 2);
        assert_eq!(summary.match_passes_at_1, 1);
        assert!((summary.recall_at_1 - 1.0 / 3.0).abs() < 1e-9);
        assert_eq!(summary.mode, RetrievalMode::Hybrid);
    }

    #[test]
    fn mcnemar_exact_p_matches_reference_values() {
        for (a, b, expected) in [
            (0, 0, 1.0),
            (6, 0, 0.03125),
            (0, 6, 0.03125),
            (5, 0, 0.0625),
            (10, 2, 0.038574),
            (2, 10, 0.038574),
            (5, 5, 1.0),
            (120, 80, 0.005685),
            (1050, 950, 0.026824),
        ] {
            let p = mcnemar_exact_p(a, b);
            assert!((p - expected).abs() < 1e-6, "({a}, {b}): {p} vs {expected}");
        }
    }

    /// One saved-run outcome as `eval retrieval --json` writes it.
    fn saved(id: &str, query: &str, kind: &str, passed: bool, rank: Option<usize>) -> String {
        let rank = rank.map_or("null".to_string(), |r| r.to_string());
        format!(
            r#"{{ "id": "{id}", "query": "{query}", "kind": "{kind}", "passed": {passed}, "matched_rank": {rank} }}"#
        )
    }

    // Valid for both the retrieval and the code (locate) golden loaders.
    const GOLDEN_JSON: &str =
        r#"{ "version": 1, "queries": [ { "id": "a", "query": "q", "kind": "empty" } ] }"#;

    fn utf16_with_bom(text: &str, little_endian: bool) -> Vec<u8> {
        let mut out = if little_endian {
            vec![0xFF, 0xFE]
        } else {
            vec![0xFE, 0xFF]
        };
        for unit in text.encode_utf16() {
            out.extend(if little_endian {
                unit.to_le_bytes()
            } else {
                unit.to_be_bytes()
            });
        }
        out
    }

    #[test]
    fn load_golden_rejects_duplicate_query_ids() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("golden.json");
        std::fs::write(
            &path,
            r#"{ "version": 1, "queries": [
                { "id": "dup", "query": "one", "kind": "empty" },
                { "id": "dup", "query": "two", "kind": "empty" } ] }"#,
        )
        .expect("write");
        let msg = load_golden(&path).expect_err("duplicate id").to_string();
        assert!(msg.contains("`dup`"), "{msg}");
        assert!(msg.contains("golden.json"), "{msg}");
    }

    #[test]
    fn shipped_golden_files_have_unique_ids_and_validate() {
        let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("tests");
        load_golden(&root.join("retrieval_golden.json")).expect("base golden");
        load_golden(&root.join("retrieval_golden_live.json")).expect("live golden");
    }

    #[test]
    fn input_loaders_accept_a_utf8_bom() {
        let dir = tempfile::tempdir().expect("tempdir");
        let with_bom = |name: &str, body: &str| {
            let path = dir.path().join(name);
            let mut bytes = vec![0xEF, 0xBB, 0xBF];
            bytes.extend_from_slice(body.as_bytes());
            std::fs::write(&path, bytes).expect("write");
            path
        };
        assert_eq!(
            load_golden(&with_bom("g.json", GOLDEN_JSON))
                .expect("golden")
                .queries
                .len(),
            1
        );
        assert_eq!(
            load_locate_golden(&with_bom("c.json", GOLDEN_JSON))
                .expect("code golden")
                .queries
                .len(),
            1
        );
        let run = r#"{ "k": 3, "mode": "fts", "outcomes": [] }"#;
        let a = with_bom("a.json", run);
        let b = with_bom("b.json", run);
        compare_runs(&a, &b).expect("compare with BOM");
    }

    #[test]
    fn input_loaders_name_utf16_and_the_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        for little_endian in [true, false] {
            let path = dir.path().join("u16.json");
            std::fs::write(&path, utf16_with_bom(GOLDEN_JSON, little_endian)).expect("write");
            let errs = [
                load_golden(&path).expect_err("golden").to_string(),
                load_locate_golden(&path)
                    .expect_err("code golden")
                    .to_string(),
                compare_runs(&path, &path).expect_err("compare").to_string(),
            ];
            for msg in errs {
                assert!(msg.contains("UTF-16"), "{msg}");
                assert!(msg.contains("u16.json"), "{msg}");
            }
        }
    }

    #[test]
    fn utf8_bom_does_not_change_the_loaded_queries() {
        let dir = tempfile::tempdir().expect("tempdir");
        let plain = dir.path().join("plain.json");
        let bom = dir.path().join("bom.json");
        std::fs::write(&plain, GOLDEN_JSON).expect("write");
        let mut bytes = vec![0xEF, 0xBB, 0xBF];
        bytes.extend_from_slice(GOLDEN_JSON.as_bytes());
        std::fs::write(&bom, bytes).expect("write");
        let queries = |path: &Path| format!("{:?}", load_golden(path).expect("golden").queries);
        assert_eq!(queries(&plain), queries(&bom));
    }

    #[test]
    fn input_loaders_name_utf32_and_the_path() {
        let dir = tempfile::tempdir().expect("tempdir");
        for bom in [[0xFF, 0xFE, 0x00, 0x00], [0x00, 0x00, 0xFE, 0xFF]] {
            let path = dir.path().join("u32.json");
            let mut bytes = bom.to_vec();
            bytes.extend_from_slice(GOLDEN_JSON.as_bytes());
            std::fs::write(&path, bytes).expect("write");
            let errs = [
                load_golden(&path).expect_err("golden").to_string(),
                load_locate_golden(&path)
                    .expect_err("code golden")
                    .to_string(),
                compare_runs(&path, &path).expect_err("compare").to_string(),
            ];
            for msg in errs {
                assert!(msg.contains("UTF-32"), "{msg}");
                assert!(msg.contains("u32.json"), "{msg}");
            }
        }
    }

    fn write_run(dir: &Path, name: &str, k: usize, outcomes: &[String]) -> PathBuf {
        let path = dir.join(name);
        std::fs::write(
            &path,
            format!(
                r#"{{ "k": {k}, "mode": "hybrid", "outcomes": [{}] }}"#,
                outcomes.join(",")
            ),
        )
        .expect("write run");
        path
    }

    #[test]
    fn compare_reports_discordant_queries_and_empty_probe_diffs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let a = write_run(
            dir.path(),
            "a.json",
            3,
            &[
                saved("m1", "q1", "match", true, Some(1)),
                saved("m2", "q2", "match", true, Some(2)),
                saved("m3", "q3", "match", false, None),
                saved("m4", "q4", "match", true, Some(1)),
                saved("e1", "q5", "empty", true, None),
                saved("e2", "q6", "empty", false, None),
            ],
        );
        let b = write_run(
            dir.path(),
            "b.json",
            3,
            &[
                saved("m1", "q1", "match", true, Some(2)),
                saved("m2", "q2", "match", true, Some(1)),
                saved("m3", "q3", "match", true, Some(3)),
                saved("m4", "q4", "match", true, Some(1)),
                saved("e1", "q5", "empty", false, None),
                saved("e2", "q6", "empty", false, None),
            ],
        );
        let c = compare_runs(&a, &b).expect("compare");
        assert_eq!((c.match_queries, c.empty_queries), (4, 2));
        assert_eq!((c.rank_1.passes_a, c.rank_1.passes_b), (2, 2));
        assert_eq!(c.rank_1.only_a, vec!["m1"]);
        assert_eq!(c.rank_1.only_b, vec!["m2"]);
        assert!((c.rank_1.p_value - 1.0).abs() < 1e-9);
        assert_eq!((c.found_at_k.passes_a, c.found_at_k.passes_b), (3, 4));
        assert!(c.found_at_k.only_a.is_empty());
        assert_eq!(c.found_at_k.only_b, vec!["m3"]);
        assert!((c.found_at_k.p_value - 1.0).abs() < 1e-9);
        assert_eq!(c.empty_probe_diffs.len(), 1);
        assert_eq!(c.empty_probe_diffs[0].id, "e1");
        assert_eq!(c.empty_probe_diffs[0].hit_in, "b");
    }

    #[test]
    fn compare_rejects_mismatched_runs() {
        let dir = tempfile::tempdir().expect("tempdir");
        let base = [saved("m1", "q1", "match", true, Some(1))];
        let a = write_run(dir.path(), "a.json", 3, &base);

        let other_k = write_run(dir.path(), "k.json", 5, &base);
        let err = compare_runs(&a, &other_k)
            .expect_err("different K")
            .to_string();
        assert!(err.contains("different K"), "{err}");

        let extra = write_run(
            dir.path(),
            "ids.json",
            3,
            &[
                saved("m1", "q1", "match", true, Some(1)),
                saved("m9", "q9", "match", true, Some(1)),
            ],
        );
        let err = compare_runs(&a, &extra)
            .expect_err("different ids")
            .to_string();
        assert!(
            err.contains("different query ids") && err.contains("m9"),
            "{err}"
        );

        let reworded = write_run(
            dir.path(),
            "text.json",
            3,
            &[saved("m1", "other text", "match", true, Some(1))],
        );
        let err = compare_runs(&a, &reworded)
            .expect_err("different text")
            .to_string();
        assert!(
            err.contains("m1") && err.contains("different text"),
            "{err}"
        );

        let not_a_run = dir.path().join("junk.json");
        std::fs::write(&not_a_run, "{}").expect("write");
        assert!(compare_runs(&a, &not_a_run).is_err());
    }
}
