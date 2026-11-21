//! Exhaustive, report-only overlap checks using the bundled compact reranker.

use std::collections::HashSet;

use rusqlite::Connection;

use super::{Action, Category, CorrectionExcerpts, Item, SourceExcerpt, Subject, identifier};
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

pub(super) fn visit(
    conn: &Connection,
    old: Option<(&str, i64)>,
    visit: &mut impl FnMut(Item),
) -> Result<()> {
    let facts = facts(conn)?;
    let decisions = decisions(conn)?;
    if old.is_none() {
        pairs(&facts, Category::DuplicateFacts, visit)?;
        pairs(&decisions, Category::UnlinkedDecisions, visit)?;
    }
    let mut targets: Vec<&Row> = facts.iter().filter(|row| row.active).collect();
    let other_targets = correction_targets(conn)?;
    targets.extend(&other_targets);
    // ponytail: exhaustive pair scoring; use a measured candidate index if
    // corpus size makes this manual scan too slow, not a silent candidate cap.
    for source in facts.iter().chain(&decisions).filter(|row| !row.active) {
        if old.is_some_and(|old| old != (source.excerpt.source_type, source.excerpt.source_id)) {
            continue;
        }
        let replacements = replacements(source, &facts);
        let candidates: Vec<_> = targets
            .iter()
            .copied()
            .filter(|target| {
                source.excerpt.source_type != target.excerpt.source_type
                    || (source.excerpt.source_id != target.excerpt.source_id
                        && !replacements.contains(&target.excerpt.source_id))
            })
            .collect();
        for target in matches(&source.excerpt.text, &candidates)? {
            let category = Category::CorrectionSweep;
            let id = identifier(category, (&source.state, &target.state))?;
            let mut actions = correction_actions(target);
            actions.push(Action::keep(&id));
            visit(Item {
                id,
                category,
                subjects: vec![source.subject.clone(), target.subject.clone()],
                reason: "Textual overlap with an old claim; not evidence of contradiction. Check version qualifiers before choosing keep or correcting the row.".into(),
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

fn matches<'a>(query: &str, candidates: &[&'a Row]) -> Result<Vec<&'a Row>> {
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

fn pairs(rows: &[Row], category: Category, visit: &mut impl FnMut(Item)) -> Result<()> {
    let active: Vec<_> = rows.iter().filter(|row| row.active).collect();
    for (i, later) in active.iter().enumerate() {
        let earlier: Vec<_> = active[..i]
            .iter()
            .copied()
            .filter(|earlier| match (&earlier.subject, &later.subject) {
                (Subject::Fact { key: a, .. }, Subject::Fact { key: b, .. }) => a != b,
                _ => true,
            })
            .collect();
        for earlier in matches(&later.excerpt.text, &earlier)? {
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
            // Editing is human work, not an invented memhub edit verb.
            vec![Action::new(
                "edit_and_re_ingest",
                ["doc", "add", &format!("--title={title}"), "--", path],
            )]
        }
        Subject::ArchitectureSection { .. } => {
            let mut action = Action::new(
                "rewrite",
                ["arch", "set", "--", "<complete-rewritten-architecture>"],
            );
            action.replace = Some("<complete-rewritten-architecture>");
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
            })
        })?
        .collect::<rusqlite::Result<_>>()?)
}

fn correction_targets(conn: &Connection) -> Result<Vec<Row>> {
    let mut out = Vec::new();
    let mut stmt = conn.prepare(
        "SELECT s.id, s.heading_path, s.body, a.created_at,
                json_array('arch_section', s.id, s.arch_id, s.ord, s.heading_path, s.body,
                           s.created_at, a.body, a.actor, a.actor_raw, a.created_at)
         FROM arch_sections s JOIN project_arch a ON a.id = s.arch_id
         WHERE s.project_id = 1 AND a.id = (SELECT max(id) FROM project_arch WHERE project_id = 1)
         ORDER BY s.id",
    )?;
    for row in stmt.query_map([], |r| {
        let id = r.get(0)?;
        let heading: String = r.get(1)?;
        Ok(Row {
            excerpt: SourceExcerpt {
                source_type: "arch_section",
                source_id: id,
                recorded_at: r.get(3)?,
                text: arch_section_embed_text(&heading, &r.get::<_, String>(2)?),
            },
            subject: Subject::ArchitectureSection { id, heading },
            state: r.get(4)?,
            active: true,
            replacement: None,
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
        })
    })? {
        out.push(row?);
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
        })
    })? {
        out.push(row?);
    }
    Ok(out)
}
