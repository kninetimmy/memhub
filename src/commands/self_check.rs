//! `memhub eval self-check` — does recall find a repository's own rows?
//!
//! The golden-set evals are tied to memhub's own database, so they cannot
//! say whether recall works in a repository memhub was dropped into. This
//! check needs no golden set: it samples that repository's own facts,
//! decisions, tasks, doc chunks and architecture sections, queries recall
//! with each sampled row's title and, separately, with a passage of the
//! row's own text, and checks whether the row comes back.
//!
//! Read-only (decision 51): it drives the shared `retrieval::recall` engine
//! with an agent's plain-recall defaults and `log_metrics: false`, so it
//! stages no pending write, logs nothing to `writes_log`, and appends
//! nothing to `recall_metrics` or the `recall_queries.jsonl` query log.

use rusqlite::{Connection, params};
use serde_json::{Value, json};

use crate::config::RetrievalMode;
use crate::db;
use crate::retrieval::{self, RecallOptions, SourceType};
use crate::{MemhubError, Result};

pub const DEFAULT_SAMPLE: usize = 10;
/// A row "came back in the top K" when it ranks at or above this.
pub const TOP_K: usize = 3;
/// Words in the passage query: a span of the row's own body.
const PASSAGE_WORDS: usize = 12;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Form {
    Title,
    Passage,
}

impl Form {
    fn as_str(self) -> &'static str {
        match self {
            Form::Title => "title",
            Form::Passage => "passage",
        }
    }
}

/// Counts for one query form of one row type.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct FormCounts {
    /// Sampled rows this form could query (a task with no notes has no
    /// passage, so it is not counted here).
    pub queried: usize,
    /// Rows recall returned at rank 1.
    pub first: usize,
    /// Rows recall returned at rank 1 to [`TOP_K`].
    pub top3: usize,
}

#[derive(Debug, Clone)]
pub struct TypeReport {
    pub source_type: &'static str,
    /// Rows of this type in the repository.
    pub total_rows: usize,
    pub sampled: usize,
    pub title: FormCounts,
    pub passage: FormCounts,
}

/// A sampled row that did not come back within the top [`TOP_K`].
#[derive(Debug, Clone)]
pub struct Miss {
    pub source_type: &'static str,
    pub source_id: i64,
    pub title: String,
    pub form: Form,
    pub query: String,
    /// Rank when it came back below the top [`TOP_K`]; `None` = not returned.
    pub rank: Option<usize>,
}

#[derive(Debug, Clone)]
pub struct SelfCheckReport {
    pub sample: usize,
    pub mode: Option<RetrievalMode>,
    pub types: Vec<TypeReport>,
    pub misses: Vec<Miss>,
}

struct SampledRow {
    id: i64,
    title: String,
    body: String,
}

/// Per-type SQL: `SELECT id, <title>, <body>` ordered by id, with `LIMIT 1
/// OFFSET ?1` appended by the caller. Doc chunks fall back to the document
/// title when the chunk has no heading.
const TYPES: [(SourceType, &str, &str); 5] = [
    (
        SourceType::Fact,
        "SELECT COUNT(*) FROM facts",
        "SELECT id, key, value FROM facts ORDER BY id LIMIT 1 OFFSET ?1",
    ),
    (
        SourceType::Decision,
        "SELECT COUNT(*) FROM decisions",
        "SELECT id, title, rationale FROM decisions ORDER BY id LIMIT 1 OFFSET ?1",
    ),
    (
        SourceType::Task,
        "SELECT COUNT(*) FROM tasks",
        "SELECT id, title, COALESCE(notes, '') FROM tasks ORDER BY id LIMIT 1 OFFSET ?1",
    ),
    (
        SourceType::DocChunk,
        "SELECT COUNT(*) FROM doc_chunks",
        "SELECT c.id, CASE WHEN trim(c.heading_path) = '' THEN d.title ELSE c.heading_path END, \
         c.body FROM doc_chunks c JOIN documents d ON d.id = c.doc_id \
         ORDER BY c.id LIMIT 1 OFFSET ?1",
    ),
    (
        SourceType::ArchSection,
        "SELECT COUNT(*) FROM arch_sections",
        "SELECT id, heading_path, body FROM arch_sections ORDER BY id LIMIT 1 OFFSET ?1",
    ),
];

pub fn run(start: &std::path::Path, sample: usize) -> Result<SelfCheckReport> {
    if sample == 0 {
        return Err(MemhubError::InvalidInput(
            "--sample must be greater than zero".to_string(),
        ));
    }
    let ctx = db::open_project(start)?;
    let mut report = SelfCheckReport {
        sample,
        mode: None,
        types: Vec::new(),
        misses: Vec::new(),
    };

    for (source_type, count_sql, row_sql) in TYPES {
        let total: i64 = ctx.conn.query_row(count_sql, [], |r| r.get(0))?;
        let total = total as usize;
        let rows = sample_rows(&ctx.conn, row_sql, total, sample)?;
        let mut tr = TypeReport {
            source_type: source_type.as_str(),
            total_rows: total,
            sampled: rows.len(),
            title: FormCounts::default(),
            passage: FormCounts::default(),
        };
        for row in &rows {
            let queries = [
                (Form::Title, Some(row.title.trim().to_string())),
                (Form::Passage, passage(&row.body)),
            ];
            for (form, query) in queries {
                let Some(query) = query.filter(|q| !q.is_empty()) else {
                    continue;
                };
                let rank = rank_of(start, source_type, row.id, &query, &mut report.mode)?;
                let counts = match form {
                    Form::Title => &mut tr.title,
                    Form::Passage => &mut tr.passage,
                };
                counts.queried += 1;
                counts.first += usize::from(rank == Some(1));
                let in_top = rank.is_some_and(|r| r <= TOP_K);
                counts.top3 += usize::from(in_top);
                if !in_top {
                    report.misses.push(Miss {
                        source_type: tr.source_type,
                        source_id: row.id,
                        title: row.title.clone(),
                        form,
                        query,
                        rank,
                    });
                }
            }
        }
        report.types.push(tr);
    }
    Ok(report)
}

/// Up to `sample` rows spread evenly across the id-ordered table, so the
/// draw is a pure function of the data: an unchanged database yields the
/// same rows every run.
fn sample_rows(
    conn: &Connection,
    row_sql: &str,
    total: usize,
    sample: usize,
) -> Result<Vec<SampledRow>> {
    let take = sample.min(total);
    let mut stmt = conn.prepare(row_sql)?;
    (0..take)
        .map(|i| {
            let offset = (i * total / take) as i64;
            stmt.query_row(params![offset], |r| {
                Ok(SampledRow {
                    id: r.get(0)?,
                    title: r.get(1)?,
                    body: r.get(2)?,
                })
            })
            .map_err(Into::into)
        })
        .collect()
}

/// A [`PASSAGE_WORDS`]-word span from the middle of the body (the whole body
/// when shorter); `None` for an empty body.
fn passage(body: &str) -> Option<String> {
    let words: Vec<&str> = body.split_whitespace().collect();
    if words.is_empty() {
        return None;
    }
    let start = words.len().saturating_sub(PASSAGE_WORDS) / 2;
    let end = (start + PASSAGE_WORDS).min(words.len());
    Some(words[start..end].join(" "))
}

/// 1-based rank of the repo row among the hits of a plain recall for `query`.
fn rank_of(
    start: &std::path::Path,
    source_type: SourceType,
    source_id: i64,
    query: &str,
    mode: &mut Option<RetrievalMode>,
) -> Result<Option<usize>> {
    let response = retrieval::recall(
        start,
        RecallOptions {
            query: query.to_string(),
            mode: None,
            max_results: 0,
            source_types: Vec::new(),
            include_stale: None,
            accepted_only: None,
            use_reranker: None,
            rerank_cutoff: None,
            log_metrics: false,
            variants: Vec::new(),
            surface: None,
            explain: false,
        },
    )?;
    mode.get_or_insert(response.mode);
    Ok(response
        .results
        .iter()
        .position(|h| {
            h.scope == retrieval::recall::SCOPE_REPO
                && h.source_type == source_type.as_str()
                && h.source_id == source_id
        })
        .map(|i| i + 1))
}

pub fn to_json(report: &SelfCheckReport) -> Value {
    let counts = |c: &FormCounts| json!({"queried": c.queried, "first": c.first, "top3": c.top3});
    json!({
        "sample": report.sample,
        "top_k": TOP_K,
        "mode": report.mode.map(mode_label),
        "types": report.types.iter().map(|t| json!({
            "source_type": t.source_type,
            "total_rows": t.total_rows,
            "sampled": t.sampled,
            "title": counts(&t.title),
            "passage": counts(&t.passage),
        })).collect::<Vec<_>>(),
        "misses": report.misses.iter().map(|m| json!({
            "source_type": m.source_type,
            "source_id": m.source_id,
            "title": m.title,
            "form": m.form.as_str(),
            "query": m.query,
            "rank": m.rank,
        })).collect::<Vec<_>>(),
    })
}

pub fn print_human(report: &SelfCheckReport) {
    println!(
        "Recall self-check (mode {}, up to {} rows per type, top-{TOP_K})",
        report.mode.map_or("n/a", mode_label),
        report.sample
    );
    println!();
    println!(
        "{:<13} {:>6} {:>7}  {:>19}  {:>19}",
        "type", "rows", "sampled", "title 1st/top3", "passage 1st/top3"
    );
    for t in &report.types {
        let cell = |c: &FormCounts| format!("{}/{} of {}", c.first, c.top3, c.queried);
        println!(
            "{:<13} {:>6} {:>7}  {:>19}  {:>19}",
            t.source_type,
            t.total_rows,
            t.sampled,
            cell(&t.title),
            cell(&t.passage)
        );
    }
    println!();
    if report.misses.is_empty() {
        println!("No sampled row missed the top {TOP_K}.");
        return;
    }
    println!("Missed the top {TOP_K} ({}):", report.misses.len());
    for m in &report.misses {
        let found = m
            .rank
            .map_or("not returned".to_string(), |r| format!("rank {r}"));
        println!(
            "  {} #{} [{} query, {found}] {}",
            m.source_type,
            m.source_id,
            m.form.as_str(),
            m.title
        );
        println!("      query: {}", m.query);
    }
}

fn mode_label(mode: RetrievalMode) -> &'static str {
    match mode {
        RetrievalMode::Fts => "fts",
        RetrievalMode::Hybrid => "hybrid",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn passage_takes_a_middle_span_or_the_whole_short_body() {
        let body: String = (1..=30).map(|n| format!("w{n} ")).collect();
        let p = passage(&body).expect("passage");
        assert_eq!(p.split(' ').count(), PASSAGE_WORDS);
        assert!(p.starts_with("w10 "), "{p}");
        assert_eq!(passage("  two words "), Some("two words".to_string()));
        assert_eq!(passage("  \n "), None);
    }
}
