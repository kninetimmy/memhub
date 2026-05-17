//! `memhub doc` command surface: ingest external reference documents
//! into the per-repo store, RAG-searchable through the same hybrid
//! recall path as facts/decisions/tasks.
//!
//! Documents are deliberately OPT-IN to recall — `doc_chunk` is never in
//! the default source-type set. They are user-initiated reference
//! material (a design spec, an API contract), not durable project
//! knowledge and not an agent claim, so ingestion writes directly with
//! `source = 'user'` and no review gate.
//!
//! Trigger invariant: SQLite `recursive_triggers` is OFF (see
//! `db::open_project`), so a foreign-key cascade delete of `doc_chunks`
//! does NOT fire `doc_chunks_delete_embeddings` / `doc_chunks_fts_ad`.
//! Every code path that drops a document's chunks therefore deletes the
//! `doc_chunks` rows EXPLICITLY first (which fires those AFTER-DELETE
//! triggers) and only then touches the parent `documents` row. The
//! `ON DELETE CASCADE` FK stays as a row-integrity backstop.

use std::fs;
use std::path::Path;

use rusqlite::{OptionalExtension, Transaction, params};

use crate::Result;
use crate::db;
use crate::models::{DocChunk, Document};
use crate::sync_md;

/// Largest chunk body (in chars) before a section is soft-split on
/// paragraph boundaries. Generous enough that well-structured docs chunk
/// at their natural section headings; the split only guards against a
/// pathologically long heading-free section.
const MAX_CHUNK_CHARS: usize = 2000;

#[derive(Debug, PartialEq, Eq)]
pub enum IngestStatus {
    Created,
    Updated,
    Unchanged,
}

#[derive(Debug)]
pub struct DocAddOutcome {
    pub doc_id: i64,
    pub title: String,
    pub path: String,
    pub chunk_count: usize,
    pub status: IngestStatus,
    /// True when this call flipped `[retrieval] include_docs_in_default`
    /// on for the repo (first successful doc add). The CLI surfaces a
    /// one-line notice so the behavior change is visible, not silent.
    pub enabled_default_recall: bool,
}

/// Ingest (or re-ingest) a markdown file. Unchanged content (same
/// SHA-256) is a no-op. Changed content replaces every chunk.
pub fn add(start: &Path, file: &Path, title: Option<&str>, actor: &str) -> Result<DocAddOutcome> {
    let content = fs::read_to_string(file).map_err(|e| {
        crate::MemhubError::InvalidInput(format!("cannot read {}: {e}", file.display()))
    })?;
    let canonical = fs::canonicalize(file).unwrap_or_else(|_| file.to_path_buf());
    let path_str = canonical.to_string_lossy().into_owned();
    let byte_len = content.len() as i64;
    let content_hash = sha256_hex(&content);
    let resolved_title = title
        .map(str::to_string)
        .filter(|t| !t.trim().is_empty())
        .unwrap_or_else(|| derive_title(&content, &canonical));

    let mut ctx = db::open_project(start)?;
    let mode = ctx.config.retrieval.mode;
    let tx = ctx.conn.transaction()?;

    // "First successful doc add in this repo" — the literal trigger for
    // auto-enabling default doc recall — means the documents table was
    // empty *before* this call. Measured pre-insert so re-adds, updates,
    // and a second new doc never re-flip a setting the user turned off
    // (that escape hatch is documented in CLAUDE.md / AGENTS.md).
    let was_first_doc: bool = tx.query_row(
        "SELECT COUNT(*) FROM documents WHERE project_id = 1",
        [],
        |r| r.get::<_, i64>(0),
    )? == 0;

    let existing: Option<(i64, String)> = tx
        .query_row(
            "SELECT id, content_hash FROM documents WHERE project_id = 1 AND path = ?1",
            params![path_str],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;

    let (doc_id, status) = match existing {
        Some((id, old_hash)) if old_hash == content_hash => {
            let chunk_count: i64 = tx.query_row(
                "SELECT COUNT(*) FROM doc_chunks WHERE doc_id = ?1",
                params![id],
                |r| r.get(0),
            )?;
            tx.commit()?;
            let enabled_default_recall =
                was_first_doc && maybe_enable_default_doc_recall(&mut ctx)?;
            return Ok(DocAddOutcome {
                doc_id: id,
                title: resolved_title,
                path: path_str,
                chunk_count: chunk_count.max(0) as usize,
                status: IngestStatus::Unchanged,
                enabled_default_recall,
            });
        }
        Some((id, _)) => {
            // Explicit chunk delete first so the AFTER-DELETE triggers
            // clear embeddings + FTS (recursive_triggers is OFF).
            tx.execute("DELETE FROM doc_chunks WHERE doc_id = ?1", params![id])?;
            tx.execute(
                "UPDATE documents
                 SET title = ?1, content_hash = ?2, byte_len = ?3,
                     ingested_at = CURRENT_TIMESTAMP
                 WHERE id = ?4",
                params![resolved_title, content_hash, byte_len, id],
            )?;
            (id, IngestStatus::Updated)
        }
        None => {
            tx.execute(
                "INSERT INTO documents(project_id, path, title, content_hash, byte_len, source)
                 VALUES (1, ?1, ?2, ?3, ?4, 'user')",
                params![path_str, resolved_title, content_hash, byte_len],
            )?;
            (tx.last_insert_rowid(), IngestStatus::Created)
        }
    };

    let chunks = chunk_markdown(&content);
    insert_chunks(&tx, doc_id, &chunks, mode)?;

    db::log_write(
        &tx,
        actor,
        "documents",
        Some(doc_id),
        if status == IngestStatus::Created {
            "insert"
        } else {
            "update"
        },
        &format!("doc add: {} ({} chunks)", path_str, chunks.len()),
    )?;
    tx.commit()?;
    let enabled_default_recall = was_first_doc && maybe_enable_default_doc_recall(&mut ctx)?;
    sync_md::sync_if_enabled(start)?;

    Ok(DocAddOutcome {
        doc_id,
        title: resolved_title,
        path: path_str,
        chunk_count: chunks.len(),
        status,
        enabled_default_recall,
    })
}

/// Flip `[retrieval] include_docs_in_default` on (decision 90). Only
/// called when `was_first_doc` held — i.e. the documents table was
/// empty before this ingest — so the user-pointed write that
/// establishes the very first doc also wires up retrieval, while a
/// later `false` set by the user is never silently re-flipped by a
/// re-add or a second doc. Returns whether it changed the config.
fn maybe_enable_default_doc_recall(ctx: &mut db::ProjectContext) -> Result<bool> {
    if ctx.config.retrieval.include_docs_in_default {
        return Ok(false);
    }
    ctx.config.retrieval.include_docs_in_default = true;
    ctx.config.save(&ctx.paths.config_path)?;
    Ok(true)
}

fn insert_chunks(
    tx: &Transaction<'_>,
    doc_id: i64,
    chunks: &[(String, String)],
    mode: crate::config::RetrievalMode,
) -> Result<()> {
    for (ord, (heading_path, body)) in chunks.iter().enumerate() {
        tx.execute(
            "INSERT INTO doc_chunks(project_id, doc_id, ord, heading_path, body)
             VALUES (1, ?1, ?2, ?3, ?4)",
            params![doc_id, ord as i64, heading_path, body],
        )?;
        let chunk_id = tx.last_insert_rowid();
        let embed_text = crate::retrieval::doc_chunk_embed_text(heading_path, body);
        crate::retrieval::eager_embed_in_tx(
            tx,
            mode,
            crate::retrieval::SourceType::DocChunk,
            chunk_id,
            &embed_text,
        )?;
    }
    Ok(())
}

pub fn list(start: &Path) -> Result<Vec<Document>> {
    let ctx = db::open_project(start)?;
    let mut stmt = ctx.conn.prepare(
        "SELECT d.id, d.path, d.title, d.content_hash, d.byte_len, d.source,
                d.ingested_at,
                (SELECT COUNT(*) FROM doc_chunks c WHERE c.doc_id = d.id) AS chunk_count
         FROM documents d
         ORDER BY d.ingested_at DESC, d.id DESC",
    )?;
    let rows = stmt.query_map([], |r| {
        Ok(Document {
            id: r.get(0)?,
            path: r.get(1)?,
            title: r.get(2)?,
            content_hash: r.get(3)?,
            byte_len: r.get(4)?,
            source: r.get(5)?,
            ingested_at: r.get(6)?,
            chunk_count: r.get(7)?,
        })
    })?;
    rows.collect::<std::result::Result<Vec<_>, _>>()
        .map_err(Into::into)
}

/// Resolve a document by numeric id or by exact/canonical path.
fn resolve_doc_id(tx: &Transaction<'_>, ident: &str) -> Result<Option<i64>> {
    if let Ok(id) = ident.parse::<i64>() {
        let found: Option<i64> = tx
            .query_row("SELECT id FROM documents WHERE id = ?1", params![id], |r| {
                r.get(0)
            })
            .optional()?;
        if found.is_some() {
            return Ok(found);
        }
    }
    let canonical = fs::canonicalize(ident)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ident.to_string());
    let by_path: Option<i64> = tx
        .query_row(
            "SELECT id FROM documents WHERE path = ?1 OR path = ?2",
            params![ident, canonical],
            |r| r.get(0),
        )
        .optional()?;
    Ok(by_path)
}

/// Remove a document and all its chunks/embeddings. Returns false when
/// no document matched `ident`.
pub fn remove(start: &Path, ident: &str, actor: &str) -> Result<bool> {
    let mut ctx = db::open_project(start)?;
    let tx = ctx.conn.transaction()?;
    let Some(doc_id) = resolve_doc_id(&tx, ident)? else {
        return Ok(false);
    };
    // Explicit chunk delete first (recursive_triggers OFF) so embeddings
    // + FTS are cleaned; then the parent row.
    tx.execute("DELETE FROM doc_chunks WHERE doc_id = ?1", params![doc_id])?;
    tx.execute("DELETE FROM documents WHERE id = ?1", params![doc_id])?;
    db::log_write(
        &tx,
        actor,
        "documents",
        Some(doc_id),
        "delete",
        &format!("doc rm: {ident}"),
    )?;
    tx.commit()?;
    sync_md::sync_if_enabled(start)?;
    Ok(true)
}

pub fn show(start: &Path, ident: &str) -> Result<Option<(Document, Vec<DocChunk>)>> {
    let docs = list(start)?;
    let parsed_id = ident.parse::<i64>().ok();
    let canonical = fs::canonicalize(ident)
        .map(|p| p.to_string_lossy().into_owned())
        .unwrap_or_else(|_| ident.to_string());
    let Some(doc) = docs
        .into_iter()
        .find(|d| parsed_id == Some(d.id) || d.path == ident || d.path == canonical)
    else {
        return Ok(None);
    };

    let ctx = db::open_project(start)?;
    let mut stmt = ctx.conn.prepare(
        "SELECT id, doc_id, ord, heading_path, body, created_at
         FROM doc_chunks WHERE doc_id = ?1 ORDER BY ord",
    )?;
    let chunks = stmt
        .query_map(params![doc.id], |r| {
            Ok(DocChunk {
                id: r.get(0)?,
                doc_id: r.get(1)?,
                ord: r.get(2)?,
                heading_path: r.get(3)?,
                body: r.get(4)?,
                created_at: r.get(5)?,
            })
        })?
        .collect::<std::result::Result<Vec<_>, _>>()?;
    Ok(Some((doc, chunks)))
}

/// Count of ingested documents (for `status` / dashboards).
pub fn count(start: &Path) -> Result<i64> {
    let ctx = db::open_project(start)?;
    let n: i64 = ctx
        .conn
        .query_row("SELECT COUNT(*) FROM documents", [], |r| r.get(0))?;
    Ok(n)
}

fn derive_title(content: &str, path: &Path) -> String {
    for (heading_path, _) in chunk_markdown(content) {
        if !heading_path.trim().is_empty() {
            // First heading's leaf is the most document-like title.
            return heading_path
                .rsplit(" > ")
                .next()
                .unwrap_or(&heading_path)
                .to_string();
        }
    }
    path.file_name()
        .map(|n| n.to_string_lossy().into_owned())
        .unwrap_or_else(|| "untitled".to_string())
}

/// Split markdown into retrievable chunks.
///
/// One chunk per ATX heading section; the heading line is kept in the
/// body so the chunk is self-describing, and `heading_path` carries the
/// ancestor breadcrumb (e.g. `Components > Buttons`). Content before the
/// first heading (including YAML front matter) becomes a leading chunk
/// with an empty `heading_path`. Fenced code blocks (``` or ~~~) are
/// never split: a `#` inside a fence is body text, not a heading, and a
/// soft-split for over-long sections only breaks on blank lines outside
/// a fence.
///
/// Returns `(heading_path, body)` pairs in document order.
pub fn chunk_markdown(content: &str) -> Vec<(String, String)> {
    let mut sections: Vec<(String, Vec<String>)> = Vec::new();
    let mut stack: Vec<(usize, String)> = Vec::new();
    let mut current_path = String::new();
    let mut current_body: Vec<String> = Vec::new();
    let mut fence: Option<String> = None;

    let flush = |path: &str, body: &Vec<String>, out: &mut Vec<(String, Vec<String>)>| {
        if body.iter().any(|l| !l.trim().is_empty()) {
            out.push((path.to_string(), body.clone()));
        }
    };

    for line in content.lines() {
        let trimmed = line.trim_start();
        if let Some(open) = fence.clone() {
            current_body.push(line.to_string());
            if is_fence_token(trimmed, Some(&open)) {
                fence = None;
            }
            continue;
        }
        if is_fence_token(trimmed, None) {
            fence = Some(fence_marker(trimmed));
            current_body.push(line.to_string());
            continue;
        }
        if let Some((level, text)) = parse_heading(trimmed) {
            flush(&current_path, &current_body, &mut sections);
            current_body = vec![line.to_string()];
            while stack.last().is_some_and(|(l, _)| *l >= level) {
                stack.pop();
            }
            stack.push((level, text));
            current_path = stack
                .iter()
                .map(|(_, t)| t.as_str())
                .collect::<Vec<_>>()
                .join(" > ");
        } else {
            current_body.push(line.to_string());
        }
    }
    flush(&current_path, &current_body, &mut sections);

    let mut out = Vec::new();
    for (path, body_lines) in sections {
        let body = body_lines.join("\n");
        let trimmed_body = body.trim();
        if trimmed_body.is_empty() {
            continue;
        }
        if body.chars().count() <= MAX_CHUNK_CHARS {
            out.push((path, trimmed_body.to_string()));
        } else {
            for piece in soft_split(&body) {
                out.push((path.clone(), piece));
            }
        }
    }
    out
}

/// Greedily pack paragraphs (blank-line separated, never breaking inside
/// a fence) into pieces of at most `MAX_CHUNK_CHARS`.
fn soft_split(body: &str) -> Vec<String> {
    let mut blocks: Vec<String> = Vec::new();
    let mut cur: Vec<String> = Vec::new();
    let mut fence: Option<String> = None;
    for line in body.lines() {
        let trimmed = line.trim_start();
        if let Some(open) = fence.clone() {
            cur.push(line.to_string());
            if is_fence_token(trimmed, Some(&open)) {
                fence = None;
            }
            continue;
        }
        if is_fence_token(trimmed, None) {
            fence = Some(fence_marker(trimmed));
            cur.push(line.to_string());
            continue;
        }
        if line.trim().is_empty() {
            if !cur.is_empty() {
                blocks.push(std::mem::take(&mut cur).join("\n"));
            }
        } else {
            cur.push(line.to_string());
        }
    }
    if !cur.is_empty() {
        blocks.push(cur.join("\n"));
    }

    let mut pieces: Vec<String> = Vec::new();
    let mut acc = String::new();
    for block in blocks {
        let candidate_len = acc.chars().count() + block.chars().count() + 2;
        if !acc.is_empty() && candidate_len > MAX_CHUNK_CHARS {
            pieces.push(std::mem::take(&mut acc));
        }
        if !acc.is_empty() {
            acc.push_str("\n\n");
        }
        acc.push_str(&block);
    }
    if !acc.trim().is_empty() {
        pieces.push(acc);
    }
    pieces
}

fn parse_heading(trimmed: &str) -> Option<(usize, String)> {
    if !trimmed.starts_with('#') {
        return None;
    }
    let level = trimmed.chars().take_while(|c| *c == '#').count();
    if level == 0 || level > 6 {
        return None;
    }
    let rest = &trimmed[level..];
    // ATX requires a space after the hashes ("#foo" is not a heading).
    if !rest.starts_with(char::is_whitespace) {
        return None;
    }
    let text = rest.trim().trim_end_matches('#').trim();
    if text.is_empty() {
        return None;
    }
    Some((level, text.to_string()))
}

/// A fence token is 3+ backticks or 3+ tildes at the start of a
/// (trimmed) line. When `open` is set, only the matching marker family
/// closes the fence.
fn is_fence_token(trimmed: &str, open: Option<&str>) -> bool {
    let matches = |ch: char| trimmed.chars().take_while(|c| *c == ch).count() >= 3;
    match open {
        Some(marker) => {
            let ch = marker.chars().next().unwrap_or('`');
            matches(ch)
        }
        None => matches('`') || matches('~'),
    }
}

fn fence_marker(trimmed: &str) -> String {
    if trimmed.starts_with('~') {
        "~~~"
    } else {
        "```"
    }
    .to_string()
}

fn sha256_hex(text: &str) -> String {
    use sha2::{Digest, Sha256};
    let mut hasher = Sha256::new();
    hasher.update(text.as_bytes());
    let digest = hasher.finalize();
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(digest.len() * 2);
    for b in digest {
        out.push(HEX[(b >> 4) as usize] as char);
        out.push(HEX[(b & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::init;
    use tempfile::tempdir;

    #[test]
    fn chunker_splits_on_headings_with_breadcrumb() {
        let md =
            "# Top\n\nintro\n\n## Alpha\n\nbody a\n\n### Nested\n\ndeep\n\n## Beta\n\nbody b\n";
        let chunks = chunk_markdown(md);
        let paths: Vec<&str> = chunks.iter().map(|(p, _)| p.as_str()).collect();
        assert_eq!(
            paths,
            vec!["Top", "Top > Alpha", "Top > Alpha > Nested", "Top > Beta"]
        );
        assert!(chunks[1].1.contains("body a"));
    }

    #[test]
    fn chunker_does_not_split_inside_fences() {
        let md = "## Code\n\n```yaml\n# this is not a heading\nkey: value\n```\n\nafter\n";
        let chunks = chunk_markdown(md);
        assert_eq!(chunks.len(), 1, "fenced # must not start a new chunk");
        assert!(chunks[0].1.contains("# this is not a heading"));
        assert!(chunks[0].1.contains("after"));
    }

    #[test]
    fn chunker_keeps_preamble_as_leading_chunk() {
        let md = "---\nname: spec\n---\n\nlead prose\n\n# First\n\nbody\n";
        let chunks = chunk_markdown(md);
        assert_eq!(chunks[0].0, "", "preamble has empty heading_path");
        assert!(chunks[0].1.contains("name: spec"));
        assert_eq!(chunks[1].0, "First");
    }

    #[test]
    fn add_then_show_roundtrips_and_reingest_is_noop() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let doc = temp.path().join("spec.md");
        fs::write(&doc, "# Title\n\n## A\n\nalpha\n\n## B\n\nbeta\n").expect("write");

        let first = add(temp.path(), &doc, None, "cli:user").expect("add");
        assert_eq!(first.status, IngestStatus::Created);
        assert!(first.chunk_count >= 3);

        let again = add(temp.path(), &doc, None, "cli:user").expect("re-add");
        assert_eq!(again.status, IngestStatus::Unchanged);
        assert_eq!(again.doc_id, first.doc_id);

        fs::write(&doc, "# Title\n\n## A\n\nalpha edited\n").expect("rewrite");
        let updated = add(temp.path(), &doc, None, "cli:user").expect("update");
        assert_eq!(updated.status, IngestStatus::Updated);

        let (meta, chunks) = show(temp.path(), &first.doc_id.to_string())
            .expect("show")
            .expect("present");
        assert_eq!(meta.id, first.doc_id);
        assert!(chunks.iter().any(|c| c.body.contains("alpha edited")));
        assert!(
            !chunks.iter().any(|c| c.body.contains("beta")),
            "stale chunk from prior ingest must be gone"
        );
    }

    #[test]
    fn hybrid_reingest_and_remove_leave_no_orphan_embeddings() {
        // The load-bearing invariant: recursive_triggers is OFF, so a
        // doc's chunks (and their embeddings + FTS rows) only get
        // cleaned because the writer deletes doc_chunks EXPLICITLY
        // before the parent. This must hold in hybrid mode, where
        // eager_embed_in_tx actually writes embedding rows. FTS-mode
        // tests can't catch a regression here.
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let cfg_path = temp.path().join(".memhub/config.toml");
        let mut cfg = crate::config::ProjectConfig::load(&cfg_path).expect("load cfg");
        cfg.retrieval.mode = crate::config::RetrievalMode::Hybrid;
        cfg.save(&cfg_path).expect("save cfg");

        let counts = |label: &str| -> (i64, i64) {
            let ctx = db::open_project(temp.path()).expect("open");
            let chunks: i64 = ctx
                .conn
                .query_row("SELECT COUNT(*) FROM doc_chunks", [], |r| r.get(0))
                .expect(label);
            let embs: i64 = ctx
                .conn
                .query_row(
                    "SELECT COUNT(*) FROM embeddings WHERE source_type = 'doc_chunk'",
                    [],
                    |r| r.get(0),
                )
                .expect(label);
            (chunks, embs)
        };

        let doc = temp.path().join("spec.md");
        fs::write(
            &doc,
            "# Spec\n\n## A\n\nalpha\n\n## B\n\nbeta\n\n## C\n\ngamma\n",
        )
        .expect("write");
        let first = add(temp.path(), &doc, None, "cli:user").expect("add");
        let (c1, e1) = counts("after add");
        assert!(c1 >= 3);
        assert_eq!(c1, e1, "every chunk must have exactly one embedding");
        assert_eq!(e1 as usize, first.chunk_count);

        // Edit + re-ingest: stale chunks AND their embeddings must go.
        fs::write(&doc, "# Spec\n\n## A\n\nalpha edited\n").expect("rewrite");
        let updated = add(temp.path(), &doc, None, "cli:user").expect("re-add");
        assert_eq!(updated.status, IngestStatus::Updated);
        let (c2, e2) = counts("after re-add");
        assert_eq!(c2, e2, "no orphan embeddings after re-ingest");
        assert_eq!(e2 as usize, updated.chunk_count);
        assert!(c2 < c1, "shrunk doc has fewer chunks");

        // Remove: everything for the doc is gone.
        assert!(remove(temp.path(), &first.doc_id.to_string(), "cli:user").expect("rm"));
        let (c3, e3) = counts("after rm");
        assert_eq!((c3, e3), (0, 0), "remove clears chunks and embeddings");
    }

    #[test]
    fn remove_clears_document_and_chunks() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let doc = temp.path().join("d.md");
        fs::write(&doc, "# T\n\n## S\n\nbody\n").expect("write");
        let added = add(temp.path(), &doc, None, "cli:user").expect("add");

        assert!(remove(temp.path(), &added.doc_id.to_string(), "cli:user").expect("rm"));
        assert!(list(temp.path()).expect("list").is_empty());
        assert!(
            !remove(temp.path(), &added.doc_id.to_string(), "cli:user").expect("rm again"),
            "second remove finds nothing"
        );
    }
}
