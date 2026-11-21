//! Exhaustive, report-only overlap checks using the bundled compact reranker.

use std::collections::HashSet;
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
    visit_matching(conn, root, old, None, true, visit)
}

/// Score only the requested pair before taking the writer lock; at commit,
/// revalidate its exact source states and eligibility without running the model.
pub(super) fn contains(conn: &Connection, root: &Path, id: &str, score: bool) -> Result<bool> {
    let mut found = false;
    visit_matching(conn, root, None, Some(id), score, &mut |_| found = true)?;
    Ok(found)
}

fn visit_matching(
    conn: &Connection,
    root: &Path,
    old: Option<(&str, i64)>,
    requested: Option<&str>,
    score: bool,
    visit: &mut impl FnMut(Item),
) -> Result<()> {
    let facts = facts(conn)?;
    let decisions = decisions(conn)?;
    if old.is_none() {
        pairs(&facts, Category::DuplicateFacts, requested, score, visit)?;
        pairs(
            &decisions,
            Category::UnlinkedDecisions,
            requested,
            score,
            visit,
        )?;
    }
    let mut targets: Vec<&Row> = facts.iter().filter(|row| row.active).collect();
    let other_targets = correction_targets(conn, root)?;
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
        for target in matches(&source.excerpt.text, &candidates, score)? {
            let id = identifier(category, (&source.state, &target.state))?;
            let mut actions = correction_actions(target);
            let mut reason = "Textual overlap with an old claim; not evidence of contradiction. Check version qualifiers before choosing keep or correcting the row.".to_string();
            if matches!(&target.subject, Subject::DocumentChunk { .. }) && actions.is_empty() {
                reason.push_str(" Edit and re-ingest unavailable: the recorded path is missing, unreadable, or redirected. Restore a regular UTF-8 file at that exact path before rescanning; the excerpt is the stored chunk, not redirected content.");
            }
            actions.push(Action::keep(&id));
            visit(Item {
                id,
                category,
                subjects: vec![source.subject.clone(), target.subject.clone()],
                reason,
                actions,
                excerpts: Some(CorrectionExcerpts {
                    old_claim: source.excerpt.clone(),
                    possible_restatement: target.excerpt.clone(),
                }),
            });
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

fn matches<'a>(query: &str, candidates: &[&'a Row], score: bool) -> Result<Vec<&'a Row>> {
    if !score || candidates.is_empty() {
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
    score: bool,
    visit: &mut impl FnMut(Item),
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
        for earlier in matches(&later.excerpt.text, &earlier_rows, score)? {
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
            });
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

fn facts(conn: &Connection) -> Result<Vec<Row>> {
    let mut stmt = conn.prepare(
        "SELECT id, key, value, created_at, superseded_by, retired_at,
                json_array('fact', id, key, value, confidence, source, verified_at, created_at,
                           kind, superseded_by, retired_at, retired_reason)
         FROM facts WHERE project_id = 1 ORDER BY id",
    )?;
    Ok(stmt
        .query_map([], |r| {
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

fn decisions(conn: &Connection) -> Result<Vec<Row>> {
    let mut stmt = conn.prepare(
        "SELECT id, title, rationale, summary, decided_at, superseded_by, status,
                json_array('decision', id, title, rationale, summary, decided_at, source,
                           status, superseded_by)
         FROM decisions WHERE project_id = 1
           AND (status IN ('active', 'superseded') OR superseded_by IS NOT NULL)
         ORDER BY julianday(decided_at), id",
    )?;
    Ok(stmt
        .query_map([], |r| {
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
        .collect::<rusqlite::Result<_>>()?)
}

fn correction_targets(conn: &Connection, root: &Path) -> Result<Vec<Row>> {
    let mut out = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT s.id, s.heading_path, s.body, a.created_at,
                json_array('arch_section', s.id, s.arch_id, s.ord, s.heading_path, s.body,
                           s.created_at, a.body, a.actor, a.actor_raw, a.created_at),
                a.body, s.ord
         FROM arch_sections s JOIN project_arch a ON a.id = s.arch_id
         WHERE s.project_id = 1 AND a.id = (
             SELECT id FROM project_arch WHERE project_id = 1
             ORDER BY created_at DESC, id DESC LIMIT 1)
         ORDER BY s.id",
    )?;
    for row in stmt.query_map([], |r| {
        let id = r.get(0)?;
        let heading: String = r.get(1)?;
        let source: String = r.get(5)?;
        let ord: usize = r.get(6)?;
        let sections = crate::commands::narrative::arch_sections(&source);
        let source_heading = sections.get(ord).map_or(heading.as_str(), |section| {
            section.source_heading_path.as_str()
        });
        Ok(Row {
            excerpt: SourceExcerpt {
                source_type: "arch_section",
                source_id: id,
                recorded_at: r.get(3)?,
                text: arch_section_embed_text(source_heading, &r.get::<_, String>(2)?),
            },
            subject: Subject::ArchitectureSection { id, heading },
            state: r.get(4)?,
            active: true,
            replacement: None,
            document_file: None,
        })
    })? {
        out.push(row?);
    }
    let mut stmt = conn.prepare(
        "SELECT c.id, c.heading_path, c.body, c.created_at, d.id, d.path, d.title,
                json_array('doc_chunk', c.id, c.doc_id, c.ord, c.heading_path, c.body, c.created_at,
                           d.path, d.title, d.content_hash, d.byte_len, d.source, d.ingested_at,
                           d.archived_at, d.archived_reason)
         FROM doc_chunks c JOIN documents d ON d.id = c.doc_id
         WHERE c.project_id = 1 AND d.project_id = 1 AND d.archived_at IS NULL ORDER BY c.id",
    )?;
    for row in stmt.query_map([], |r| {
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
    })? {
        let mut row = row?;
        if let Subject::DocumentChunk { path, .. } = &row.subject {
            let on_disk = document_file(root, Path::new(path));
            row.state = serde_json::to_string(&(&row.state, &on_disk))?;
            row.document_file = Some(on_disk);
        }
        out.push(row);
    }
    let mut stmt = conn.prepare(
        "SELECT id, title, notes, created_at,
                json_array('task', id, title, notes, status, created_at, updated_at, dropped_reason,
                           pre_drop_status, pre_drop_updated_at, pre_keep_updated_at, kept_at)
         FROM tasks WHERE project_id = 1 AND status = 'open' ORDER BY id",
    )?;
    for row in stmt.query_map([], |r| {
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
    })? {
        out.push(row?);
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rusqlite::TransactionBehavior;

    #[test]
    fn dismissal_scores_in_a_read_snapshot_then_revalidates_current_state_without_inference() {
        let temp = tempfile::tempdir().unwrap();
        crate::commands::init::run(temp.path()).unwrap();
        let paths = crate::db::discover_paths(temp.path()).unwrap();
        let mut reader = Connection::open(&paths.db_path).unwrap();
        reader
            .execute_batch(
                "INSERT INTO facts(key,value) VALUES
             ('datastore','We will run a Postgres server to hold project memory.'),
             ('database','We will run a Postgres server to hold project memory.'),
             ('animal','Cats are domesticated felines kept as household pets.');",
            )
            .unwrap();
        let rows = facts(&reader).unwrap();
        let duplicate =
            identifier(Category::DuplicateFacts, (&rows[0].state, &rows[1].state)).unwrap();
        let unrelated =
            identifier(Category::DuplicateFacts, (&rows[0].state, &rows[2].state)).unwrap();
        let snapshot = reader.transaction().unwrap();
        // Establish the read snapshot before another connection changes a source.
        assert!(contains(&snapshot, temp.path(), &duplicate, false).unwrap());
        let mut writer = Connection::open(&paths.db_path).unwrap();
        let write = writer
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        // Model scoring needs no writer lock, and only the requested pair is scored.
        assert!(contains(&snapshot, temp.path(), &duplicate, true).unwrap());
        assert!(!contains(&snapshot, temp.path(), &unrelated, true).unwrap());
        write
            .execute("UPDATE facts SET value='new value' WHERE id=2", [])
            .unwrap();
        write.commit().unwrap();
        assert!(contains(&snapshot, temp.path(), &duplicate, false).unwrap());
        drop(snapshot);
        let commit = reader
            .transaction_with_behavior(TransactionBehavior::Immediate)
            .unwrap();
        assert!(!contains(&commit, temp.path(), &duplicate, false).unwrap());
        assert!(!contains(&commit, temp.path(), "not-a-candidate", false).unwrap());
        let rows = facts(&commit).unwrap();
        let current =
            identifier(Category::DuplicateFacts, (&rows[0].state, &rows[1].state)).unwrap();
        assert!(contains(&commit, temp.path(), &current, false).unwrap());
    }
}
