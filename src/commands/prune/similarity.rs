//! Exhaustive, report-only overlap checks using the bundled compact reranker.

mod dismissal;
pub(super) use dismissal::prepare as prepare_dismissal;

use std::collections::{HashMap, HashSet};
use std::path::Path;

use rusqlite::Connection;

use super::{
    Action, Category, CorrectionExcerpts, DocumentFile, Item, SourceExcerpt, Subject,
    document_file, identifier,
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
        pairs(&facts, Category::DuplicateFacts, requested, visit)?;
        pairs(&decisions, Category::UnlinkedDecisions, requested, visit)?;
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
        for target in matches(&source.excerpt.text, &candidates)? {
            let id = identifier(category, (&source.state, &target.state))?;
            let mut actions = correction_actions(target);
            let mut reason = "Textual overlap with an old claim; not evidence of contradiction. Check version qualifiers before choosing keep or correcting the row.".to_string();
            if matches!(&target.subject, Subject::DocumentChunk { .. }) && actions.is_empty() {
                reason.push_str(" Edit and re-ingest unavailable: the recorded path is missing, unreadable, or redirected. Restore a regular UTF-8 file at that exact path before rescanning; the excerpt is the stored chunk, not redirected content.");
            }
            actions.push(Action::keep(&id));
            visit(
                Item {
                    id,
                    category,
                    subjects: vec![source.subject.clone(), target.subject.clone()],
                    reason,
                    actions,
                    excerpts: Some(CorrectionExcerpts {
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
    let docs: Vec<_> = candidates
        .iter()
        .map(|row| row.excerpt.text.clone())
        .collect();
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
    rows: &[Row],
    category: Category,
    requested: Option<&str>,
    visit: &mut impl FnMut(Item, &Row, &Row),
) -> Result<()> {
    let active: Vec<_> = rows.iter().filter(|row| row.active).collect();
    for (i, later) in active.iter().enumerate() {
        let mut earlier_rows = Vec::new();
        for &earlier in &active[..i] {
            let different_keys = match (&earlier.subject, &later.subject) {
                (Subject::Fact { key: a, .. }, Subject::Fact { key: b, .. }) => a != b,
                _ => true,
            };
            if different_keys && selected(category, earlier, later, requested)? {
                earlier_rows.push(earlier);
            }
        }
        for earlier in matches(&later.excerpt.text, &earlier_rows)? {
            let id = identifier(category, (&earlier.state, &later.state))?;
            let actions = if category == Category::DuplicateFacts {
                vec![
                    retire("retire_earlier", earlier.excerpt.source_id),
                    retire("retire_later", later.excerpt.source_id),
                    Action::new("keep_both", ["prune", "dismiss", &id]),
                ]
            } else {
                vec![
                    Action::new(
                        "supersede_earlier",
                        [
                            "decision",
                            "supersede",
                            &earlier.excerpt.source_id.to_string(),
                            "--by",
                            &later.excerpt.source_id.to_string(),
                        ],
                    ),
                    Action::new("keep_both", ["prune", "dismiss", &id]),
                ]
            };
            visit(Item {
                id,
                category,
                subjects: vec![earlier.subject.clone(), later.subject.clone()],
                reason: "Textual overlap meets the accept-time similarity cutoff; not evidence of contradiction.".into(),
                actions,
                excerpts: None,
            }, earlier, later);
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

fn correction_actions(row: &Row) -> Vec<Action> {
    match &row.subject {
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
            if !matches!(row.document_file, Some(DocumentFile::Readable { .. })) {
                return Vec::new();
            }
            // Editing is human work, not an invented memhub edit verb.
            vec![Action::new(
                "edit_and_re_ingest",
                ["doc", "add", &format!("--title={title}"), "--", path],
            )]
        }
        Subject::ArchitectureSection { .. } => {
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
    }
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
                    text: fact_embed_text(&key, &r.get::<_, String>(2)?),
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
    let rows = stmt
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
                    text: decision_embed_text(
                        &title,
                        &r.get::<_, String>(2)?,
                        r.get::<_, Option<String>>(3)?.as_deref(),
                    ),
                },
                subject: Subject::Decision { id, title },
                state: r.get(7)?,
                active: status == "active" && replacement.is_none(),
                replacement,
                document_file: None,
            })
        })?
        .collect::<rusqlite::Result<Vec<_>>>()?;
    let mut ordered = Vec::with_capacity(rows.len());
    for row in rows {
        let time = decision_time(conn, row.excerpt.recorded_at.as_deref().unwrap_or(""))?;
        ordered.push((time, row));
    }
    // SQLite's Julian day loses sub-millisecond precision. Keep its timezone
    // handling for whole seconds, then compare the exact decimal fraction.
    ordered.sort_by(|a, b| {
        a.0.cmp(&b.0)
            .then_with(|| a.1.excerpt.source_id.cmp(&b.1.excerpt.source_id))
    });
    Ok(ordered.into_iter().map(|(_, row)| row).collect())
}

fn decision_time(conn: &Connection, date: &str) -> Result<Option<(i64, String)>> {
    let mut whole = date.to_string();
    let mut fraction = "";
    if let Some((prefix, suffix)) = date
        .split_once('.')
        .filter(|(prefix, _)| prefix.len() >= 19)
    {
        let digits = suffix.bytes().take_while(u8::is_ascii_digit).count();
        if digits > 0 {
            whole = format!("{prefix}{}", &suffix[digits..]);
            fraction = suffix[..digits].trim_end_matches('0');
        }
    }
    let seconds: Option<i64> = conn.query_row("SELECT unixepoch(?1)", [&whole], |r| r.get(0))?;
    // Unknown dates sort first, with row ID breaking ties, as in SQLite.
    Ok(seconds.map(|seconds| (seconds, fraction.to_string())))
}

fn source_heading(source: &str, heading: &str, body: &str) -> String {
    let mut headings: HashSet<_> = crate::commands::narrative::arch_sections(source)
        .into_iter()
        .filter(|section| {
            section.body == body
                && (section.heading_path == heading || section.source_heading_path == heading)
        })
        .map(|section| section.source_heading_path)
        .collect();
    // Legacy rows may retain the heading-only wrapper the current parser drops.
    // Never use an ordinal (or a heading alone) to borrow a sibling's context.
    if headings.len() == 1 {
        headings.drain().next().unwrap()
    } else {
        heading.to_string()
    }
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
                a.body
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
                let source: String = r.get(5)?;
                let body: String = r.get(2)?;
                let source_heading = source_heading(&source, &heading, &body);
                Ok(Row {
                    excerpt: SourceExcerpt {
                        source_type: "arch_section",
                        source_id: id,
                        recorded_at: r.get(3)?,
                        text: arch_section_embed_text(&source_heading, &body),
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
                        text: doc_chunk_embed_text(&heading, &r.get::<_, String>(2)?),
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
                        text: task_embed_text(&title, r.get::<_, Option<String>>(2)?.as_deref()),
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
            ("2025-01-01T00:00:00.9999999999Z", "2025-01-01T00:00:01Z"),
            (
                "2025-01-01T00:00:00.0001-01:00",
                "2025-01-01T01:00:00.0002Z",
            ),
            (
                "2025-01-01T01:00:00.0001+01:00",
                "2025-01-01T00:00:00.0002Z",
            ),
        ] {
            assert!(decision_time(&conn, earlier).unwrap() < decision_time(&conn, later).unwrap());
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
    }

    #[test]
    fn heading_context_requires_matching_body_and_never_guesses_an_ambiguous_ancestor() {
        let source =
            "# Architecture\n\n## v1\n\n### Storage\n\nOne.\n\n## v2\n\n### Storage\n\nOne.";
        assert_eq!(
            source_heading(source, "Storage", "### Storage\n\nOne."),
            "Storage"
        );
        assert_eq!(
            source_heading(source, "v1 > Storage", "### Storage\n\nOne."),
            "Architecture > v1 > Storage"
        );
        assert_eq!(
            source_heading(source, "v1 > Storage", "Different stored text."),
            "v1 > Storage"
        );
        assert_eq!(
            source_heading(source, "Architecture", "# Architecture"),
            "Architecture"
        );
    }
}
