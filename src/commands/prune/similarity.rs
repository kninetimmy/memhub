//! Exhaustive, report-only overlap checks using the bundled compact reranker.

mod dismissal;
pub(super) use dismissal::{Pair, prepare as prepare_dismissal};

use std::collections::{HashMap, HashSet};
use std::path::Path;

use chrono::{DateTime, NaiveDate};
use rusqlite::Connection;

use super::{
    Action, ArchitectureSource, Category, DocumentFile, FileIdentity, Item, PairExcerpts,
    SourceExcerpt, Subject, document_file, identifier,
};
use crate::Result;
use crate::commands::review::ACCEPT_SIMILARITY_RERANK_THRESHOLD;
use crate::retrieval::{
    arch_section_embed_text, decision_embed_text, doc_chunk_embed_text, fact_embed_text, rerank,
    task_embed_text,
};

struct Row {
    subject: Subject,
    excerpt: SourceExcerpt,
    state: String,
    active: bool,
    replacement: Option<i64>,
}

impl Row {
    fn match_text(&self) -> String {
        let text = &self.excerpt.text;
        match &self.subject {
            Subject::Fact { key, .. } => fact_embed_text(key, text),
            Subject::Decision { title, .. } => {
                decision_embed_text(title, text, self.excerpt.summary.as_deref())
            }
            Subject::Task { .. } => task_embed_text(text, self.excerpt.notes.as_deref()),
            Subject::ArchitectureSection { heading, .. } => arch_section_embed_text(heading, text),
            Subject::DocumentChunk { heading, .. } => doc_chunk_embed_text(heading, text),
            _ => unreachable!("only similarity source types"),
        }
    }
}

pub(super) fn visit(
    conn: &Connection,
    root: &Path,
    old: Option<(&str, i64)>,
    protected_file: Option<&FileIdentity>,
    visit: &mut impl FnMut(Item),
) -> Result<()> {
    visit_matching(conn, root, old, None, protected_file, &mut |item, _, _| {
        visit(item)
    })
}

fn visit_matching(
    conn: &Connection,
    root: &Path,
    old: Option<(&str, i64)>,
    requested: Option<&str>,
    protected_file: Option<&FileIdentity>,
    visit: &mut impl FnMut(Item, &Row, &Row),
) -> Result<()> {
    let facts = facts(conn, None)?;
    let decisions = decisions(conn, None)?;
    if old.is_none() {
        pairs(conn, &facts, Category::DuplicateFacts, requested, visit)?;
        pairs(
            conn,
            &decisions,
            Category::UnlinkedDecisions,
            requested,
            visit,
        )?;
    }
    let mut targets: Vec<&Row> = facts.iter().filter(|row| row.active).collect();
    let other_targets = correction_targets(conn, root, None, protected_file)?;
    targets.extend(&other_targets);
    // ponytail: exhaustive pair scoring; use a measured candidate index if
    // corpus size makes this manual scan too slow, not a silent candidate cap.
    for source in facts.iter().chain(&decisions).filter(|row| !row.active) {
        if old.is_some_and(|old| old != (source.excerpt.source_type, source.excerpt.source_id)) {
            continue;
        }
        let replacements = replacements(source, &facts);
        let category = Category::CorrectionSweep;
        let mut candidates = Vec::new();
        for &target in &targets {
            if (source.excerpt.source_type != target.excerpt.source_type
                || (source.excerpt.source_id != target.excerpt.source_id
                    && !replacements.contains(&target.excerpt.source_id)))
                && selected(category, source, target, requested)?
            {
                candidates.push(target);
            }
        }
        for target in matches(&source.match_text(), &candidates)? {
            let id = identifier(category, (&source.state, &target.state))?;
            visit(
                Item {
                    actions: vec![Action::dismiss(&id)],
                    id,
                    category,
                    subjects: vec![source.subject.clone(), target.subject.clone()],
                    reason: "Textual overlap with an old claim; not evidence of contradiction."
                        .into(),
                    excerpts: Some(PairExcerpts::Correction {
                        old_claim: source.excerpt.clone(),
                        possible_restatement: target.excerpt.clone(),
                    }),
                },
                source,
                target,
            );
        }
    }
    Ok(())
}

fn replacements(source: &Row, facts: &[Row]) -> HashSet<i64> {
    let mut out = HashSet::new();
    if source.excerpt.source_type == "fact" {
        let mut next = source.replacement;
        while let Some(id) = next {
            if !out.insert(id) {
                break;
            }
            next = facts
                .iter()
                .find(|row| row.excerpt.source_id == id)
                .and_then(|row| row.replacement);
        }
    }
    out
}

fn selected(category: Category, a: &Row, b: &Row, requested: Option<&str>) -> Result<bool> {
    match requested {
        Some(id) => Ok(identifier(category, (&a.state, &b.state))? == id),
        None => Ok(true),
    }
}

fn matches<'a>(query: &str, candidates: &[&'a Row]) -> Result<Vec<&'a Row>> {
    if candidates.is_empty() {
        return Ok(candidates.to_vec());
    }
    let docs: Vec<_> = candidates.iter().map(|row| row.match_text()).collect();
    let mut scored = rerank::rerank(query, &docs)?;
    // Stable source order, independent of score ordering or result caps.
    scored.sort_by_key(|&(index, _)| index);
    Ok(scored
        .into_iter()
        .filter(|&(_, score)| score >= ACCEPT_SIMILARITY_RERANK_THRESHOLD)
        .map(|(index, _)| candidates[index])
        .collect())
}

fn pairs(
    conn: &Connection,
    rows: &[Row],
    category: Category,
    requested: Option<&str>,
    visit: &mut impl FnMut(Item, &Row, &Row),
) -> Result<()> {
    let mut active = Vec::new();
    for row in rows.iter().filter(|row| row.active) {
        let time = match &row.subject {
            Subject::Decision { .. } => {
                decision_sort_key(conn, row.excerpt.recorded_at.as_deref().unwrap_or(""))?
            }
            _ => None,
        };
        active.push((time, row));
    }
    // Preserve the existing asymmetric rerank inputs and dismissal fingerprints.
    // Display order is separate and must not imply chronology or preference.
    active.sort_by_key(|(time, row)| (*time, row.excerpt.source_id));
    for (i, &(_, query)) in active.iter().enumerate() {
        let mut candidates = Vec::new();
        for &(_, candidate) in &active[..i] {
            let different_keys = match (&candidate.subject, &query.subject) {
                (Subject::Fact { key: a, .. }, Subject::Fact { key: b, .. }) => a != b,
                _ => true,
            };
            if different_keys && selected(category, candidate, query, requested)? {
                candidates.push(candidate);
            }
        }
        for candidate in matches(&query.match_text(), &candidates)? {
            let id = identifier(category, (&candidate.state, &query.state))?;
            let (first, second) = if candidate.excerpt.source_id < query.excerpt.source_id {
                (candidate, query)
            } else {
                (query, candidate)
            };
            visit(
                Item {
                    actions: vec![Action::dismiss(&id)],
                    id,
                    category,
                    subjects: vec![first.subject.clone(), second.subject.clone()],
                    reason: "Textual overlap meets the accept-time similarity cutoff; not evidence of contradiction.".into(),
                    excerpts: Some(PairExcerpts::Comparison {
                        first: first.excerpt.clone(),
                        second: second.excerpt.clone(),
                    }),
                },
                candidate,
                query,
            );
        }
    }
    Ok(())
}

fn row_filter(id: Option<i64>) -> &'static str {
    if id.is_some() { " AND id = ?1" } else { "" }
}

fn facts(conn: &Connection, id: Option<i64>) -> Result<Vec<Row>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT id, key, value, created_at, superseded_by, retired_at,
                json_array('fact', id, key, value, confidence, source, verified_at, created_at,
                           kind, superseded_by, retired_at, retired_reason)
         FROM facts WHERE project_id = 1 {} ORDER BY id",
        row_filter(id),
    ))?;
    Ok(stmt
        .query_map(rusqlite::params_from_iter(id), |r| {
            let id = r.get(0)?;
            let key: String = r.get(1)?;
            let replacement: Option<i64> = r.get(4)?;
            let retired: Option<String> = r.get(5)?;
            Ok(Row {
                excerpt: SourceExcerpt {
                    source_type: "fact",
                    source_id: id,
                    recorded_at: r.get(3)?,
                    text: r.get(2)?,
                    summary: None,
                    notes: None,
                    original_wording_unavailable: None,
                    architecture_source: None,
                },
                subject: Subject::Fact { id, key },
                state: r.get(6)?,
                active: replacement.is_none() && retired.is_none(),
                replacement,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn decisions(conn: &Connection, id: Option<i64>) -> Result<Vec<Row>> {
    let mut stmt = conn.prepare(&format!(
        "SELECT id, title, rationale, summary, decided_at, superseded_by, status,
                json_array('decision', id, title, rationale, summary, decided_at, source,
                           status, superseded_by)
         FROM decisions WHERE project_id = 1
           AND (status IN ('active', 'superseded') OR superseded_by IS NOT NULL)
          {} ORDER BY id",
        row_filter(id),
    ))?;
    Ok(stmt
        .query_map(rusqlite::params_from_iter(id), |r| {
            let id = r.get(0)?;
            let title: String = r.get(1)?;
            let replacement: Option<i64> = r.get(5)?;
            let status: String = r.get(6)?;
            Ok(Row {
                excerpt: SourceExcerpt {
                    source_type: "decision",
                    source_id: id,
                    recorded_at: r.get(4)?,
                    text: r.get(2)?,
                    summary: r.get(3)?,
                    notes: None,
                    original_wording_unavailable: None,
                    architecture_source: None,
                },
                subject: Subject::Decision { id, title },
                state: r.get(7)?,
                active: status == "active" && replacement.is_none(),
                replacement,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

// Existing model-input order: UTC second, leap-second flag, exact fraction.
// Retained solely for match and fingerprint compatibility, not as evidence.
type DecisionSortKey<'a> = (i64, bool, &'a str);

fn decision_sort_key<'a>(conn: &Connection, date: &'a str) -> Result<Option<DecisionSortKey<'a>>> {
    let date = date.trim();
    let decimal = date.split_once('.').map(|(prefix, suffix)| {
        let digits = suffix.bytes().take_while(u8::is_ascii_digit).count();
        (prefix, &suffix[..digits], &suffix[digits..])
    });
    let fraction = decimal.map_or("", |(_, digits, _)| digits.trim_end_matches('0'));
    if let Ok(time) = DateTime::parse_from_rfc3339(date) {
        return Ok(Some((
            time.timestamp(),
            time.timestamp_subsec_nanos() >= 1_000_000_000,
            fraction,
        )));
    }

    let Some((day, tail)) = date.split_at_checked(10) else {
        return Ok(None);
    };
    if NaiveDate::parse_from_str(day, "%Y-%m-%d").is_err() || date.contains('\0') {
        return Ok(None);
    }
    // Keep the prior sort-key behavior for date-only values and for clocks
    // with explicit hours and minutes; SQLite can fill in a missing clock.
    let clock = tail.trim_start_matches(|c: char| c == 'T' || c.is_ascii_whitespace());
    if !tail.is_empty()
        && !matches!(
            clock.as_bytes(),
            [b'0'..=b'9', b'0'..=b'9', b':', b'0'..=b'9', b'0'..=b'9', ..]
        )
    {
        return Ok(None);
    }
    // Only exact end-of-day midnight is allowed at hour 24; keep this
    // validation before replacing any fraction with zero.
    if clock.starts_with("24:") && !fraction.is_empty() {
        return Ok(None);
    }
    // Leave a zero fraction in place so SQLite validates its placement but
    // cannot round it into a different second (or outside its date range).
    let whole = decimal
        .filter(|(_, digits, _)| !digits.is_empty())
        .map(|(prefix, _, suffix)| format!("{prefix}.0{suffix}"));
    let seconds: Option<i64> = conn.query_row(
        "SELECT unixepoch(?1)",
        [whole.as_deref().unwrap_or(date)],
        |r| r.get(0),
    )?;
    Ok(seconds.map(|seconds| (seconds, false, fraction)))
}

fn correction_targets(
    conn: &Connection,
    root: &Path,
    selected: Option<(&str, i64)>,
    protected_file: Option<&FileIdentity>,
) -> Result<Vec<Row>> {
    let mut out = Vec::new();
    if selected.is_none_or(|(kind, _)| kind == "arch_section") {
        let mut stmt = conn.prepare(&format!(
            "SELECT s.id, s.heading_path, s.body, a.created_at,
                json_array('arch_section', s.id, s.arch_id, s.ord, s.heading_path, s.body,
                           s.created_at, a.body, a.actor, a.actor_raw, a.created_at),
                 a.body, a.id, s.ord
         FROM arch_sections s JOIN project_arch a ON a.id = s.arch_id
         WHERE s.project_id = 1 AND a.id = (
             SELECT id FROM project_arch WHERE project_id = 1
             ORDER BY created_at DESC, id DESC LIMIT 1)
          {} ORDER BY s.id",
            if selected.is_some() {
                "AND s.id = ?1"
            } else {
                ""
            },
        ))?;
        for row in stmt.query_map(
            rusqlite::params_from_iter(selected.map(|(_, id)| id)),
            |r| {
                let id = r.get(0)?;
                let heading: String = r.get(1)?;
                let text: String = r.get(2)?;
                let original: String = r.get(5)?;
                let original_wording_unavailable = (!original.contains(&text)).then_some(
                    "The indexed section is not a verbatim slice of the retained architecture. See the full retained source.",
                );
                Ok(Row {
                    excerpt: SourceExcerpt {
                        source_type: "arch_section",
                        source_id: id,
                        recorded_at: r.get(3)?,
                        text,
                        summary: None,
                        notes: None,
                        original_wording_unavailable,
                        architecture_source: Some(ArchitectureSource {
                            text: original,
                            architecture_id: r.get(6)?,
                            section_ordinal: r.get(7)?,
                        }),
                    },
                    subject: Subject::ArchitectureSection { id, heading },
                    state: r.get(4)?,
                    active: true,
                    replacement: None,
                })
            },
        )? {
            out.push(row?);
        }
    }
    if selected.is_none_or(|(kind, _)| kind == "doc_chunk") {
        let mut files = HashMap::new();
        let mut stmt = conn.prepare(&format!(
            "SELECT c.id, c.heading_path, c.body, c.created_at, d.id, d.path, d.title,
                json_array('doc_chunk', c.id, c.doc_id, c.ord, c.heading_path, c.body, c.created_at,
                           d.path, d.title, d.content_hash, d.byte_len, d.source, d.ingested_at,
                           d.archived_at, d.archived_reason)
         FROM doc_chunks c JOIN documents d ON d.id = c.doc_id
          WHERE c.project_id = 1 AND d.project_id = 1 AND d.archived_at IS NULL {} ORDER BY c.id",
            if selected.is_some() {
                "AND c.id = ?1"
            } else {
                ""
            },
        ))?;
        for row in stmt.query_map(
            rusqlite::params_from_iter(selected.map(|(_, id)| id)),
            |r| {
                let id = r.get(0)?;
                let heading: String = r.get(1)?;
                Ok(Row {
                    excerpt: SourceExcerpt {
                        source_type: "doc_chunk",
                        source_id: id,
                        recorded_at: r.get(3)?,
                        text: r.get(2)?,
                        summary: None,
                        notes: None,
                        original_wording_unavailable: Some(
                            "The original document and exact ancestor headings are not retained with this chunk.",
                        ),
                        architecture_source: None,
                    },
                    subject: Subject::DocumentChunk {
                        id,
                        document_id: r.get(4)?,
                        path: r.get(5)?,
                        title: r.get(6)?,
                        heading,
                    },
                    state: r.get(7)?,
                    active: true,
                    replacement: None,
                })
            },
        )? {
            let mut row = row?;
            if let Subject::DocumentChunk { path, .. } = &row.subject {
                let on_disk = files
                    .entry(path.clone())
                    .or_insert_with(|| document_file(root, Path::new(path), protected_file));
                if matches!(on_disk, DocumentFile::LiveDatabase) {
                    continue;
                }
                row.state = serde_json::to_string(&(&row.state, &*on_disk))?;
            }
            out.push(row);
        }
    }
    if selected.is_none_or(|(kind, _)| kind == "task") {
        let mut stmt = conn.prepare(&format!(
            "SELECT id, title, notes, created_at,
                json_array('task', id, title, notes, status, created_at, updated_at, dropped_reason,
                           pre_drop_status, pre_drop_updated_at, pre_keep_updated_at, kept_at)
          FROM tasks WHERE project_id = 1 AND status = 'open' {} ORDER BY id",
            row_filter(selected.map(|(_, id)| id)),
        ))?;
        for row in stmt.query_map(
            rusqlite::params_from_iter(selected.map(|(_, id)| id)),
            |r| {
                let id = r.get(0)?;
                let title: String = r.get(1)?;
                Ok(Row {
                    excerpt: SourceExcerpt {
                        source_type: "task",
                        source_id: id,
                        recorded_at: r.get(3)?,
                        text: title.clone(),
                        summary: None,
                        notes: r.get(2)?,
                        original_wording_unavailable: None,
                        architecture_source: None,
                    },
                    subject: Subject::Task { id, title },
                    state: r.get(4)?,
                    active: true,
                    replacement: None,
                })
            },
        )? {
            out.push(row?);
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decision_sort_key_preserves_decimal_precision_across_offsets_and_second_boundaries() {
        let conn = Connection::open_in_memory().unwrap();
        for (earlier, later) in [
            ("2025-01-01T00:00:00.0001Z", "2025-01-01T00:00:00.0002Z"),
            (
                "2025-01-01 00:00:00.0000000001",
                "2025-01-01t00:00:00.0000000002z",
            ),
            ("2025-01-01T00:00:00.9999999999Z", "2025-01-01T00:00:01Z"),
            ("2025-01-01 00:00:00.9999999999", "2025-01-01 00:00:01"),
            (
                "2025-01-01T00:00:00.0001-01:00",
                "2025-01-01T01:00:00.0002Z",
            ),
            (
                "2025-01-01T01:00:00.0001+01:00",
                "2025-01-01T00:00:00.0002Z",
            ),
            ("2016-12-31T23:59:59.999Z", "2016-12-31T23:59:60Z"),
            ("2016-12-31T23:59:60.999Z", "2017-01-01T00:00:00Z"),
        ] {
            assert!(
                decision_sort_key(&conn, earlier).unwrap().unwrap()
                    < decision_sort_key(&conn, later).unwrap().unwrap(),
                "{earlier} < {later}"
            );
        }
        for same in [
            "2025-01-01T00:00:00.000100Z",
            "2025-01-01 01:00:00.0001+01:00",
        ] {
            assert_eq!(
                decision_sort_key(&conn, same).unwrap(),
                decision_sort_key(&conn, "2025-01-01T00:00:00.0001Z").unwrap()
            );
        }
        for unknown in [
            "unset",
            "now",
            "00:00:00",
            "2025-02-30 00:00:00",
            "2025-01-01T",
            "2025-01-01 T T",
            "2025-01-01T12:60:00",
            "2025-01-01 24:00:00.0001",
        ] {
            assert_eq!(
                decision_sort_key(&conn, unknown).unwrap(),
                None,
                "{unknown}"
            );
        }
    }

    #[test]
    fn decision_sort_key_preserves_date_only_and_minute_buckets() {
        let conn = Connection::open_in_memory().unwrap();
        let time = |date| decision_sort_key(&conn, date).unwrap().unwrap();
        let day = time("2025-01-01");
        let minute = time("2025-01-01 01:00+01:00");
        let instant = time("2025-01-01T00:00:00.1Z");
        assert_eq!(day, minute);
        assert!(minute < instant);
        assert_eq!(
            time("2025-01-01 24:00:00.0000"),
            time("2025-01-02T00:00:00Z")
        );
    }
}
