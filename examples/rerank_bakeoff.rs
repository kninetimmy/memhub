//! Throwaway bake-off harness for task #20 (cross-encoder re-ranker bundle decision).
//!
//! Compares two candidate cross-encoders for memhub's hybrid recall:
//!   * cross-encoder/ms-marco-MiniLM-L-6-v2 (~80 MB, Xenova ONNX export)
//!   * BAAI/bge-reranker-v2-m3              (~280 MB + external data, rozgo ONNX export)
//!
//! Each candidate runs over the **same** top-N hybrid pool fetched from
//! `memhub recall --json --mode hybrid --max-results N`, then is scored on
//! `tests/retrieval_golden.json` (Recall@1, Recall@3, MRR) plus the gibberish
//! safety probe (max rerank score — lower is better). Latency is mean
//! per-query reranking time only; recall fetch is excluded.
//!
//! This is not production code. After the bake-off informs the bundle
//! decision, `src/retrieval/rerank.rs` + `build.rs` integration for the
//! winner happens in a separate PR; this example can stay (as a regression
//! harness for future re-ranker swaps) or be deleted.
//!
//! Run:
//!   cargo run --release --example rerank_bakeoff -- --candidate-pool 30
//!
//! Model files cache at /tmp/memhub-rerank-bakeoff/{minilm,bge_v2_m3}/.
//! ~1.5 GB of HuggingFace downloads on first run; subsequent runs reuse.

use std::{
    fs, io,
    path::{Path, PathBuf},
    process::Command,
    time::Instant,
};

use anyhow::{Context, Result, anyhow};
use fastembed::{
    OnnxSource, RerankInitOptionsUserDefined, TextRerank, TokenizerFiles, UserDefinedRerankingModel,
};
use serde::Deserialize;

const CACHE_ROOT: &str = "/tmp/memhub-rerank-bakeoff";

struct ModelSpec {
    label: &'static str,
    cache_subdir: &'static str,
    base_url: &'static str,
    /// (remote_path_under_base_url, local_filename)
    files: &'static [(&'static str, &'static str)],
}

const MINILM: ModelSpec = ModelSpec {
    label: "cross-encoder/ms-marco-MiniLM-L-6-v2 (Xenova)",
    cache_subdir: "minilm",
    base_url: "https://huggingface.co/Xenova/ms-marco-MiniLM-L-6-v2/resolve/main",
    files: &[
        ("onnx/model.onnx", "model.onnx"),
        ("tokenizer.json", "tokenizer.json"),
        ("config.json", "config.json"),
        ("special_tokens_map.json", "special_tokens_map.json"),
        ("tokenizer_config.json", "tokenizer_config.json"),
    ],
};

const BGE_V2_M3: ModelSpec = ModelSpec {
    label: "BAAI/bge-reranker-v2-m3 (rozgo)",
    cache_subdir: "bge_v2_m3",
    base_url: "https://huggingface.co/rozgo/bge-reranker-v2-m3/resolve/main",
    files: &[
        ("model.onnx", "model.onnx"),
        ("model.onnx.data", "model.onnx.data"),
        ("tokenizer.json", "tokenizer.json"),
        ("config.json", "config.json"),
        ("special_tokens_map.json", "special_tokens_map.json"),
        ("tokenizer_config.json", "tokenizer_config.json"),
    ],
};

#[derive(Debug, Deserialize)]
struct GoldenFile {
    queries: Vec<GoldenQuery>,
}

#[derive(Debug, Deserialize, Clone)]
struct GoldenQuery {
    id: String,
    query: String,
    kind: String,
    #[serde(default)]
    source_type: Option<String>,
    #[serde(default)]
    title_contains: Vec<String>,
    #[serde(default)]
    body_contains: Vec<String>,
}

#[derive(Debug, Deserialize, Clone)]
struct RecallResponse {
    results: Vec<RecallHit>,
}

#[derive(Debug, Deserialize, Clone)]
struct RecallHit {
    #[allow(dead_code)]
    rank: usize,
    source_type: String,
    title: String,
    body: String,
}

fn ensure_model(spec: &ModelSpec) -> Result<PathBuf> {
    let dir = Path::new(CACHE_ROOT).join(spec.cache_subdir);
    fs::create_dir_all(&dir).with_context(|| format!("mkdir {}", dir.display()))?;
    for (remote, local) in spec.files {
        let dest = dir.join(local);
        if dest.exists() {
            continue;
        }
        let url = format!("{}/{remote}", spec.base_url);
        eprintln!("  fetch {url}");
        eprintln!("     -> {}", dest.display());
        let resp = ureq::get(&url)
            .call()
            .with_context(|| format!("GET {url}"))?;
        let mut writer = fs::File::create(&dest)?;
        let mut reader = resp.into_reader();
        let copied = io::copy(&mut reader, &mut writer)?;
        eprintln!("     {} bytes", copied);
    }
    Ok(dir)
}

fn load_reranker(spec: &ModelSpec) -> Result<TextRerank> {
    let dir = ensure_model(spec)?;
    // OnnxSource::File so the runtime can resolve sibling `model.onnx.data`
    // for split-weight models (BGE-reranker-v2-m3). Memory mode strips that
    // context and fails to initialize.
    let onnx_path = dir.join("model.onnx");
    let tokenizer_files = TokenizerFiles {
        tokenizer_file: fs::read(dir.join("tokenizer.json"))?,
        config_file: fs::read(dir.join("config.json"))?,
        special_tokens_map_file: fs::read(dir.join("special_tokens_map.json"))?,
        tokenizer_config_file: fs::read(dir.join("tokenizer_config.json"))?,
    };
    let model = UserDefinedRerankingModel::new(OnnxSource::File(onnx_path), tokenizer_files);
    TextRerank::try_new_from_user_defined(model, RerankInitOptionsUserDefined::default())
        .with_context(|| format!("init reranker {}", spec.label))
}

fn run_recall(query: &str, pool: usize) -> Result<Vec<RecallHit>> {
    let out = Command::new("memhub")
        .args([
            "recall",
            query,
            "--json",
            "--mode",
            "hybrid",
            "--max-results",
        ])
        .arg(pool.to_string())
        .output()
        .context("spawn memhub recall")?;
    if !out.status.success() {
        return Err(anyhow!(
            "memhub recall failed: {}",
            String::from_utf8_lossy(&out.stderr)
        ));
    }
    let resp: RecallResponse = serde_json::from_slice(&out.stdout)
        .with_context(|| format!("parse recall JSON for {query:?}"))?;
    Ok(resp.results)
}

fn matches_hit(hit: &RecallHit, q: &GoldenQuery) -> bool {
    if let Some(st) = &q.source_type
        && !hit.source_type.eq_ignore_ascii_case(st)
    {
        return false;
    }
    let t = hit.title.to_lowercase();
    let b = hit.body.to_lowercase();
    q.title_contains
        .iter()
        .all(|s| t.contains(&s.to_lowercase()))
        && q.body_contains
            .iter()
            .all(|s| b.contains(&s.to_lowercase()))
}

/// Score an ordering (permutation of candidate indices) against a query.
fn score_order(order: &[usize], hits: &[RecallHit], q: &GoldenQuery, m: &mut MatchMetrics) {
    m.queries_total += 1;
    for (rank0, &idx) in order.iter().enumerate() {
        if matches_hit(&hits[idx], q) {
            let r = rank0 + 1;
            if r == 1 {
                m.recall_at_1 += 1;
            }
            if r <= 3 {
                m.recall_at_3 += 1;
            }
            m.mrr_sum += 1.0 / r as f64;
            m.found_ranks.push((q.id.clone(), r));
            return;
        }
    }
    m.found_ranks.push((q.id.clone(), 0)); // 0 = not found
}

#[derive(Default, Debug)]
struct MatchMetrics {
    queries_total: usize,
    recall_at_1: usize,
    recall_at_3: usize,
    mrr_sum: f64,
    rerank_ms_total: f64,
    found_ranks: Vec<(String, usize)>, // (query_id, found_rank or 0)
}

impl MatchMetrics {
    fn r_at_1(&self) -> f64 {
        100.0 * self.recall_at_1 as f64 / self.queries_total as f64
    }
    fn r_at_3(&self) -> f64 {
        100.0 * self.recall_at_3 as f64 / self.queries_total as f64
    }
    fn mrr(&self) -> f64 {
        self.mrr_sum / self.queries_total as f64
    }
    fn mean_rerank_ms(&self) -> f64 {
        if self.queries_total == 0 {
            0.0
        } else {
            self.rerank_ms_total / self.queries_total as f64
        }
    }
}

fn rerank_score(
    reranker: &mut TextRerank,
    query: &str,
    docs: &[String],
) -> Result<(Vec<usize>, f64, f32)> {
    let doc_refs: Vec<&str> = docs.iter().map(|s| s.as_str()).collect();
    let t0 = Instant::now();
    let results = reranker.rerank(query, &doc_refs, false, None)?;
    let elapsed_ms = t0.elapsed().as_secs_f64() * 1000.0;
    let order: Vec<usize> = results.iter().map(|r| r.index).collect();
    let top_score = results
        .first()
        .map(|r| r.score)
        .unwrap_or(f32::NEG_INFINITY);
    Ok((order, elapsed_ms, top_score))
}

fn print_table_row(label: &str, m: &MatchMetrics) {
    println!(
        "  {:<48}  {:>7.1}%  {:>7.1}%  {:>5.3}   {:>7.1} ms",
        label,
        m.r_at_1(),
        m.r_at_3(),
        m.mrr(),
        m.mean_rerank_ms()
    );
}

fn main() -> Result<()> {
    let mut pool: usize = 30;
    let mut args = std::env::args().skip(1);
    while let Some(a) = args.next() {
        match a.as_str() {
            "--candidate-pool" => {
                pool = args
                    .next()
                    .ok_or_else(|| anyhow!("--candidate-pool needs N"))?
                    .parse()?
            }
            "-h" | "--help" => {
                eprintln!(
                    "Usage: cargo run --release --example rerank_bakeoff -- [--candidate-pool N]"
                );
                return Ok(());
            }
            other => return Err(anyhow!("unknown arg: {other}")),
        }
    }

    let golden_path = "tests/retrieval_golden.json";
    let golden: GoldenFile = serde_json::from_slice(&fs::read(golden_path)?)
        .with_context(|| format!("parse {golden_path}"))?;
    let match_queries: Vec<&GoldenQuery> = golden
        .queries
        .iter()
        .filter(|q| q.kind == "match")
        .collect();
    let empty_queries: Vec<&GoldenQuery> = golden
        .queries
        .iter()
        .filter(|q| q.kind == "empty")
        .collect();
    eprintln!(
        "Golden set: {} match queries, {} empty (safety) queries",
        match_queries.len(),
        empty_queries.len()
    );
    eprintln!("Candidate pool: top-{pool} by hybrid blend\n");

    eprintln!("Loading MiniLM...");
    let mut minilm = load_reranker(&MINILM)?;
    eprintln!("Loading BGE-reranker-v2-m3...");
    let mut bge = load_reranker(&BGE_V2_M3)?;
    eprintln!("Models loaded.\n");

    let mut base = MatchMetrics::default();
    let mut m_minilm = MatchMetrics::default();
    let mut m_bge = MatchMetrics::default();

    for q in &match_queries {
        let hits = run_recall(&q.query, pool)?;
        if hits.is_empty() {
            eprintln!("  [skip] {} — recall returned 0 candidates", q.id);
            continue;
        }
        let baseline_order: Vec<usize> = (0..hits.len()).collect();
        let docs: Vec<String> = hits
            .iter()
            .map(|h| format!("{}\n\n{}", h.title, h.body))
            .collect();

        let (minilm_order, minilm_ms, _) = rerank_score(&mut minilm, &q.query, &docs)?;
        m_minilm.rerank_ms_total += minilm_ms;
        let (bge_order, bge_ms, _) = rerank_score(&mut bge, &q.query, &docs)?;
        m_bge.rerank_ms_total += bge_ms;

        score_order(&baseline_order, &hits, q, &mut base);
        // Use the matching reranker's metrics for the rest, but rerank_ms_total
        // is already added above; score_order also bumps queries_total/etc.
        score_order(&minilm_order, &hits, q, &mut m_minilm);
        score_order(&bge_order, &hits, q, &mut m_bge);
    }

    println!(
        "=== Match-query results ({} queries, candidate pool top-{}) ===\n",
        match_queries.len(),
        pool
    );
    println!(
        "  {:<48}  {:>8}  {:>8}  {:>5}   {:>10}",
        "config", "Recall@1", "Recall@3", "MRR", "mean ms"
    );
    println!("  {}", "-".repeat(48 + 2 + 8 + 2 + 8 + 2 + 5 + 3 + 10));
    print_table_row("baseline (hybrid blend, no rerank)", &base);
    print_table_row("+ MiniLM-L-6-v2", &m_minilm);
    print_table_row("+ BGE-reranker-v2-m3", &m_bge);

    // Per-query rank table — useful for spotting regressions
    println!("\n=== Per-query target rank (0 = not found in top-{pool}) ===\n");
    println!(
        "  {:<46}  {:>8}  {:>8}  {:>8}",
        "query id", "baseline", "minilm", "bge-v2-m3"
    );
    println!("  {}", "-".repeat(46 + 2 + 8 + 2 + 8 + 2 + 8));
    for (i, q) in match_queries.iter().enumerate() {
        let b = base.found_ranks.get(i).map(|(_, r)| *r).unwrap_or(0);
        let mm = m_minilm.found_ranks.get(i).map(|(_, r)| *r).unwrap_or(0);
        let bg = m_bge.found_ranks.get(i).map(|(_, r)| *r).unwrap_or(0);
        println!("  {:<46}  {:>8}  {:>8}  {:>8}", q.id, b, mm, bg);
    }

    // Safety probes: how well do the rerankers discriminate against gibberish?
    println!("\n=== Safety probe (max rerank score over top-{pool} nonsense candidates) ===\n");
    println!(
        "  {:<46}  {:>14}  {:>14}",
        "query id", "minilm top", "bge top"
    );
    println!("  {}", "-".repeat(46 + 2 + 14 + 2 + 14));
    for q in &empty_queries {
        let hits = run_recall(&q.query, pool)?;
        if hits.is_empty() {
            println!(
                "  {:<46}  {:>14}  {:>14}",
                q.id, "(empty pool)", "(empty pool)"
            );
            continue;
        }
        let docs: Vec<String> = hits
            .iter()
            .map(|h| format!("{}\n\n{}", h.title, h.body))
            .collect();
        let (_, _, minilm_top) = rerank_score(&mut minilm, &q.query, &docs)?;
        let (_, _, bge_top) = rerank_score(&mut bge, &q.query, &docs)?;
        println!("  {:<46}  {:>14.4}  {:>14.4}", q.id, minilm_top, bge_top);
    }
    println!("\n(Lower top score = better safety discrimination.)");

    Ok(())
}
