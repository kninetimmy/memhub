//! Read-only local dashboard for inspecting a memhub project.
//!
//! The dashboard is intentionally scoped to the current project and process:
//! `memhub viz` opens a localhost server, protects the API with a per-run
//! token, and exits when the foreground CLI process is stopped.

use std::collections::BTreeMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use axum::extract::{Query, State};
use axum::http::StatusCode;
use axum::response::{Html, IntoResponse, Response};
use axum::routing::get;
use axum::{Json, Router};
use rusqlite::{Connection, OptionalExtension, params};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use tokio::net::TcpListener;
use tower_http::trace::TraceLayer;

use crate::config::RetrievalMode;
use crate::db;
use crate::retrieval::embeddings::EMBEDDING_MODEL_NAME;
use crate::retrieval::{RecallOptions, recall};
use crate::{MemhubError, Result};

const INDEX_HTML: &str = include_str!("static/index.html");
const APP_CSS: &str = include_str!("static/app.css");
const APP_JS: &str = include_str!("static/app.js");
const UPLOT_JS: &str = include_str!("static/vendor/uplot.min.js");
const UPLOT_CSS: &str = include_str!("static/vendor/uplot.min.css");

#[derive(Debug, Clone)]
pub struct DashboardOptions {
    pub host: String,
    pub port: u16,
    pub open: bool,
}

#[derive(Clone)]
struct AppState {
    repo_root: Arc<PathBuf>,
    token: Arc<String>,
}

pub fn serve_blocking(start: &Path, options: DashboardOptions) -> Result<()> {
    let ctx = db::open_project(start)?;
    let repo_root = ctx.paths.repo_root;
    let token = generate_token(&repo_root);
    let bind_addr = resolve_bind_addr(&options.host, options.port)?;

    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()?;
    runtime.block_on(serve(repo_root, token, bind_addr, options.open))
}

async fn serve(repo_root: PathBuf, token: String, bind_addr: SocketAddr, open: bool) -> Result<()> {
    let listener = TcpListener::bind(bind_addr).await?;
    let bound_addr = listener.local_addr()?;
    let url = format!("http://{bound_addr}/?token={token}");
    println!("memhub viz serving {}", repo_root.display());
    println!("URL: {url}");
    println!("Press Ctrl-C to stop.");

    if open && let Err(error) = open_url(&url) {
        eprintln!("warning: could not open browser: {error}");
    }

    let app = router(AppState {
        repo_root: Arc::new(repo_root),
        token: Arc::new(token),
    });

    axum::serve(listener, app)
        .with_graceful_shutdown(async {
            let _ = tokio::signal::ctrl_c().await;
        })
        .await?;
    Ok(())
}

fn router(state: AppState) -> Router {
    Router::new()
        .route("/", get(index_html))
        .route("/app.css", get(app_css))
        .route("/app.js", get(app_js))
        .route("/vendor/uplot.css", get(uplot_css))
        .route("/vendor/uplot.js", get(uplot_js))
        .route("/api/overview", get(api_overview))
        .route("/api/embeddings", get(api_embeddings))
        .route("/api/activity", get(api_activity))
        .route("/api/audit", get(api_audit))
        .route("/api/metrics", get(api_metrics))
        .route("/api/metrics/series", get(api_metrics_series))
        .route("/api/recall", get(api_recall))
        .layer(TraceLayer::new_for_http())
        .with_state(state)
}

async fn index_html() -> Html<&'static str> {
    Html(INDEX_HTML)
}

async fn app_css() -> Response {
    ([(axum::http::header::CONTENT_TYPE, "text/css")], APP_CSS).into_response()
}

async fn app_js() -> Response {
    (
        [(axum::http::header::CONTENT_TYPE, "application/javascript")],
        APP_JS,
    )
        .into_response()
}

async fn uplot_css() -> Response {
    ([(axum::http::header::CONTENT_TYPE, "text/css")], UPLOT_CSS).into_response()
}

async fn uplot_js() -> Response {
    (
        [(axum::http::header::CONTENT_TYPE, "application/javascript")],
        UPLOT_JS,
    )
        .into_response()
}

#[derive(Debug, Deserialize)]
struct TokenQuery {
    token: Option<String>,
}

#[derive(Debug, Deserialize)]
struct RecallQuery {
    token: Option<String>,
    q: Option<String>,
}

#[derive(Debug, Deserialize)]
struct SeriesQuery {
    token: Option<String>,
    window: Option<String>,
}

async fn api_overview(
    State(state): State<AppState>,
    Query(query): Query<TokenQuery>,
) -> std::result::Result<Json<OverviewPayload>, ApiError> {
    authorize(&state, query.token.as_deref())?;
    Ok(Json(read_overview(&state.repo_root)?))
}

async fn api_embeddings(
    State(state): State<AppState>,
    Query(query): Query<TokenQuery>,
) -> std::result::Result<Json<EmbeddingPayload>, ApiError> {
    authorize(&state, query.token.as_deref())?;
    Ok(Json(read_embeddings(&state.repo_root)?))
}

async fn api_activity(
    State(state): State<AppState>,
    Query(query): Query<TokenQuery>,
) -> std::result::Result<Json<ActivityPayload>, ApiError> {
    authorize(&state, query.token.as_deref())?;
    Ok(Json(read_activity(&state.repo_root)?))
}

async fn api_audit(
    State(state): State<AppState>,
    Query(query): Query<TokenQuery>,
) -> std::result::Result<Json<AuditPayload>, ApiError> {
    authorize(&state, query.token.as_deref())?;
    Ok(Json(read_audit(&state.repo_root)?))
}

async fn api_metrics(
    State(state): State<AppState>,
    Query(query): Query<TokenQuery>,
) -> std::result::Result<Json<MetricsPayload>, ApiError> {
    authorize(&state, query.token.as_deref())?;
    Ok(Json(read_metrics(&state.repo_root)?))
}

async fn api_metrics_series(
    State(state): State<AppState>,
    Query(query): Query<SeriesQuery>,
) -> std::result::Result<Json<SeriesPayload>, ApiError> {
    authorize(&state, query.token.as_deref())?;
    let window =
        crate::commands::metrics::SeriesWindow::parse(query.window.as_deref().unwrap_or("7d"));
    Ok(Json(read_series(&state.repo_root, window)?))
}

async fn api_recall(
    State(state): State<AppState>,
    Query(query): Query<RecallQuery>,
) -> std::result::Result<Json<RecallPayload>, ApiError> {
    authorize(&state, query.token.as_deref())?;
    let q = query.q.unwrap_or_default();
    if q.trim().is_empty() {
        return Err(ApiError::bad_request("recall query is required"));
    }
    Ok(Json(run_recall(&state.repo_root, q)?))
}

fn authorize(state: &AppState, token: Option<&str>) -> std::result::Result<(), ApiError> {
    match token {
        Some(value) if value == state.token.as_str() => Ok(()),
        _ => Err(ApiError::unauthorized()),
    }
}

#[derive(Debug, Serialize)]
struct OverviewPayload {
    project_name: String,
    repo_root: String,
    schema_version: String,
    retrieval_mode: String,
    counts: BTreeMap<String, i64>,
    recent_writes: Vec<WriteLogRow>,
    latest_state: Option<NarrativeRow>,
    latest_arch: Option<NarrativeRow>,
}

#[derive(Debug, Serialize)]
struct NarrativeRow {
    body: String,
    actor: String,
    created_at: String,
}

#[derive(Debug, Serialize)]
struct WriteLogRow {
    id: i64,
    actor: String,
    table_name: String,
    row_id: Option<i64>,
    action: String,
    reason: Option<String>,
    at: String,
}

#[derive(Debug, Serialize)]
struct ActivityPayload {
    writes: Vec<WriteLogRow>,
    by_actor: Vec<CountRow>,
    by_table: Vec<CountRow>,
}

#[derive(Debug, Serialize)]
struct CountRow {
    label: String,
    count: i64,
}

#[derive(Debug, Serialize)]
struct EmbeddingPayload {
    model: String,
    points: Vec<EmbeddingPoint>,
}

#[derive(Debug, Serialize)]
struct EmbeddingPoint {
    source_type: String,
    source_id: i64,
    title: String,
    body: String,
    source: String,
    x: f64,
    y: f64,
}

#[derive(Debug)]
struct EmbeddingRow {
    source_type: String,
    source_id: i64,
    title: String,
    body: String,
    source: String,
    vector: Vec<f64>,
}

#[derive(Debug, Serialize)]
struct AuditPayload {
    source_counts: Vec<CountRow>,
    stale_facts: i64,
    embedding_coverage: Vec<CoverageRow>,
    pending_writes: Vec<CountRow>,
}

#[derive(Debug, Serialize)]
struct CoverageRow {
    source_type: String,
    total: i64,
    embedded: i64,
    missing: i64,
}

#[derive(Debug, Serialize)]
struct RecallPayload {
    query: String,
    mode: String,
    candidate_count: usize,
    returned_count: usize,
    elapsed_ms: u128,
    warnings: Vec<RecallWarningPayload>,
    results: Vec<RecallHitPayload>,
}

#[derive(Debug, Serialize)]
struct RecallWarningPayload {
    kind: String,
    stale_count: usize,
    total_count: usize,
    reason: String,
    fix: String,
}

#[derive(Debug, Serialize)]
struct RecallHitPayload {
    rank: usize,
    source_type: String,
    source_id: i64,
    title: String,
    body: String,
    score: f64,
    fts_score: f64,
    vector_score: f64,
    stale: bool,
    source: String,
    created_at: String,
}

#[derive(Debug, Serialize)]
struct MetricsPayload {
    enabled: bool,
    recall_proxy: bool,
    session_accounting: bool,
    claude_transcripts_dir: Option<String>,
    last_scrape_ts: Option<String>,
    totals_7d: PeriodPayload,
    totals_30d: PeriodPayload,
    sessions: Vec<SessionPayload>,
}

#[derive(Debug, Serialize)]
struct PeriodPayload {
    recalls: i64,
    bundle_tokens: i64,
    ledger_tokens: i64,
    context_offset_pct: Option<f64>,
    sessions: i64,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_creation_tokens: i64,
}

#[derive(Debug, Serialize)]
struct SessionPayload {
    session_id: String,
    agent: String,
    started_at: String,
    ended_at: Option<String>,
    input_tokens: i64,
    output_tokens: i64,
    recall_calls: i64,
}

#[derive(Debug, Serialize)]
struct SeriesPayload {
    enabled: bool,
    window: String,
    granularity: String,
    session_id: Option<String>,
    has_recall_signal: bool,
    points: Vec<SeriesPointPayload>,
}

#[derive(Debug, Serialize)]
struct SeriesPointPayload {
    x: i64,
    label: String,
    actual: i64,
    counterfactual: i64,
    delta: i64,
    recall_offset: i64,
    recalls: i64,
}

fn read_series(
    start: &Path,
    window: crate::commands::metrics::SeriesWindow,
) -> Result<SeriesPayload> {
    let d = crate::commands::metrics::query_series(start, window)?;
    Ok(SeriesPayload {
        enabled: d.enabled,
        window: d.window,
        granularity: d.granularity,
        session_id: d.session_id,
        has_recall_signal: d.has_recall_signal,
        points: d
            .points
            .into_iter()
            .map(|p| SeriesPointPayload {
                x: p.x,
                label: p.label,
                actual: p.actual,
                counterfactual: p.counterfactual,
                delta: p.delta,
                recall_offset: p.recall_offset,
                recalls: p.recalls,
            })
            .collect(),
    })
}

fn read_metrics(start: &Path) -> Result<MetricsPayload> {
    let data = crate::commands::metrics::query_tool_data(start)?;
    Ok(MetricsPayload {
        enabled: data.enabled,
        recall_proxy: data.recall_proxy,
        session_accounting: data.session_accounting,
        claude_transcripts_dir: data.claude_transcripts_dir,
        last_scrape_ts: data.last_scrape_ts,
        totals_7d: period_payload(&data.totals_7d),
        totals_30d: period_payload(&data.totals_30d),
        sessions: data
            .last_sessions
            .into_iter()
            .map(session_payload)
            .collect(),
    })
}

fn period_payload(t: &crate::metrics::formatter::PeriodTotals) -> PeriodPayload {
    PeriodPayload {
        recalls: t.recalls,
        bundle_tokens: t.bundle_tokens,
        ledger_tokens: t.ledger_tokens,
        context_offset_pct: t.context_offset_pct(),
        sessions: t.sessions,
        input_tokens: t.input_tokens,
        output_tokens: t.output_tokens,
        cache_read_tokens: t.cache_read_tokens,
        cache_creation_tokens: t.cache_creation_tokens,
    }
}

fn session_payload(s: crate::metrics::formatter::SessionSummary) -> SessionPayload {
    SessionPayload {
        session_id: s.session_id,
        agent: s.agent,
        started_at: s.started_at,
        ended_at: s.ended_at,
        input_tokens: s.input_tokens,
        output_tokens: s.output_tokens,
        recall_calls: s.recall_calls,
    }
}

fn read_overview(start: &Path) -> Result<OverviewPayload> {
    let ctx = db::open_project(start)?;
    let conn = &ctx.conn;
    let mut counts = BTreeMap::new();
    for table in [
        "facts",
        "decisions",
        "tasks",
        "documents",
        "doc_chunks",
        "commands",
        "pending_writes",
        "writes_log",
        "embeddings",
    ] {
        counts.insert(table.to_string(), count_table(conn, table)?);
    }
    let schema_version = conn.query_row(
        "SELECT schema_version FROM projects WHERE id = 1",
        [],
        |row| row.get(0),
    )?;

    Ok(OverviewPayload {
        project_name: ctx.config.project_name,
        repo_root: ctx.paths.repo_root.display().to_string(),
        schema_version,
        retrieval_mode: retrieval_mode_label(ctx.config.retrieval.mode).to_string(),
        counts,
        recent_writes: read_recent_writes(conn, 8)?,
        latest_state: read_latest_narrative(conn, "project_state")?,
        latest_arch: read_latest_narrative(conn, "project_arch")?,
    })
}

fn read_activity(start: &Path) -> Result<ActivityPayload> {
    let ctx = db::open_project(start)?;
    let conn = &ctx.conn;
    Ok(ActivityPayload {
        writes: read_recent_writes(conn, 50)?,
        by_actor: count_grouped(conn, "actor", "writes_log")?,
        by_table: count_grouped(conn, "table_name", "writes_log")?,
    })
}

fn read_audit(start: &Path) -> Result<AuditPayload> {
    let ctx = db::open_project(start)?;
    let conn = &ctx.conn;
    let stale_facts = conn.query_row(
        "SELECT COUNT(*) FROM facts
         WHERE verified_at IS NULL
            OR julianday('now') - julianday(verified_at) > 90",
        [],
        |row| row.get(0),
    )?;
    Ok(AuditPayload {
        source_counts: read_source_counts(conn)?,
        stale_facts,
        embedding_coverage: read_embedding_coverage(conn)?,
        pending_writes: count_grouped(conn, "status", "pending_writes")?,
    })
}

fn read_embeddings(start: &Path) -> Result<EmbeddingPayload> {
    let ctx = db::open_project(start)?;
    let rows = read_embedding_rows(&ctx.conn)?;
    let coords = pca_2d(rows.iter().map(|row| row.vector.as_slice()).collect());
    let points = rows
        .into_iter()
        .zip(coords)
        .map(|(row, (x, y))| EmbeddingPoint {
            source_type: row.source_type,
            source_id: row.source_id,
            title: row.title,
            body: row.body,
            source: row.source,
            x,
            y,
        })
        .collect();
    Ok(EmbeddingPayload {
        model: EMBEDDING_MODEL_NAME.to_string(),
        points,
    })
}

fn run_recall(start: &Path, query: String) -> Result<RecallPayload> {
    let response = recall(
        start,
        RecallOptions {
            query,
            mode: None,
            max_results: 8,
            source_types: Vec::new(),
            include_stale: Some(true),
            accepted_only: None,
            use_reranker: None,
            min_rerank_score: None,
            // Dashboard inspector replays/explores queries; that's
            // not "real usage" the metrics dashboard should be
            // reporting on.
            log_metrics: false,
            surface: None,
        },
    )?;
    Ok(RecallPayload {
        query: response.query,
        mode: retrieval_mode_label(response.mode).to_string(),
        candidate_count: response.candidate_count,
        returned_count: response.returned_count,
        elapsed_ms: response.elapsed_ms,
        warnings: response
            .warnings
            .into_iter()
            .map(|w| RecallWarningPayload {
                kind: w.kind,
                stale_count: w.stale_count,
                total_count: w.total_count,
                reason: w.reason,
                fix: w.fix,
            })
            .collect(),
        results: response
            .results
            .into_iter()
            .map(|hit| RecallHitPayload {
                rank: hit.rank,
                source_type: hit.source_type,
                source_id: hit.source_id,
                title: hit.title,
                body: hit.body,
                score: hit.score,
                fts_score: hit.fts_score,
                vector_score: hit.vector_score,
                stale: hit.stale,
                source: hit.source,
                created_at: hit.created_at,
            })
            .collect(),
    })
}

fn count_table(conn: &Connection, table: &str) -> Result<i64> {
    Ok(
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })?,
    )
}

fn read_latest_narrative(conn: &Connection, table: &str) -> Result<Option<NarrativeRow>> {
    Ok(conn
        .query_row(
            &format!("SELECT body, actor, created_at FROM {table} ORDER BY created_at DESC, id DESC LIMIT 1"),
            [],
            |row| {
                Ok(NarrativeRow {
                    body: row.get(0)?,
                    actor: row.get(1)?,
                    created_at: row.get(2)?,
                })
            },
        )
        .optional()?)
}

fn read_recent_writes(conn: &Connection, limit: i64) -> Result<Vec<WriteLogRow>> {
    let mut stmt = conn.prepare(
        "SELECT id, actor, table_name, row_id, action, reason, at
         FROM writes_log
         ORDER BY at DESC, id DESC
         LIMIT ?1",
    )?;
    let rows = stmt.query_map(params![limit], |row| {
        Ok(WriteLogRow {
            id: row.get(0)?,
            actor: row.get(1)?,
            table_name: row.get(2)?,
            row_id: row.get(3)?,
            action: row.get(4)?,
            reason: row.get(5)?,
            at: row.get(6)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(MemhubError::from)
}

fn count_grouped(conn: &Connection, column: &str, table: &str) -> Result<Vec<CountRow>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT COALESCE(NULLIF({column}, ''), '(blank)') AS label, COUNT(*)
         FROM {table}
         GROUP BY label
         ORDER BY COUNT(*) DESC, label ASC"
    ))?;
    let rows = stmt.query_map([], |row| {
        Ok(CountRow {
            label: row.get(0)?,
            count: row.get(1)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(MemhubError::from)
}

fn read_source_counts(conn: &Connection) -> Result<Vec<CountRow>> {
    let mut stmt = conn.prepare(
        "SELECT COALESCE(NULLIF(source, ''), '(blank)') AS label, COUNT(*)
         FROM (
            SELECT source FROM facts
            UNION ALL
            SELECT source FROM decisions
         )
         GROUP BY label
         ORDER BY COUNT(*) DESC, label ASC",
    )?;
    let rows = stmt.query_map([], |row| {
        Ok(CountRow {
            label: row.get(0)?,
            count: row.get(1)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(MemhubError::from)
}

fn read_embedding_coverage(conn: &Connection) -> Result<Vec<CoverageRow>> {
    let sources = [
        ("fact", "facts"),
        ("decision", "decisions"),
        ("task", "tasks"),
        ("doc_chunk", "doc_chunks"),
    ];
    let mut out = Vec::with_capacity(sources.len());
    for (source_type, table) in sources {
        let total = count_table(conn, table)?;
        let embedded: i64 = conn.query_row(
            "SELECT COUNT(*)
             FROM embeddings
             WHERE source_type = ?1 AND model_name = ?2",
            params![source_type, EMBEDDING_MODEL_NAME],
            |row| row.get(0),
        )?;
        out.push(CoverageRow {
            source_type: source_type.to_string(),
            total,
            embedded,
            missing: total.saturating_sub(embedded),
        });
    }
    Ok(out)
}

fn read_embedding_rows(conn: &Connection) -> Result<Vec<EmbeddingRow>> {
    let mut stmt = conn.prepare(
        "SELECT 'fact' AS source_type, f.id AS source_id, f.key, f.value, f.source, e.vector
         FROM facts f
         JOIN embeddings e ON e.source_type = 'fact' AND e.source_id = f.id AND e.model_name = ?1
         UNION ALL
         SELECT 'decision' AS source_type, d.id AS source_id, d.title, d.rationale, d.source, e.vector
         FROM decisions d
         JOIN embeddings e ON e.source_type = 'decision' AND e.source_id = d.id AND e.model_name = ?1
         UNION ALL
         SELECT 'task' AS source_type, t.id AS source_id, t.title, COALESCE(t.notes, ''), '', e.vector
         FROM tasks t
         JOIN embeddings e ON e.source_type = 'task' AND e.source_id = t.id AND e.model_name = ?1
         ORDER BY source_type, source_id",
    )?;
    let rows = stmt.query_map(params![EMBEDDING_MODEL_NAME], |row| {
        let blob: Vec<u8> = row.get(5)?;
        Ok(EmbeddingRow {
            source_type: row.get(0)?,
            source_id: row.get(1)?,
            title: row.get(2)?,
            body: row.get(3)?,
            source: row.get(4)?,
            vector: decode_vector(&blob),
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(MemhubError::from)
}

fn decode_vector(blob: &[u8]) -> Vec<f64> {
    blob.chunks_exact(4)
        .map(|chunk| f32::from_le_bytes([chunk[0], chunk[1], chunk[2], chunk[3]]) as f64)
        .collect()
}

fn pca_2d(vectors: Vec<&[f64]>) -> Vec<(f64, f64)> {
    if vectors.is_empty() {
        return Vec::new();
    }
    let dim = vectors[0].len();
    if vectors.len() == 1 || dim == 0 {
        return vec![(0.0, 0.0); vectors.len()];
    }

    let mut mean = vec![0.0; dim];
    for vector in &vectors {
        for (idx, value) in vector.iter().enumerate() {
            mean[idx] += value;
        }
    }
    for value in &mut mean {
        *value /= vectors.len() as f64;
    }

    let centered: Vec<Vec<f64>> = vectors
        .iter()
        .map(|vector| {
            vector
                .iter()
                .zip(&mean)
                .map(|(value, mean)| value - mean)
                .collect()
        })
        .collect();

    let pc1 = power_component(&centered, None);
    let pc2 = power_component(&centered, Some(&pc1));
    let mut coords: Vec<(f64, f64)> = centered
        .iter()
        .map(|row| (dot(row, &pc1), dot(row, &pc2)))
        .collect();

    let scale = coords
        .iter()
        .fold(0.0_f64, |acc, (x, y)| acc.max(x.abs()).max(y.abs()));
    if scale > 0.0 {
        for (x, y) in &mut coords {
            *x /= scale;
            *y /= scale;
        }
    }
    coords
}

fn power_component(rows: &[Vec<f64>], orthogonal_to: Option<&[f64]>) -> Vec<f64> {
    let dim = rows.first().map(|row| row.len()).unwrap_or(0);
    let mut v = vec![1.0 / (dim.max(1) as f64).sqrt(); dim];
    for _ in 0..40 {
        let mut next = vec![0.0; dim];
        for row in rows {
            let weight = dot(row, &v);
            for idx in 0..dim {
                next[idx] += row[idx] * weight;
            }
        }
        if let Some(base) = orthogonal_to {
            let projection = dot(&next, base);
            for idx in 0..dim {
                next[idx] -= projection * base[idx];
            }
        }
        normalize(&mut next);
        if next.iter().all(|value| value.abs() < f64::EPSILON) {
            break;
        }
        v = next;
    }
    v
}

fn dot(a: &[f64], b: &[f64]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}

fn normalize(v: &mut [f64]) {
    let norm = v.iter().map(|value| value * value).sum::<f64>().sqrt();
    if norm > 0.0 {
        for value in v {
            *value /= norm;
        }
    }
}

fn retrieval_mode_label(mode: RetrievalMode) -> &'static str {
    match mode {
        RetrievalMode::Fts => "fts",
        RetrievalMode::Hybrid => "hybrid",
    }
}

fn resolve_bind_addr(host: &str, port: u16) -> Result<SocketAddr> {
    let ip = if host == "localhost" {
        IpAddr::V4(Ipv4Addr::LOCALHOST)
    } else {
        host.parse::<IpAddr>().map_err(|_| {
            MemhubError::InvalidInput(format!(
                "viz host must be localhost or a loopback IP, got {host:?}"
            ))
        })?
    };
    if !ip.is_loopback() {
        return Err(MemhubError::InvalidInput(
            "viz only binds loopback addresses".to_string(),
        ));
    }
    Ok(SocketAddr::new(ip, port))
}

fn generate_token(repo_root: &Path) -> String {
    let now = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_nanos())
        .unwrap_or_default();
    let mut hasher = Sha256::new();
    hasher.update(repo_root.to_string_lossy().as_bytes());
    hasher.update(std::process::id().to_le_bytes());
    hasher.update(now.to_le_bytes());
    let digest = hasher.finalize();
    digest[..18]
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect()
}

fn open_url(url: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    let mut command = Command::new("open");
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut cmd = Command::new("cmd");
        cmd.arg("/C").arg("start");
        cmd
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = Command::new("xdg-open");

    command.arg(url).status().map(|_| ())
}

#[derive(Debug)]
struct ApiError {
    status: StatusCode,
    message: String,
}

impl ApiError {
    fn unauthorized() -> Self {
        Self {
            status: StatusCode::UNAUTHORIZED,
            message: "invalid dashboard token".to_string(),
        }
    }

    fn bad_request(message: &str) -> Self {
        Self {
            status: StatusCode::BAD_REQUEST,
            message: message.to_string(),
        }
    }
}

impl From<MemhubError> for ApiError {
    fn from(error: MemhubError) -> Self {
        Self {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            message: error.to_string(),
        }
    }
}

impl IntoResponse for ApiError {
    fn into_response(self) -> Response {
        let body = Json(serde_json::json!({ "error": self.message }));
        (self.status, body).into_response()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_non_loopback_bind_address() {
        let err = resolve_bind_addr("0.0.0.0", 0).expect_err("non-loopback should fail");
        assert!(err.to_string().contains("loopback"));
    }

    #[test]
    fn projects_vectors_to_stable_two_dimensional_space() {
        let vectors = vec![
            &[1.0, 0.0, 0.0][..],
            &[0.0, 1.0, 0.0][..],
            &[0.0, 0.0, 1.0][..],
        ];
        let coords = pca_2d(vectors);
        assert_eq!(coords.len(), 3);
        assert!(
            coords
                .iter()
                .all(|(x, y)| x.is_finite() && y.is_finite() && x.abs() <= 1.0 && y.abs() <= 1.0)
        );
    }

    #[test]
    fn metrics_payload_reports_disabled_when_not_enabled() {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        let payload = read_metrics(temp.path()).expect("read_metrics");
        assert!(!payload.enabled);
        assert!(payload.sessions.is_empty());
        assert_eq!(payload.totals_7d.recalls, 0);
        assert!(payload.totals_7d.context_offset_pct.is_none());
    }

    #[test]
    fn metrics_payload_is_empty_when_enabled_with_no_data() {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        crate::commands::metrics::enable(temp.path()).expect("enable");
        let payload = read_metrics(temp.path()).expect("read_metrics");
        assert!(payload.enabled);
        assert!(payload.sessions.is_empty());
        assert_eq!(payload.totals_30d.sessions, 0);
    }

    #[test]
    fn metrics_payload_maps_session_and_offset_when_data_present() {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        crate::commands::metrics::enable(temp.path()).expect("enable");
        {
            let ctx = crate::db::open_project(temp.path()).expect("open");
            ctx.conn
                .execute(
                    "INSERT INTO session_metrics \
                     (session_id, agent, started_at, ended_at, \
                      input_tokens, output_tokens, cache_read_tokens, \
                      cache_creation_tokens, recall_calls) \
                     VALUES ('dash-session-001', 'claude-code', \
                             datetime('now', '-1 hour'), datetime('now'), \
                             1000, 500, 200, 100, 3)",
                    [],
                )
                .expect("insert session");
            ctx.conn
                .execute(
                    "INSERT INTO recall_metrics \
                     (ts, session_id, query_hash, bundle_tokens, ledger_tokens, \
                      rerank_used, result_count) \
                     VALUES (datetime('now'), 'dash-session-001', 'deadbeef', \
                             250, 1000, 1, 6)",
                    [],
                )
                .expect("insert recall");
        }
        let payload = read_metrics(temp.path()).expect("read_metrics");
        assert!(payload.enabled);
        assert_eq!(payload.sessions.len(), 1);
        assert_eq!(payload.sessions[0].input_tokens, 1000);
        assert_eq!(payload.sessions[0].output_tokens, 500);
        assert_eq!(payload.totals_7d.recalls, 1);
        assert_eq!(
            payload.totals_7d.context_offset_pct,
            Some(25.0),
            "250/1000 should be 25%"
        );
    }

    #[test]
    fn series_payload_is_empty_and_disabled_when_metrics_off() {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        let payload = read_series(temp.path(), crate::commands::metrics::SeriesWindow::Days(7))
            .expect("read_series");
        assert!(!payload.enabled);
        assert!(payload.points.is_empty());
        assert_eq!(payload.window, "7d");
        assert_eq!(payload.granularity, "session");
    }

    #[test]
    fn series_payload_maps_per_turn_points_and_counterfactual() {
        let temp = tempfile::tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        crate::commands::metrics::enable(temp.path()).expect("enable");
        {
            let ctx = crate::db::open_project(temp.path()).expect("open");
            ctx.conn
                .execute(
                    "INSERT INTO session_metrics \
                     (session_id, agent, started_at, ended_at, \
                      input_tokens, output_tokens) \
                     VALUES ('dash-s', 'claude-code', \
                             '2026-05-15T10:00:00.000Z', \
                             '2026-05-15T10:05:00.000Z', 400, 100)",
                    [],
                )
                .expect("session");
            ctx.conn
                .execute(
                    "INSERT INTO session_turn_metrics \
                     (session_id, ts, input_tokens, output_tokens) VALUES \
                     ('dash-s', '2026-05-15T10:00:00.000Z', 100, 20), \
                     ('dash-s', '2026-05-15T10:05:00.000Z', 280, 80)",
                    [],
                )
                .expect("turns");
            ctx.conn
                .execute(
                    "INSERT INTO recall_metrics \
                     (ts, session_id, query_hash, bundle_tokens, \
                      ledger_tokens, rerank_used, result_count) \
                     VALUES ('2026-05-15 10:06:00', 'dash-s', 'h', 50, 950, 1, 4)",
                    [],
                )
                .expect("recall");
        }
        let payload = read_series(temp.path(), crate::commands::metrics::SeriesWindow::Session)
            .expect("read_series");
        assert!(payload.enabled);
        assert_eq!(payload.session_id.as_deref(), Some("dash-s"));
        assert_eq!(payload.points.len(), 2);
        assert_eq!(payload.points[0].actual, 120);
        assert_eq!(payload.points[1].actual, 480);
        // recall ts 10:06 is after both turns → flushed onto last turn.
        assert_eq!(payload.points[1].counterfactual, 480 + 900);
        assert!(payload.has_recall_signal);
    }

    #[test]
    fn audit_and_embeddings_payloads_omit_confidence() {
        // Regression guard for issue #54: the stored `confidence` column is
        // always 1.0 and must not leak into either dashboard surface that
        // used to report it (the Audit histogram and the embeddings scatter).
        let temp = tempfile::tempdir().expect("tempdir");
        crate::commands::init::run(temp.path()).expect("init");
        crate::commands::fact::add(temp.path(), "k", "v", "user", "cli:user").expect("add fact");

        let ctx = db::open_project(temp.path()).expect("open project");
        let fact_id: i64 = ctx
            .conn
            .query_row("SELECT id FROM facts WHERE key = 'k'", [], |r| r.get(0))
            .expect("fact id");

        let vector: Vec<f32> = (0..8).map(|i| i as f32 * 0.01).collect();
        let mut blob = Vec::with_capacity(vector.len() * 4);
        for v in &vector {
            blob.extend_from_slice(&v.to_le_bytes());
        }
        ctx.conn
            .execute(
                "INSERT INTO embeddings(project_id, source_type, source_id, model_name, dimension, vector, content_hash)
                 VALUES (1, 'fact', ?1, ?2, 8, ?3, 'hash')",
                params![fact_id, EMBEDDING_MODEL_NAME, blob],
            )
            .expect("insert embedding");

        let audit = read_audit(temp.path()).expect("read_audit");
        let audit_json = serde_json::to_string(&audit).expect("serialize audit");
        assert!(
            !audit_json.contains("confidence"),
            "audit payload must not mention confidence: {audit_json}"
        );

        let embeddings = read_embeddings(temp.path()).expect("read_embeddings");
        assert_eq!(embeddings.points.len(), 1);
        let embeddings_json = serde_json::to_string(&embeddings).expect("serialize embeddings");
        assert!(
            !embeddings_json.contains("confidence"),
            "embeddings payload must not mention confidence: {embeddings_json}"
        );
    }
}
