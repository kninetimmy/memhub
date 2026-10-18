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

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::time::Instant;

use rmcp::schemars;
use rusqlite::{Connection, Row, params};

use crate::config::{RetrievalMode, RetrievalScoringConfig};
use crate::db;
use crate::retrieval::embeddings::{
    EMBEDDING_DIMENSION, EMBEDDING_MODEL_NAME, embed_batch, embed_one,
};
use crate::retrieval::persist::{
    SourceType, arch_section_embed_text, decision_embed_text, doc_chunk_embed_text,
    fact_embed_text, note_embed_text, task_embed_text,
};
use crate::retrieval::rerank;
use crate::retrieval::util::{
    build_fts_match, build_fts_match_any, bytes_to_vector, cosine_similarity, normalize_fts,
    sha256_hex,
};
use crate::{MemhubError, Result};

const PER_SOURCE_FTS_LIMIT: i64 = 50;

/// Most extra phrasings one recall call may carry. Each variant costs an
/// embedding, a full gather and (via the merged pool) up to
/// `rerank_candidate_pool` more cross-encoder inputs, so the cap bounds the
/// per-call cost.
pub const MAX_QUERY_VARIANTS: usize = 4;

/// Shared check for caller-written query variants (recall CLI, MCP tool and
/// the golden-set loader): at most [`MAX_QUERY_VARIANTS`], none blank.
/// Returns the variants trimmed.
pub fn validate_variants(variants: &[String]) -> Result<Vec<String>> {
    if variants.len() > MAX_QUERY_VARIANTS {
        return Err(MemhubError::InvalidInput(format!(
            "recall accepts at most {MAX_QUERY_VARIANTS} query variants per call, got {}; \
             each variant adds an embedding and more re-rank work",
            variants.len()
        )));
    }
    variants
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let v = v.trim();
            if v.is_empty() {
                Err(MemhubError::InvalidInput(format!(
                    "recall query variant {} is blank; send a non-empty alternative \
                     phrasing or omit it",
                    i + 1
                )))
            } else {
                Ok(v.to_string())
            }
        })
        .collect()
}

/// Strength multiplier on the normalized FTS score when it comes from the
/// partial-token (`OR`) fallback. bm25 is min-max normalized over the hits,
/// so without this the best partial hit would blend as if it matched every
/// query token (about 0.82 vs about 0.36 for a vector-only row) and crowd
/// the rerank pool.
const PARTIAL_FTS_FACTOR: f64 = 0.5;

/// Which agent-facing surface issued a recall — the CLI process or the
/// MCP server's `recall` tool (issue #70, Wave 4 gate Q17 / decision
/// 148). Threaded into `RecallOptions` purely so `recall_proxy::log_recall`
/// can tag the `recall_metrics.surface` column; nothing else reads it.
/// Q17 asks whether CLI-issued recalls feel slower than MCP-issued ones,
/// and this is the signal a follow-up analysis needs to split by caller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecallSurface {
    Cli,
    Mcp,
}

impl RecallSurface {
    pub fn as_str(self) -> &'static str {
        match self {
            RecallSurface::Cli => "cli",
            RecallSurface::Mcp => "mcp",
        }
    }
}

/// Caller-supplied recall request.
#[derive(Debug, Clone)]
pub struct RecallOptions {
    pub query: String,
    /// Optional override of `[retrieval] mode`. None = use project config.
    pub mode: Option<RetrievalMode>,
    /// 0 means "use config default".
    pub max_results: usize,
    /// Empty = the default bundle: facts, decisions, tasks and
    /// architecture sections, plus doc chunks when the repo includes docs
    /// in default recall. Never session notes; scope to `Note` explicitly.
    pub source_types: Vec<SourceType>,
    /// Accepted for compatibility and ignored: stale facts are always
    /// returned, flagged `stale: true` and ranked like fresh ones.
    pub include_stale: Option<bool>,
    /// Override of `accepted_only_by_default`.
    pub accepted_only: Option<bool>,
    /// Override of `[retrieval] use_reranker`. None = use project config.
    /// Ignored when mode resolves to fts (re-ranker only runs on hybrid).
    pub use_reranker: Option<bool>,
    /// Extra caller-written phrasings of `query` (at most
    /// [`MAX_QUERY_VARIANTS`], none blank). They only widen the candidate
    /// pool: the re-ranker and the cutoff still score every candidate
    /// against `query` or, for a two-question query, its best part, never a
    /// variant. Empty = no caller variants.
    pub variants: Vec<String>,
    /// Override of `[retrieval.scoring] rerank_cutoff`. None = use project
    /// config. Ignored when mode resolves to fts or when the re-ranker is
    /// disabled (the cutoff only applies to rerank scores).
    pub rerank_cutoff: Option<f32>,
    /// Append a `recall_metrics` row for this call (component A of
    /// decision 74's token-accounting subsystem). The agent-facing
    /// call sites — CLI and MCP server — set this to `true`. Eval
    /// sweeps pass `false` because calibration runs are not the
    /// "real usage" the metrics panel reports on. The `metrics.enabled`
    /// master switch gates the actual insert separately, so setting
    /// this `true` on a non-opted-in install is still a no-op.
    /// Doubles as the "agent-facing recall" marker for the opt-in
    /// `[retrieval] log_queries` JSONL log (see `query_log`).
    pub log_metrics: bool,
    /// Which surface issued this call — `Some(Cli)` / `Some(Mcp)` at the
    /// two agent-facing entry points, `None` everywhere else (eval
    /// sweeps, the upgrade smoke
    /// check, and unit tests that never log a row). Only consulted when
    /// `log_metrics` is true; `None` there is accurate, not a gap, since
    /// those callers never write to `recall_metrics` at all.
    pub surface: Option<RecallSurface>,
    /// The caller will show `response.explain`. Gates the one diagnostic
    /// that costs a filesystem check (`global_store_not_searched`); every
    /// other explain counter is always computed.
    pub explain: bool,
}

/// Corpus a recall hit came from. Repo-local always; `"global"` only
/// when the repo opted into machine-global memory and the store had a
/// match (M9). Precedence is provenance-tag-only: recall never drops a
/// hit for being global — the agent applies repo-overrides-global.
pub const SCOPE_REPO: &str = "repo";
pub const SCOPE_GLOBAL: &str = "global";

#[derive(Debug, Clone)]
pub struct RecallHit {
    pub rank: usize,
    pub source_type: String,
    /// `"repo"` or `"global"` (M9 provenance). Sibling to
    /// `source_type`; surfaced in CLI JSON and the MCP recall bundle.
    pub scope: String,
    pub source_id: i64,
    pub title: String,
    /// The row's full stored text. The CLI and MCP surfaces show only
    /// [`hit_text`] of it; eval matches against all of it.
    pub body: String,
    pub score: f64,
    pub fts_score: f64,
    pub vector_score: f64,
    pub stale: bool,
    /// `Some(new_id)` when this hit was superseded by another row of the
    /// same source type (Wave 3 L3). The hit is kept and demoted (via
    /// `scoring.superseded_penalty`), not excluded — this tag lets the
    /// caller see the replacement id. Only facts/decisions can carry it.
    pub superseded_by: Option<i64>,
    /// Task status (`open`, `done`, ...); `None` for every other row type.
    pub status: Option<String>,
    pub source: String,
    pub created_at: String,
    /// Cross-encoder relevance score. Some(score) when the re-ranker ran
    /// for this query (hybrid mode + `use_reranker`), None otherwise. Hits
    /// are ordered by it, highest first, after any row the query names by
    /// number; rows under `[retrieval.scoring] rerank_cutoff` are never
    /// returned (decision 235). The caller judges relevance.
    pub rerank_score: Option<f32>,
    /// Optional, unenforced classifier tag (Wave 6 W4, migration 0021).
    /// Fact-only -- always `None` for decisions, tasks, and doc chunks.
    /// A pure passthrough: never read by `score()`, so its presence never
    /// perturbs ranking (see `score_is_byte_identical_regardless_of_kind`).
    /// `None` for an untagged fact, which is every fact recall ever
    /// returned before this field existed.
    pub kind: Option<String>,
}

/// Most characters of a row's text a recall hit carries on the CLI and MCP
/// surfaces (decision 235). The full text is one `get_row` call away.
pub const HIT_TEXT_MAX_CHARS: usize = 300;

/// A hit's compact text: `body` whole when it fits in
/// [`HIT_TEXT_MAX_CHARS`] characters, else its first 299 characters and `…`.
pub fn hit_text(body: &str) -> String {
    if body.chars().count() <= HIT_TEXT_MAX_CHARS {
        return body.to_string();
    }
    let mut out: String = body.chars().take(HIT_TEXT_MAX_CHARS - 1).collect();
    out.push('…');
    out
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
    /// surfaced in `results` on this call. Zero when the caller explicitly
    /// scoped to docs (`--source-type doc`), when none are ingested, or
    /// when every ingested chunk made the returned bundle. A non-zero
    /// value is a cheap cue for the agent to decide whether a follow-up
    /// doc-scoped recall is worthwhile. Doc chunks join the *default*
    /// bundle (no explicit `source_types`) only when `[retrieval]
    /// include_docs_in_default` is set — auto-enabled by the first `doc
    /// add` (decision 90) — and even then at most `rerank_doc_cap` of them
    /// are re-ranked and only those that score high enough take a slot, so
    /// most ingested chunks still don't surface and this count stays
    /// meaningfully non-zero.
    pub available_docs: usize,
    /// Per-reason accounting of rows this call dropped, demoted or cut.
    /// Always computed (a handful of counters); the CLI and MCP surfaces
    /// print it only when the caller passes `explain`.
    pub explain: RecallExplain,
}

/// Why a recall came back empty or thin: how many candidate rows each
/// stage dropped, demoted or cut, summed over every store the call
/// searched. Pure bookkeeping -- nothing here feeds ranking.
// `JsonSchema` so the MCP recall tool publishes the field names and types.
#[derive(Debug, Clone, Default, PartialEq, Eq, serde::Serialize, schemars::JsonSchema)]
pub struct RecallExplain {
    /// Whether the cross-encoder re-ranker ran (hybrid mode, enabled, and
    /// at least one candidate). Without it the cutoff does not apply.
    pub reranker_ran: bool,
    /// Re-ranked rows under `rerank_cutoff`, of any row type.
    pub dropped_by_cutoff: usize,
    /// Rows excluded by accepted-only.
    pub accepted_only_excluded: usize,
    /// Superseded rows kept but demoted by `superseded_penalty`.
    pub superseded_demoted: usize,
    /// Candidates never sent to the re-ranker: those left outside the
    /// by-retriever fill of `rerank_candidate_pool` rows, and default-included
    /// doc chunks over `rerank_doc_cap`.
    pub rerank_pool_cut: usize,
    /// Rows beyond `max_results` cut from the final bundle (rows under the
    /// cutoff are counted only in `dropped_by_cutoff`).
    pub result_limit_cut: usize,
    /// Default-included doc chunks dropped because the re-ranker did not
    /// run to vet them.
    pub docs_dropped_no_rerank: usize,
    /// Ingested doc chunks (over the stores searched) this call did not
    /// search: the full chunk total when doc chunks were not among the
    /// searched source types, 0 when they were (default inclusion or an
    /// explicit doc scope). Unlike `available_docs`, a chunk that was
    /// searched but not surfaced does not count. The machine-global store's
    /// chunks are not counted when this repo has not opted in to it;
    /// `global_store_not_searched` reports that case.
    pub docs_not_searched: usize,
    /// A machine-global store exists but was not searched because this
    /// repo has not opted in (`[global] enabled`). Only checked when the
    /// caller requested explain (`RecallOptions::explain`); false otherwise.
    pub global_store_not_searched: bool,
}

/// Top-level entry: resolves config, gathers scored candidates from
/// the repo store and (when this repo opted into machine-global memory
/// and the store exists) the global store, merges them with `scope`
/// provenance, then runs one rerank pass over the unified pool.
///
/// The global corpus is consulted only when `[global] enabled` is set,
/// `~/.memhub/global.sqlite` exists, AND its migration ledger is at this
/// binary's newest schema (a store whose ledger is older or newer than
/// that, or cannot be read, is skipped with a `global_store_skipped`
/// warning); otherwise the repo store alone is
/// searched, as in the pre-M9 single-corpus path (the eval-regression
/// guarantee for a query without variants).
///
/// Caller-written `variants` are extra phrasings of the same question: each
/// is gathered like the main query and its own `rerank_candidate_pool` rows
/// (see `select_rerank_pool`) join the pool, but the rerank pass (and the
/// cutoff) never scores a candidate against a variant: a row is
/// scored against the query or, when the re-ranker will run and the main query
/// asks two things (`split_query`), its best part. Each part gathers like a
/// variant.
pub fn recall(start: &Path, options: RecallOptions) -> Result<RecallResponse> {
    recall_with(&db::open_project(start)?, options)
}

/// [`recall`] against an already-open project, so a caller issuing many
/// queries (the self-check) opens the project, and so runs
/// maintenance-on-open, once.
pub fn recall_with(ctx: &db::ProjectContext, options: RecallOptions) -> Result<RecallResponse> {
    let resolved = ResolvedOptions::from(&options, &ctx.config.retrieval)?;
    let scoring = &ctx.config.retrieval.scoring;
    let started = Instant::now();

    // Embed each phrasing once (main query first) and reuse it for every
    // corpus gather so the cross-corpus blended scores are computed on the
    // same basis. A lone main query (no variants, no parts) takes the plain
    // single-text embed.
    let texts: Vec<&str> = std::iter::once(resolved.query.as_str())
        .chain(resolved.variants.iter().map(String::as_str))
        .chain(resolved.parts.iter().map(String::as_str))
        .collect();
    let query_vecs: Option<Vec<Vec<f32>>> = if resolved.mode == RetrievalMode::Hybrid {
        let vecs = if texts.len() == 1 {
            vec![embed_one(&resolved.query)?]
        } else {
            embed_batch(&texts)?
        };
        if vecs.len() != texts.len() {
            return Err(MemhubError::Embedding(format!(
                "embedded {} phrasings but got {} vectors",
                texts.len(),
                vecs.len()
            )));
        }
        for v in &vecs {
            if v.len() != EMBEDDING_DIMENSION {
                return Err(MemhubError::Embedding(format!(
                    "query embedding produced {}-dim vector, expected {EMBEDDING_DIMENSION}",
                    v.len()
                )));
            }
        }
        Some(vecs)
    } else {
        None
    };
    let phrasings: Vec<Phrasing> = texts
        .iter()
        .enumerate()
        .map(|(i, text)| Phrasing {
            text,
            vec: query_vecs.as_ref().map(|v| v[i].as_slice()),
        })
        .collect();

    let mut repo = gather_scored(&ctx.conn, &resolved, scoring, &phrasings)?;
    for list in &mut repo.per_phrasing {
        for h in list {
            h.scope = SCOPE_REPO.to_string();
        }
    }

    let mut per_phrasing = repo.per_phrasing;
    let mut candidate_count = repo.candidate_count;
    let mut warnings = repo.warnings;
    // Store-wide, unlike `stale_embeddings` (candidates only): a row with no
    // vector can never be a vector candidate, so only this count reveals it.
    // The global store joins the count below when this recall searches it.
    let mut lacking_counts = if resolved.mode == RetrievalMode::Hybrid {
        Some(crate::retrieval::persist::count_lacking_embeddings(
            &ctx.conn,
        )?)
    } else {
        None
    };
    let mut global_lacking = 0;
    let mut doc_chunk_total = repo.doc_chunk_total;
    let mut explain = repo.explain;
    let repo_excluded = repo.excluded;

    // Never migrates the shared global store; one at another schema than
    // this binary's is skipped with a `global_store_skipped` warning.
    let gctx = if ctx.config.global.enabled {
        db::open_global_unmigrated()?
    } else {
        None
    };
    let skipped = gctx.as_ref().and_then(db::global_schema_mismatch);
    if let Some(reason) = skipped {
        warnings.push(RecallWarning {
            kind: "global_store_skipped".to_string(),
            stale_count: 0,
            total_count: 0,
            reason,
            fix: "run `memhub upgrade`".to_string(),
        });
    } else if let Some(gctx) = gctx {
        let g = gather_scored(&gctx.conn, &resolved, scoring, &phrasings)?;
        for (merged, mut list) in per_phrasing.iter_mut().zip(g.per_phrasing) {
            for h in &mut list {
                h.scope = SCOPE_GLOBAL.to_string();
            }
            merged.extend(list);
        }
        candidate_count += g.candidate_count;
        warnings.extend(g.warnings);
        if let Some((lacking, total)) = &mut lacking_counts {
            let (gl, gt) = crate::retrieval::persist::count_lacking_embeddings(&gctx.conn)?;
            global_lacking = gl;
            *lacking += gl;
            *total += gt;
        }
        doc_chunk_total += g.doc_chunk_total;
        explain.accepted_only_excluded += g.explain.accepted_only_excluded;
        explain.superseded_demoted += g.explain.superseded_demoted;
    } else if options.explain {
        // Diagnostic only: an unresolvable home dir must not fail a recall
        // that never needed the global store.
        explain.global_store_not_searched = matches!(db::global_store_exists(), Ok(true));
    }

    if let Some((lacking, total)) = lacking_counts.filter(|(lacking, _)| *lacking > 0) {
        let scope = if global_lacking > 0 {
            "in this repo and the global store"
        } else {
            "in this repo"
        };
        warnings.push(RecallWarning {
            kind: "missing_embeddings".to_string(),
            stale_count: lacking,
            total_count: total,
            reason: format!(
                "{lacking} of {total} row(s) {scope} have no {EMBEDDING_MODEL_NAME} \
                 embedding, so only keyword matching can find them"
            ),
            fix: "Run /reindex (memhub index rebuild) to embed them.".to_string(),
        });
    }

    // A decision, task or fact the query names by number goes first and out
    // of the ranked pool (see `named_rows`). Done before the counters below
    // so a row only the routing loaded is counted in them.
    let named = take_named_rows(
        &ctx.conn,
        &resolved,
        scoring,
        &mut per_phrasing,
        &repo_excluded,
        &mut candidate_count,
        &mut explain,
    )?;

    // Re-sort each phrasing's unified pool by blended score. Without the
    // re-ranker this is the final order; with it, `select_rerank_pool` uses
    // this order to break ties within its vector and FTS rankings.
    for list in &mut per_phrasing {
        list.sort_by(cmp_merged);
    }
    let mut merged = merge_phrasings(&resolved, per_phrasing);
    merged.named = named;

    let response = finalize(
        &resolved,
        merged,
        candidate_count,
        warnings,
        doc_chunk_total,
        explain,
        started,
    )?;

    #[cfg(feature = "metrics")]
    if options.log_metrics {
        crate::metrics::recall_proxy::log_recall(
            &ctx.conn,
            &ctx.config.metrics,
            &ctx.paths.repo_root,
            &ctx.config.render.output_dir,
            &resolved.query,
            &response,
            options.surface,
        );
    }

    // Opt-in plaintext query log (`[retrieval] log_queries`). Same
    // agent-facing gate as metrics (CLI/MCP only, never eval/hooks), but not
    // behind the hibernated `metrics` feature.
    if options.log_metrics
        && ctx.config.retrieval.log_queries
        && let Some(surface) = options.surface
    {
        let source_types: Vec<&str> = options.source_types.iter().map(|s| s.as_str()).collect();
        super::query_log::log_recall(
            &ctx.conn,
            &ctx.paths.memhub_dir,
            &super::query_log::LoggedRequest {
                surface,
                variants: &resolved.variants,
                source_types: &source_types,
                max_results: resolved.max_results,
            },
            &response,
        );
    }
    Ok(response)
}

/// One stored row in full: what `get_row` returns for a compact recall hit.
#[derive(Debug, Clone)]
pub struct StoredRow {
    pub source_type: String,
    pub scope: String,
    pub source_id: i64,
    pub title: String,
    /// The row's full stored text.
    pub body: String,
    pub created_at: String,
    /// Task status; `None` for every other row type.
    pub status: Option<String>,
    pub stale: bool,
    pub superseded_by: Option<i64>,
    pub source: String,
    pub kind: Option<String>,
}

/// The full stored text of the row a recall hit names (decision 235), by
/// its `source_type` (as a hit reports it, or the recall aliases `doc` and
/// `arch`), id and `scope` (`"repo"` or `"global"`). A global row is read
/// only when this repo opted in to machine-global memory, as recall does.
/// Read-only like recall: no `writes_log`, metrics row or query-log line.
pub fn get_row(start: &Path, source_type: &str, source_id: i64, scope: &str) -> Result<StoredRow> {
    let st = match source_type {
        "doc" => Some(SourceType::DocChunk),
        "arch" => Some(SourceType::ArchSection),
        other => parse_source_type(other),
    }
    .ok_or_else(|| {
        MemhubError::InvalidInput(format!(
            "invalid source_type '{source_type}'; expected fact, decision, task, doc_chunk, \
             note or arch_section"
        ))
    })?;
    let ctx = db::open_project(start)?;
    let stale_after = ctx.config.retrieval.fact_stale_after_days;
    let row = match scope {
        SCOPE_REPO => load_source_row(&ctx.conn, st, source_id, stale_after)?,
        SCOPE_GLOBAL => {
            if !ctx.config.global.enabled {
                return Err(MemhubError::InvalidInput(
                    "this repository has not enabled machine-global memory \
                     (`memhub global enable`), so it has no global rows"
                        .to_string(),
                ));
            }
            let gctx = db::open_global_unmigrated()?.ok_or_else(|| {
                MemhubError::InvalidInput("no machine-global store exists".to_string())
            })?;
            if let Some(reason) = db::global_schema_mismatch(&gctx) {
                return Err(MemhubError::InvalidInput(format!(
                    "{reason}; run `memhub upgrade`"
                )));
            }
            load_source_row(&gctx.conn, st, source_id, stale_after)?
        }
        other => {
            return Err(MemhubError::InvalidInput(format!(
                "invalid scope '{other}'; expected 'repo' or 'global'"
            )));
        }
    };
    let row = row.ok_or_else(|| {
        MemhubError::InvalidInput(format!(
            "no {} with id {source_id} in the {scope} store",
            st.as_str()
        ))
    })?;
    Ok(StoredRow {
        source_type: st.as_str().to_string(),
        scope: scope.to_string(),
        source_id,
        title: row.title,
        body: row.body,
        created_at: row.created_at,
        status: row.status,
        stale: row.is_stale,
        superseded_by: row.superseded_by,
        source: row.source,
        kind: row.kind,
    })
}

#[derive(Debug)]
struct ResolvedOptions {
    query: String,
    /// Validated, trimmed extra phrasings (see `validate_variants`).
    variants: Vec<String>,
    /// The parts of a two-question main query (`split_query`); empty unless
    /// the re-ranker will run. Internal: gathered like variants, and scored
    /// against by the re-ranker, but never logged or reported.
    parts: Vec<String>,
    mode: RetrievalMode,
    max_results: usize,
    source_types: Vec<SourceType>,
    /// Fact-staleness horizon in days (`[retrieval] fact_stale_after_days`).
    /// A fact never verified, or last verified more than this many days ago,
    /// is stale: flagged `stale: true`, never excluded or demoted.
    fact_stale_after_days: i64,
    accepted_only: bool,
    /// Effective rerank toggle; honored only when mode = hybrid.
    use_reranker: bool,
    /// Effective candidate pool size for the cross-encoder when active.
    rerank_candidate_pool: usize,
    /// Most default-included doc chunks the pool may hold
    /// (`[retrieval] rerank_doc_cap`); consulted only when `docs_via_default`.
    rerank_doc_cap: usize,
    /// Effective cross-encoder cutoff, for every row type; honored only
    /// when mode = hybrid and the re-ranker actually runs (decision 235).
    rerank_cutoff: f32,
    /// True only when doc chunks entered the pool implicitly via
    /// `[retrieval] include_docs_in_default` (caller passed no
    /// `source_types`). Explicit `--source-type doc` keeps this false
    /// so that path behaves exactly as before. Gates the doc cap and the
    /// "drop unvetted docs" safety rule below.
    docs_via_default: bool,
}

impl ResolvedOptions {
    fn from(opts: &RecallOptions, cfg: &crate::config::RetrievalConfig) -> Result<Self> {
        let query = opts.query.trim();
        if query.is_empty() {
            return Err(MemhubError::InvalidInput(
                "recall query cannot be empty".to_string(),
            ));
        }
        let variants = validate_variants(&opts.variants)?;
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
        // Docs join the *default* pool only when the caller passed no
        // explicit scope AND the repo opted in. Explicit scopes
        // (including `--source-type doc`) bypass this entirely.
        let docs_via_default = opts.source_types.is_empty() && cfg.include_docs_in_default;
        let source_types = if opts.source_types.is_empty() {
            // Architecture sections are always in the default bundle,
            // without the docs' opt-in (issue #232). Notes never are
            // (gate Q9).
            let mut base = vec![
                SourceType::Fact,
                SourceType::Decision,
                SourceType::Task,
                SourceType::ArchSection,
            ];
            if docs_via_default {
                base.push(SourceType::DocChunk);
            }
            base
        } else {
            let mut deduped = Vec::with_capacity(opts.source_types.len());
            for st in &opts.source_types {
                if !deduped.contains(st) {
                    deduped.push(*st);
                }
            }
            deduped
        };
        let mode = opts.mode.unwrap_or(cfg.mode);
        let use_reranker = opts.use_reranker.unwrap_or(cfg.use_reranker);
        let parts = if mode == RetrievalMode::Hybrid && use_reranker {
            split_query(query)
        } else {
            Vec::new()
        };
        Ok(Self {
            query: query.to_string(),
            variants,
            parts,
            mode,
            max_results,
            source_types,
            fact_stale_after_days: cfg.fact_stale_after_days,
            accepted_only: opts.accepted_only.unwrap_or(cfg.accepted_only_by_default),
            use_reranker,
            rerank_candidate_pool: cfg.rerank_candidate_pool.max(max_results),
            rerank_doc_cap: cfg.rerank_doc_cap,
            rerank_cutoff: opts.rerank_cutoff.unwrap_or(cfg.scoring.rerank_cutoff),
            docs_via_default,
        })
    }
}

/// Words that open a question; ` and ` followed by one marks a split point.
const QUESTION_WORDS: [&str; 14] = [
    "what", "which", "how", "why", "where", "when", "who", "do", "does", "did", "is", "are", "can",
    "should",
];

/// Split a query that asks two things (`what does X measure, and where is Y
/// set?`) at each ` and ` (a comma before it is allowed) immediately followed
/// by a whole question word, any letter case. Parts are trimmed of
/// surrounding whitespace and a trailing comma; the question word stays with
/// its part. Returns no parts (empty) when there is no split point.
fn split_query(query: &str) -> Vec<String> {
    let lower = query.to_ascii_lowercase(); // same byte offsets as `query`
    let mut parts = Vec::new();
    let mut start = 0;
    // Every " and " is a candidate, overlapping ones included ("x and and what").
    for at in (0..lower.len()).filter(|&i| lower.as_bytes()[i..].starts_with(b" and ")) {
        let rest = &lower[at + 5..];
        let word_len = rest
            .find(|c: char| !c.is_ascii_alphabetic())
            .unwrap_or(rest.len());
        // A part must hold more than a bare question word, on either side.
        let bare = |text: &str| {
            let word = text.trim_matches(|c: char| !c.is_alphanumeric());
            word.is_empty() || QUESTION_WORDS.contains(&word)
        };
        if !QUESTION_WORDS.contains(&&rest[..word_len])
            || bare(&rest[word_len..])
            || bare(&lower[start..at])
        {
            continue;
        }
        parts.push(&query[start..at]);
        start = at + 5;
    }
    if parts.is_empty() {
        return Vec::new();
    }
    parts.push(&query[start..]);
    parts
        .into_iter()
        .map(|p| p.trim().trim_end_matches(',').trim().to_string())
        .filter(|p| !p.is_empty())
        .collect()
}

/// One phrasing of the question: the main query, a caller variant, or a part.
struct Phrasing<'a> {
    text: &'a str,
    /// Query embedding (hybrid mode only).
    vec: Option<&'a [f32]>,
}

struct GatherResult {
    /// Blended-scored, sorted candidates for this single corpus
    /// (pre-rerank), one list per phrasing in `gather_scored`'s input
    /// order (main query first). `scope` is left empty here; the
    /// orchestrator tags it before the merged rerank pass.
    per_phrasing: Vec<Vec<ScoredHit>>,
    /// Distinct rows that survived the filters, whichever phrasing found
    /// them.
    candidate_count: usize,
    warnings: Vec<RecallWarning>,
    /// `COUNT(*)` of `doc_chunks` in this corpus, summed across corpora by
    /// the orchestrator. It feeds both `available_docs` and
    /// `RecallExplain::docs_not_searched`.
    doc_chunk_total: i64,
    /// Rows a retriever found that the accepted-only filter then excluded;
    /// already counted in `explain`.
    excluded: HashSet<(SourceType, i64)>,
    /// Only the per-store counters (`accepted_only_excluded`,
    /// `superseded_demoted`) are filled here.
    explain: RecallExplain,
}

/// Connection-scoped candidate gather: FTS + (hybrid) vector + stale
/// detection + hydrate + filter + blended score + sort. Connection-
/// agnostic so the orchestrator can run it once per corpus. The query
/// embedding is computed once by the caller and passed in so both the
/// repo and global gathers score on the same vector.
fn gather_scored(
    conn: &Connection,
    opts: &ResolvedOptions,
    scoring: &RetrievalScoringConfig,
    phrasings: &[Phrasing],
) -> Result<GatherResult> {
    // Per-phrasing candidates (their own FTS/vector signals); the corrupt
    // vector keys are the same for every phrasing, so they are unioned.
    let mut per: Vec<HashMap<(SourceType, i64), CandidateRow>> =
        Vec::with_capacity(phrasings.len());
    let mut corrupt_vectors: HashSet<(SourceType, i64)> = HashSet::new();
    for phrasing in phrasings {
        let (cands, corrupt) = phrasing_candidates(conn, opts, phrasing)?;
        corrupt_vectors.extend(corrupt);
        per.push(cands);
    }
    // One phrasing: its candidate map is the whole pool. Several: the union
    // of keys, so hydration, stale detection and filtering (and their
    // explain counters) see each distinct row once.
    let mut candidates: HashMap<(SourceType, i64), CandidateRow> = if per.len() == 1 {
        std::mem::take(&mut per[0])
    } else {
        let mut union = HashMap::new();
        for cands in &per {
            for &(st, id) in cands.keys() {
                union
                    .entry((st, id))
                    .or_insert_with(|| CandidateRow::empty(st, id));
            }
        }
        union
    };

    let mut warnings: Vec<RecallWarning> = Vec::new();
    if opts.mode == RetrievalMode::Hybrid {
        // Stale-embedding detection: across the candidate set, count rows
        // that are missing an embedding, whose content_hash doesn't match
        // the current body, or whose stored vector blob is the wrong
        // length (a corrupt/truncated write that `content_hash` alone
        // can't catch, since the hash covers the source text, not the
        // vector bytes — `vector_lookup` skips these for scoring but
        // reports them here rather than silently degrading to FTS-only).
        let stale = detect_stale_candidates(conn, &candidates, &corrupt_vectors)?;
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

    // Hydrate source rows for every candidate. The staleness horizon is
    // config-driven (`[retrieval] fact_stale_after_days`) so the caller can
    // widen or tighten the window without a rebuild.
    hydrate_sources(conn, &mut candidates, opts.fact_stale_after_days)?;

    // Apply filters (per §4 of the addendum, filters apply before scoring).
    // Staleness is informational (decision 236): stale facts are never
    // filtered or demoted, only flagged `stale: true`.
    let mut explain = RecallExplain::default();
    let mut excluded = HashSet::new();
    let surviving: Vec<CandidateRow> = candidates
        .into_values()
        .filter(|c| c.has_source_row())
        .filter(|c| {
            if opts.accepted_only && !is_accepted_source(&c.source) {
                explain.accepted_only_excluded += 1;
                excluded.insert((c.source_type, c.source_id));
                false
            } else {
                true
            }
        })
        .collect();
    explain.superseded_demoted = surviving
        .iter()
        .filter(|c| c.superseded_by.is_some())
        .count();

    let candidate_count = surviving.len();

    // Each phrasing is scored over the surviving rows it found, with its own
    // FTS/vector signals (the bm25 min-max is per phrasing).
    let rows_per_phrasing: Vec<Vec<CandidateRow>> = if per.len() == 1 {
        vec![surviving]
    } else {
        let by_key: HashMap<(SourceType, i64), &CandidateRow> = surviving
            .iter()
            .map(|c| ((c.source_type, c.source_id), c))
            .collect();
        per.iter()
            .map(|cands| {
                cands
                    .iter()
                    .filter_map(|(key, found)| {
                        by_key.get(key).map(|hydrated| {
                            let mut row = (*hydrated).clone();
                            row.fts_raw = found.fts_raw;
                            row.fts_partial = found.fts_partial;
                            row.vector_score = found.vector_score;
                            row
                        })
                    })
                    .collect()
            })
            .collect()
    };
    let per_phrasing: Vec<Vec<ScoredHit>> = rows_per_phrasing
        .iter()
        .map(|rows| {
            let mut scored = score(rows, scoring);
            scored.sort_by(|a, b| {
                b.score
                    .partial_cmp(&a.score)
                    .unwrap_or(std::cmp::Ordering::Equal)
                    .then_with(|| a.source_type.as_str().cmp(b.source_type.as_str()))
                    .then_with(|| a.source_id.cmp(&b.source_id))
            });
            scored
        })
        .collect();

    let doc_chunk_total = conn
        .query_row(
            "SELECT COUNT(*) FROM doc_chunks WHERE project_id = 1",
            [],
            |r| r.get::<_, i64>(0),
        )
        .optional_row()?
        .unwrap_or(0)
        .max(0);

    Ok(GatherResult {
        per_phrasing,
        candidate_count,
        warnings,
        doc_chunk_total,
        excluded,
        explain,
    })
}

/// `(candidates keyed by row, source keys whose stored vector was corrupt)`.
type PhrasingCandidates = (
    HashMap<(SourceType, i64), CandidateRow>,
    HashSet<(SourceType, i64)>,
);

/// FTS + (hybrid) vector candidates for one phrasing from one corpus.
fn phrasing_candidates(
    conn: &Connection,
    opts: &ResolvedOptions,
    phrasing: &Phrasing,
) -> Result<PhrasingCandidates> {
    let mut candidates: HashMap<(SourceType, i64), CandidateRow> = HashMap::new();
    let mut corrupt_vectors = HashSet::new();

    let fts_all = |match_expr: &str| -> Result<Vec<(SourceType, i64, f64)>> {
        let mut acc = Vec::new();
        for st in &opts.source_types {
            let hits = fts_lookup(conn, *st, match_expr)?;
            acc.extend(hits.into_iter().map(|(id, score)| (*st, id, score)));
        }
        Ok(acc)
    };
    let mut fts_results = match build_fts_match(phrasing.text) {
        Some(expr) => fts_all(&expr)?,
        None => Vec::new(),
    };
    // No row holds every query token (typical of a keyword bag or a natural
    // question): let rows matching some of the content tokens contribute
    // FTS signal instead of leaving the pool to cosine alone (issue #225).
    let fts_partial = fts_results.is_empty();
    if fts_partial && let Some(expr) = build_fts_match_any(phrasing.text) {
        fts_results = fts_all(&expr)?;
    }

    // Hydrate source rows for every FTS hit so we can apply filters and
    // assemble the response.
    for (st, id, fts_raw) in &fts_results {
        let entry = candidates
            .entry((*st, *id))
            .or_insert_with(|| CandidateRow::empty(*st, *id));
        entry.fts_raw = Some(*fts_raw);
        entry.fts_partial = fts_partial;
    }

    // Vector path (hybrid only). The query embedding is supplied by the
    // orchestrator (computed once per phrasing, reused across corpora).
    if opts.mode == RetrievalMode::Hybrid {
        let query_vec = phrasing.vec.ok_or_else(|| {
            MemhubError::Embedding("hybrid gather requires a precomputed query embedding".into())
        })?;
        // No vector-path floor — D70/D71 retired `min_vector_score` after
        // the MiniLM bundle landed. Nonsense rejection now lives in the
        // rerank cutoff applied by `finalize`.
        let (vector_hits, corrupt) = vector_lookup(conn, &opts.source_types, query_vec)?;
        corrupt_vectors = corrupt;
        for hit in &vector_hits {
            let entry = candidates
                .entry((hit.source_type, hit.source_id))
                .or_insert_with(|| CandidateRow::empty(hit.source_type, hit.source_id));
            entry.vector_score = Some(hit.cosine);
        }
    }
    Ok((candidates, corrupt_vectors))
}

/// Order of the merged pool: blended score first, then a deterministic
/// source-type / scope / id tie-break.
fn cmp_merged(a: &ScoredHit, b: &ScoredHit) -> std::cmp::Ordering {
    b.score
        .partial_cmp(&a.score)
        .unwrap_or(std::cmp::Ordering::Equal)
        .then_with(|| a.source_type.as_str().cmp(b.source_type.as_str()))
        .then_with(|| a.scope.cmp(&b.scope))
        .then_with(|| a.source_id.cmp(&b.source_id))
}

/// The merged candidate pool `finalize` re-ranks.
struct MergedPool {
    /// Sorted by `cmp_merged`. When the re-ranker will run, already cut to
    /// the rows sent to it.
    hits: Vec<ScoredHit>,
    /// Distinct rows not sent to the re-ranker (`rerank_pool_cut`); 0 when
    /// the re-ranker will not run.
    rerank_pool_cut: usize,
    /// Rows the query names by number (`take_named_rows`), in query order;
    /// set by `recall`, empty out of `merge_phrasings`. Not part of `hits`.
    named: Vec<ScoredHit>,
}

/// Choose the rows sent to the re-ranker from one phrasing's merged list.
///
/// Rows are taken from the vector-similarity ranking and the FTS ranking in
/// turn (vector first), each skipping rows already taken, until
/// `rerank_candidate_pool` rows are chosen or both rankings run out; when one
/// ranking runs out the other fills the remaining slots. So every row in the
/// top half (rounded down) of either ranking is chosen whatever its blended
/// score. Each ranking applies the stale, superseded and age-decay
/// adjustments of the blended score (`ScoredHit::{vector,fts}_rank`), the
/// stale and superseded penalties at about half their blended strength (see
/// `score`).
/// Default-included doc chunks stop being eligible once
/// `rerank_doc_cap` of them are chosen. The result keeps the input order.
fn select_rerank_pool(opts: &ResolvedOptions, hits: Vec<ScoredHit>) -> Vec<ScoredHit> {
    // `hits` is already in `cmp_merged` order, so its index breaks ties.
    let ranking = |key: fn(&ScoredHit) -> Option<f64>| -> Vec<usize> {
        let mut idx: Vec<usize> = (0..hits.len())
            .filter(|&i| key(&hits[i]).is_some())
            .collect();
        idx.sort_by(|&a, &b| {
            key(&hits[b])
                .partial_cmp(&key(&hits[a]))
                .unwrap_or(std::cmp::Ordering::Equal)
                .then(a.cmp(&b))
        });
        idx
    };
    let rankings = [ranking(|h| h.vector_rank), ranking(|h| h.fts_rank)];
    let mut next = [0usize; 2];
    let mut chosen = vec![false; hits.len()];
    let (mut chosen_count, mut doc_count) = (0, 0);
    'fill: while chosen_count < opts.rerank_candidate_pool {
        let mut progressed = false;
        for (ranked, cursor) in rankings.iter().zip(next.iter_mut()) {
            while let Some(&i) = ranked.get(*cursor) {
                *cursor += 1;
                if chosen[i] {
                    continue;
                }
                if opts.docs_via_default && hits[i].source_type == SourceType::DocChunk {
                    if doc_count >= opts.rerank_doc_cap {
                        continue;
                    }
                    doc_count += 1;
                }
                chosen[i] = true;
                chosen_count += 1;
                progressed = true;
                break;
            }
            if chosen_count >= opts.rerank_candidate_pool {
                break 'fill;
            }
        }
        if !progressed {
            break;
        }
    }
    hits.into_iter()
        .zip(chosen)
        .filter_map(|(hit, keep)| keep.then_some(hit))
        .collect()
}

/// Union the per-phrasing pools (each already sorted by `cmp_merged`) into
/// the one list `finalize` re-ranks.
///
/// Without the re-ranker nothing is cut: a single phrasing passes through
/// untouched, and several are merged ordered by each row's best blended
/// score across phrasings. When the re-ranker will run, each phrasing
/// contributes the rows `select_rerank_pool` chooses for it; the union is
/// deduplicated by row identity (scope, source type, id), so the main
/// query's pool is always a subset and a variant never pushes out a row the
/// main query would have re-ranked. A row found by several phrasings keeps
/// its best blended hit. `rerank_pool_cut` is how many distinct rows no
/// phrasing sent to the re-ranker, doc chunks over the cap included.
fn merge_phrasings(opts: &ResolvedOptions, per_phrasing: Vec<Vec<ScoredHit>>) -> MergedPool {
    let will_rerank = opts.mode == RetrievalMode::Hybrid && opts.use_reranker;
    if per_phrasing.len() == 1 && !will_rerank {
        return MergedPool {
            hits: per_phrasing.into_iter().flatten().collect(),
            rerank_pool_cut: 0,
            named: Vec::new(),
        };
    }
    let row_key = |h: &ScoredHit| (h.source_type, h.scope.clone(), h.source_id);
    let mut seen = HashSet::new();
    let mut best: HashMap<_, ScoredHit> = HashMap::new();
    for mut list in per_phrasing {
        if will_rerank {
            seen.extend(list.iter().map(row_key));
            list = select_rerank_pool(opts, list);
        }
        for hit in list {
            match best.entry(row_key(&hit)) {
                std::collections::hash_map::Entry::Occupied(mut e) => {
                    if hit.score > e.get().score {
                        e.insert(hit);
                    }
                }
                std::collections::hash_map::Entry::Vacant(e) => {
                    e.insert(hit);
                }
            }
        }
    }
    let pool_cut = seen.len().saturating_sub(best.len());
    let mut hits: Vec<ScoredHit> = best.into_values().collect();
    hits.sort_by(cmp_merged);
    MergedPool {
        hits,
        rerank_pool_cut: pool_cut,
        named: Vec::new(),
    }
}

/// Repo rows a query names by number: the words `decision`, `task` or `fact`
/// (any letter case, a whole word) followed by whitespace and/or `#` and a
/// number, e.g. `decision 201`, `Task #99`. Deduplicated, in the order the
/// query names them. Plurals (`decisions 12`), shorthand (`D17`), any other
/// word before the number, and a number glued to letters or a decimal point
/// (`task 5b`, `fact 3.5`) do not match, nor does a number followed by a unit
/// or count word (`in fact 3 rows`, `the decision 2 days ago`), by `of them`
/// (`task 3 of them`), or by a hyphen joining it to a word (`decision 2-day`,
/// `task 3-week`).
fn named_rows(query: &str) -> Vec<(SourceType, i64)> {
    let lower = query.to_ascii_lowercase();
    let b = lower.as_bytes();
    let in_word = |c: u8| c.is_ascii_alphanumeric() || c == b'_' || c == b'-';
    // Words that make `<keyword> <n> <word>` ordinary prose, not a row id.
    const UNIT_WORDS: [&str; 20] = [
        "second", "seconds", "minute", "minutes", "hour", "hours", "day", "days", "week", "weeks",
        "month", "months", "year", "years", "row", "rows", "time", "times", "item", "items",
    ];
    let mut found: Vec<(usize, SourceType, i64)> = Vec::new();
    for (word, st) in [
        ("decision", SourceType::Decision),
        ("task", SourceType::Task),
        ("fact", SourceType::Fact),
    ] {
        for (start, _) in lower.match_indices(word) {
            if start > 0 && in_word(b[start - 1]) {
                continue;
            }
            let mut i = start + word.len();
            let sep = i;
            while b.get(i).is_some_and(u8::is_ascii_whitespace) {
                i += 1;
            }
            if b.get(i) == Some(&b'#') {
                i += 1;
            }
            let digits = i;
            while b.get(i).is_some_and(u8::is_ascii_digit) {
                i += 1;
            }
            let glued = b
                .get(i)
                .is_some_and(|&c| c.is_ascii_alphanumeric() || c == b'_')
                || (b.get(i) == Some(&b'.') && b.get(i + 1).is_some_and(u8::is_ascii_digit));
            // `2-day`: a hyphen directly joining the number to a word.
            let hyphenated =
                b.get(i) == Some(&b'-') && b.get(i + 1).is_some_and(u8::is_ascii_alphabetic);
            let rest = lower[i..].trim_start();
            let next_word = rest
                .split(|c: char| !c.is_ascii_alphabetic())
                .next()
                .unwrap_or("");
            // "of them" is two words, so it cannot sit in `UNIT_WORDS`; a bare
            // "of" must keep routing. Any run of characters that are not ASCII
            // letters or digits (spaces, a hyphen, a quote) may sit between
            // "of" and "them".
            let of_them = next_word == "of"
                && rest[2..]
                    .trim_start_matches(|c: char| !c.is_ascii_alphanumeric())
                    .split(|c: char| !c.is_ascii_alphabetic())
                    .next()
                    == Some("them");
            if digits == sep
                || i == digits
                || glued
                || hyphenated
                || of_them
                || UNIT_WORDS.contains(&next_word)
            {
                continue;
            }
            if let Ok(id) = lower[digits..i].parse::<i64>() {
                found.push((start, st, id));
            }
        }
    }
    found.sort_by_key(|&(start, ..)| start);
    let mut seen = HashSet::new();
    found
        .into_iter()
        .filter_map(|(_, st, id)| seen.insert((st, id)).then_some((st, id)))
        .collect()
}

/// Pull the repo-store rows the query names by number (`named_rows`) out of
/// the per-phrasing pools and return them in query order, at most
/// `max_results`. A named row is skipped when its source type is outside the
/// search scope, it does not exist, or the stale / accepted-only filters
/// exclude it; global-store rows are never touched (their ids are their own).
/// A row the retrievers already found keeps its blended hit and the counts the
/// ranked gather gave it. Any other row is loaded and counted as the gather
/// would have counted it had a retriever found it: in `candidate_count`, the
/// superseded counter, or the accepted-only exclusion.
/// A row a retriever found but a filter excluded (`excluded`) is already
/// counted by the gather and is skipped without counting it again.
/// Named rows past `max_results` are added to `explain.result_limit_cut`.
fn take_named_rows(
    conn: &Connection,
    opts: &ResolvedOptions,
    scoring: &RetrievalScoringConfig,
    per_phrasing: &mut [Vec<ScoredHit>],
    excluded: &HashSet<(SourceType, i64)>,
    candidate_count: &mut usize,
    explain: &mut RecallExplain,
) -> Result<Vec<ScoredHit>> {
    let mut named = Vec::new();
    for (st, id) in named_rows(&opts.query) {
        if !opts.source_types.contains(&st) {
            continue;
        }
        let mut found: Option<ScoredHit> = None;
        for list in per_phrasing.iter_mut() {
            if let Some(pos) = list
                .iter()
                .position(|h| h.source_type == st && h.source_id == id && h.scope == SCOPE_REPO)
            {
                let hit = list.remove(pos);
                found.get_or_insert(hit);
            }
        }
        let hit = match found {
            Some(hit) => hit,
            None if excluded.contains(&(st, id)) => continue,
            None => {
                let mut one = HashMap::from([((st, id), CandidateRow::empty(st, id))]);
                hydrate_sources(conn, &mut one, opts.fact_stale_after_days)?;
                let row = one.remove(&(st, id)).expect("inserted above");
                if !row.has_source_row() {
                    continue;
                }
                if opts.accepted_only && !is_accepted_source(&row.source) {
                    explain.accepted_only_excluded += 1;
                    continue;
                }
                *candidate_count += 1;
                explain.superseded_demoted += usize::from(row.superseded_by.is_some());
                let mut hit = score(&[row], scoring).remove(0);
                hit.scope = SCOPE_REPO.to_string();
                hit
            }
        };
        named.push(hit);
    }
    explain.result_limit_cut += named.len().saturating_sub(opts.max_results);
    named.truncate(opts.max_results);
    Ok(named)
}

/// Merged-pool finalize: one cross-encoder rerank pass over the unified
/// (repo + global) candidate set, the doc-safety rule, truncation, and
/// `RecallResponse` assembly with `scope` provenance carried through.
fn finalize(
    opts: &ResolvedOptions,
    merged: MergedPool,
    candidate_count: usize,
    mut warnings: Vec<RecallWarning>,
    doc_chunk_total: i64,
    mut explain: RecallExplain,
    started: Instant,
) -> Result<RecallResponse> {
    let MergedPool {
        hits: mut scored,
        rerank_pool_cut,
        mut named,
    } = merged;
    // Rows the query named by number are returned first and are not subject
    // to the cutoff (an explicit lookup, not ranking); they take
    // `named.len()` of the `max_results` slots, and the ranked rows below
    // fill the rest.
    let named_count = named.len();
    let slots = opts.max_results - named_count;
    // Optional cross-encoder re-rank pass (decision 68). Only runs on
    // hybrid mode and only when the operator hasn't opted out. Takes the
    // `rerank_candidate_pool` rows `merge_phrasings` chose from the vector
    // and FTS rankings, orders them by the cross-encoder score, highest
    // first, drops anything below `rerank_cutoff` (decision 235: one low
    // cutoff that only removes nonsense; the caller judges relevance), then
    // truncates to `max_results`. fts-only callers, `use_reranker = false`
    // and a call with neither pooled nor named rows bypass entirely and keep
    // the blended-score order, with no cutoff.
    let reranked = opts.mode == RetrievalMode::Hybrid
        && opts.use_reranker
        && !(scored.is_empty() && named.is_empty());
    explain.reranker_ran = reranked;
    if reranked {
        explain.rerank_pool_cut = rerank_pool_cut;
        // Cross-encoder input mirrors the bi-encoder's embed text shape:
        // prepend the row's natural-language summary when present so the
        // re-ranker can score paraphrase matches, not just title+body
        // surface tokens (decision 72 / task #23).
        // Named rows go first so their indices are `0..named_count`; each
        // passage is re-ranked on its own, so this does not move any score.
        let docs: Vec<String> = named
            .iter()
            .chain(scored.iter())
            .map(|h| match h.summary.as_deref() {
                Some(s) if !s.trim().is_empty() => {
                    format!("{}\n\n{}\n\n{}", s, h.title, h.body)
                }
                _ => format!("{}\n\n{}", h.title, h.body),
            })
            .collect();
        let mut scored_order = rerank::rerank(&opts.query, &docs)?;
        if !opts.parts.is_empty() {
            // A row's score is its best against the whole query or any part
            // (named rows too: they ride the same pass). Back to best-first
            // for the cutoff loop.
            for part in &opts.parts {
                for (idx, score) in rerank::rerank(part, &docs)? {
                    if let Some(entry) = scored_order.iter_mut().find(|(i, _)| *i == idx)
                        && score > entry.1
                    {
                        entry.1 = score;
                    }
                }
            }
            scored_order.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        }
        // `rerank::rerank` yields best-first, so `kept` is in score order.
        let mut kept: Vec<ScoredHit> = Vec::with_capacity(scored.len());
        let mut highest_dropped = None;
        for (idx, score) in scored_order {
            if idx < named_count {
                named[idx].rerank_score = Some(score);
                continue;
            }
            if let Some(hit) = scored.get(idx - named_count) {
                if score < opts.rerank_cutoff {
                    explain.dropped_by_cutoff += 1;
                    if highest_dropped.is_none_or(|highest| score > highest) {
                        highest_dropped = Some(score);
                    }
                    continue;
                }
                let mut hit = hit.clone();
                hit.rerank_score = Some(score);
                kept.push(hit);
            }
        }
        // A returned named row means the call is not empty-handed.
        if kept.is_empty()
            && named.is_empty()
            && let Some(highest_score) = highest_dropped
        {
            warnings.push(RecallWarning {
                kind: "rerank_cutoff_dropped_all".to_string(),
                stale_count: 0,
                total_count: scored.len(),
                reason: format!(
                    "Candidates existed, but every one scored below the re-ranker cutoff \
                     ([retrieval.scoring] rerank_cutoff = {:.3}); the highest score among \
                     them was {highest_score:.3}.",
                    opts.rerank_cutoff
                ),
                fix: "Rephrase the query or add `variants`; lower [retrieval.scoring] \
                      rerank_cutoff only if real answers are being dropped."
                    .to_string(),
            });
        }
        scored = kept;
    }
    // Safety invariant: a doc may enter the *default* bundle only when
    // the cross-encoder actually vetted it. If the re-rank pass did
    // not run (fts mode, use_reranker = false, or no candidates at all:
    // neither pooled nor named rows), drop default-included docs entirely
    // rather than letting unscored chunks displace project memory. Explicit
    // `--source-type doc` (docs_via_default = false) is unaffected.
    if opts.docs_via_default && !reranked {
        let before = scored.len();
        scored.retain(|h| h.source_type != SourceType::DocChunk);
        explain.docs_dropped_no_rerank = before - scored.len();
    }
    explain.result_limit_cut += scored.len().saturating_sub(slots);
    scored.truncate(slots);
    named.extend(scored);
    let scored = named;

    let mut results = Vec::with_capacity(scored.len());
    for (idx, hit) in scored.into_iter().enumerate() {
        results.push(RecallHit {
            rank: idx + 1,
            source_type: hit.source_type.as_str().to_string(),
            scope: if hit.scope.is_empty() {
                SCOPE_REPO.to_string()
            } else {
                hit.scope
            },
            source_id: hit.source_id,
            title: hit.title,
            body: hit.body,
            score: hit.score,
            fts_score: hit.fts_score,
            vector_score: hit.vector_score,
            stale: hit.stale,
            superseded_by: hit.superseded_by,
            status: hit.status,
            source: hit.source,
            created_at: hit.created_at,
            rerank_score: hit.rerank_score,
            kind: hit.kind,
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

    // Cheap awareness signal. Three states:
    //  - explicit `--source-type doc`: 0, the caller already has docs.
    //  - docs_via_default: docs are in the pool but most won't fit the
    //    bundle, so report how many chunks did NOT surface here — the
    //    agent still benefits from knowing a doc-scoped follow-up
    //    exists for the long tail.
    //  - flag off / docs not pooled: total chunks skipped (legacy).
    let explicitly_doc_scoped =
        !opts.docs_via_default && opts.source_types.contains(&SourceType::DocChunk);
    let available_docs: usize = if explicitly_doc_scoped {
        0
    } else {
        // Repo + global doc-chunk count, summed by the orchestrator.
        let total = doc_chunk_total.max(0) as usize;
        if opts.docs_via_default {
            let surfaced = results
                .iter()
                .filter(|r| r.source_type == SourceType::DocChunk.as_str())
                .count();
            total.saturating_sub(surfaced)
        } else {
            total
        }
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
        explain: RecallExplain {
            docs_not_searched: if opts.source_types.contains(&SourceType::DocChunk) {
                0
            } else {
                doc_chunk_total.max(0) as usize
            },
            ..explain
        },
    })
}

#[derive(Debug, Clone)]
struct CandidateRow {
    source_type: SourceType,
    source_id: i64,
    fts_raw: Option<f64>,
    /// `fts_raw` came from the partial-token (`OR`) fallback; `score()`
    /// discounts it by `PARTIAL_FTS_FACTOR`.
    fts_partial: bool,
    vector_score: Option<f64>,
    title: String,
    body: String,
    /// Optional augmenting paraphrase. When `Some`, the cross-encoder
    /// rerank input is built as `summary\n\ntitle\n\nbody`; otherwise
    /// the existing `title\n\nbody` shape is preserved. Today only
    /// populated for decisions (migration 0011 / decision 72).
    summary: Option<String>,
    source: String,
    is_stale: bool,
    /// `Some(new_id)` once hydrated for a superseded fact/decision (Wave 3
    /// L3). Drives the `superseded_penalty` demotion in `score()` and the
    /// `superseded_by` tag on the returned hit.
    superseded_by: Option<i64>,
    /// Task status once hydrated; `None` for every other row type.
    status: Option<String>,
    created_at: String,
    /// Decay-eligible age in days (Wave 3 L6): `julianday('now') -
    /// julianday(verified_at)` for facts, `... - julianday(updated_at)` for
    /// *done* tasks. `None` for decisions, doc chunks, open/blocked tasks,
    /// and never-verified facts — rows that carry no age and so never decay.
    /// Drives the `age_half_life_days` multiplier in `score()`.
    age_days: Option<f64>,
    /// Optional, unenforced classifier tag (Wave 6 W4, migration 0021),
    /// fact-only. Carried straight through to the returned `RecallHit`;
    /// `score()` never reads it.
    kind: Option<String>,
    hydrated: bool,
}

impl CandidateRow {
    fn empty(source_type: SourceType, source_id: i64) -> Self {
        Self {
            source_type,
            source_id,
            fts_raw: None,
            fts_partial: false,
            vector_score: None,
            title: String::new(),
            body: String::new(),
            summary: None,
            source: String::new(),
            is_stale: false,
            superseded_by: None,
            status: None,
            created_at: String::new(),
            age_days: None,
            kind: None,
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
    /// Provenance tag, assigned by the orchestrator after a per-corpus
    /// gather (`SCOPE_REPO` / `SCOPE_GLOBAL`). `score()` leaves it
    /// empty; it is filled before the merged rerank pass.
    scope: String,
    source_id: i64,
    title: String,
    body: String,
    /// Carried forward from the candidate row so the cross-encoder
    /// rerank input can be built as `summary\n\ntitle\n\nbody` when
    /// present (decision 72). Not exposed on the public RecallHit
    /// shape — callers see only title/body/score/etc.
    summary: Option<String>,
    score: f64,
    /// Position keys for `select_rerank_pool`'s two rankings: the row's FTS
    /// (`fts_score`) / vector (`vector_score`) component with the same
    /// age-decay, stale and superseded adjustments as `score`. `None` when
    /// the phrasing found the row through the other retriever only.
    fts_rank: Option<f64>,
    vector_rank: Option<f64>,
    fts_score: f64,
    vector_score: f64,
    stale: bool,
    /// Carried from the candidate row so the returned hit can be tagged
    /// `superseded_by: N` (Wave 3 L3). `score()` also uses it to apply
    /// `superseded_penalty`.
    superseded_by: Option<i64>,
    source: String,
    created_at: String,
    /// Cross-encoder relevance score, populated only when the re-ranker
    /// ran (hybrid mode + use_reranker + a non-empty candidate set).
    /// Used both as the final ordering key and against the nonsense
    /// cutoff (`rerank_cutoff`). See decisions 68, 235.
    rerank_score: Option<f32>,
    /// Task status, carried to the returned hit.
    status: Option<String>,
    /// Carried from the candidate row so the returned hit can be tagged
    /// with its fact `kind` (Wave 6 W4, migration 0021). `score()` never
    /// reads this -- it is a pure passthrough, unlike `superseded_by`.
    kind: Option<String>,
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
        SourceType::Note => {
            "SELECT session_notes_fts.rowid, bm25(session_notes_fts) AS score \
             FROM session_notes_fts \
             WHERE session_notes_fts MATCH ?1 \
             ORDER BY score ASC \
             LIMIT ?2"
        }
        SourceType::ArchSection => {
            "SELECT arch_sections_fts.rowid, bm25(arch_sections_fts) AS score \
             FROM arch_sections_fts \
             WHERE arch_sections_fts MATCH ?1 \
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

/// `(matching vector hits, source keys whose stored vector was corrupt)`.
type VectorLookupResult = (Vec<VectorHit>, HashSet<(SourceType, i64)>);

fn vector_lookup(
    conn: &Connection,
    source_types: &[SourceType],
    query_vec: &[f32],
) -> Result<VectorLookupResult> {
    // Filter the source types in SQL so the scan never fetches (or decodes)
    // vectors of types this call does not search.
    let allowed: Vec<&'static str> = source_types.iter().map(|st| st.as_str()).collect();
    let placeholders = (0..allowed.len())
        .map(|i| format!("?{}", i + 3))
        .collect::<Vec<_>>()
        .join(", ");
    let mut stmt = conn.prepare(&format!(
        "SELECT source_type, source_id, vector \
         FROM embeddings \
         WHERE model_name = ?1 AND dimension = ?2 AND source_type IN ({placeholders})"
    ))?;
    let dimension = EMBEDDING_DIMENSION as i64;
    let mut bound: Vec<&dyn rusqlite::ToSql> = vec![&EMBEDDING_MODEL_NAME, &dimension];
    bound.extend(allowed.iter().map(|st| st as &dyn rusqlite::ToSql));
    let rows = stmt.query_map(bound.as_slice(), |row| {
        let st: String = row.get(0)?;
        let id: i64 = row.get(1)?;
        let blob: Vec<u8> = row.get(2)?;
        Ok((st, id, blob))
    })?;

    let mut out = Vec::new();
    // Rows whose stored vector blob is the wrong byte length for
    // `EMBEDDING_DIMENSION` -- a corrupt or truncated write. These are
    // skipped for scoring (there's nothing to compute cosine similarity
    // against) but the caller folds them into the `stale_embeddings`
    // warning rather than letting them vanish silently.
    let mut corrupt = HashSet::new();
    for row in rows {
        let (st, id, blob) = row?;
        let source_type = match parse_source_type(&st) {
            Some(t) => t,
            None => continue,
        };
        if blob.len() != EMBEDDING_DIMENSION * 4 {
            corrupt.insert((source_type, id));
            continue;
        }
        let candidate_vec = bytes_to_vector(&blob);
        let cosine = cosine_similarity(query_vec, &candidate_vec);
        // The only caller always passed 0.0 here (see the call site's
        // comment) — D70/D71 retired the configurable floor, leaving a
        // parameter that never varied. Issue #69 inlines that dead
        // plumbing; a negative-cosine candidate still never enters the
        // vector path, unchanged from before.
        if cosine < 0.0 {
            continue;
        }
        out.push(VectorHit {
            source_type,
            source_id: id,
            cosine,
        });
    }
    Ok((out, corrupt))
}

struct StaleSummary {
    stale_count: usize,
    total_count: usize,
    reason: String,
}

fn detect_stale_candidates(
    conn: &Connection,
    candidates: &HashMap<(SourceType, i64), CandidateRow>,
    corrupt_vectors: &HashSet<(SourceType, i64)>,
) -> Result<StaleSummary> {
    // Union of this query's candidates with any corrupt-length embedding
    // rows `vector_lookup` found in the requested source types -- the
    // latter may include rows this query never matched via FTS, since
    // that scan covers every embedding row in scope, not just the
    // candidate set.
    let mut keys: HashSet<(SourceType, i64)> = candidates.keys().copied().collect();
    keys.extend(corrupt_vectors.iter().copied());
    if keys.is_empty() {
        return Ok(StaleSummary {
            stale_count: 0,
            total_count: 0,
            reason: String::new(),
        });
    }

    let mut stale_count = 0;
    let total_count = keys.len();
    let mut reason_missing = false;
    let mut reason_drift = false;
    let mut reason_corrupt = false;

    let mut stmt = conn.prepare(
        "SELECT content_hash FROM embeddings \
         WHERE source_type = ?1 AND source_id = ?2 AND model_name = ?3",
    )?;

    for key in &keys {
        if corrupt_vectors.contains(key) {
            // A length-mismatched blob is decisively stale regardless of
            // what content_hash says -- the hash covers the source text,
            // not the vector bytes, so it can't detect this corruption.
            stale_count += 1;
            reason_corrupt = true;
            continue;
        }
        let current_text = match current_embed_text(conn, key.0, key.1)? {
            Some(t) => t,
            None => continue,
        };
        let current_hash = sha256_hex(current_text.as_bytes());
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

    let reason = match (reason_missing, reason_drift, reason_corrupt) {
        (false, false, false) => String::new(),
        (true, false, false) => "missing_embeddings".to_string(),
        (false, true, false) => "content_drift".to_string(),
        (false, false, true) => "corrupt_length_mismatch".to_string(),
        (true, true, false) => "missing_or_drift".to_string(),
        (true, false, true) => "missing_or_corrupt".to_string(),
        (false, true, true) => "drift_or_corrupt".to_string(),
        (true, true, true) => "missing_drift_or_corrupt".to_string(),
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
        SourceType::Note => {
            let row: std::result::Result<String, rusqlite::Error> = conn.query_row(
                "SELECT text FROM session_notes WHERE id = ?1",
                params![source_id],
                |row| row.get(0),
            );
            match row.optional_row()? {
                Some(t) => Ok(Some(note_embed_text(&t))),
                None => Ok(None),
            }
        }
        SourceType::ArchSection => {
            let row: std::result::Result<(String, String), rusqlite::Error> = conn.query_row(
                "SELECT heading_path, body FROM arch_sections WHERE id = ?1",
                params![source_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            );
            match row.optional_row()? {
                Some((h, b)) => Ok(Some(arch_section_embed_text(&h, &b))),
                None => Ok(None),
            }
        }
    }
}

fn hydrate_sources(
    conn: &Connection,
    candidates: &mut HashMap<(SourceType, i64), CandidateRow>,
    fact_stale_after_days: i64,
) -> Result<()> {
    // Group by source type to minimize prepared statements.
    let mut by_type: HashMap<SourceType, Vec<i64>> = HashMap::new();
    for key in candidates.keys() {
        by_type.entry(key.0).or_default().push(key.1);
    }

    for (st, ids) in by_type {
        for id in ids {
            if let Some(row) = load_source_row(conn, st, id, fact_stale_after_days)?
                && let Some(entry) = candidates.get_mut(&(st, id))
            {
                entry.title = row.title;
                entry.body = row.body;
                entry.summary = row.summary;
                entry.source = row.source;
                entry.is_stale = row.is_stale;
                entry.superseded_by = row.superseded_by;
                entry.status = row.status;
                entry.created_at = row.created_at;
                entry.age_days = row.age_days;
                entry.kind = row.kind;
                entry.hydrated = true;
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
    is_stale: bool,
    /// `Some(new_id)` for a superseded fact/decision (Wave 3 L3); always
    /// `None` for tasks and doc chunks, which cannot be superseded.
    superseded_by: Option<i64>,
    /// Task status; `None` for every other row type.
    status: Option<String>,
    created_at: String,
    /// Decay-eligible age in days (Wave 3 L6): `Some` only for facts (from
    /// `verified_at`) and *done* tasks (from `updated_at`); `None` for
    /// decisions, doc chunks, open/blocked tasks, and never-verified facts.
    age_days: Option<f64>,
    /// Optional, unenforced classifier tag (Wave 6 W4, migration 0021).
    /// `Some` only for a tagged fact; always `None` for decisions, tasks,
    /// and doc chunks, which have no `kind` column.
    kind: Option<String>,
}

fn load_source_row(
    conn: &Connection,
    source_type: SourceType,
    source_id: i64,
    fact_stale_after_days: i64,
) -> Result<Option<HydratedSource>> {
    match source_type {
        SourceType::Fact => {
            let mut stmt = conn.prepare(
                "SELECT key, value, source, verified_at, created_at, \
                    CASE \
                        WHEN verified_at IS NULL THEN 1 \
                        WHEN (julianday('now') - julianday(verified_at)) > ?2 THEN 1 \
                        ELSE 0 \
                    END AS is_stale, \
                    superseded_by, \
                    julianday('now') - julianday(verified_at) AS age_days, \
                    kind \
                FROM facts WHERE id = ?1",
            )?;
            let row: std::result::Result<HydratedSource, rusqlite::Error> =
                stmt.query_row(params![source_id, fact_stale_after_days], |r: &Row<'_>| {
                    let key: String = r.get(0)?;
                    let value: String = r.get(1)?;
                    let source: String = r.get(2)?;
                    let created_at: String = r.get(4)?;
                    let stale_int: i64 = r.get(5)?;
                    let superseded_by: Option<i64> = r.get(6)?;
                    // `NULL` (never-verified fact) => julianday() is NULL =>
                    // `None` => no decay (already flagged stale + demoted).
                    let age_days: Option<f64> = r.get(7)?;
                    let kind: Option<String> = r.get(8)?;
                    Ok(HydratedSource {
                        title: key,
                        body: value,
                        summary: None,
                        source,
                        is_stale: stale_int != 0,
                        superseded_by,
                        status: None,
                        created_at,
                        age_days,
                        kind,
                    })
                });
            row.optional_row().map_err(Into::into)
        }
        SourceType::Decision => {
            let mut stmt = conn.prepare(
                "SELECT title, rationale, source, decided_at, summary, superseded_by \
                 FROM decisions WHERE id = ?1",
            )?;
            let row: std::result::Result<HydratedSource, rusqlite::Error> =
                stmt.query_row(params![source_id], |r: &Row<'_>| {
                    let title: String = r.get(0)?;
                    let rationale: String = r.get(1)?;
                    let source: String = r.get(2)?;
                    let decided_at: String = r.get(3)?;
                    let summary: Option<String> = r.get(4)?;
                    let superseded_by: Option<i64> = r.get(5)?;
                    Ok(HydratedSource {
                        title,
                        body: rationale,
                        summary,
                        source,
                        is_stale: false,
                        superseded_by,
                        status: None,
                        created_at: decided_at,
                        // Decisions are excluded from age decay entirely (Q2 /
                        // decision 145): standing policy retires by
                        // supersession, not age.
                        age_days: None,
                        // `kind` (migration 0021) is a facts-only column.
                        kind: None,
                    })
                });
            row.optional_row().map_err(Into::into)
        }
        SourceType::Task => {
            // Age decay keys on `updated_at` for *done* tasks only (Wave 3
            // L6); open/blocked tasks are live work and carry no decay age
            // (`NULL` => `None`).
            let mut stmt = conn.prepare(
                "SELECT title, notes, created_at, \
                    CASE WHEN status = 'done' \
                         THEN julianday('now') - julianday(updated_at) \
                         ELSE NULL END AS age_days, \
                    status \
                 FROM tasks WHERE id = ?1",
            )?;
            let row: std::result::Result<HydratedSource, rusqlite::Error> =
                stmt.query_row(params![source_id], |r: &Row<'_>| {
                    let title: String = r.get(0)?;
                    let notes: Option<String> = r.get(1)?;
                    let created_at: String = r.get(2)?;
                    let age_days: Option<f64> = r.get(3)?;
                    let status: String = r.get(4)?;
                    Ok(HydratedSource {
                        title,
                        body: notes.unwrap_or_default(),
                        summary: None,
                        source: String::new(),
                        status: Some(status),
                        is_stale: false,
                        superseded_by: None,
                        created_at,
                        age_days,
                        // `kind` (migration 0021) is a facts-only column.
                        kind: None,
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
                        is_stale: false,
                        superseded_by: None,
                        status: None,
                        created_at,
                        // Doc chunks are excluded from age decay (Wave 3 L6).
                        age_days: None,
                        // `kind` (migration 0021) is a facts-only column.
                        kind: None,
                    })
                });
            row.optional_row().map_err(Into::into)
        }
        SourceType::Note => {
            // No title-analog column exists for a note (unlike a fact's
            // key or a doc chunk's heading_path), so `actor` — the
            // provenance tag — doubles as the short scannable label;
            // `text` is the full body.
            let mut stmt =
                conn.prepare("SELECT actor, text, created_at FROM session_notes WHERE id = ?1")?;
            let row: std::result::Result<HydratedSource, rusqlite::Error> =
                stmt.query_row(params![source_id], |r: &Row<'_>| {
                    let actor: String = r.get(0)?;
                    let text: String = r.get(1)?;
                    let created_at: String = r.get(2)?;
                    Ok(HydratedSource {
                        title: actor,
                        body: text,
                        summary: None,
                        // No source-vocabulary column on session_notes
                        // (mirrors tasks, which also carry no `source`) —
                        // `accepted_only` therefore excludes notes just
                        // like it does tasks, since neither is a vetted
                        // claim in the fact/decision sense.
                        source: String::new(),
                        is_stale: false,
                        superseded_by: None,
                        status: None,
                        created_at,
                        // Notes carry no decay-eligible age (Wave 3 L6
                        // scope): write-only scratch, not a durable claim
                        // that ages toward staleness.
                        age_days: None,
                        // `kind` (migration 0021) is a facts-only column.
                        kind: None,
                    })
                });
            row.optional_row().map_err(Into::into)
        }
        SourceType::ArchSection => {
            // Title names the narrative plus the section's heading path so
            // a hit says which part of the architecture it is.
            let mut stmt = conn.prepare(
                "SELECT heading_path, body, created_at FROM arch_sections WHERE id = ?1",
            )?;
            let row: std::result::Result<HydratedSource, rusqlite::Error> =
                stmt.query_row(params![source_id], |r: &Row<'_>| {
                    let heading_path: String = r.get(0)?;
                    let body: String = r.get(1)?;
                    let created_at: String = r.get(2)?;
                    let title = if heading_path.trim().is_empty() {
                        ARCH_SECTION_TITLE.to_string()
                    } else {
                        format!("{ARCH_SECTION_TITLE} — {heading_path}")
                    };
                    Ok(HydratedSource {
                        title,
                        body,
                        summary: None,
                        // No source-vocabulary column (like tasks), so
                        // `accepted_only` excludes sections as it does tasks.
                        source: String::new(),
                        is_stale: false,
                        superseded_by: None,
                        status: None,
                        created_at,
                        // Only the latest body has sections, so no decay age.
                        age_days: None,
                        // `kind` (migration 0021) is a facts-only column.
                        kind: None,
                    })
                });
            row.optional_row().map_err(Into::into)
        }
    }
}

/// Title prefix of an architecture-section hit; the heading path follows.
const ARCH_SECTION_TITLE: &str = "Architecture";

fn is_accepted_source(source: &str) -> bool {
    source == "user" || source.starts_with("user+agent:")
}

fn parse_source_type(raw: &str) -> Option<SourceType> {
    match raw {
        "fact" => Some(SourceType::Fact),
        "decision" => Some(SourceType::Decision),
        "task" => Some(SourceType::Task),
        "doc_chunk" => Some(SourceType::DocChunk),
        "note" => Some(SourceType::Note),
        "arch_section" => Some(SourceType::ArchSection),
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
                Some(raw) => {
                    let strength = if c.fts_partial {
                        PARTIAL_FTS_FACTOR
                    } else {
                        1.0
                    };
                    normalize_fts(-raw, fts_min, fts_max) * strength
                }
                None => 0.0,
            };
            let vector_score = c.vector_score.unwrap_or(0.0).clamp(0.0, 1.0);
            // Demotion signal (Wave 3 L3): a superseded row is kept and
            // tagged but pushed down, never excluded (demote-with-link,
            // decision 145). Staleness is deliberately not a ranking input
            // (decision 236): `stale` is only a flag on the hit.
            let superseded_penalty = if c.superseded_by.is_some() {
                scoring.superseded_penalty
            } else {
                0.0
            };
            // Optional continuous age decay (Wave 3 L6). OFF by default
            // (`age_half_life_days = 0` => multiplier is exactly 1.0, an
            // IEEE-754 no-op) so an untouched install scores byte-identically
            // to pre-L6 memhub. When enabled, the blended relevance is scaled
            // by an exponential half-life keyed on `verified_at` (facts) /
            // `updated_at` (done tasks); decisions and doc chunks carry no
            // `age_days` and are never decayed (Q2 / decision 145 — decisions
            // retire by supersession, not age). Applied as a multiplier on
            // the positive relevance term *before* the additive penalties so
            // decay never flips the sign of a penalized score. Under the
            // cross-encoder re-ranker this mostly shifts which candidates
            // enter the rerank pool rather than the final reranked order
            // (`finalize`).
            let age_decay = age_decay_multiplier(c.age_days, scoring.age_half_life_days);
            let relevance = scoring.fts_weight * fts_score + scoring.vector_weight * vector_score;
            let score = relevance * age_decay - superseded_penalty;
            // Pool rankings (`select_rerank_pool`) adjust one unweighted
            // component, which carries only its `fts_weight` / `vector_weight`
            // (0.5 by default) of the blended relevance. A penalty therefore
            // moves a row about half as far in a pool ranking as in `score`.
            // Intended: the pool is a wide net the re-ranker then judges, so
            // the penalties only need to order rows within a ranking.
            let adjusted = |component: f64| component * age_decay - superseded_penalty;
            ScoredHit {
                source_type: c.source_type,
                scope: String::new(),
                source_id: c.source_id,
                title: c.title.clone(),
                body: c.body.clone(),
                summary: c.summary.clone(),
                score,
                fts_rank: c.fts_raw.map(|_| adjusted(fts_score)),
                vector_rank: c.vector_score.map(|_| adjusted(vector_score)),
                fts_score,
                vector_score,
                stale: c.is_stale,
                superseded_by: c.superseded_by,
                source: c.source.clone(),
                created_at: c.created_at.clone(),
                rerank_score: None,
                status: c.status.clone(),
                kind: c.kind.clone(),
            }
        })
        .collect()
}

/// Continuous age-decay multiplier for a candidate's blended relevance
/// (Wave 3 L6). Returns a value in `(0, 1]`.
///
/// `half_life_days <= 0` is the OFF state (the default, `age_half_life_days
/// = 0`) and returns **exactly** `1.0` — multiplying the blended relevance
/// by `1.0` is a bit-for-bit IEEE-754 no-op, which is what makes an
/// untouched install score byte-identically to pre-L6 memhub.
///
/// When enabled (`> 0`), a row whose decay-eligible age is `days` old is
/// scaled by `2^(-days / half_life)`, i.e. it halves every `half_life_days`.
/// `age_days == None` (decisions, doc chunks, open/blocked tasks, or a fact
/// never verified) and non-positive ages both return `1.0`: decay only ever
/// demotes an aged row, it never boosts a fresh or timestamp-less one.
fn age_decay_multiplier(age_days: Option<f64>, half_life_days: i64) -> f64 {
    if half_life_days <= 0 {
        return 1.0;
    }
    match age_days {
        Some(days) if days > 0.0 => {
            // 2^(-days / half_life) == exp(-ln2 * days / half_life).
            (-days * std::f64::consts::LN_2 / half_life_days as f64).exp()
        }
        _ => 1.0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{decision, doc, fact, init, narrative, session_note, task};
    use crate::config::{ProjectConfig, RetrievalMode};
    use crate::models::NarrativeKind;
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
                rerank_cutoff: None,
                log_metrics: false,
                variants: Vec::new(),
                surface: None,
                explain: false,
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
                rerank_cutoff: None,
                log_metrics: false,
                variants: Vec::new(),
                surface: None,
                explain: false,
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
                rerank_cutoff: None,
                log_metrics: false,
                variants: Vec::new(),
                surface: None,
                explain: false,
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
                rerank_cutoff: None,
                log_metrics: false,
                variants: Vec::new(),
                surface: None,
                explain: false,
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
                rerank_cutoff: None,
                log_metrics: false,
                variants: Vec::new(),
                surface: None,
                explain: false,
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
                rerank_cutoff: None,
                log_metrics: false,
                variants: Vec::new(),
                surface: None,
                explain: false,
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
                rerank_cutoff: None,
                log_metrics: false,
                variants: Vec::new(),
                surface: None,
                explain: false,
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
                rerank_cutoff: None,
                log_metrics: false,
                variants: Vec::new(),
                surface: None,
                explain: false,
            },
        )
        .expect("recall");

        assert!(
            response.results.iter().any(|hit| hit.source_id == task_id),
            "done task should remain recallable by default"
        );
    }

    /// Force a fact's `verified_at` back by `days` so recall's staleness
    /// SQL (`julianday('now') - julianday(verified_at) > horizon`) treats it
    /// as stale without waiting real wall-clock time. `fact::add` stamps
    /// `verified_at = CURRENT_TIMESTAMP`, so a fresh fact is never stale.
    fn age_fact(temp: &std::path::Path, fact_id: i64, days: i64) {
        let ctx = crate::db::open_project(temp).expect("open");
        ctx.conn
            .execute(
                "UPDATE facts SET verified_at = datetime('now', ?1) WHERE id = ?2",
                params![format!("-{days} days"), fact_id],
            )
            .expect("age fact verified_at");
    }

    /// Pins the retired stale settings into `config.toml` as raw TOML, the
    /// way an old config carries them (`ProjectConfig` no longer writes
    /// them).
    fn pin_retired_stale_settings(temp: &std::path::Path, include: bool, penalty: f64) {
        let path = temp.join(".memhub/config.toml");
        let mut doc: toml::Table =
            toml::from_str(&std::fs::read_to_string(&path).expect("read cfg")).expect("parse cfg");
        let retrieval = doc
            .get_mut("retrieval")
            .and_then(toml::Value::as_table_mut)
            .expect("retrieval table");
        retrieval.insert(
            "include_stale_by_default".to_string(),
            toml::Value::Boolean(include),
        );
        retrieval
            .get_mut("scoring")
            .and_then(toml::Value::as_table_mut)
            .expect("scoring table")
            .insert("stale_penalty".to_string(), toml::Value::Float(penalty));
        std::fs::write(&path, toml::to_string(&doc).expect("serialize cfg")).expect("write cfg");
    }

    fn stale_probe_opts(query: &str, include_stale: Option<bool>) -> RecallOptions {
        RecallOptions {
            query: query.to_string(),
            mode: Some(RetrievalMode::Fts),
            max_results: 5,
            source_types: vec![SourceType::Fact],
            include_stale,
            accepted_only: None,
            use_reranker: None,
            rerank_cutoff: None,
            log_metrics: false,
            variants: Vec::new(),
            surface: None,
            explain: false,
        }
    }

    // Decision 236: staleness is informational. A stale fact ranks exactly
    // like the same fact freshly verified (same position, same score) and
    // carries `stale: true`; recall reports nothing about demotion.
    #[test]
    fn stale_fact_ranks_like_a_fresh_one_and_is_flagged() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let (target, _) = fact::add(
            temp.path(),
            "deploy-command",
            "kubectl apply staleflagprobe rollout",
            "user",
            "cli:user",
        )
        .expect("fact");
        for (i, body) in [
            "staleflagprobe alone",
            "staleflagprobe mentioned twice staleflagprobe here",
        ]
        .into_iter()
        .enumerate()
        {
            fact::add(
                temp.path(),
                &format!("filler-{i}"),
                body,
                "user",
                "cli:user",
            )
            .expect("filler");
        }
        let observe = || {
            let response =
                recall(temp.path(), stale_probe_opts("staleflagprobe", None)).expect("recall");
            let rank = response
                .results
                .iter()
                .position(|h| h.source_id == target)
                .expect("target fact must surface");
            let hit = &response.results[rank];
            let order: Vec<i64> = response.results.iter().map(|h| h.source_id).collect();
            (rank, hit.score, hit.stale, order, response.warnings.len())
        };

        let fresh = observe();
        assert!(!fresh.2, "a freshly verified fact is not stale");

        // 200 days old and never verified are both stale (horizon 90 days).
        age_fact(temp.path(), target, 200);
        let aged = observe();
        let ctx = crate::db::open_project(temp.path()).expect("open");
        ctx.conn
            .execute(
                "UPDATE facts SET verified_at = NULL WHERE id = ?1",
                [target],
            )
            .expect("clear verified_at");
        let unverified = observe();
        for stale in [aged, unverified] {
            assert!(stale.2, "stale fact must carry stale: true");
            assert_eq!(stale.0, fresh.0, "same rank as when fresh");
            assert_eq!(stale.1, fresh.1, "same score as when fresh");
            assert_eq!(stale.3, fresh.3, "same result order as when fresh");
            assert_eq!(stale.4, 0, "no stale warning");
        }
    }

    // The include-stale override and the retired config settings are still
    // accepted, but none of them changes which rows come back or their order.
    #[test]
    fn include_stale_and_retired_stale_settings_change_nothing() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let (stale_id, _) = fact::add(
            temp.path(),
            "deploy-command",
            "kubectl apply staleflagprobe",
            "user",
            "cli:user",
        )
        .expect("fact");
        fact::add(
            temp.path(),
            "other-command",
            "helm install staleflagprobe",
            "user",
            "cli:user",
        )
        .expect("fact");
        age_fact(temp.path(), stale_id, 200);

        let observe = |include_stale| {
            let response = recall(
                temp.path(),
                stale_probe_opts("staleflagprobe", include_stale),
            )
            .expect("recall");
            let rows: Vec<(i64, bool, f64)> = response
                .results
                .iter()
                .map(|h| (h.source_id, h.stale, h.score))
                .collect();
            assert!(
                rows.iter().any(|r| r.0 == stale_id && r.1),
                "stale fact must surface flagged: {rows:?}"
            );
            rows
        };
        let baseline = observe(None);
        assert_eq!(observe(Some(true)), baseline);
        assert_eq!(observe(Some(false)), baseline);

        for (include, penalty) in [(false, 0.9), (true, 0.0), (false, 7.5)] {
            pin_retired_stale_settings(temp.path(), include, penalty);
            ProjectConfig::load(&temp.path().join(".memhub/config.toml"))
                .expect("config with retired keys still loads");
            assert_eq!(
                observe(None),
                baseline,
                "include={include} penalty={penalty}"
            );
        }
    }

    // Wave 6 W4 (issue #97): a tagged fact's `kind` rides all the way
    // through the SQL hydration seam into the returned hit; an untagged
    // sibling in the same corpus stays `None` -- proving the migration
    // 0021 SELECT wiring end to end, not just the `score()` passthrough.
    #[test]
    fn recall_surfaces_fact_kind_when_present_and_none_when_absent() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let (tagged_id, _) = fact::add_with_kind(
            temp.path(),
            "deploy-command",
            "kubectl apply kindprobe",
            Some("command"),
            "user",
            "cli:user",
        )
        .expect("tagged fact");
        let (untagged_id, _) = fact::add(
            temp.path(),
            "other-command",
            "echo kindprobe",
            "user",
            "cli:user",
        )
        .expect("untagged fact");

        let response = recall(
            temp.path(),
            RecallOptions {
                query: "kindprobe".to_string(),
                mode: Some(RetrievalMode::Fts),
                max_results: 5,
                source_types: vec![SourceType::Fact],
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                rerank_cutoff: None,
                log_metrics: false,
                variants: Vec::new(),
                surface: None,
                explain: false,
            },
        )
        .expect("recall");

        let tagged_hit = response
            .results
            .iter()
            .find(|h| h.source_id == tagged_id)
            .expect("tagged fact must surface");
        assert_eq!(tagged_hit.kind.as_deref(), Some("command"));

        let untagged_hit = response
            .results
            .iter()
            .find(|h| h.source_id == untagged_id)
            .expect("untagged fact must surface");
        assert_eq!(untagged_hit.kind, None);
    }

    // The horizon is config-driven: `[retrieval] fact_stale_after_days`
    // widens or tightens which facts count as stale.
    #[test]
    fn fact_stale_after_days_config_controls_the_horizon() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let (fact_id, _) = fact::add(
            temp.path(),
            "deploy-command",
            "kubectl apply horizonprobe",
            "user",
            "cli:user",
        )
        .expect("fact");
        // 120 days old: stale under the default 90-day horizon.
        age_fact(temp.path(), fact_id, 120);

        let opts = |include_stale| RecallOptions {
            query: "horizonprobe".to_string(),
            mode: Some(RetrievalMode::Fts),
            max_results: 5,
            source_types: vec![SourceType::Fact],
            include_stale,
            accepted_only: None,
            use_reranker: None,
            rerank_cutoff: None,
            log_metrics: false,
            variants: Vec::new(),
            surface: None,
            explain: false,
        };

        let before = recall(temp.path(), opts(None)).expect("recall default horizon");
        let hit = before
            .results
            .iter()
            .find(|h| h.source_id == fact_id)
            .expect("fact present");
        assert!(hit.stale, "120d-old fact is stale under the 90d default");

        // Widen the horizon to a year: the same fact is now within it.
        let cfg_path = temp.path().join(".memhub/config.toml");
        let mut cfg = ProjectConfig::load(&cfg_path).expect("load cfg");
        cfg.retrieval.fact_stale_after_days = 365;
        cfg.save(&cfg_path).expect("save cfg");

        let after = recall(temp.path(), opts(None)).expect("recall widened horizon");
        let hit = after
            .results
            .iter()
            .find(|h| h.source_id == fact_id)
            .expect("fact still present");
        assert!(
            !hit.stale,
            "120d-old fact must be fresh once the horizon widens to 365d"
        );
    }

    // Wave 3 L3 — a superseded fact is kept, tagged with its replacement
    // id, and demoted below the live replacement, NOT excluded (decision
    // 145's demote-with-link). Identical values keep the two facts' BM25
    // tied so the `superseded_penalty` is the sole ordering difference.
    #[test]
    fn superseded_fact_is_demoted_and_tagged_not_excluded() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let (old_id, _) = fact::add(
            temp.path(),
            "deploy-alpha",
            "kubectl apply supersedeprobe now",
            "user",
            "cli:user",
        )
        .expect("old");
        let (new_id, _) = fact::add(
            temp.path(),
            "deploy-bravo",
            "kubectl apply supersedeprobe now",
            "user",
            "cli:user",
        )
        .expect("new");
        fact::supersede(
            temp.path(),
            &old_id.to_string(),
            &new_id.to_string(),
            "cli:user",
        )
        .expect("supersede");

        let response = recall(
            temp.path(),
            RecallOptions {
                query: "supersedeprobe".to_string(),
                mode: Some(RetrievalMode::Fts),
                max_results: 5,
                source_types: vec![SourceType::Fact],
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                rerank_cutoff: None,
                log_metrics: false,
                variants: Vec::new(),
                surface: None,
                explain: false,
            },
        )
        .expect("recall");

        // Present (not excluded) and tagged with the replacement id.
        let old_hit = response
            .results
            .iter()
            .find(|h| h.source_id == old_id)
            .expect("superseded fact must still surface, demoted");
        assert_eq!(
            old_hit.superseded_by,
            Some(new_id),
            "the hit must be tagged superseded_by: N"
        );
        let new_hit = response
            .results
            .iter()
            .find(|h| h.source_id == new_id)
            .expect("replacement present");
        assert_eq!(new_hit.superseded_by, None);
        // Demoted strictly below the live replacement.
        assert!(
            old_hit.score < new_hit.score,
            "superseded_penalty must lower the blended score: old={} new={}",
            old_hit.score,
            new_hit.score,
        );
        let old_rank = response.results.iter().position(|h| h.source_id == old_id);
        let new_rank = response.results.iter().position(|h| h.source_id == new_id);
        assert!(
            new_rank < old_rank,
            "replacement must rank above the superseded row"
        );
    }

    // ------------------------------------------------------------------
    // Wave 3 L6 — optional, default-off continuous age decay.
    // ------------------------------------------------------------------

    // The load-bearing default-off guarantee: with the knob at 0 the decay
    // multiplier is *exactly* 1.0 for every input — any age, no age, even a
    // future timestamp — so multiplying blended relevance by it is a
    // bit-for-bit no-op and an untouched install scores byte-identically to
    // pre-L6 memhub. A negative half-life is treated as off too (defensive).
    #[test]
    fn age_decay_multiplier_is_exact_noop_when_off() {
        assert_eq!(age_decay_multiplier(Some(3650.0), 0), 1.0);
        assert_eq!(age_decay_multiplier(Some(0.0), 0), 1.0);
        assert_eq!(age_decay_multiplier(Some(-10.0), 0), 1.0);
        assert_eq!(age_decay_multiplier(None, 0), 1.0);
        assert_eq!(age_decay_multiplier(Some(3650.0), -5), 1.0);
    }

    // When enabled, decay is a true exponential half-life: the multiplier
    // halves every `half_life_days`. Fresh/timestamp-less/future rows never
    // decay (and never boost above 1.0).
    #[test]
    fn age_decay_multiplier_halves_every_half_life_when_on() {
        let one = age_decay_multiplier(Some(30.0), 30);
        assert!((one - 0.5).abs() < 1e-9, "one half-life => 0.5, got {one}");
        let two = age_decay_multiplier(Some(60.0), 30);
        assert!(
            (two - 0.25).abs() < 1e-9,
            "two half-lives => 0.25, got {two}"
        );
        // Fresh (age 0), decisions/docs/non-done tasks (None), and future
        // timestamps (negative age) all stay at exactly 1.0.
        assert_eq!(age_decay_multiplier(Some(0.0), 30), 1.0);
        assert_eq!(age_decay_multiplier(None, 30), 1.0);
        assert_eq!(age_decay_multiplier(Some(-5.0), 30), 1.0);
    }

    // Proven at the `score()` seam: two facts identical except age receive
    // bit-for-bit identical blended scores when the knob is off, and that
    // score equals the plain blended value with no decay term at all.
    #[test]
    fn score_is_byte_identical_when_age_decay_off() {
        let mut fresh = CandidateRow::empty(SourceType::Fact, 1);
        fresh.vector_score = Some(0.8);
        fresh.age_days = Some(0.0);
        let mut aged = CandidateRow::empty(SourceType::Fact, 2);
        aged.vector_score = Some(0.8);
        aged.age_days = Some(4000.0);

        let scoring = RetrievalScoringConfig::default(); // age_half_life_days = 0
        assert_eq!(scoring.age_half_life_days, 0, "default must be off");
        let hits = score(&[fresh, aged], &scoring);
        let fresh_bits = hits
            .iter()
            .find(|h| h.source_id == 1)
            .unwrap()
            .score
            .to_bits();
        let aged_bits = hits
            .iter()
            .find(|h| h.source_id == 2)
            .unwrap()
            .score
            .to_bits();
        assert_eq!(
            fresh_bits, aged_bits,
            "age must not affect scoring when the knob is 0 (byte-identical)"
        );
        // No fts_raw => fts_score 0.0; not stale, not superseded => no
        // penalties. Score must equal the untouched blended relevance.
        let expected = scoring.fts_weight * 0.0 + scoring.vector_weight * 0.8;
        assert_eq!(fresh_bits, expected.to_bits());
    }

    // Wave 6 W4 (issue #97) byte-identical guarantee, proven at the same
    // `score()` seam as the L6 test above: `kind` is a pure passthrough
    // tag that `score()` never reads, so two otherwise-identical facts --
    // one tagged, one untagged -- receive bit-for-bit identical blended
    // scores. An untagged corpus's recall ranking is therefore
    // byte-identical to pre-#97 memhub.
    #[test]
    fn score_is_byte_identical_regardless_of_kind() {
        let mut untagged = CandidateRow::empty(SourceType::Fact, 1);
        untagged.vector_score = Some(0.8);
        untagged.kind = None;
        let mut tagged = CandidateRow::empty(SourceType::Fact, 2);
        tagged.vector_score = Some(0.8);
        tagged.kind = Some("gotcha".to_string());

        let scoring = RetrievalScoringConfig::default();
        let hits = score(&[untagged, tagged], &scoring);
        let untagged_bits = hits
            .iter()
            .find(|h| h.source_id == 1)
            .unwrap()
            .score
            .to_bits();
        let tagged_bits = hits
            .iter()
            .find(|h| h.source_id == 2)
            .unwrap()
            .score
            .to_bits();
        assert_eq!(
            untagged_bits, tagged_bits,
            "kind must not affect scoring, tagged or not (byte-identical)"
        );
        let expected = scoring.fts_weight * 0.0 + scoring.vector_weight * 0.8;
        assert_eq!(untagged_bits, expected.to_bits());

        // The tag itself still rides through to the returned hit untouched.
        assert_eq!(hits.iter().find(|h| h.source_id == 1).unwrap().kind, None);
        assert_eq!(
            hits.iter()
                .find(|h| h.source_id == 2)
                .unwrap()
                .kind
                .as_deref(),
            Some("gotcha")
        );
    }

    // Q2 / decision 145: decisions retire by supersession, not age. A
    // decision carries no `age_days`, so even an aggressive half-life leaves
    // its blended score bit-for-bit unchanged.
    #[test]
    fn age_decay_never_touches_decisions_when_on() {
        let mut dec = CandidateRow::empty(SourceType::Decision, 1);
        dec.vector_score = Some(0.9);
        dec.age_days = None; // decisions are never decay-keyed

        let off = RetrievalScoringConfig::default();
        let on = RetrievalScoringConfig {
            age_half_life_days: 1, // aggressive decay
            ..RetrievalScoringConfig::default()
        };
        let off_bits = score(std::slice::from_ref(&dec), &off)[0].score.to_bits();
        let on_bits = score(std::slice::from_ref(&dec), &on)[0].score.to_bits();
        assert_eq!(
            off_bits, on_bits,
            "decisions must be immune to age decay regardless of the knob"
        );
    }

    // With the knob on, an aged fact is demoted below a fresh one of equal
    // relevance — the wiring actually reaches `score()` and moves ranking.
    #[test]
    fn age_decay_demotes_an_aged_fact_when_on() {
        let mut fresh = CandidateRow::empty(SourceType::Fact, 1);
        fresh.vector_score = Some(0.8);
        fresh.age_days = Some(0.0);
        let mut aged = CandidateRow::empty(SourceType::Fact, 2);
        aged.vector_score = Some(0.8);
        aged.age_days = Some(60.0); // two 30-day half-lives => x0.25

        let on = RetrievalScoringConfig {
            age_half_life_days: 30,
            ..RetrievalScoringConfig::default()
        };
        let hits = score(&[fresh, aged], &on);
        let fresh_score = hits.iter().find(|h| h.source_id == 1).unwrap().score;
        let aged_score = hits.iter().find(|h| h.source_id == 2).unwrap().score;
        assert!(
            aged_score < fresh_score,
            "aged fact must be demoted when decay is on: fresh={fresh_score} aged={aged_score}"
        );
    }

    // Decisions hydrate `superseded_by` too, and a superseded decision is
    // likewise demoted-with-link rather than dropped from recall.
    #[test]
    fn superseded_decision_surfaces_tagged_in_recall() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let old_id = decision::add(
            temp.path(),
            "Retire supersededdecisionprobe approach",
            "supersededdecisionprobe original rationale",
            "user",
            "cli:user",
        )
        .expect("old");
        let new_id = decision::add(
            temp.path(),
            "Adopt supersededdecisionprobe successor",
            "supersededdecisionprobe replacement rationale",
            "user",
            "cli:user",
        )
        .expect("new");
        decision::supersede(temp.path(), old_id, new_id, "cli:user").expect("supersede");

        let response = recall(
            temp.path(),
            RecallOptions {
                query: "supersededdecisionprobe".to_string(),
                mode: Some(RetrievalMode::Fts),
                max_results: 5,
                source_types: vec![SourceType::Decision],
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                rerank_cutoff: None,
                log_metrics: false,
                variants: Vec::new(),
                surface: None,
                explain: false,
            },
        )
        .expect("recall");

        let old_hit = response
            .results
            .iter()
            .find(|h| h.source_id == old_id)
            .expect("superseded decision must still surface");
        assert_eq!(old_hit.superseded_by, Some(new_id));
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
        // stale_embeddings warning for the missing vector, plus (issue
        // #382) the store-wide missing_embeddings count.
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let cfg_path = temp.path().join(".memhub").join("config.toml");
        let mut cfg = crate::config::ProjectConfig::load(&cfg_path).expect("load config");
        cfg.retrieval.mode = RetrievalMode::Fts;
        cfg.save(&cfg_path).expect("save config");
        task::add(
            temp.path(),
            "Unrelated row outside the candidates",
            None,
            "cli:user",
        )
        .expect("seed task");
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
                rerank_cutoff: None,
                log_metrics: false,
                variants: Vec::new(),
                surface: None,
                explain: false,
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
        let missing = response
            .warnings
            .iter()
            .find(|w| w.kind == "missing_embeddings")
            .expect("missing_embeddings warning present");
        assert_eq!(
            (missing.stale_count, missing.total_count),
            (2, 2),
            "the task outside the candidate set counts too"
        );
        assert!(
            missing.reason.contains("2 of 2 row(s)"),
            "{}",
            missing.reason
        );
        assert!(
            missing.fix.contains("memhub index rebuild"),
            "{}",
            missing.fix
        );
        assert!(missing.fix.starts_with("Run /reindex"), "{}", missing.fix);

        // fts recall in the same repo never reports it (criterion 2).
        let fts = recall(
            temp.path(),
            RecallOptions {
                query: "cargo build".to_string(),
                mode: None,
                max_results: 5,
                source_types: vec![SourceType::Fact],
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                rerank_cutoff: None,
                log_metrics: false,
                variants: Vec::new(),
                surface: None,
                explain: false,
            },
        )
        .expect("fts recall");
        assert!(fts.warnings.iter().all(|w| w.kind != "missing_embeddings"));
    }

    #[test]
    fn hybrid_mode_flags_corrupt_length_embedding_as_stale() {
        // A vector blob whose byte length doesn't match
        // EMBEDDING_DIMENSION*4 is silently skipped by `vector_lookup`
        // (nothing to compute cosine against) -- but `content_hash` still
        // matches the source text, so the old `detect_stale_candidates`
        // never saw it. Regression for the bug where a corrupt/truncated
        // vector quietly degraded recall to FTS-only with no
        // `stale_embeddings` warning.
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let cfg_path = temp.path().join(".memhub/config.toml");
        let mut cfg = ProjectConfig::load(&cfg_path).expect("load");
        cfg.retrieval.mode = RetrievalMode::Hybrid;
        cfg.save(&cfg_path).expect("save");

        fact::add(
            temp.path(),
            "build-command",
            "cargo build",
            "user",
            "cli:user",
        )
        .expect("seed fact");

        // Corrupt the stored vector's byte length while leaving
        // content_hash untouched, simulating a truncated/corrupted write.
        let ctx = crate::db::open_project(temp.path()).expect("open");
        let updated = ctx
            .conn
            .execute(
                "UPDATE embeddings SET vector = ?1 WHERE source_type = 'fact'",
                params![vec![0u8; 4]],
            )
            .expect("corrupt vector");
        assert_eq!(updated, 1, "exactly one fact embedding row to corrupt");
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
                rerank_cutoff: None,
                log_metrics: false,
                variants: Vec::new(),
                surface: None,
                explain: false,
            },
        )
        .expect("recall");

        assert!(
            !response.results.is_empty(),
            "FTS should still surface the row"
        );
        let warn = response
            .warnings
            .iter()
            .find(|w| w.kind == "stale_embeddings")
            .expect("stale_embeddings warning must fire for a corrupt-length vector");
        assert!(warn.stale_count >= 1);
        assert!(
            warn.reason.contains("corrupt"),
            "reason should surface the corrupt-length signal, got {:?}",
            warn.reason
        );
    }

    #[test]
    fn vector_lookup_reads_only_the_searched_source_types() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let ctx = crate::db::open_project(temp.path()).expect("open");
        let good = crate::retrieval::util::vector_to_le_bytes(&vec![1.0; EMBEDDING_DIMENSION]);
        for (source_type, id, blob) in [
            ("fact", 1, good.clone()),
            ("decision", 2, good),
            ("task", 3, vec![0u8; 4]),
        ] {
            ctx.conn
                .execute(
                    "INSERT INTO embeddings(project_id, source_type, source_id, model_name, dimension, vector, content_hash) VALUES (1, ?1, ?2, ?3, ?4, ?5, 'h')",
                    params![
                        source_type,
                        id,
                        EMBEDDING_MODEL_NAME,
                        EMBEDDING_DIMENSION as i64,
                        blob
                    ],
                )
                .expect("seed embedding");
        }
        let query = vec![1.0; EMBEDDING_DIMENSION];

        let (hits, corrupt) =
            vector_lookup(&ctx.conn, &[SourceType::Fact], &query).expect("lookup facts");
        assert_eq!(hits.len(), 1);
        assert_eq!(
            (hits[0].source_type, hits[0].source_id),
            (SourceType::Fact, 1)
        );
        assert!(
            corrupt.is_empty(),
            "an unsearched type's corrupt row is not reported"
        );

        let (hits, corrupt) =
            vector_lookup(&ctx.conn, &[SourceType::Task, SourceType::Decision], &query)
                .expect("lookup tasks and decisions");
        assert_eq!(hits.len(), 1);
        assert_eq!(corrupt, HashSet::from([(SourceType::Task, 3)]));

        let (hits, corrupt) = vector_lookup(&ctx.conn, &[], &query).expect("lookup nothing");
        assert!(hits.is_empty() && corrupt.is_empty());
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
            (cfg.retrieval.scoring.rerank_cutoff
                - super::super::super::config::DEFAULT_RERANK_CUTOFF)
                .abs()
                < 1e-6
        );
        assert_eq!(
            cfg.retrieval.fact_stale_after_days,
            super::super::super::config::DEFAULT_FACT_STALE_AFTER_DAYS,
        );
        assert!(
            cfg.retrieval.include_stale_by_default,
            "retired knob keeps its serde default; recall ignores it (decision 236)"
        );
        assert_eq!(
            cfg.retrieval.rerank_doc_cap,
            super::super::super::config::DEFAULT_RERANK_DOC_CAP,
            "a config.toml without the key uses the default"
        );
    }

    /// Rerank scores (best first) of every candidate for `query`, read with
    /// the cutoff pinned below any logit so the tests can place a cutoff
    /// between two real scores whatever the model emits.
    fn probe_rerank_scores(
        temp: &std::path::Path,
        query: &str,
        source_types: Vec<SourceType>,
    ) -> Vec<f32> {
        let response = recall(
            temp,
            RecallOptions {
                rerank_cutoff: Some(-1000.0),
                ..explain_opts(query, RetrievalMode::Hybrid, 20, source_types)
            },
        )
        .expect("probe recall");
        response
            .results
            .iter()
            .map(|h| h.rerank_score.expect("reranked"))
            .collect()
    }

    #[test]
    fn nothing_clearing_the_cutoff_returns_empty_with_a_warning() {
        // Decision 235: rows under the cutoff are never returned, so a call
        // whose every candidate scores below it comes back empty and says
        // why, instead of filling the bundle with flagged rows.
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        edit_config(temp.path(), |cfg| {
            cfg.retrieval.mode = RetrievalMode::Hybrid
        });
        seed(temp.path());
        let query = "zxqv-pure-nonsense-no-real-token-anywhere-in-this-repo";
        let scores = probe_rerank_scores(temp.path(), query, vec![]);
        assert!(scores.len() > 3, "the seeded rows are all candidates");
        let cutoff = scores[0] + 1.0;

        let response = recall(
            temp.path(),
            RecallOptions {
                rerank_cutoff: Some(cutoff),
                ..explain_opts(query, RetrievalMode::Hybrid, 5, vec![])
            },
        )
        .expect("recall");

        assert!(response.results.is_empty(), "{:?}", response.results);
        assert_eq!(response.matcher, "recall:hybrid+rerank");
        assert_eq!(response.explain.dropped_by_cutoff, scores.len());
        let warning = response
            .warnings
            .iter()
            .find(|warning| warning.kind == "rerank_cutoff_dropped_all")
            .expect("an all-dropped rerank must warn callers");
        assert!(
            warning
                .reason
                .contains(&format!("rerank_cutoff = {cutoff:.3}"))
        );
        assert!(warning.reason.contains(&format!("{:.3}", scores[0])));
        assert!(warning.fix.contains("rerank_cutoff"));
        assert!(!warning.reason.contains("low_confidence"));
    }

    #[test]
    fn one_cutoff_applies_to_default_docs_and_the_retired_floors_do_nothing() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        fact::add(
            temp.path(),
            "cutoffprobe",
            "ordinary fact candidate for the rerank cutoff",
            "user",
            "cli:user",
        )
        .expect("fact");
        let doc_file = temp.path().join("cutoff-probe.md");
        std::fs::write(
            &doc_file,
            "# cutoffprobe\n\nDefault doc candidate for the rerank cutoff.\n",
        )
        .expect("write doc");
        doc::add(temp.path(), &doc_file, None, "cli:user").expect("ingest doc");

        // The retired floors are pinned impossibly high on disk; the cutoff
        // alone decides, so both rows come back, in score order. They are
        // written as raw TOML because a config save drops them (they are
        // skip_serializing), so edit_config alone would never put them on disk.
        edit_config(temp.path(), |cfg| {
            cfg.retrieval.mode = RetrievalMode::Hybrid;
            cfg.retrieval.scoring.rerank_cutoff = -1000.0;
        });
        let cfg_path = temp.path().join(".memhub/config.toml");
        let text = std::fs::read_to_string(&cfg_path).expect("read config");
        assert!(text.contains("[retrieval.scoring]"), "{text}");
        std::fs::write(
            &cfg_path,
            text.replacen(
                "[retrieval.scoring]",
                "[retrieval.scoring]
min_rerank_score = 1000.0
doc_min_rerank_score = 1001.0",
                1,
            ),
        )
        .expect("write config");
        let on_disk = std::fs::read_to_string(&cfg_path).expect("reread config");
        assert!(on_disk.contains("min_rerank_score = 1000.0"), "{on_disk}");
        assert!(
            on_disk.contains("doc_min_rerank_score = 1001.0"),
            "{on_disk}"
        );
        let loaded = ProjectConfig::load(&cfg_path).expect("load config");
        assert_eq!(loaded.retrieval.scoring.min_rerank_score, 1000.0);
        assert_eq!(loaded.retrieval.scoring.doc_min_rerank_score, 1001.0);
        let kept = recall(
            temp.path(),
            explain_opts("cutoffprobe", RetrievalMode::Hybrid, 5, vec![]),
        )
        .expect("recall");
        let mut types: Vec<&str> = kept
            .results
            .iter()
            .map(|h| h.source_type.as_str())
            .collect();
        types.sort_unstable();
        assert_eq!(types, ["doc_chunk", "fact"], "{:?}", kept.results);
        assert!(
            kept.results
                .windows(2)
                .all(|w| w[0].rerank_score >= w[1].rerank_score),
            "doc chunks and facts share one score order"
        );

        // The configured cutoff drops the doc chunk and the fact alike.
        edit_config(temp.path(), |cfg| {
            cfg.retrieval.scoring.rerank_cutoff = 1000.0;
        });
        let dropped = recall(
            temp.path(),
            explain_opts("cutoffprobe", RetrievalMode::Hybrid, 5, vec![]),
        )
        .expect("recall");
        assert!(dropped.results.is_empty(), "{:?}", dropped.results);
        assert_eq!(dropped.explain.dropped_by_cutoff, 2);
        let warning = dropped
            .warnings
            .iter()
            .find(|warning| warning.kind == "rerank_cutoff_dropped_all")
            .expect("all-dropped mixed pool must warn callers");
        assert!(warning.reason.contains("rerank_cutoff = 1000.000"));
        assert!(!warning.reason.contains("min_rerank_score"));
    }

    #[test]
    fn hybrid_cutoff_below_every_logit_keeps_every_candidate() {
        // Inverse check: with the cutoff pinned below any cross-encoder
        // logit (-1000.0), even a nonsense query returns its candidates.
        // Guards against a future refactor that hard-codes a cutoff.
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        edit_config(temp.path(), |cfg| {
            cfg.retrieval.mode = RetrievalMode::Hybrid
        });
        seed(temp.path());

        let response = recall(
            temp.path(),
            RecallOptions {
                rerank_cutoff: Some(-1000.0),
                ..explain_opts(
                    "zxqv-pure-nonsense-no-real-token-anywhere-in-this-repo",
                    RetrievalMode::Hybrid,
                    5,
                    vec![],
                )
            },
        )
        .expect("recall");

        assert!(!response.results.is_empty());
        assert!(
            response
                .warnings
                .iter()
                .all(|warning| warning.kind != "rerank_cutoff_dropped_all"),
            "a non-empty rerank result must not report an all-dropped warning"
        );
    }

    #[test]
    fn hits_come_in_strict_rerank_order_and_the_cutoff_drops_the_rest() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        edit_config(temp.path(), |cfg| {
            cfg.retrieval.mode = RetrievalMode::Hybrid
        });
        seed(temp.path());
        let query = "how do I build the project with cargo";
        let scores = probe_rerank_scores(temp.path(), query, vec![]);
        assert!(scores.len() >= 2 && scores[0] > scores[1]);
        assert!(
            scores.windows(2).all(|w| w[0] >= w[1]),
            "every row type in one strict score order: {scores:?}"
        );

        // A cutoff between the best and the second-best score: exactly one
        // candidate clears it.
        let cutoff = (scores[0] + scores[1]) / 2.0;
        let response = recall(
            temp.path(),
            RecallOptions {
                rerank_cutoff: Some(cutoff),
                ..explain_opts(query, RetrievalMode::Hybrid, 20, vec![])
            },
        )
        .expect("recall");
        assert_eq!(response.results.len(), 1);
        assert_eq!(response.results[0].rerank_score, Some(scores[0]));
        assert_eq!(response.explain.dropped_by_cutoff, scores.len() - 1);
        assert!(
            response
                .warnings
                .iter()
                .all(|w| w.kind != "rerank_cutoff_dropped_all"),
            "a recall in which a candidate clears the cutoff carries no all-dropped warning"
        );
    }

    #[test]
    fn single_row_pool_is_reranked_and_cut_with_and_without_variants() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        edit_config(temp.path(), |cfg| {
            cfg.retrieval.mode = RetrievalMode::Hybrid
        });
        fact::add(temp.path(), "lone-fact", "cargo build", "user", "cli:user").expect("fact");
        let nonsense = "zxqv-pure-nonsense-no-real-token-anywhere-in-this-repo";
        let lone = probe_rerank_scores(temp.path(), nonsense, vec![]);
        assert_eq!(lone.len(), 1);

        for variants in [
            Vec::new(),
            vec!["zxqv another nonsense phrasing".to_string()],
        ] {
            let response = recall(
                temp.path(),
                RecallOptions {
                    variants: variants.clone(),
                    rerank_cutoff: Some(lone[0] + 1.0),
                    ..explain_opts(nonsense, RetrievalMode::Hybrid, 5, vec![])
                },
            )
            .expect("recall");
            assert!(response.explain.reranker_ran, "variants {variants:?}");
            assert!(response.results.is_empty(), "variants {variants:?}");
            assert!(
                response
                    .warnings
                    .iter()
                    .any(|w| w.kind == "rerank_cutoff_dropped_all"),
                "variants {variants:?}"
            );

            // A cutoff the lone row clears: returned.
            let cleared = recall(
                temp.path(),
                RecallOptions {
                    variants,
                    rerank_cutoff: Some(-1000.0),
                    ..explain_opts(nonsense, RetrievalMode::Hybrid, 5, vec![])
                },
            )
            .expect("recall");
            assert_eq!(cleared.results.len(), 1);
        }
    }

    /// Without the re-ranker (fts mode, or `use_reranker = false` in hybrid)
    /// there is no rerank score: hits keep the blended-score order and no
    /// cutoff applies, however high it is set.
    #[test]
    fn without_the_reranker_hits_follow_the_blended_score_and_no_cutoff_applies() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        edit_config(temp.path(), |cfg| {
            cfg.retrieval.mode = RetrievalMode::Hybrid;
            cfg.retrieval.scoring.rerank_cutoff = 1000.0;
        });
        seed(temp.path());
        for (mode, use_reranker) in [
            (RetrievalMode::Fts, None),
            (RetrievalMode::Hybrid, Some(false)),
        ] {
            let response = recall(
                temp.path(),
                RecallOptions {
                    use_reranker,
                    ..explain_opts("cargo build", mode, 5, vec![])
                },
            )
            .expect("recall");
            assert!(!response.results.is_empty(), "{mode:?}");
            assert!(!response.explain.reranker_ran);
            assert_eq!(response.explain.dropped_by_cutoff, 0);
            assert!(response.results.iter().all(|h| h.rerank_score.is_none()));
            assert!(
                response
                    .results
                    .windows(2)
                    .all(|w| w[0].score >= w[1].score),
                "{mode:?}: blended-score order"
            );
        }
    }

    /// A hit carries the task status; every other row type carries none.
    #[test]
    fn task_hits_carry_their_status() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        seed(temp.path());
        let response = recall(
            temp.path(),
            explain_opts("Ship recall surface", RetrievalMode::Fts, 10, vec![]),
        )
        .expect("recall");
        let task = response
            .results
            .iter()
            .find(|h| h.source_type == "task")
            .expect("task hit");
        assert_eq!(task.status.as_deref(), Some("open"));
        assert!(
            response
                .results
                .iter()
                .filter(|h| h.source_type != "task")
                .all(|h| h.status.is_none())
        );
    }

    #[test]
    fn hit_text_keeps_short_text_and_cuts_long_text_to_the_limit() {
        assert_eq!(hit_text("short"), "short");
        let exact = "é".repeat(HIT_TEXT_MAX_CHARS);
        assert_eq!(hit_text(&exact), exact);
        let cut = hit_text(&"é".repeat(HIT_TEXT_MAX_CHARS + 1));
        assert_eq!(cut.chars().count(), HIT_TEXT_MAX_CHARS);
        assert!(cut.ends_with('…'));
    }

    /// `get_row` returns the full stored text of the row a hit names, and is
    /// read-only (no `writes_log` row).
    #[test]
    fn get_row_returns_the_full_text_and_writes_nothing() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let long = "a long decision rationale ".repeat(40);
        let id = decision::add(temp.path(), "long decision", &long, "user", "cli:user")
            .expect("decision");
        let log_rows = |p: &std::path::Path| -> i64 {
            crate::db::open_project(p)
                .expect("open")
                .conn
                .query_row("SELECT COUNT(*) FROM writes_log", [], |r| r.get(0))
                .expect("count")
        };
        let before = log_rows(temp.path());
        let row = get_row(temp.path(), "decision", id, SCOPE_REPO).expect("get_row");
        assert_eq!(row.body, long);
        assert_eq!(row.title, "long decision");
        assert_eq!(row.source_type, "decision");
        assert_eq!(row.status, None);
        assert_eq!(log_rows(temp.path()), before);

        let err = get_row(temp.path(), "decision", id + 100, SCOPE_REPO).expect_err("missing");
        assert!(err.to_string().contains("no decision"), "{err}");
        assert!(get_row(temp.path(), "widget", id, SCOPE_REPO).is_err());
        assert!(get_row(temp.path(), "decision", id, "elsewhere").is_err());
        let err = get_row(temp.path(), "decision", id, SCOPE_GLOBAL).expect_err("not opted in");
        assert!(err.to_string().contains("global"), "{err}");
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
        // `doc add` auto-enables include_docs_in_default, but in FTS
        // mode the cross-encoder never runs, so the safety rule must
        // still keep unvetted docs out of the default bundle while
        // `available_docs` advertises the un-surfaced chunks. Explicit
        // doc scope returns the chunk and zeroes the signal.
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        fact::add(
            temp.path(),
            "build-command",
            "cargo build",
            "user",
            "cli:user",
        )
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
                rerank_cutoff: None,
                log_metrics: false,
                variants: Vec::new(),
                surface: None,
                explain: false,
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
                rerank_cutoff: None,
                log_metrics: false,
                variants: Vec::new(),
                surface: None,
                explain: false,
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

    // Issue #318: `RecallExplain` accounts for every row a call dropped,
    // demoted or cut, per reason.
    fn explain_opts(
        query: &str,
        mode: RetrievalMode,
        max_results: usize,
        source_types: Vec<SourceType>,
    ) -> RecallOptions {
        RecallOptions {
            query: query.to_string(),
            mode: Some(mode),
            max_results,
            source_types,
            include_stale: None,
            accepted_only: None,
            use_reranker: None,
            rerank_cutoff: None,
            log_metrics: false,
            variants: Vec::new(),
            surface: None,
            explain: false,
        }
    }

    fn edit_config(temp: &std::path::Path, edit: impl FnOnce(&mut ProjectConfig)) {
        let cfg_path = temp.join(".memhub/config.toml");
        let mut cfg = ProjectConfig::load(&cfg_path).expect("load config");
        edit(&mut cfg);
        cfg.save(&cfg_path).expect("save config");
    }

    #[test]
    fn explain_counts_rows_excluded_by_accepted_only() {
        let temp = tempdir().expect("tempdir");
        seed(temp.path()); // `lint-command` is agent-sourced
        let response = recall(
            temp.path(),
            RecallOptions {
                accepted_only: Some(true),
                ..explain_opts("command", RetrievalMode::Fts, 10, vec![SourceType::Fact])
            },
        )
        .expect("recall");
        assert_eq!(response.explain.accepted_only_excluded, 1);
    }

    #[test]
    fn explain_counts_superseded_rows_demoted() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let (old_id, _) = fact::add(
            temp.path(),
            "deploy-alpha",
            "kubectl apply explainsupersedeprobe",
            "user",
            "cli:user",
        )
        .expect("old");
        let (new_id, _) = fact::add(
            temp.path(),
            "deploy-bravo",
            "kubectl apply explainsupersedeprobe",
            "user",
            "cli:user",
        )
        .expect("new");
        fact::supersede(
            temp.path(),
            &old_id.to_string(),
            &new_id.to_string(),
            "cli:user",
        )
        .expect("supersede");
        let response = recall(
            temp.path(),
            explain_opts(
                "explainsupersedeprobe",
                RetrievalMode::Fts,
                5,
                vec![SourceType::Fact],
            ),
        )
        .expect("recall");
        assert_eq!(response.results.len(), 2);
        assert_eq!(response.explain.superseded_demoted, 1);
    }

    #[test]
    fn explain_counts_rows_cut_by_the_result_limit() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        for key in ["limit-a", "limit-b", "limit-c"] {
            fact::add(temp.path(), key, "explainlimitprobe", "user", "cli:user").expect("fact");
        }
        let response = recall(
            temp.path(),
            explain_opts(
                "explainlimitprobe",
                RetrievalMode::Fts,
                1,
                vec![SourceType::Fact],
            ),
        )
        .expect("recall");
        assert_eq!(response.returned_count, 1);
        assert_eq!(response.explain.result_limit_cut, 2);
        assert!(!response.explain.reranker_ran);
    }

    #[test]
    fn explain_counts_default_docs_dropped_when_the_reranker_did_not_run() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let doc_file = temp.path().join("spec.md");
        std::fs::write(
            &doc_file,
            "# Design Spec\n\n## Shapes\n\nButtons use a 4px explaindocprobe corner radius.\n",
        )
        .expect("write doc");
        doc::add(temp.path(), &doc_file, None, "cli:user").expect("ingest doc");

        // Default scope + FTS: the matching chunk is pooled, then dropped
        // unvetted because the re-ranker never runs in FTS mode.
        let response = recall(
            temp.path(),
            explain_opts("explaindocprobe", RetrievalMode::Fts, 10, vec![]),
        )
        .expect("recall");
        assert!(response.results.is_empty());
        assert_eq!(response.explain.docs_dropped_no_rerank, 1);
        // The chunk was searched (docs join the default bundle), so it is
        // dropped but not "not searched" even though `available_docs` > 0.
        assert!(response.available_docs >= 1);
        assert_eq!(response.explain.docs_not_searched, 0);
        assert!(!response.explain.reranker_ran);

        // Doc-scoped: nothing dropped, nothing unsearched.
        let scoped = recall(
            temp.path(),
            explain_opts(
                "explaindocprobe",
                RetrievalMode::Fts,
                10,
                vec![SourceType::DocChunk],
            ),
        )
        .expect("recall");
        assert_eq!(scoped.explain.docs_dropped_no_rerank, 0);
        assert_eq!(scoped.explain.docs_not_searched, 0);
    }

    #[test]
    fn explain_counts_doc_chunks_not_searched_when_docs_are_out_of_scope() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        fact::add(
            temp.path(),
            "scope-fact",
            "explainscopeprobe",
            "user",
            "cli:user",
        )
        .expect("fact");
        let doc_file = temp.path().join("scope.md");
        std::fs::write(
            &doc_file,
            "# Scope

## One

explainscopeprobe first.

## Two

explainscopeprobe second.
",
        )
        .expect("write doc");
        doc::add(temp.path(), &doc_file, None, "cli:user").expect("ingest doc");
        let ingested: i64 = crate::db::open_project(temp.path())
            .expect("open")
            .conn
            .query_row("SELECT COUNT(*) FROM doc_chunks", [], |r| r.get(0))
            .expect("count chunks");
        assert!(ingested >= 2);

        // Fact-scoped: every ingested chunk is outside the searched corpora.
        let response = recall(
            temp.path(),
            explain_opts(
                "explainscopeprobe",
                RetrievalMode::Fts,
                10,
                vec![SourceType::Fact],
            ),
        )
        .expect("recall");
        assert_eq!(response.explain.docs_not_searched, ingested as usize);

        // Docs excluded from the default bundle: same count.
        edit_config(temp.path(), |cfg| {
            cfg.retrieval.include_docs_in_default = false;
        });
        let response = recall(
            temp.path(),
            explain_opts("explainscopeprobe", RetrievalMode::Fts, 10, vec![]),
        )
        .expect("recall");
        assert_eq!(response.explain.docs_not_searched, ingested as usize);
    }

    #[test]
    fn explain_counts_rows_dropped_by_the_cutoff() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        edit_config(temp.path(), |cfg| {
            cfg.retrieval.mode = RetrievalMode::Hybrid
        });
        seed(temp.path()); // after the mode switch so the rows get embeddings
        let response = recall(
            temp.path(),
            RecallOptions {
                rerank_cutoff: Some(1000.0),
                ..explain_opts("build and lint", RetrievalMode::Hybrid, 10, vec![])
            },
        )
        .expect("recall");
        assert!(response.explain.reranker_ran);
        assert!(response.candidate_count > 1);
        assert_eq!(response.explain.dropped_by_cutoff, response.candidate_count);
        assert!(response.results.is_empty());
    }

    #[test]
    fn explain_counts_rows_cut_by_the_rerank_candidate_pool() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        edit_config(temp.path(), |cfg| {
            cfg.retrieval.mode = RetrievalMode::Hybrid;
            cfg.retrieval.rerank_candidate_pool = 2;
        });
        seed(temp.path()); // after the mode switch so the rows get embeddings
        let response = recall(
            temp.path(),
            explain_opts("build and lint", RetrievalMode::Hybrid, 1, vec![]),
        )
        .expect("recall");
        assert!(response.explain.reranker_ran);
        assert!(response.candidate_count > 2);
        assert_eq!(
            response.explain.rerank_pool_cut,
            response.candidate_count - 2
        );
    }

    // Issue #319: caller-written query variants widen the candidate pool;
    // the re-ranker and the cutoff still score the main query.
    fn variant_opts(query: &str, variants: &[&str], mode: RetrievalMode) -> RecallOptions {
        RecallOptions {
            variants: variants.iter().map(|v| v.to_string()).collect(),
            ..explain_opts(query, mode, 10, vec![SourceType::Fact])
        }
    }

    #[test]
    fn variant_surfaces_a_row_the_main_query_misses() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        fact::add(
            temp.path(),
            "quokka-habitat",
            "quokkavariantprobe lives on the island",
            "user",
            "cli:user",
        )
        .expect("fact");
        fact::add(
            temp.path(),
            "wombat-habitat",
            "wombatvariantprobe digs burrows",
            "user",
            "cli:user",
        )
        .expect("fact");
        let main = "which marsupial dwells offshore";

        let plain =
            recall(temp.path(), variant_opts(main, &[], RetrievalMode::Fts)).expect("recall");
        assert!(plain.results.is_empty(), "main query alone must miss");

        let with = recall(
            temp.path(),
            variant_opts(
                main,
                &["quokkavariantprobe", "wombatvariantprobe burrows"],
                RetrievalMode::Fts,
            ),
        )
        .expect("recall");
        let keys: Vec<&str> = with.results.iter().map(|h| h.title.as_str()).collect();
        assert_eq!(with.results.len(), 2, "{keys:?}");
        assert!(
            keys.iter().any(|t| t.contains("quokka-habitat")),
            "{keys:?}"
        );
        assert!(
            keys.iter().any(|t| t.contains("wombat-habitat")),
            "{keys:?}"
        );
        assert_eq!(
            with.query, main,
            "the response still reports the main query"
        );
        // Without the re-ranker the merged set is ordered by blended score.
        assert!(with.results[0].score >= with.results[1].score);
    }

    #[test]
    fn row_found_by_several_phrasings_appears_once() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let (fact_id, _) = fact::add(
            temp.path(),
            "dedupe-probe",
            "variantdedupeprobe shared value",
            "user",
            "cli:user",
        )
        .expect("fact");
        age_fact(temp.path(), fact_id, 200);

        let kept = recall(
            temp.path(),
            variant_opts(
                "variantdedupeprobe",
                &[
                    "variantdedupeprobe shared",
                    "shared value variantdedupeprobe",
                ],
                RetrievalMode::Fts,
            ),
        )
        .expect("recall");
        assert_eq!(kept.results.len(), 1);
        assert_eq!(kept.candidate_count, 1);
        assert!(kept.results[0].stale);
        assert_eq!(kept.results[0].source_id, fact_id);
    }

    #[test]
    fn rerank_score_is_against_the_main_query_whatever_the_variants() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        edit_config(temp.path(), |cfg| {
            cfg.retrieval.mode = RetrievalMode::Hybrid;
        });
        seed(temp.path()); // after the mode switch so the rows get embeddings
        let main = "build and lint";
        let response = recall(
            temp.path(),
            RecallOptions {
                rerank_cutoff: Some(-100.0),
                variants: vec![
                    "Ship recall surface rolls out".to_string(),
                    "agents propose facts, durable rows need human review".to_string(),
                ],
                ..explain_opts(main, RetrievalMode::Hybrid, 10, vec![])
            },
        )
        .expect("recall");
        assert!(response.explain.reranker_ran);
        // Every row is in the pool, so scoring all of them against the main
        // query in one batch reproduces the scores recall must have used.
        let docs: Vec<String> = response
            .results
            .iter()
            .map(|h| {
                format!(
                    "{}

{}",
                    h.title, h.body
                )
            })
            .collect();
        assert!(docs.len() > 1);
        let mut expected = vec![0.0_f32; docs.len()];
        for (idx, score) in rerank::rerank(main, &docs).expect("rerank") {
            expected[idx] = score;
        }
        for (hit, want) in response.results.iter().zip(expected) {
            let got = hit.rerank_score.expect("reranked hit carries its score");
            assert!(
                (got - want).abs() < 1e-3,
                "{}: rerank_score {got} is not the main-query score {want}",
                hit.title
            );
        }
    }

    #[test]
    fn row_found_by_several_phrasings_appears_once_in_hybrid_mode() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let (fact_id, _) = fact::add(
            temp.path(),
            "hybrid-dedupe-probe",
            "hybriddedupeprobe shared value",
            "user",
            "cli:user",
        )
        .expect("fact");

        let response = recall(
            temp.path(),
            RecallOptions {
                rerank_cutoff: Some(-100.0),
                ..variant_opts(
                    "hybriddedupeprobe",
                    &["hybriddedupeprobe shared", "shared value hybriddedupeprobe"],
                    RetrievalMode::Hybrid,
                )
            },
        )
        .expect("recall");
        assert!(response.explain.reranker_ran);
        let ids: Vec<i64> = response.results.iter().map(|h| h.source_id).collect();
        assert_eq!(ids, vec![fact_id]);
        assert_eq!(response.candidate_count, 1);
    }

    #[test]
    fn equal_score_hits_without_variants_order_by_source_id() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        // Same body, same-length keys: identical blended scores.
        let ids: Vec<i64> = ["tie-a", "tie-b", "tie-c"]
            .iter()
            .map(|key| {
                fact::add(
                    temp.path(),
                    key,
                    "tiebreakprobe shared value",
                    "user",
                    "cli:user",
                )
                .expect("fact")
                .0
            })
            .collect();

        let response = recall(
            temp.path(),
            explain_opts(
                "tiebreakprobe",
                RetrievalMode::Fts,
                10,
                vec![SourceType::Fact],
            ),
        )
        .expect("recall");
        assert_eq!(response.results.len(), 3);
        assert!(
            response
                .results
                .windows(2)
                .all(|w| w[0].score == w[1].score),
            "the probe needs equal scores: {:?}",
            response.results.iter().map(|h| h.score).collect::<Vec<_>>()
        );
        let got: Vec<i64> = response.results.iter().map(|h| h.source_id).collect();
        assert_eq!(got, ids);
    }

    #[test]
    fn variants_keep_the_rerank_pool_cut_counting_distinct_rows() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        edit_config(temp.path(), |cfg| {
            cfg.retrieval.mode = RetrievalMode::Hybrid;
            cfg.retrieval.rerank_candidate_pool = 2;
        });
        seed(temp.path());
        let response = recall(
            temp.path(),
            RecallOptions {
                rerank_cutoff: Some(-100.0),
                variants: vec!["Ship recall surface".to_string()],
                ..explain_opts("build and lint", RetrievalMode::Hybrid, 1, vec![])
            },
        )
        .expect("recall");
        assert!(response.explain.reranker_ran);
        // Nothing falls under the cutoff, so every candidate is either cut from
        // the pool, cut by the result limit, or the one returned row.
        assert_eq!(response.explain.dropped_by_cutoff, 0);
        assert_eq!(
            response.explain.rerank_pool_cut + response.explain.result_limit_cut + 1,
            response.candidate_count
        );
        // The union of two 2-row pools reranks at least 2 rows.
        assert!(response.explain.result_limit_cut >= 1);
        assert!(response.explain.rerank_pool_cut <= response.candidate_count - 2);
    }

    // Issue #337: the re-ranker's pool is filled from the vector and FTS
    // rankings in turn, with default-included doc chunks capped.
    fn pool_opts(pool: usize, doc_cap: usize, docs_via_default: bool) -> ResolvedOptions {
        let mut opts = ResolvedOptions::from(
            &explain_opts("q", RetrievalMode::Hybrid, 1, vec![]),
            &crate::config::RetrievalConfig::default(),
        )
        .expect("resolve");
        opts.use_reranker = true;
        opts.rerank_candidate_pool = pool;
        opts.rerank_doc_cap = doc_cap;
        opts.docs_via_default = docs_via_default;
        opts
    }

    /// A hit whose blended score is `0.4 * fts + 0.6 * vector`; a missing
    /// component means the retriever did not find the row.
    fn pool_hit(st: SourceType, id: i64, fts: Option<f64>, vector: Option<f64>) -> ScoredHit {
        ScoredHit {
            source_type: st,
            scope: SCOPE_REPO.to_string(),
            source_id: id,
            title: String::new(),
            body: String::new(),
            summary: None,
            score: 0.4 * fts.unwrap_or(0.0) + 0.6 * vector.unwrap_or(0.0),
            fts_rank: fts,
            vector_rank: vector,
            fts_score: fts.unwrap_or(0.0),
            vector_score: vector.unwrap_or(0.0),
            stale: false,
            superseded_by: None,
            source: String::new(),
            created_at: String::new(),
            rerank_score: None,
            status: None,
            kind: None,
        }
    }

    fn fact_hit(id: i64, fts: Option<f64>, vector: Option<f64>) -> ScoredHit {
        pool_hit(SourceType::Fact, id, fts, vector)
    }

    fn doc_hit(id: i64, fts: Option<f64>, vector: Option<f64>) -> ScoredHit {
        pool_hit(SourceType::DocChunk, id, fts, vector)
    }

    fn by_blended_score(mut hits: Vec<ScoredHit>) -> Vec<ScoredHit> {
        hits.sort_by(cmp_merged);
        hits
    }

    fn ids(hits: &[ScoredHit]) -> Vec<i64> {
        let mut ids: Vec<i64> = hits.iter().map(|h| h.source_id).collect();
        ids.sort_unstable();
        ids
    }

    #[test]
    fn pool_takes_a_top_vector_row_outside_the_blended_top_pool() {
        // Rows 1-5 match on both retrievers and fill the blended top 4; row
        // 9 has the best cosine but no word overlap, so it blends low.
        let mut hits: Vec<ScoredHit> = (1..=5)
            .map(|i| fact_hit(i, Some(1.0), Some(0.9 - 0.01 * i as f64)))
            .collect();
        hits.push(fact_hit(9, None, Some(0.95)));
        let hits = by_blended_score(hits);
        let opts = pool_opts(4, 5, false);
        let blended_top: Vec<i64> = hits.iter().take(4).map(|h| h.source_id).collect();
        assert!(
            !blended_top.contains(&9),
            "row 9 is outside the blended top 4"
        );

        let pool = select_rerank_pool(&opts, hits);
        assert_eq!(pool.len(), 4);
        assert!(ids(&pool).contains(&9), "pool: {:?}", ids(&pool));
    }

    #[test]
    fn pool_fills_from_the_other_ranking_when_one_runs_out() {
        let mut hits: Vec<ScoredHit> = (1..=6)
            .map(|i| fact_hit(i, Some(0.9 - 0.01 * i as f64), None))
            .collect();
        hits.extend((7..=8).map(|i| fact_hit(i, None, Some(0.5))));
        let pool = select_rerank_pool(&pool_opts(5, 5, false), by_blended_score(hits.clone()));
        assert_eq!(pool.len(), 5, "pool: {:?}", ids(&pool));
        let pool = select_rerank_pool(&pool_opts(20, 5, false), by_blended_score(hits));
        assert_eq!(pool.len(), 8, "every eligible row when the pool is larger");
    }

    #[test]
    fn pool_ranks_apply_the_superseded_and_age_decay_adjustments() {
        let scoring = RetrievalScoringConfig {
            age_half_life_days: 30,
            ..RetrievalScoringConfig::default()
        };
        // The penalized row (id 1) has the stronger raw signal, so only its
        // adjustment can put the current row (id 2) ahead in the ranking.
        // Rows come from one retriever at a time so the other ranking cannot
        // mask a missing adjustment; pool 1 takes the top of the ranking
        // that exists. The FTS case adds a weak third row so the min-max
        // normalization leaves the first two close together.
        let row = |id: i64, vector: bool, signal: f64| {
            let mut c = CandidateRow::empty(SourceType::Fact, id);
            if vector {
                c.vector_score = Some(signal);
            } else {
                c.fts_raw = Some(-signal); // BM25: lower raw is better
            }
            c.age_days = Some(0.0);
            c
        };
        type Adjust = fn(&mut CandidateRow);
        let cases: [(&str, Adjust); 2] = [
            ("superseded", |c| c.superseded_by = Some(9)),
            ("aged", |c| c.age_days = Some(400.0)),
        ];
        for (ranking, vector) in [("vector", true), ("FTS", false)] {
            for (label, adjust) in cases {
                let mut penalized = row(1, vector, if vector { 0.85 } else { 1.1 });
                adjust(&mut penalized);
                let mut rows = vec![penalized, row(2, vector, if vector { 0.8 } else { 1.0 })];
                if !vector {
                    rows.push(row(3, vector, 0.5));
                }
                let mut hits = score(&rows, &scoring);
                hits.sort_by(cmp_merged);
                let picked = select_rerank_pool(&pool_opts(1, 5, false), hits);
                assert_eq!(
                    ids(&picked),
                    vec![2],
                    "{ranking} ranking: the {label} row must not take the slot ahead of the current row"
                );
            }
        }
    }

    #[test]
    fn pool_ranks_ignore_staleness() {
        let scoring = RetrievalScoringConfig::default();
        for vector in [true, false] {
            let row = |id: i64, signal: f64, stale: bool| {
                let mut c = CandidateRow::empty(SourceType::Fact, id);
                if vector {
                    c.vector_score = Some(signal);
                } else {
                    c.fts_raw = Some(-signal); // BM25: lower raw is better
                }
                c.is_stale = stale;
                c
            };
            let rows = vec![row(1, 0.9, true), row(2, 0.8, false), row(3, 0.2, false)];
            let mut hits = score(&rows, &scoring);
            hits.sort_by(cmp_merged);
            let picked = select_rerank_pool(&pool_opts(1, 5, false), hits);
            assert_eq!(
                ids(&picked),
                vec![1],
                "the stale row with the strongest signal keeps the slot (vector={vector})"
            );
        }
    }

    #[test]
    fn doc_cap_limits_default_docs_but_not_explicit_source_types() {
        let mut hits: Vec<ScoredHit> = (1..=5)
            .map(|i| doc_hit(i, Some(0.9), Some(0.9 - 0.01 * i as f64)))
            .collect();
        hits.extend((11..=14).map(|i| fact_hit(i, Some(0.5), Some(0.5 - 0.01 * i as f64))));
        let hits = by_blended_score(hits);

        let docs_in = |pool: &[ScoredHit]| {
            pool.iter()
                .filter(|h| h.source_type == SourceType::DocChunk)
                .count()
        };
        let capped = select_rerank_pool(&pool_opts(6, 2, true), hits.clone());
        assert_eq!(capped.len(), 6);
        assert_eq!(docs_in(&capped), 2);
        let uncapped = select_rerank_pool(&pool_opts(6, 2, false), hits);
        assert_eq!(uncapped.len(), 6);
        assert_eq!(
            docs_in(&uncapped),
            5,
            "explicit source types are not capped"
        );
    }

    #[test]
    fn rerank_pool_cut_counts_doc_chunks_over_the_cap() {
        let opts = pool_opts(10, 1, true);
        let hits = by_blended_score(vec![
            doc_hit(1, Some(0.9), Some(0.9)),
            doc_hit(2, Some(0.8), Some(0.8)),
            doc_hit(3, Some(0.7), Some(0.7)),
            fact_hit(4, Some(0.5), Some(0.5)),
        ]);
        let merged = merge_phrasings(&opts, vec![hits]);
        assert_eq!(merged.hits.len(), 2);
        assert_eq!(merged.rerank_pool_cut, 2);
    }

    #[test]
    fn variants_fill_each_phrasings_pool_before_the_union() {
        let opts = pool_opts(2, 5, false);
        // Main query: vector picks C, FTS picks A, so B is left out.
        let main = by_blended_score(vec![
            fact_hit(1, Some(0.9), Some(0.1)),
            fact_hit(2, Some(0.5), Some(0.5)),
            fact_hit(3, Some(0.1), Some(0.9)),
        ]);
        // Variant: vector picks D, FTS picks B; E is left out.
        let variant = by_blended_score(vec![
            fact_hit(2, Some(0.9), Some(0.1)),
            fact_hit(4, Some(0.1), Some(0.9)),
            fact_hit(5, Some(0.05), Some(0.05)),
        ]);
        let merged = merge_phrasings(&opts, vec![main, variant]);
        assert_eq!(ids(&merged.hits), vec![1, 2, 3, 4]);
        assert_eq!(merged.rerank_pool_cut, 1, "only row 5 reached no pool");
    }

    #[test]
    fn blank_or_too_many_variants_are_rejected_with_a_reason() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        for blank in ["", "   "] {
            let err = recall(
                temp.path(),
                variant_opts("anything", &["ok phrasing", blank], RetrievalMode::Fts),
            )
            .expect_err("blank variant must be rejected");
            assert!(matches!(err, MemhubError::InvalidInput(_)), "{err}");
            assert!(err.to_string().contains("variant 2 is blank"), "{err}");
        }
        let err = recall(
            temp.path(),
            variant_opts("anything", &["a", "b", "c", "d", "e"], RetrievalMode::Fts),
        )
        .expect_err("5 variants must be rejected");
        assert!(matches!(err, MemhubError::InvalidInput(_)), "{err}");
        assert!(err.to_string().contains("at most 4"), "{err}");

        let four: Vec<String> = ["a", "b", "c", "d"].map(String::from).to_vec();
        assert_eq!(validate_variants(&four).expect("4 are fine"), four);
    }

    // Q9 / issue #98: session notes are write-only scratch by default —
    // never in the plain recall bundle, mirroring the pre-decision-90 docs
    // default-exclusion pattern (migration 0014) but with NO opt-in flip
    // like docs got in decision 90: notes have no
    // `include_notes_in_default` equivalent and must never surface here,
    // no matter how many are logged. Modeled on
    // `docs_are_opt_in_and_signal_availability` above.
    #[test]
    fn session_notes_are_excluded_from_default_recall() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        // Same distinctive keyword in both rows: proves the default bundle
        // still surfaces the fact (the query mechanism genuinely matches)
        // while the note carrying the identical keyword stays out — not a
        // vacuous pass from an empty result set.
        fact::add(
            temp.path(),
            "flake-triage-command",
            "recallnoteprobe: re-run failing suite with RUST_LOG=debug",
            "user",
            "cli:user",
        )
        .expect("fact");
        session_note::add(
            temp.path(),
            "recallnoteprobe: root cause was a stale embedding, not a real flake.",
            "claude-code",
            "claude-code:log_session_note",
        )
        .expect("session note");

        let response = recall(
            temp.path(),
            RecallOptions {
                query: "recallnoteprobe".to_string(),
                mode: Some(RetrievalMode::Fts),
                max_results: 10,
                source_types: vec![],
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                rerank_cutoff: None,
                log_metrics: false,
                variants: Vec::new(),
                surface: None,
                explain: false,
            },
        )
        .expect("default recall");

        assert!(
            response.results.iter().any(|h| h.source_type == "fact"),
            "sanity: the seeded fact must still surface in the default bundle"
        );
        assert!(
            response.results.iter().all(|h| h.source_type != "note"),
            "session notes must never appear in the default recall bundle, \
             even when they match the query"
        );
    }

    #[test]
    fn session_notes_surface_only_on_explicit_source_type_request() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let note = session_note::add(
            temp.path(),
            "recallnoteprobe: root cause was a stale embedding, not a real flake.",
            "claude-code",
            "claude-code:log_session_note",
        )
        .expect("session note");

        let scoped = recall(
            temp.path(),
            RecallOptions {
                query: "recallnoteprobe".to_string(),
                mode: Some(RetrievalMode::Fts),
                max_results: 10,
                source_types: vec![SourceType::Note],
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                rerank_cutoff: None,
                log_metrics: false,
                variants: Vec::new(),
                surface: None,
                explain: false,
            },
        )
        .expect("note-scoped recall");

        let hit = scoped
            .results
            .iter()
            .find(|h| h.source_type == "note" && h.source_id == note.id)
            .expect("note-scoped recall must return the logged note");
        assert_eq!(hit.title, "claude-code");
        assert!(hit.body.contains("recallnoteprobe"));
    }

    // The two tests above run in fts mode, where exclusion is structural
    // and trivial: `fts_lookup` is only ever called for types already in
    // `opts.source_types`, so a note is never even queried. The load-
    // bearing guard is different in HYBRID mode: `vector_lookup` scans
    // every row in `embeddings` with no source_type filter in the SQL
    // itself (`SELECT source_type, source_id, vector FROM embeddings
    // WHERE model_name = ?1 AND dimension = ?2`) and relies entirely on
    // the Rust-side `if !allowed.contains(&st.as_str()) { continue; }`
    // check to keep a note's vector out of a default-scoped call. A note
    // only has a vector at all once eager-embedded (hybrid mode), so this
    // is the one path a fts-mode test structurally cannot exercise. Pins
    // the invariant against a future `vector_lookup` refactor.
    #[test]
    fn session_notes_are_excluded_from_default_recall_in_hybrid_mode() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let cfg_path = temp.path().join(".memhub/config.toml");
        let mut cfg = ProjectConfig::load(&cfg_path).expect("load cfg");
        cfg.retrieval.mode = RetrievalMode::Hybrid;
        cfg.save(&cfg_path).expect("save cfg");

        fact::add(
            temp.path(),
            "flake-triage-command",
            "recallnoteprobe: re-run failing suite with RUST_LOG=debug",
            "user",
            "cli:user",
        )
        .expect("fact");
        // Hybrid mode is already on, so this note is eager-embedded on
        // write — it has a real row in `embeddings`, which is exactly
        // what's needed to exercise `vector_lookup`'s allowlist check.
        session_note::add(
            temp.path(),
            "recallnoteprobe: root cause was a stale embedding, not a real flake.",
            "claude-code",
            "claude-code:log_session_note",
        )
        .expect("session note");

        let response = recall(
            temp.path(),
            RecallOptions {
                query: "recallnoteprobe".to_string(),
                mode: Some(RetrievalMode::Hybrid),
                max_results: 10,
                source_types: vec![],
                include_stale: None,
                accepted_only: None,
                // Disabled so this test isolates vector_lookup's allowlist
                // check specifically — the cross-encoder's rerank cutoff is
                // a separate mechanism that must not be the only thing
                // standing between a note and a false-negative "pass".
                use_reranker: Some(false),
                rerank_cutoff: None,
                log_metrics: false,
                variants: Vec::new(),
                surface: None,
                explain: false,
            },
        )
        .expect("default hybrid recall");

        assert!(
            response.results.iter().any(|h| h.source_type == "fact"),
            "sanity: the seeded fact must still surface in the default bundle"
        );
        assert!(
            response.results.iter().all(|h| h.source_type != "note"),
            "session notes must never leak into hybrid default recall via the vector path"
        );
    }

    // Issue #232: heading sections of the latest architecture body are a
    // default-recall source type (`arch_section`, CLI/MCP spelling `arch`).
    const ARCH_BODY: &str = "# Overview\n\nmemhub is a local-first project memory CLI.\n\n\
        ## Storage\n\nThe zebrafinch ledger keeps every durable row in one SQLite file per repo.\n\n\
        ## Sync\n\nSnapshots travel between machines through a shared Drive folder.\n";

    fn arch_recall(
        start: &std::path::Path,
        query: &str,
        mode: RetrievalMode,
        source_types: Vec<SourceType>,
        rerank_cutoff: Option<f32>,
    ) -> RecallResponse {
        recall(
            start,
            RecallOptions {
                query: query.to_string(),
                mode: Some(mode),
                max_results: 10,
                source_types,
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                rerank_cutoff,
                log_metrics: false,
                variants: Vec::new(),
                surface: None,
                explain: false,
            },
        )
        .expect("recall")
    }

    fn set_arch(start: &std::path::Path, body: &str) {
        narrative::set(start, NarrativeKind::Arch, body, "user", "cli:user").expect("arch set");
    }

    #[test]
    fn arch_sections_join_default_recall_with_heading_path_title() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        set_arch(temp.path(), ARCH_BODY);

        let response = arch_recall(
            temp.path(),
            "zebrafinch ledger",
            RetrievalMode::Fts,
            vec![],
            None,
        );
        let hit = response
            .results
            .iter()
            .find(|h| h.source_type == "arch_section")
            .expect("default recall must return the matching architecture section");
        assert_eq!(hit.title, "Architecture — Storage");
        assert!(hit.body.contains("zebrafinch"));
        assert!(
            !hit.body.contains("Drive folder"),
            "a hit is one section, not the whole body"
        );
    }

    #[test]
    fn arch_sections_join_default_recall_in_hybrid_mode_and_fall_back() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let cfg_path = temp.path().join(".memhub/config.toml");
        let mut cfg = ProjectConfig::load(&cfg_path).expect("load cfg");
        cfg.retrieval.mode = RetrievalMode::Hybrid;
        cfg.save(&cfg_path).expect("save cfg");
        // Hybrid is on, so `arch set` eager-embeds every section.
        set_arch(temp.path(), ARCH_BODY);

        // Default cutoff: a question-shaped query must clear it.
        let response = arch_recall(
            temp.path(),
            "Where does the zebrafinch ledger keep every durable row?",
            RetrievalMode::Hybrid,
            vec![],
            None,
        );
        assert!(
            response
                .warnings
                .iter()
                .all(|w| w.kind != "stale_embeddings"),
            "arch set must eager-embed sections in hybrid mode: {:?}",
            response.warnings
        );
        let hit = response
            .results
            .iter()
            .find(|h| h.source_type == "arch_section" && h.title.ends_with("Storage"))
            .expect("hybrid default recall must return the Storage section");
        assert!(hit.rerank_score.is_some());

        // An unreachable cutoff drops arch sections like every other row.
        let dropped = arch_recall(
            temp.path(),
            "Where does the zebrafinch ledger keep every durable row?",
            RetrievalMode::Hybrid,
            vec![],
            Some(100.0),
        );
        assert!(dropped.results.is_empty(), "{:?}", dropped.results);
        assert!(dropped.explain.dropped_by_cutoff > 0);
    }

    #[test]
    fn later_arch_set_retires_earlier_sections_from_recall() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        set_arch(temp.path(), ARCH_BODY);
        set_arch(
            temp.path(),
            "# Overview\n\n## Storage\n\nRows now live in the pelicanvault store.\n",
        );

        for source_types in [vec![], vec![SourceType::ArchSection]] {
            let old = arch_recall(
                temp.path(),
                "zebrafinch",
                RetrievalMode::Fts,
                source_types,
                None,
            );
            assert!(
                old.results.iter().all(|h| h.source_type != "arch_section"),
                "sections of an earlier architecture body must never be returned"
            );
        }
        let new = arch_recall(
            temp.path(),
            "pelicanvault",
            RetrievalMode::Fts,
            vec![],
            None,
        );
        assert!(new.results.iter().any(|h| h.source_type == "arch_section"));

        let ctx = crate::db::open_project(temp.path()).expect("open");
        let (owners, latest): (i64, i64) = ctx
            .conn
            .query_row(
                "SELECT (SELECT COUNT(DISTINCT arch_id) FROM arch_sections),
                        (SELECT MAX(id) FROM project_arch)",
                [],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("owners");
        let owner: i64 = ctx
            .conn
            .query_row("SELECT arch_id FROM arch_sections LIMIT 1", [], |r| {
                r.get(0)
            })
            .expect("owner");
        assert_eq!((owners, owner), (1, latest));
    }

    #[test]
    fn arch_scoped_recall_returns_only_arch_sections_and_notes_stay_out() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        fact::add(
            temp.path(),
            "storage-engine",
            "the zebrafinch ledger is SQLite",
            "user",
            "cli:user",
        )
        .expect("fact");
        session_note::add(
            temp.path(),
            "zebrafinch ledger scratch note",
            "claude-code",
            "claude-code:log_session_note",
        )
        .expect("note");
        set_arch(temp.path(), ARCH_BODY);

        let default = arch_recall(temp.path(), "zebrafinch", RetrievalMode::Fts, vec![], None);
        let types: Vec<&str> = default
            .results
            .iter()
            .map(|h| h.source_type.as_str())
            .collect();
        assert!(types.contains(&"fact") && types.contains(&"arch_section"));
        assert!(!types.contains(&"note"), "notes never join default recall");

        let scoped = arch_recall(
            temp.path(),
            "zebrafinch",
            RetrievalMode::Fts,
            vec![SourceType::ArchSection],
            None,
        );
        assert!(!scoped.results.is_empty());
        assert!(
            scoped
                .results
                .iter()
                .all(|h| h.source_type == "arch_section")
        );
    }

    /// Criterion 4: a DB whose latest architecture body predates migration
    /// 0025 (simulated by dropping its ledger row) gets sections on the
    /// next ordinary open, with no new `arch set`.
    #[test]
    fn arch_body_stored_before_migration_is_recallable_without_new_set() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        {
            let ctx = crate::db::open_project(temp.path()).expect("open");
            ctx.conn
                .execute(
                    "INSERT INTO project_arch(project_id, body, actor, actor_raw)
                     VALUES (1, ?1, 'user', 'cli:user')",
                    params![ARCH_BODY],
                )
                .expect("legacy arch body");
            ctx.conn
                .execute(
                    "DELETE FROM schema_migrations WHERE version = '0025_arch_sections'",
                    [],
                )
                .expect("forget 0025");
        }

        let response = arch_recall(
            temp.path(),
            "zebrafinch ledger",
            RetrievalMode::Fts,
            vec![],
            None,
        );
        assert!(
            response
                .results
                .iter()
                .any(|h| h.source_type == "arch_section" && h.title.ends_with("Storage")),
            "a pre-existing architecture body must be recallable after upgrade"
        );
    }

    /// Two ingested docs (a code style guide and a UI style guide).
    /// A code-flavored query must surface the code doc and the rerank
    /// cutoff must keep the off-topic UI doc out of the default bundle;
    /// the inverse query flips it. This is the user's own
    /// "depends on the task at hand" example: off-topic chunks score far
    /// below `DEFAULT_RERANK_CUTOFF`, so if the constant is mis-tuned one of
    /// these assertions fails with the offending rerank scores.
    #[test]
    fn doc_default_recall_cutoff_routes_by_task_relevance() {
        let temp = tempdir().expect("tempdir");
        seed(temp.path());

        // Doc chunks embed only in hybrid mode; the cross-encoder must
        // run for the cutoff to apply at all.
        let cfg_path = temp.path().join(".memhub/config.toml");
        let mut cfg = ProjectConfig::load(&cfg_path).expect("load cfg");
        cfg.retrieval.mode = RetrievalMode::Hybrid;
        cfg.save(&cfg_path).expect("save cfg");

        let code_doc = temp.path().join("code-style-guide.md");
        std::fs::write(
            &code_doc,
            "# Rust Code Style Guide\n\n\
             ## Error Handling\n\n\
             New fallible functions return `crate::Result<T>`. Never call \
             `unwrap()` outside tests. Convert an IO failure with `map_err` \
             into `MemhubError::InvalidInput` and propagate it upward with \
             the `?` operator so the caller decides how to recover.\n\n\
             ## Naming\n\n\
             Functions are snake_case verbs; modules are nouns. Avoid \
             abbreviations in any public API signature.\n",
        )
        .expect("write code doc");
        doc::add(temp.path(), &code_doc, None, "cli:user").expect("ingest code doc");

        let ui_doc = temp.path().join("ui-style-guide.md");
        std::fs::write(
            &ui_doc,
            "# UI Style Guide\n\n\
             ## Color Palette\n\n\
             The primary accent is teal #1F8A8A painted on a near-black \
             #0B0B0C canvas. Never use pure black for the background.\n\n\
             ## Spacing And Typography\n\n\
             Lay everything out on an 8px base grid. Buttons take 12px \
             vertical and 20px horizontal padding. Headings use a humanist \
             sans on a 1.25 modular scale; body copy is 16px at 1.5 line \
             height.\n",
        )
        .expect("write ui doc");
        doc::add(temp.path(), &ui_doc, None, "cli:user").expect("ingest ui doc");

        let is_ui_chunk = |b: &str| {
            b.contains("#1F8A8A") || b.contains("8px base grid") || b.contains("modular scale")
        };
        let is_code_chunk =
            |b: &str| b.contains("map_err") || b.contains("snake_case") || b.contains("Result<T>");

        // Code-flavored query: code doc in, UI doc out.
        let code_q = recall(
            temp.path(),
            RecallOptions {
                query: "convention for returning and propagating errors from a new function"
                    .to_string(),
                mode: Some(RetrievalMode::Hybrid),
                max_results: 6,
                source_types: vec![],
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                rerank_cutoff: None,
                log_metrics: false,
                variants: Vec::new(),
                surface: None,
                explain: false,
            },
        )
        .expect("code-flavored recall");
        let doc_hits = |r: &RecallResponse| -> Vec<(String, Option<f32>)> {
            r.results
                .iter()
                .filter(|h| h.source_type == "doc_chunk")
                .map(|h| (h.body.clone(), h.rerank_score))
                .collect()
        };
        assert!(
            code_q
                .results
                .iter()
                .any(|h| h.source_type == "doc_chunk" && is_code_chunk(&h.body)),
            "code query must surface the code style guide; doc hits = {:?}",
            doc_hits(&code_q),
        );
        assert!(
            !code_q
                .results
                .iter()
                .any(|h| h.source_type == "doc_chunk" && is_ui_chunk(&h.body)),
            "off-topic UI doc must be filtered by the cutoff on a code query; \
             doc hits = {:?}",
            doc_hits(&code_q),
        );

        // Inverse: a UI-flavored query surfaces the UI doc, not the code one.
        let ui_q = recall(
            temp.path(),
            RecallOptions {
                query: "primary accent color and button padding for the interface".to_string(),
                mode: Some(RetrievalMode::Hybrid),
                max_results: 6,
                source_types: vec![],
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                rerank_cutoff: None,
                log_metrics: false,
                variants: Vec::new(),
                surface: None,
                explain: false,
            },
        )
        .expect("ui-flavored recall");
        assert!(
            ui_q.results
                .iter()
                .any(|h| h.source_type == "doc_chunk" && is_ui_chunk(&h.body)),
            "ui query must surface the UI style guide; doc hits = {:?}",
            doc_hits(&ui_q),
        );
        assert!(
            !ui_q
                .results
                .iter()
                .any(|h| h.source_type == "doc_chunk" && is_code_chunk(&h.body)),
            "off-topic code doc must be filtered on a UI query; doc hits = {:?}",
            doc_hits(&ui_q),
        );

        // Flag off: even in hybrid + reranked, no doc may surface.
        let mut off = ProjectConfig::load(&cfg_path).expect("reload cfg");
        off.retrieval.include_docs_in_default = false;
        off.save(&cfg_path).expect("save cfg off");
        let gated = recall(
            temp.path(),
            RecallOptions {
                query: "convention for returning and propagating errors from a new function"
                    .to_string(),
                mode: Some(RetrievalMode::Hybrid),
                max_results: 6,
                source_types: vec![],
                include_stale: None,
                accepted_only: None,
                use_reranker: None,
                rerank_cutoff: None,
                log_metrics: false,
                variants: Vec::new(),
                surface: None,
                explain: false,
            },
        )
        .expect("flag-off recall");
        assert!(
            gated.results.iter().all(|h| h.source_type != "doc_chunk"),
            "with include_docs_in_default=false no doc may enter the default \
             bundle; got {:?}",
            doc_hits(&gated),
        );
        assert!(
            gated.available_docs >= 1,
            "flag-off recall still advertises available docs, got {}",
            gated.available_docs,
        );
    }

    /// Only the *first* doc in a repo auto-enables default doc recall.
    /// Once a user sets the flag false, neither a re-add nor a second
    /// new doc may silently re-flip it — that escape hatch is promised
    /// in CLAUDE.md / AGENTS.md, so it gets a regression guard.
    #[test]
    fn first_doc_auto_enables_then_user_opt_out_sticks() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let cfg_path = temp.path().join(".memhub/config.toml");

        let d1 = temp.path().join("one.md");
        std::fs::write(&d1, "# One\n\nFirst doc body.\n").expect("write d1");
        let out1 = doc::add(temp.path(), &d1, None, "cli:user").expect("add d1");
        assert!(
            out1.enabled_default_recall,
            "first doc must auto-enable default recall"
        );
        assert!(
            ProjectConfig::load(&cfg_path)
                .expect("load")
                .retrieval
                .include_docs_in_default,
            "config must persist the auto-enable"
        );

        // User deliberately opts back out.
        let mut off = ProjectConfig::load(&cfg_path).expect("load");
        off.retrieval.include_docs_in_default = false;
        off.save(&cfg_path).expect("save off");

        // A second, brand-new doc must NOT re-flip it.
        let d2 = temp.path().join("two.md");
        std::fs::write(&d2, "# Two\n\nSecond doc body.\n").expect("write d2");
        let out2 = doc::add(temp.path(), &d2, None, "cli:user").expect("add d2");
        assert!(
            !out2.enabled_default_recall,
            "a second new doc must not re-enable after explicit opt-out"
        );
        assert!(
            !ProjectConfig::load(&cfg_path)
                .expect("load")
                .retrieval
                .include_docs_in_default,
            "user opt-out must survive a second doc add"
        );

        // Neither may a re-add of the first doc (unchanged).
        let out1b = doc::add(temp.path(), &d1, None, "cli:user").expect("re-add d1");
        assert!(!out1b.enabled_default_recall);
        assert!(
            !ProjectConfig::load(&cfg_path)
                .expect("load")
                .retrieval
                .include_docs_in_default,
            "user opt-out must survive an unchanged re-add"
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
                rerank_cutoff: None,
                log_metrics: true,
                variants: Vec::new(),
                surface: Some(RecallSurface::Cli),
                explain: false,
            },
        )
        .expect("recall");

        assert!(!response.results.is_empty());
        assert_eq!(count_recall_metrics(temp.path()), 0);
    }

    #[cfg(feature = "metrics")]
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
                rerank_cutoff: None,
                log_metrics: true,
                variants: Vec::new(),
                surface: Some(RecallSurface::Cli),
                explain: false,
            },
        )
        .expect("recall");
        assert!(!response.results.is_empty());

        let ctx = crate::db::open_project(temp.path()).expect("open");
        let mut stmt = ctx
            .conn
            .prepare(
                "SELECT query_hash, bundle_tokens, ledger_tokens, rerank_used, result_count, \
                        surface \
                 FROM recall_metrics",
            )
            .expect("prepare");
        let rows: Vec<(String, i64, i64, i64, i64, Option<String>)> = stmt
            .query_map(params![], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })
            .expect("query")
            .collect::<std::result::Result<Vec<_>, _>>()
            .expect("collect");

        assert_eq!(rows.len(), 1, "exactly one row per recall");
        let (query_hash, bundle_tokens, ledger_tokens, rerank_used, result_count, surface) =
            &rows[0];
        assert_eq!(query_hash.len(), 64);
        assert!(
            *bundle_tokens > 0,
            "bundle_tokens must reflect returned hits"
        );
        assert_eq!(
            *ledger_tokens, 0,
            "no rendered ledger in this tempdir, so ledger_tokens = 0"
        );
        assert_eq!(*rerank_used, 0, "fts mode never invokes the re-ranker");
        assert_eq!(*result_count, response.results.len() as i64);
        assert_eq!(
            surface.as_deref(),
            Some("cli"),
            "RecallOptions::surface must thread through to the persisted row (issue #70)"
        );
    }

    #[cfg(feature = "metrics")]
    #[test]
    fn metrics_log_opt_out_skips_insert_even_when_enabled() {
        // Eval sets log_metrics = false. Pin that
        // behavior so a future refactor can't silently start logging
        // calibration sweeps to recall_metrics.
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
                rerank_cutoff: None,
                log_metrics: false,
                variants: Vec::new(),
                surface: None,
                explain: false,
            },
        )
        .expect("recall");

        assert_eq!(count_recall_metrics(temp.path()), 0);
    }

    // Issue #340: a decision, task or fact named by number is returned first.
    #[test]
    fn named_rows_parses_only_the_word_then_number_forms() {
        use SourceType::{Decision, Fact, Task};
        assert_eq!(
            named_rows("Task #99 then DECISION 201, (fact 7?) and task 99"),
            vec![(Task, 99), (Decision, 201), (Fact, 7)]
        );
        assert_eq!(
            named_rows("decision#12 task	5."),
            vec![(Decision, 12), (Task, 5)]
        );
        for q in [
            "issue 290",
            "PR #328",
            "run 2",
            "D17 and T164",
            "decisions 12",
            "decision12",
            "task 5b",
            "fact 3.5",
            "subtask 5",
            "sub-task 5",
            "decision # 5",
            "decision",
            "decision 99999999999999999999",
            "in fact 3 rows were dropped",
            "the decision 2 days ago",
            "task 4 weeks",
            "decision 3 times",
            "fact 5 items",
            "task 3 of them",
            "Task 3 OF  them.",
            "decision 1 time",
            "task 1 item",
            "fact 2 of-them",
            "task 3 of \"them\"",
            "task 3 of 'them'",
            "decision 2-day",
            "task 3-week",
        ] {
            assert!(named_rows(q).is_empty(), "{q:?} must not name a row");
        }
        // A bare "of", a trailing hyphen and a hyphen before a non-letter still route.
        assert_eq!(
            named_rows("task #5, fact 3? decision 12 of the plan, task 7- done, fact 8-9"),
            vec![(Task, 5), (Fact, 3), (Decision, 12), (Task, 7), (Fact, 8)]
        );
        assert_eq!(
            named_rows("decision 186, task #203, fact 33"),
            vec![(Decision, 186), (Task, 203), (Fact, 33)]
        );
        assert_eq!(
            named_rows("What did decision 186 and decision 189 decide"),
            vec![(Decision, 186), (Decision, 189)]
        );
    }

    fn result_ids(response: &RecallResponse) -> Vec<(String, i64)> {
        response
            .results
            .iter()
            .map(|h| (h.source_type.clone(), h.source_id))
            .collect()
    }

    #[test]
    fn named_decision_task_and_fact_lead_in_query_order_once() {
        let temp = tempdir().expect("tempdir");
        seed(temp.path());

        let response = recall(
            temp.path(),
            explain_opts(
                "zxqv task 1 and cargo build then decision #1 and Fact 2",
                RetrievalMode::Fts,
                5,
                vec![],
            ),
        )
        .expect("recall");

        let got = result_ids(&response);
        assert_eq!(
            got[..4],
            [
                ("task".to_string(), 1),
                ("decision".to_string(), 1),
                ("fact".to_string(), 2),
                ("fact".to_string(), 1),
            ],
            "{got:?}"
        );
        assert_eq!(got.len(), got.iter().collect::<HashSet<_>>().len());
        assert_eq!(response.returned_count, response.results.len());
    }

    #[test]
    fn named_row_alone_is_returned_even_when_nothing_else_matches() {
        let temp = tempdir().expect("tempdir");
        seed(temp.path());

        let response = recall(
            temp.path(),
            explain_opts("zxqv decision 1", RetrievalMode::Fts, 5, vec![]),
        )
        .expect("recall");

        assert_eq!(result_ids(&response), vec![("decision".to_string(), 1)]);
        assert_eq!(response.results[0].rank, 1);
        assert!(response.candidate_count >= 1);
    }

    #[test]
    fn named_rows_count_toward_max_results() {
        let temp = tempdir().expect("tempdir");
        seed(temp.path());

        let two = recall(
            temp.path(),
            explain_opts("task 1 decision 1 cargo", RetrievalMode::Fts, 2, vec![]),
        )
        .expect("recall");
        assert_eq!(
            result_ids(&two),
            vec![("task".to_string(), 1), ("decision".to_string(), 1)]
        );

        let one = recall(
            temp.path(),
            explain_opts("task 1 decision 1 cargo", RetrievalMode::Fts, 1, vec![]),
        )
        .expect("recall");
        assert_eq!(result_ids(&one), vec![("task".to_string(), 1)]);

        let three = recall(
            temp.path(),
            explain_opts("task 1 cargo", RetrievalMode::Fts, 2, vec![]),
        )
        .expect("recall");
        assert_eq!(three.results.len(), 2);
        assert_eq!(three.results[0].source_type, "task");
        assert_eq!(three.results[1].source_type, "fact");
    }

    #[test]
    fn named_row_outside_filters_or_missing_is_not_routed() {
        let temp = tempdir().expect("tempdir");
        seed(temp.path());

        // Source-type filter excludes decisions.
        let scoped = recall(
            temp.path(),
            explain_opts(
                "decision 1 cargo build",
                RetrievalMode::Fts,
                5,
                vec![SourceType::Fact],
            ),
        )
        .expect("recall");
        assert!(scoped.results.iter().all(|h| h.source_type == "fact"));

        // Accepted-only excludes the task (tasks carry no source).
        let mut opts = explain_opts("zxqv task 1", RetrievalMode::Fts, 5, vec![]);
        opts.accepted_only = Some(true);
        let accepted = recall(temp.path(), opts).expect("recall");
        assert!(accepted.results.is_empty(), "{:?}", result_ids(&accepted));

        // No such row: nothing is invented and the ranked path is unchanged.
        let missing = recall(
            temp.path(),
            explain_opts(
                "decision 99 task 77 cargo build",
                RetrievalMode::Fts,
                5,
                vec![],
            ),
        )
        .expect("recall");
        let plain = recall(
            temp.path(),
            explain_opts(
                "xdecision 99 xtask 77 cargo build",
                RetrievalMode::Fts,
                5,
                vec![],
            ),
        )
        .expect("recall");
        assert_eq!(result_ids(&missing), result_ids(&plain));
    }

    #[test]
    fn non_triggering_forms_do_not_route() {
        let temp = tempdir().expect("tempdir");
        seed(temp.path());

        for q in ["issue 1 zxqv", "PR #1 zxqv", "run 1 zxqv", "D1 T1 zxqv"] {
            let response = recall(temp.path(), explain_opts(q, RetrievalMode::Fts, 5, vec![]))
                .expect("recall");
            assert!(
                response.results.is_empty(),
                "{q:?}: {:?}",
                result_ids(&response)
            );
        }
    }

    // Issue #348: variants must not displace the named rows in fts mode, nor
    // in hybrid mode without the re-ranker.
    #[test]
    fn named_row_leads_with_variants_in_fts_and_in_hybrid_without_the_reranker() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        edit_config(temp.path(), |cfg| {
            cfg.retrieval.mode = RetrievalMode::Hybrid
        });
        seed(temp.path());

        for mode in [RetrievalMode::Fts, RetrievalMode::Hybrid] {
            let mut opts = explain_opts("zxqv decision 1", mode, 5, vec![]);
            opts.variants = vec!["cargo build command".to_string()];
            opts.use_reranker = Some(false);
            let response = recall(temp.path(), opts).expect("recall");

            assert!(!response.explain.reranker_ran);
            let got = result_ids(&response);
            assert_eq!(got[0], ("decision".to_string(), 1), "{mode:?}: {got:?}");
            assert!(got.len() > 1, "{mode:?}: the variant still finds rows");
            assert_eq!(got.len(), got.iter().collect::<HashSet<_>>().len());
        }
    }

    // Issue #348: a row only the routing loaded is counted like a found one.
    #[test]
    fn named_row_loaded_only_by_the_routing_is_counted_like_a_found_row() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let (stale_id, _) = fact::add(
            temp.path(),
            "alpha",
            "unrelated aging text",
            "user",
            "cli:user",
        )
        .expect("stale fact");
        age_fact(temp.path(), stale_id, 200);
        let (old_id, _) = fact::add(
            temp.path(),
            "bravo",
            "unrelated old text",
            "user",
            "cli:user",
        )
        .expect("old fact");
        let (new_id, _) = fact::add(
            temp.path(),
            "charlie",
            "unrelated new text",
            "user",
            "cli:user",
        )
        .expect("new fact");
        fact::supersede(
            temp.path(),
            &old_id.to_string(),
            &new_id.to_string(),
            "cli:user",
        )
        .expect("supersede");
        let (agent_id, _) = fact::add(
            temp.path(),
            "delta",
            "unrelated agent text",
            "agent:codex",
            "cli:user",
        )
        .expect("agent fact");
        let ask = |id: i64| explain_opts(&format!("zxqv fact {id}"), RetrievalMode::Fts, 5, vec![]);

        // Stale: kept, flagged, and counted as a candidate like any other.
        let stale = recall(temp.path(), ask(stale_id)).expect("recall");
        assert_eq!(result_ids(&stale), vec![("fact".to_string(), stale_id)]);
        assert!(stale.results[0].stale);
        assert!(stale.candidate_count >= 1);

        // Superseded: kept with its link, counted as demoted.
        let superseded = recall(temp.path(), ask(old_id)).expect("recall");
        assert_eq!(superseded.results[0].superseded_by, Some(new_id));
        assert_eq!(superseded.explain.superseded_demoted, 1);

        // Accepted-only: the agent-sourced row is excluded and counted.
        let accepted = recall(
            temp.path(),
            RecallOptions {
                accepted_only: Some(true),
                ..ask(agent_id)
            },
        )
        .expect("recall");
        assert!(accepted.results.is_empty());
        assert_eq!(accepted.explain.accepted_only_excluded, 1);
    }

    // Issue #348: a named row a retriever found and a filter excluded is
    // counted once, not again by the routing.
    #[test]
    fn named_row_found_then_excluded_is_counted_once() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let (agent_id, _) = fact::add(
            temp.path(),
            "agent-zebra",
            "the zebra crossing rule",
            "agent:codex",
            "cli:user",
        )
        .expect("agent fact");

        // Without the number the retriever finds the row; with it, the
        // routing sees the same row. The exclusion counter must agree.
        let opts = |query: &str| RecallOptions {
            accepted_only: Some(true),
            ..explain_opts(query, RetrievalMode::Fts, 5, vec![SourceType::Fact])
        };
        let plain = recall(temp.path(), opts("zebra")).expect("recall");
        let named = recall(temp.path(), opts(&format!("zebra fact {agent_id}"))).expect("recall");
        assert_eq!(plain.explain.accepted_only_excluded, 1);
        assert_eq!(named.explain.accepted_only_excluded, 1);
        assert!(named.results.iter().all(|h| h.source_id != agent_id));
    }

    // Issue #348: named rows past `max_results` are counted in the limit cut.
    #[test]
    fn named_rows_cut_by_max_results_are_counted_in_the_result_limit_cut() {
        let temp = tempdir().expect("tempdir");
        seed(temp.path());

        let response = recall(
            temp.path(),
            explain_opts(
                "task 1 decision 1 fact 1 cargo",
                RetrievalMode::Fts,
                1,
                vec![],
            ),
        )
        .expect("recall");
        assert_eq!(result_ids(&response), vec![("task".to_string(), 1)]);
        // Every candidate but the returned row was cut: two named rows plus
        // the ranked remainder.
        assert!(response.explain.result_limit_cut >= 2);
        assert_eq!(
            response.explain.result_limit_cut,
            response.candidate_count - response.returned_count
        );
    }

    #[test]
    fn named_row_carries_rerank_score_in_hybrid_with_and_without_variants() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        edit_config(temp.path(), |cfg| {
            cfg.retrieval.mode = RetrievalMode::Hybrid
        });
        seed(temp.path());

        for variants in [Vec::new(), vec!["build command".to_string()]] {
            let mut opts =
                explain_opts("zxqv-nonsense decision 1", RetrievalMode::Hybrid, 5, vec![]);
            opts.variants = variants;
            let response = recall(temp.path(), opts).expect("recall");

            assert!(
                response.matcher.ends_with("+rerank"),
                "{}",
                response.matcher
            );
            let top = &response.results[0];
            assert_eq!((top.source_type.as_str(), top.source_id), ("decision", 1));
            assert!(top.rerank_score.is_some());
            let got = result_ids(&response);
            assert_eq!(
                got.len(),
                got.iter().collect::<HashSet<_>>().len(),
                "{got:?}"
            );
            // The ranked rows after it clear the cutoff, best first.
            let ranked: Vec<f32> = response.results[1..]
                .iter()
                .map(|h| h.rerank_score.expect("reranked"))
                .collect();
            assert!(ranked.windows(2).all(|w| w[0] >= w[1]), "{ranked:?}");
            assert!(
                ranked
                    .iter()
                    .all(|&s| s >= crate::config::DEFAULT_RERANK_CUTOFF),
                "{ranked:?}"
            );

            // The named row is an explicit lookup, exempt from the cutoff:
            // it comes back alone under an unreachable one, with no warning.
            let mut opts =
                explain_opts("zxqv-nonsense decision 1", RetrievalMode::Hybrid, 5, vec![]);
            opts.rerank_cutoff = Some(1000.0);
            let response = recall(temp.path(), opts).expect("recall");
            assert_eq!(result_ids(&response), [("decision".to_string(), 1)]);
            assert!(
                response
                    .warnings
                    .iter()
                    .all(|w| w.kind != "rerank_cutoff_dropped_all"),
                "a returned named row means the cutoff did not empty the call: {:?}",
                response.warnings
            );
        }

        // Re-ranker off: still routed, no rerank score.
        let mut opts = explain_opts("zxqv-nonsense decision 1", RetrievalMode::Hybrid, 5, vec![]);
        opts.use_reranker = Some(false);
        let response = recall(temp.path(), opts).expect("recall");
        let top = &response.results[0];
        assert_eq!((top.source_type.as_str(), top.source_id), ("decision", 1));
        assert!(top.rerank_score.is_none());
    }

    // Issue #341: a two-question main query is split so each half can rank.
    #[test]
    fn split_query_splits_at_and_plus_a_question_word_only() {
        assert_eq!(
            split_query("What does X measure, and where is Y set?"),
            ["What does X measure", "where is Y set?"]
        );
        assert_eq!(
            split_query("how is a built AND Why is b slow and can c run"),
            ["how is a built", "Why is b slow", "can c run"]
        );
        for q in [
            "build and lint",
            "build and whatever",
            "build and isolate the cache",
            "build and  where",
            "x and and",
            "é and é where",
            "tests and what",
            "build the index, and what",
            "foo and why?",
            "what and what is x",
            "what and",
            "Q and A about docs",
            "rock-and-roll who",
        ] {
            let parts = split_query(q);
            assert!(parts.is_empty(), "{q:?}: {parts:?}");
        }
        assert_eq!(split_query("x and and what is it"), ["x and", "what is it"]);
        assert_eq!(
            split_query("a and what and why is b"),
            ["a", "what and why is b"]
        );
        for word in QUESTION_WORDS {
            assert_eq!(split_query(&format!("a and {word} b")).len(), 2, "{word}");
        }
    }

    #[test]
    fn parts_exist_only_when_the_reranker_will_run() {
        let cfg = crate::config::RetrievalConfig::default();
        let q = "what does X measure, and where is Y set?";
        let parts = |mode, rerank: Option<bool>| {
            let opts = RecallOptions {
                mode: Some(mode),
                use_reranker: rerank,
                ..explain_opts(q, mode, 5, vec![])
            };
            ResolvedOptions::from(&opts, &cfg).expect("opts").parts
        };
        assert_eq!(parts(RetrievalMode::Hybrid, Some(true)).len(), 2);
        assert!(parts(RetrievalMode::Hybrid, Some(false)).is_empty());
        assert!(parts(RetrievalMode::Fts, Some(true)).is_empty());
    }

    // Issue #348: with a pool of 2, the whole query's own pool is taken by
    // two decoys that echo its wording; each answer reaches the re-ranker only
    // through its own part.
    #[test]
    fn row_pooled_only_through_a_part_is_returned() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        edit_config(temp.path(), |cfg| {
            cfg.retrieval.mode = RetrievalMode::Hybrid;
            cfg.retrieval.rerank_candidate_pool = 2;
        });
        for (key, value) in [
            (
                "latency-metric",
                "The latency metric measures end-to-end request time in milliseconds",
            ),
            (
                "timeout-setting",
                "The gadget timeout is set in the GADGET_TIMEOUT environment variable",
            ),
            (
                "decoy-one",
                "Questions about what the latency metric measures and where the gadget timeout is set",
            ),
            (
                "decoy-two",
                "Asking what does the latency metric measure and where is the gadget timeout set",
            ),
        ] {
            fact::add(temp.path(), key, value, "user", "cli:user").expect("fact");
        }
        let run = |query: &str| {
            recall(
                temp.path(),
                RecallOptions {
                    rerank_cutoff: Some(-1000.0),
                    ..explain_opts(query, RetrievalMode::Hybrid, 2, vec![SourceType::Fact])
                },
            )
            .expect("recall")
        };
        let titles = |r: &RecallResponse| -> Vec<String> {
            r.results.iter().map(|h| h.title.clone()).collect()
        };
        // Without a split point the whole query's pool holds only the decoys.
        assert_eq!(
            titles(&run(
                "what does the latency metric measure; where is the gadget timeout set?"
            )),
            ["decoy-two", "decoy-one"]
        );
        let split =
            run("what does the latency metric measure, and where is the gadget timeout set?");
        assert_eq!(titles(&split), ["latency-metric", "timeout-setting"]);
    }

    #[test]
    fn two_question_query_lifts_the_row_one_half_answers() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        edit_config(temp.path(), |cfg| {
            cfg.retrieval.mode = RetrievalMode::Hybrid;
        });
        for (key, value) in [
            (
                "latency-metric",
                "The latency metric measures end-to-end request time in milliseconds",
            ),
            (
                "timeout-setting",
                "The gadget timeout is set in the GADGET_TIMEOUT environment variable",
            ),
            ("cache-note", "The cache is cleared every night at midnight"),
        ] {
            fact::add(temp.path(), key, value, "user", "cli:user").expect("fact");
        }
        let main = "what does the latency metric measure, and where is the gadget timeout set?";
        let parts = split_query(main);
        assert_eq!(parts.len(), 2);

        let run = |variants: Vec<String>| {
            recall(
                temp.path(),
                RecallOptions {
                    rerank_cutoff: Some(-1000.0),
                    variants,
                    ..explain_opts(main, RetrievalMode::Hybrid, 10, vec![SourceType::Fact])
                },
            )
            .expect("recall")
        };
        let plain = run(Vec::new());
        // A variant that states one answer must not be scored against.
        let with_variant = run(vec!["cache is cleared at midnight".to_string()]);
        assert!(plain.explain.reranker_ran);
        assert_eq!(plain.query, main, "the response reports the query as given");
        assert_eq!(with_variant.query, main);

        let doc_of = |h: &RecallHit| {
            format!(
                "{}

{}",
                h.title, h.body
            )
        };
        let docs: Vec<String> = plain.results.iter().map(doc_of).collect();
        let mut expected = vec![f32::MIN; docs.len()];
        let mut whole = vec![0.0_f32; docs.len()];
        for (i, q) in std::iter::once(main)
            .chain(parts.iter().map(String::as_str))
            .enumerate()
        {
            for (idx, score) in rerank::rerank(q, &docs).expect("rerank") {
                expected[idx] = expected[idx].max(score);
                if i == 0 {
                    whole[idx] = score;
                }
            }
        }
        for resp in [&plain, &with_variant] {
            assert_eq!(resp.results.len(), 3);
            for hit in &resp.results {
                let at = plain
                    .results
                    .iter()
                    .position(|p| p.source_id == hit.source_id)
                    .expect("same rows");
                let got = hit.rerank_score.expect("score");
                assert!(
                    (got - expected[at]).abs() < 1e-3,
                    "{}: {got} is not the best-of-parts score {}",
                    hit.title,
                    expected[at]
                );
            }
        }
        // Order follows the best-of-parts score, best first.
        assert!(
            plain
                .results
                .windows(2)
                .all(|w| { w[0].rerank_score >= w[1].rerank_score })
        );
        // Each half's answer is lifted over its whole-query score.
        for key in ["latency-metric", "timeout-setting"] {
            let at = plain
                .results
                .iter()
                .position(|h| h.title.contains(key))
                .expect(key);
            assert!(
                expected[at] > whole[at] + 0.5,
                "{key}: best {} vs whole {}",
                expected[at],
                whole[at]
            );
        }
        // Both answers lead the bundle; the unrelated row trails.
        assert!(plain.results[2].title.contains("cache-note"));
        // A cutoff between a row's whole-query score and its best part
        // score: the row clears it only because of the part.
        let at = plain
            .results
            .iter()
            .position(|h| h.title.contains("timeout-setting"))
            .unwrap();
        let floored = recall(
            temp.path(),
            RecallOptions {
                rerank_cutoff: Some((whole[at] + expected[at]) / 2.0),
                ..explain_opts(main, RetrievalMode::Hybrid, 10, vec![SourceType::Fact])
            },
        )
        .expect("recall");
        assert!(
            floored
                .results
                .iter()
                .any(|h| h.title.contains("timeout-setting")),
            "lifted over the cutoff by its part"
        );
    }
}
