use std::path::Path;

use rusqlite::{OptionalExtension, Transaction, params};

use crate::MemhubError;
use crate::Result;
use crate::config::RetrievalMode;
use crate::db;
use crate::models::{NarrativeEntry, NarrativeKind};
use crate::retrieval::{SourceType, arch_section_embed_text, eager_embed_batch_in_tx};

pub const MAX_BODY_LEN: usize = 65_536;
/// Advisory length for a `project_state` body: PROJECT.md renders it in
/// full, so `memhub state set` warns past this. `MAX_BODY_LEN` stays the cap.
pub const STATE_SOFT_LIMIT: usize = 4_000;
pub const DEFAULT_HISTORY_LIMIT: usize = 25;

pub fn set(
    start: &Path,
    kind: NarrativeKind,
    body: &str,
    actor: &str,
    actor_raw: &str,
) -> Result<NarrativeEntry> {
    let trimmed = body.trim();
    if trimmed.is_empty() {
        return Err(MemhubError::InvalidInput(format!(
            "{} body must not be empty",
            kind.as_str()
        )));
    }
    if trimmed.chars().count() > MAX_BODY_LEN {
        return Err(MemhubError::InvalidInput(format!(
            "{} body must be {MAX_BODY_LEN} characters or fewer",
            kind.as_str()
        )));
    }
    if actor.trim().is_empty() {
        return Err(MemhubError::InvalidInput(format!(
            "{} actor must not be empty",
            kind.as_str()
        )));
    }

    let mut ctx = db::open_project(start)?;
    let mode = ctx.config.retrieval.mode;
    let tx = ctx.conn.transaction()?;

    let insert_sql = format!(
        "INSERT INTO {}(project_id, body, actor, actor_raw)
         VALUES (1, ?1, ?2, ?3)",
        kind.table()
    );
    tx.execute(&insert_sql, params![trimmed, actor, actor_raw])?;
    let row_id = tx.last_insert_rowid();

    db::log_write(
        &tx,
        actor,
        kind.table(),
        Some(row_id),
        "insert",
        &format!("{} set", kind.as_str()),
    )?;

    let select_sql = format!(
        "SELECT id, body, actor, actor_raw, created_at
         FROM {} WHERE id = ?1",
        kind.table()
    );
    let entry = tx.query_row(&select_sql, params![row_id], row_to_entry)?;

    if matches!(kind, NarrativeKind::Arch) {
        rederive_arch_sections(&tx, mode)?;
    }

    tx.commit()?;
    Ok(entry)
}

pub fn show(start: &Path, kind: NarrativeKind) -> Result<Option<NarrativeEntry>> {
    let ctx = db::open_project(start)?;
    let sql = format!(
        "SELECT id, body, actor, actor_raw, created_at
         FROM {}
         WHERE project_id = 1
         ORDER BY created_at DESC, id DESC
         LIMIT 1",
        kind.table()
    );
    let entry = ctx.conn.query_row(&sql, [], row_to_entry).optional()?;
    Ok(entry)
}

pub fn history(start: &Path, kind: NarrativeKind, limit: usize) -> Result<Vec<NarrativeEntry>> {
    if limit == 0 {
        return Err(MemhubError::InvalidInput(format!(
            "{} history limit must be greater than zero",
            kind.as_str()
        )));
    }
    let ctx = db::open_project(start)?;
    let sql = format!(
        "SELECT id, body, actor, actor_raw, created_at
         FROM {}
         WHERE project_id = 1
         ORDER BY created_at DESC, id DESC
         LIMIT ?1",
        kind.table()
    );
    let mut stmt = ctx.conn.prepare(&sql)?;
    let rows = stmt
        .query_map(params![limit as i64], row_to_entry)?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(rows)
}

/// Replace every `arch_sections` row with the heading sections of the
/// latest `project_arch` body (issue #232). Sections are derived data:
/// only the current architecture is ever searchable. Old rows are deleted
/// explicitly so their FTS and embedding delete triggers fire without
/// relying on the `project_arch` FK cascade. `mode` gates eager embedding
/// exactly as for every other writer.
pub(crate) fn rederive_arch_sections(tx: &Transaction<'_>, mode: RetrievalMode) -> Result<()> {
    tx.execute("DELETE FROM arch_sections WHERE project_id = 1", [])?;
    let latest: Option<(i64, String)> = tx
        .query_row(
            "SELECT id, body FROM project_arch
             WHERE project_id = 1
             ORDER BY created_at DESC, id DESC
             LIMIT 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()?;
    let Some((arch_id, body)) = latest else {
        return Ok(());
    };

    let mut embed_rows: Vec<(i64, String)> = Vec::new();
    for (
        ord,
        ArchSection {
            heading_path,
            body: section,
            ..
        },
    ) in arch_sections(&body).iter().enumerate()
    {
        tx.execute(
            "INSERT INTO arch_sections(project_id, arch_id, ord, heading_path, body)
             VALUES (1, ?1, ?2, ?3, ?4)",
            params![arch_id, ord as i64, heading_path, section],
        )?;
        embed_rows.push((
            tx.last_insert_rowid(),
            arch_section_embed_text(heading_path, section),
        ));
    }
    eager_embed_batch_in_tx(tx, mode, SourceType::ArchSection, embed_rows)
}

/// One retrievable piece of the architecture body (see [`arch_sections`]).
pub(crate) struct ArchSection {
    pub heading_path: String,
    pub body: String,
    /// False for the follow-on pieces of one over-long section; the PROJECT.md
    /// index lists one entry per piece that starts a section.
    pub starts_section: bool,
}

/// Split an architecture body into sections for recall and the PROJECT.md
/// index. Chunks come from `chunk_markdown` (shared with document ingestion,
/// so it stays untouched); the architecture-specific rules live here:
///
/// - A piece starts a section when its text opens with a heading line, which
///   keeps two adjacent sections with the same heading apart from the several
///   pieces of one over-long section.
/// - A wrapper is a first heading, with nothing before it, that every other
///   heading is nested under (e.g. `# memhub architecture` over the whole
///   body). It is left out of every heading path, and dropped as a section of
///   its own when it has no text beyond the heading line.
///
/// shortcut: nesting is detected on the joined `" > "` path text, so a later
/// top-level heading literally named `"<wrapper> > x"` would pass as nested;
/// upgrade if chunk_markdown ever exposes heading levels.
pub(crate) fn arch_sections(body: &str) -> Vec<ArchSection> {
    let mut sections: Vec<ArchSection> = crate::commands::doc::chunk_markdown(body)
        .into_iter()
        .map(|(heading_path, body)| {
            let starts_section = body
                .lines()
                .next()
                .is_some_and(|l| crate::commands::doc::parse_heading(l.trim_start()).is_some());
            ArchSection {
                heading_path,
                body,
                starts_section,
            }
        })
        .collect();
    // The untitled opening has no heading line but still starts a section.
    if let Some(first) = sections.first_mut() {
        first.starts_section = true;
    }

    let Some(wrapper) = sections.first().and_then(|first| {
        let (_, text) =
            crate::commands::doc::parse_heading(first.body.lines().next()?.trim_start())?;
        (first.heading_path == text).then_some(text)
    }) else {
        return sections;
    };
    let prefix = format!("{wrapper} > ");
    let wrapper_pieces = 1 + sections[1..]
        .iter()
        .take_while(|s| !s.starts_section && s.heading_path == wrapper)
        .count();
    let rest = &sections[wrapper_pieces..];
    if rest.is_empty() || !rest.iter().all(|s| s.heading_path.starts_with(&prefix)) {
        return sections;
    }

    let heading_only = wrapper_pieces == 1
        && sections[0]
            .body
            .lines()
            .filter(|l| !l.trim().is_empty())
            .count()
            == 1;
    let mut out = Vec::with_capacity(sections.len());
    for (i, mut section) in sections.into_iter().enumerate() {
        if i < wrapper_pieces {
            if heading_only {
                continue;
            }
            section.heading_path.clear();
        } else {
            section.heading_path.drain(..prefix.len());
        }
        out.push(section);
    }
    out
}

fn row_to_entry(row: &rusqlite::Row<'_>) -> rusqlite::Result<NarrativeEntry> {
    Ok(NarrativeEntry {
        id: row.get(0)?,
        body: row.get(1)?,
        actor: row.get(2)?,
        actor_raw: row.get(3)?,
        created_at: row.get(4)?,
    })
}

#[cfg(test)]
mod tests {
    use super::arch_sections;

    fn index(body: &str) -> Vec<String> {
        arch_sections(body)
            .into_iter()
            .filter(|s| s.starts_section)
            .map(|s| s.heading_path)
            .collect()
    }

    fn paths(body: &str) -> Vec<String> {
        arch_sections(body)
            .into_iter()
            .map(|s| s.heading_path)
            .collect()
    }

    fn long_text() -> String {
        vec!["word ".repeat(300); 8].join("\n\n")
    }

    #[test]
    fn adjacent_sections_with_the_same_heading_stay_distinct() {
        let body = "## Storage\n\nOne.\n\n## Storage\n\nTwo.\n";
        assert_eq!(index(body), ["Storage", "Storage"]);
    }

    #[test]
    fn pieces_of_one_over_long_section_count_once() {
        let body = format!("## Big\n\n{}\n\n## Big\n\nTail.\n", long_text());
        assert!(paths(&body).len() > 3, "the first section splits");
        assert_eq!(index(&body), ["Big", "Big"]);
    }

    #[test]
    fn an_over_long_untitled_opening_counts_once() {
        let body = format!("{}\n\n## After\n\nTail.\n", long_text());
        assert!(paths(&body).len() > 2, "the opening splits");
        assert_eq!(index(&body), ["", "After"]);
    }

    #[test]
    fn a_heading_only_wrapper_is_left_out_of_paths_and_not_listed() {
        let body =
            "# memhub architecture\n\n## Purpose\n\nWhy.\n\n## Storage\n\nA.\n\n### Tables\n\nB.\n";
        assert_eq!(paths(body), ["Purpose", "Storage", "Storage > Tables"]);
        assert_eq!(index(body), ["Purpose", "Storage", "Storage > Tables"]);
    }

    #[test]
    fn a_wrapper_with_its_own_text_stays_as_an_untitled_section() {
        let body = "# memhub architecture\n\nIntro text.\n\n## Purpose\n\nWhy.\n";
        assert_eq!(paths(body), ["", "Purpose"]);
    }

    #[test]
    fn no_wrapper_when_a_heading_sits_outside_the_first_one() {
        let two_tops = "# One\n\n## A\n\nx\n\n# Two\n\n## B\n\ny\n";
        assert_eq!(paths(two_tops), ["One", "One > A", "Two", "Two > B"]);
        let opening = "Opening.\n\n# Overview\n\n## Storage\n\nx\n";
        assert_eq!(paths(opening), ["", "Overview", "Overview > Storage"]);
        assert_eq!(paths("# Only\n\nJust text.\n"), ["Only"]);
    }
}
