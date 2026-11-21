//! Exhaustive, report-only overlap checks using the bundled compact reranker.

mod dismissal;
pub(super) use dismissal::{Pair, prepare as prepare_dismissal};

use std::collections::{HashMap, HashSet};
use std::path::Path;

use chrono::{DateTime, NaiveDate};
use rusqlite::Connection;

use super::{
    Action, ArchitectureSource, Category, DocumentFile, Item, PairExcerpts, SourceExcerpt, Subject,
    document_file, identifier, re_ingest_action,
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
    document_file: Option<DocumentFile>,
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
    visit: &mut impl FnMut(Item),
) -> Result<()> {
    visit_matching(conn, root, old, None, &mut |item, _, _| visit(item))
}

fn visit_matching(
    conn: &Connection,
    root: &Path,
    old: Option<(&str, i64)>,
    requested: Option<&str>,
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
    let other_targets = correction_targets(conn, root, None)?;
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
            let mut reason = "Textual overlap with an old claim; not evidence of contradiction. Check version qualifiers before choosing keep or correcting the row.".to_string();
            let mut actions = correction_actions(target).unwrap_or_else(|why| {
                reason.push(' ');
                reason.push_str(&why);
                Vec::new()
            });
            actions.push(Action::keep(&id));
            visit(
                Item {
                    id,
                    category,
                    subjects: vec![source.subject.clone(), target.subject.clone()],
                    reason,
                    actions,
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
                decision_time(conn, row.excerpt.recorded_at.as_deref().unwrap_or(""))?
            }
            _ => None,
        };
        active.push((time, row));
    }
    // ID only stabilizes enumeration and fingerprints; it is never evidence of order.
    active.sort_by_key(|(time, row)| (time.map(|t| t.start), row.excerpt.source_id));
    let times: HashMap<_, _> = active
        .iter()
        .map(|&(time, row)| (row.excerpt.source_id, time))
        .collect();
    for (i, &(later_time, later)) in active.iter().enumerate() {
        let mut earlier_rows = Vec::new();
        for &(_, earlier) in &active[..i] {
            let different_keys = match (&earlier.subject, &later.subject) {
                (Subject::Fact { key: a, .. }, Subject::Fact { key: b, .. }) => a != b,
                _ => true,
            };
            if different_keys && selected(category, earlier, later, requested)? {
                earlier_rows.push(earlier);
            }
        }
        for earlier in matches(&later.match_text(), &earlier_rows)? {
            let id = identifier(category, (&earlier.state, &later.state))?;
            let mut reason = "Textual overlap meets the accept-time similarity cutoff; not evidence of contradiction.".to_string();
            let keep = Action::new("keep_both", ["prune", "dismiss", &id]);
            let a = earlier.excerpt.source_id;
            let b = later.excerpt.source_id;
            let mut actions = Vec::new();
            if category == Category::DuplicateFacts {
                for (action, id) in [("retire_earlier", a), ("retire_later", b)] {
                    if id < 0 {
                        reason.push_str(&format!(
                            " Retire unavailable for fact #{id}: negative IDs are not offered as CLI operands."
                        ));
                    } else {
                        actions.push(retire(action, id));
                    }
                }
            } else if !times[&a]
                .zip(later_time)
                .is_some_and(|(a, b)| a.is_before(b))
            {
                reason.push_str(" Decision chronology is unknown (missing, equal, or overlapping); no supersession command is offered.");
            } else if a < 0 || b < 0 {
                reason.push_str(
                    " Supersession unavailable: negative IDs are not offered as CLI operands.",
                );
            } else {
                actions.push(Action::new(
                    "supersede_earlier",
                    [
                        "decision",
                        "supersede",
                        &a.to_string(),
                        "--by",
                        &b.to_string(),
                    ],
                ));
            }
            actions.push(keep);
            visit(
                Item {
                    id,
                    category,
                    subjects: vec![earlier.subject.clone(), later.subject.clone()],
                    reason,
                    actions,
                    excerpts: Some(PairExcerpts::Comparison {
                        first: earlier.excerpt.clone(),
                        second: later.excerpt.clone(),
                    }),
                },
                earlier,
                later,
            );
        }
    }
    Ok(())
}

fn retire(action: &'static str, id: i64) -> Action {
    Action::new(
        action,
        [
            "fact",
            "retire",
            &id.to_string(),
            "--reason",
            "prune review: obsolete fact",
        ],
    )
}

fn correction_actions(row: &Row) -> std::result::Result<Vec<Action>, String> {
    Ok(match &row.subject {
        Subject::Fact { id, .. } | Subject::Task { id, .. } if *id < 0 => {
            return Err(
                "Correction commands unavailable: negative IDs are not offered as CLI operands."
                    .into(),
            );
        }
        Subject::Fact { id, .. } => vec![retire("retire", *id)],
        Subject::Task { id, .. } => vec![
            Action::new(
                "drop",
                [
                    "task",
                    "drop",
                    &id.to_string(),
                    "--reason",
                    "prune review: obsolete task",
                ],
            ),
            Action::new("keep_task", ["task", "keep", &id.to_string()]),
        ],
        Subject::DocumentChunk { path, title, .. } => {
            // Editing is human work, not an invented memhub edit verb.
            vec![
                re_ingest_action(
                    "edit_and_re_ingest",
                    row.document_file.as_ref().expect("document file state"),
                    path,
                    title,
                )
                .map_err(|why| format!("Edit and re-ingest unavailable: {why}"))?,
            ]
        }
        Subject::ArchitectureSection { .. } => {
            if !row
                .excerpt
                .architecture_source
                .as_ref()
                .is_some_and(|arch| arch.rewrite_supported)
            {
                return Err("Rewrite unavailable: an appended architecture is not guaranteed to become current under the stored timestamp and ID ordering.".into());
            }
            let mut action = Action::new(
                "rewrite",
                [
                    "arch",
                    "set",
                    "--from-file",
                    "<rewritten-architecture-file>",
                ],
            );
            action.replace = Some("<rewritten-architecture-file>");
            vec![action]
        }
        _ => unreachable!("only correction target types"),
    })
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
                document_file: None,
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
                document_file: None,
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

// Order by UTC second (including leap seconds), then the exact decimal
// fraction: the parsers' built-in subsecond precision can round or truncate.
type Instant<'a> = (i64, bool, &'a str);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct DecisionTime<'a> {
    start: Instant<'a>,
    end_second: Option<i64>,
}

impl DecisionTime<'_> {
    fn is_before(self, other: Self) -> bool {
        match self.end_second {
            Some(end) => end <= other.start.0,
            None => self.start < other.start,
        }
    }
}

fn decision_time<'a>(conn: &Connection, date: &'a str) -> Result<Option<DecisionTime<'a>>> {
    let date = date.trim();
    let decimal = date.split_once('.').map(|(prefix, suffix)| {
        let digits = suffix.bytes().take_while(u8::is_ascii_digit).count();
        (prefix, &suffix[..digits], &suffix[digits..])
    });
    let fraction = decimal.map_or("", |(_, digits, _)| digits.trim_end_matches('0'));
    if let Ok(time) = DateTime::parse_from_rfc3339(date) {
        return Ok(Some(DecisionTime {
            start: (
                time.timestamp(),
                time.timestamp_subsec_nanos() >= 1_000_000_000,
                fraction,
            ),
            end_second: None,
        }));
    }

    // Use SQLite for its native calendar-bearing layouts. Reject invalid
    // calendar dates, time-only strings, and ambiguous numeric values.
    if date
        .get(..10)
        .is_none_or(|day| NaiveDate::parse_from_str(day, "%Y-%m-%d").is_err())
    {
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
    // SQLite fills in missing fields with zeroes, but an imported day or
    // minute is not evidence of the exact instant those zeroes imply.
    let uncertainty = if date.len() == 10 {
        Some(86_400)
    } else {
        let clock = date[10..].split(['+', '-', 'Z', 'z']).next().unwrap_or("");
        (clock.matches(':').count() == 1).then_some(60)
    };
    Ok(seconds.map(|seconds| DecisionTime {
        start: (seconds, false, fraction),
        end_second: uncertainty.map(|width| seconds + width),
    }))
}

fn correction_targets(
    conn: &Connection,
    root: &Path,
    selected: Option<(&str, i64)>,
) -> Result<Vec<Row>> {
    let mut out = Vec::new();
    if selected.is_none_or(|(kind, _)| kind == "arch_section") {
        let mut stmt = conn.prepare(&format!(
            "SELECT s.id, s.heading_path, s.body, a.created_at,
                json_array('arch_section', s.id, s.arch_id, s.ord, s.heading_path, s.body,
                           s.created_at, a.body, a.actor, a.actor_raw, a.created_at),
                a.body, a.id, s.ord,
                (a.created_at < CURRENT_TIMESTAMP OR (a.created_at = CURRENT_TIMESTAMP
                 AND (SELECT max(id) FROM project_arch) < 9223372036854775807))
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
                            rewrite_supported: r.get(8)?,
                        }),
                    },
                    subject: Subject::ArchitectureSection { id, heading },
                    state: r.get(4)?,
                    active: true,
                    replacement: None,
                    document_file: None,
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
                    document_file: None,
                })
            },
        )? {
            let mut row = row?;
            if let Subject::DocumentChunk { path, .. } = &row.subject {
                let on_disk = files
                    .entry(path.clone())
                    .or_insert_with(|| document_file(root, Path::new(path)));
                row.state = serde_json::to_string(&(&row.state, &*on_disk))?;
                row.document_file = Some(on_disk.clone());
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
                    document_file: None,
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
    fn decision_time_preserves_decimal_precision_across_offsets_and_second_boundaries() {
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
                decision_time(&conn, earlier)
                    .unwrap()
                    .unwrap()
                    .is_before(decision_time(&conn, later).unwrap().unwrap()),
                "{earlier} < {later}"
            );
        }
        for same in [
            "2025-01-01T00:00:00.000100Z",
            "2025-01-01 01:00:00.0001+01:00",
        ] {
            assert_eq!(
                decision_time(&conn, same).unwrap(),
                decision_time(&conn, "2025-01-01T00:00:00.0001Z").unwrap()
            );
        }
        for unknown in ["unset", "now", "00:00:00", "2025-02-30 00:00:00"] {
            assert_eq!(decision_time(&conn, unknown).unwrap(), None, "{unknown}");
        }
    }

    #[test]
    fn decision_order_requires_non_overlapping_times() {
        let conn = Connection::open_in_memory().unwrap();
        let time = |date| decision_time(&conn, date).unwrap().unwrap();
        let day = time("2025-01-01");
        let minute = time("2025-01-01 01:00+01:00");
        let instant = time("2025-01-01T00:00:00.1Z");
        for (a, b) in [
            (day, instant),
            (day, day),
            (minute, instant),
            (instant, instant),
        ] {
            assert!(!a.is_before(b) && !b.is_before(a));
        }
        assert!(day.is_before(time("2025-01-02T00:00:00Z")));
        assert!(minute.is_before(time("2025-01-01T00:01:00Z")));
    }
}
