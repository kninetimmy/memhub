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

use crate::config::RetrievalMode;
use crate::retrieval::{self, RecallHit, RecallOptions, RecallResponse};
use crate::{MemhubError, Result};

pub const DEFAULT_GOLDEN_PATH: &str = "tests/retrieval_golden.json";
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
    #[serde(default)]
    pub notes: String,
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
    /// Total returned by recall for this query.
    pub returned_count: usize,
    /// Optional reason string for failed outcomes (debug aid in markdown output).
    pub failure_reason: Option<String>,
}

#[derive(Debug, Clone)]
pub struct EvalSummary {
    pub golden_path: PathBuf,
    pub mode: RetrievalMode,
    pub k: usize,
    pub total_queries: usize,
    pub match_queries: usize,
    pub empty_queries: usize,
    pub match_passes: usize,
    pub empty_passes: usize,
    pub recall_at_k: f64,
    pub safety_failures: usize,
    pub outcomes: Vec<QueryOutcome>,
    pub elapsed_ms: u128,
}

pub fn run_retrieval(start: &Path, opts: EvalOptions) -> Result<EvalSummary> {
    if opts.k == 0 {
        return Err(MemhubError::InvalidInput(
            "--k must be greater than zero".to_string(),
        ));
    }
    let golden = load_golden(&opts.golden_path)?;
    let started = Instant::now();
    let mut outcomes = Vec::with_capacity(golden.queries.len());
    let mut resolved_mode = opts.mode;

    for query in &golden.queries {
        let recall_opts = RecallOptions {
            query: query.query.clone(),
            mode: opts.mode,
            max_results: opts.k,
            source_types: Vec::new(),
            include_stale: None,
            accepted_only: None,
        };
        let response = retrieval::recall(start, recall_opts)?;
        if resolved_mode.is_none() {
            resolved_mode = Some(response.mode);
        }
        outcomes.push(evaluate_query(query, &response, opts.k));
    }

    let mode = resolved_mode.unwrap_or(RetrievalMode::Fts);
    let summary = summarize(&golden, opts.k, mode, opts.golden_path.clone(), outcomes, started);
    Ok(summary)
}

pub fn load_golden(path: &Path) -> Result<GoldenFile> {
    let bytes = fs::read(path).map_err(|err| match err.kind() {
        std::io::ErrorKind::NotFound => MemhubError::InvalidInput(format!(
            "golden file not found at {} — pass --golden <path> or create one",
            path.display()
        )),
        _ => MemhubError::from(err),
    })?;
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
    for q in &parsed.queries {
        validate_query(q)?;
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
    if let Some(st) = q.source_type.as_deref() {
        match st {
            "fact" | "decision" | "task" => {}
            other => {
                return Err(MemhubError::InvalidInput(format!(
                    "golden query `{}` has unknown source_type `{}` (expected fact|decision|task)",
                    q.id, other
                )));
            }
        }
    }
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
    Ok(())
}

pub fn evaluate_query(query: &GoldenQuery, response: &RecallResponse, k: usize) -> QueryOutcome {
    let returned_count = response.results.len();
    match query.kind {
        GoldenKind::Empty => {
            if returned_count == 0 {
                QueryOutcome {
                    id: query.id.clone(),
                    query: query.query.clone(),
                    kind: GoldenKind::Empty,
                    passed: true,
                    matched_rank: None,
                    matched_score: None,
                    returned_count,
                    failure_reason: None,
                }
            } else {
                let leaked: Vec<String> = response
                    .results
                    .iter()
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
                    returned_count,
                    failure_reason: Some(format!(
                        "expected empty bundle but recall returned {} hit(s): {}",
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
                    .map(|hit| format!("{}#{}:{}", hit.source_type, hit.source_id, truncate(&hit.title, 40)))
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
                returned_count,
                failure_reason: Some(reason),
            }
        }
    }
}

fn hit_matches(query: &GoldenQuery, hit: &RecallHit) -> bool {
    if let Some(expected) = query.source_type.as_deref()
        && hit.source_type != expected
    {
        return false;
    }
    let title_lower = hit.title.to_lowercase();
    for needle in &query.title_contains {
        if !title_lower.contains(&needle.to_lowercase()) {
            return false;
        }
    }
    let body_lower = hit.body.to_lowercase();
    for needle in &query.body_contains {
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
    mode: RetrievalMode,
    golden_path: PathBuf,
    outcomes: Vec<QueryOutcome>,
    started: Instant,
) -> EvalSummary {
    let total_queries = golden.queries.len();
    let mut match_queries = 0usize;
    let mut empty_queries = 0usize;
    let mut match_passes = 0usize;
    let mut empty_passes = 0usize;
    let mut safety_failures = 0usize;
    for outcome in &outcomes {
        match outcome.kind {
            GoldenKind::Match => {
                match_queries += 1;
                if outcome.passed {
                    match_passes += 1;
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
    let recall_at_k = if match_queries == 0 {
        0.0
    } else {
        match_passes as f64 / match_queries as f64
    };
    EvalSummary {
        golden_path,
        mode,
        k,
        total_queries,
        match_queries,
        empty_queries,
        match_passes,
        empty_passes,
        recall_at_k,
        safety_failures,
        outcomes,
        elapsed_ms: started.elapsed().as_millis(),
    }
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
        }
    }

    fn hit(rank: usize, source_type: &str, source_id: i64, title: &str, body: &str) -> RecallHit {
        RecallHit {
            rank,
            source_type: source_type.to_string(),
            source_id,
            title: title.to_string(),
            body: body.to_string(),
            score: 0.9,
            fts_score: 0.9,
            vector_score: 0.0,
            confidence: 1.0,
            stale: false,
            source: "user".to_string(),
            created_at: "2026-05-13".to_string(),
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
            notes: String::new(),
        };
        let resp = fake_response(
            "test",
            vec![
                hit(1, "fact", 1, "build-command", "cargo build"),
                hit(2, "fact", 2, "test-command", "cargo test"),
                hit(3, "task", 1, "Ship something", "irrelevant"),
                hit(4, "decision", 48, "memhub recall is read-only", "irrelevant"),
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
            notes: String::new(),
        };
        let resp = fake_response(
            "test",
            vec![
                hit(1, "decision", 48, "memhub recall is read-only", "no ledger here"),
                hit(2, "decision", 34, "Agents prefer recall over reading the ledger", "ok"),
            ],
        );
        let outcome = evaluate_query(&q, &resp, 3);
        assert!(outcome.passed);
        assert_eq!(outcome.matched_rank, Some(2));
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
                    notes: String::new(),
                },
                GoldenQuery {
                    id: "b".into(),
                    query: "q".into(),
                    kind: GoldenKind::Match,
                    source_type: None,
                    title_contains: vec!["x".into()],
                    body_contains: vec![],
                    notes: String::new(),
                },
                GoldenQuery {
                    id: "n".into(),
                    query: "q".into(),
                    kind: GoldenKind::Empty,
                    source_type: None,
                    title_contains: vec![],
                    body_contains: vec![],
                    notes: String::new(),
                },
            ],
        };
        let summary = summarize(
            &golden,
            3,
            RetrievalMode::Fts,
            PathBuf::from("x.json"),
            outcomes,
            Instant::now(),
        );
        assert_eq!(summary.total_queries, 3);
        assert_eq!(summary.match_queries, 2);
        assert_eq!(summary.empty_queries, 1);
        assert_eq!(summary.match_passes, 1);
        assert_eq!(summary.empty_passes, 0);
        assert_eq!(summary.safety_failures, 1);
        assert!((summary.recall_at_k - 0.5).abs() < 1e-9);
    }
}
