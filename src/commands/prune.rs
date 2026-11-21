//! Pull-based, repo-local cleanup review. Only explicit actions write.

mod backups;
pub(super) use backups::RETAIN as BACKUPS_RETENTION_COUNT;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::path::{Path, PathBuf};

use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use serde::Serialize;

use crate::config::ProjectConfig;
use crate::db::{self, ProjectPaths};
use crate::retrieval::util::sha256_hex;
use crate::{MemhubError, Result};

pub const CATEGORY_LIMIT: usize = 25;
const IDLE_TASK_DAYS: i64 = 60;

#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Category {
    StaleFact,
    IdleTask,
    OutOfDateDocument,
    SurplusBackups,
}

impl Category {
    const ALL: [Self; 4] = [
        Self::StaleFact,
        Self::IdleTask,
        Self::OutOfDateDocument,
        Self::SurplusBackups,
    ];

    pub fn heading(self) -> &'static str {
        match self {
            Self::StaleFact => "Stale facts",
            Self::IdleTask => "Idle tasks",
            Self::OutOfDateDocument => "Out-of-date documents",
            Self::SurplusBackups => "Surplus backups",
        }
    }
}

#[derive(Debug, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum Subject {
    Fact {
        id: i64,
        key: String,
    },
    Task {
        id: i64,
        title: String,
    },
    Document {
        id: i64,
        path: String,
    },
    BackupFiles {
        directory: String,
        files: Vec<String>,
    },
}

#[derive(Debug, Serialize)]
pub struct Action {
    pub action: &'static str,
    /// Exact argv including `memhub`. Never interpolate into a shell.
    pub command: Vec<String>,
    /// The one argument the human must supply, if any.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub replace: Option<&'static str>,
}

impl Action {
    pub fn new<const N: usize>(action: &'static str, args: [&str; N]) -> Self {
        Self {
            action,
            command: std::iter::once("memhub")
                .chain(args)
                .map(str::to_owned)
                .collect(),
            replace: None,
        }
    }

    fn keep(id: &str) -> Self {
        Self::new("keep", ["prune", "dismiss", id])
    }
}

#[derive(Debug, Serialize)]
pub struct Item {
    pub id: String,
    pub category: Category,
    pub subjects: Vec<Subject>,
    pub reason: String,
    pub actions: Vec<Action>,
}

#[derive(Debug, Default, Serialize)]
pub struct CategoryCount {
    pub total: usize,
    pub overflow: usize,
}

#[derive(Debug, Serialize)]
pub struct Report {
    pub total: usize,
    pub counts: BTreeMap<Category, CategoryCount>,
    pub items: Vec<Item>,
}

pub fn scan(start: &Path) -> Result<Report> {
    let paths = db::discover_paths(start)?;
    let config = ProjectConfig::load(&paths.config_path)?;
    let mut conn = Connection::open_with_flags(&paths.db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    let tx = conn.transaction()?;
    let schema = db::ledger_schema(&tx)?;
    let has_dismissals = match schema.as_deref() {
        Some(db::PRUNE_DISMISSALS_MIGRATION) => true,
        // 0026 already has all candidate columns; an older database just
        // needs a maintained open before it can participate.
        Some(db::MEMORY_LIFECYCLE_MIGRATION) => false,
        _ => {
            return Err(MemhubError::InvalidInput(format!(
                "prune scan cannot read schema {}; run `memhub upgrade` and try again",
                schema.as_deref().unwrap_or("(unknown)")
            )));
        }
    };
    scan_in(&tx, &paths, &config, has_dismissals)
}

/// Status already has a maintained open; reuse it without another open.
pub(super) fn scan_project(ctx: &db::ProjectContext) -> Result<Report> {
    let tx = ctx.conn.unchecked_transaction()?;
    scan_in(&tx, &ctx.paths, &ctx.config, true)
}

fn scan_in(
    conn: &Connection,
    paths: &ProjectPaths,
    config: &ProjectConfig,
    has_dismissals: bool,
) -> Result<Report> {
    let dismissed = if has_dismissals {
        conn.prepare("SELECT identifier FROM prune_dismissals WHERE project_id = 1")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<HashSet<_>>>()?
    } else {
        HashSet::new()
    };
    let mut report = Report {
        total: 0,
        counts: Category::ALL
            .into_iter()
            .map(|category| (category, CategoryCount::default()))
            .collect(),
        items: Vec::new(),
    };
    visit_candidates(conn, paths, config, |item| {
        if dismissed.contains(&item.id) {
            return;
        }
        let count = report
            .counts
            .get_mut(&item.category)
            .expect("all categories");
        count.total += 1;
        report.total += 1;
        if count.total <= CATEGORY_LIMIT {
            report.items.push(item);
        } else {
            count.overflow += 1;
        }
    })?;
    Ok(report)
}

pub fn dismiss(start: &Path, id: &str, actor: &str) -> Result<()> {
    let mut ctx = db::open_project(start)?;
    let tx = ctx
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut found = false;
    visit_candidates(&tx, &ctx.paths, &ctx.config, |item| found |= item.id == id)?;
    if !found {
        return Err(MemhubError::InvalidInput(format!(
            "no current prune candidate '{id}'; run `memhub prune scan` again"
        )));
    }
    if tx.execute(
        "INSERT INTO prune_dismissals(identifier) VALUES (?1)
         ON CONFLICT(project_id, identifier) DO NOTHING",
        [id],
    )? > 0
    {
        db::log_write(
            &tx,
            actor,
            "prune_dismissals",
            Some(tx.last_insert_rowid()),
            "insert",
            &format!("prune dismiss {id}"),
        )?;
    }
    tx.commit()?;
    Ok(())
}

pub fn delete_backups(start: &Path, candidate: Option<&str>) -> Result<usize> {
    backups::delete(&db::discover_paths(start)?, candidate)
}

fn visit_candidates(
    conn: &Connection,
    paths: &ProjectPaths,
    config: &ProjectConfig,
    mut visit: impl FnMut(Item),
) -> Result<()> {
    facts(conn, config.retrieval.fact_stale_after_days, &mut visit)?;
    tasks(conn, &mut visit)?;
    documents(conn, &paths.repo_root, &mut visit)?;
    for group in backups::surplus(paths)? {
        visit(group.item());
    }
    Ok(())
}

/// Fingerprint the candidate's source state, not current time, report text,
/// ordering, or thresholds. JSON supplies unambiguous field boundaries.
fn identifier(category: Category, state: impl Serialize) -> Result<String> {
    Ok(sha256_hex(&serde_json::to_vec(&(category, state))?))
}

fn facts(conn: &Connection, horizon: i64, visit: &mut impl FnMut(Item)) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT id, key, kind, verified_at,
                json_array(id, key, value, confidence, source, verified_at, created_at, kind)
         FROM facts WHERE project_id = 1 AND superseded_by IS NULL AND retired_at IS NULL
           AND (verified_at IS NULL OR julianday('now') - julianday(verified_at) > ?1)
         ORDER BY verified_at IS NOT NULL, verified_at, id",
    )?;
    let mut rows = stmt.query([horizon])?;
    while let Some(row) = rows.next()? {
        let row_id: i64 = row.get(0)?;
        let key: String = row.get(1)?;
        let kind: Option<String> = row.get(2)?;
        let verified: Option<String> = row.get(3)?;
        let state: String = row.get(4)?;
        let category = Category::StaleFact;
        let id = identifier(category, state)?;
        let ident = row_id.to_string();
        let mut update = if let Some(kind) = &kind {
            Action::new(
                "update_value",
                [
                    "fact",
                    "add",
                    &format!("--kind={kind}"),
                    "--",
                    &key,
                    "<new-value>",
                ],
            )
        } else {
            Action::new("update_value", ["fact", "add", "--", &key, "<new-value>"])
        };
        update.replace = Some("<new-value>");
        let age = verified.map_or_else(
            || "never verified".into(),
            |ts| format!("last verified {ts}"),
        );
        visit(Item {
            id,
            category,
            subjects: vec![Subject::Fact { id: row_id, key }],
            reason: format!("Past the {horizon}-day fact staleness horizon ({age})."),
            actions: vec![
                Action::new("verify", ["fact", "verify", &ident]),
                update,
                Action::new(
                    "retire",
                    [
                        "fact",
                        "retire",
                        &ident,
                        "--reason",
                        "prune review: obsolete fact",
                    ],
                ),
            ],
        });
    }
    Ok(())
}

fn tasks(conn: &Connection, visit: &mut impl FnMut(Item)) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT id, title, status, updated_at,
                json_array(id, title, status, notes, created_at, updated_at, dropped_reason,
                           pre_drop_status, pre_drop_updated_at, pre_keep_updated_at, kept_at)
         FROM tasks WHERE project_id = 1 AND status IN ('open', 'blocked')
           AND julianday('now') - julianday(updated_at) > ?1
         ORDER BY updated_at, id",
    )?;
    let mut rows = stmt.query([IDLE_TASK_DAYS])?;
    while let Some(row) = rows.next()? {
        let row_id: i64 = row.get(0)?;
        let title = row.get(1)?;
        let status: String = row.get(2)?;
        let updated: String = row.get(3)?;
        let state: String = row.get(4)?;
        let category = Category::IdleTask;
        let id = identifier(category, state)?;
        let ident = row_id.to_string();
        visit(Item {
            id,
            category,
            subjects: vec![Subject::Task { id: row_id, title }],
            reason: format!(
                "{status} task last updated {updated}, more than {IDLE_TASK_DAYS} days ago."
            ),
            actions: vec![
                Action::new("keep", ["task", "keep", &ident]),
                Action::new(
                    "drop",
                    [
                        "task",
                        "drop",
                        &ident,
                        "--reason",
                        "prune review: idle task",
                    ],
                ),
                Action::new("done", ["task", "done", &ident]),
            ],
        });
    }
    Ok(())
}

fn documents(conn: &Connection, root: &Path, visit: &mut impl FnMut(Item)) -> Result<()> {
    let mut stmt = conn.prepare(
        "SELECT id, path, title, content_hash,
                json_array(id, path, title, content_hash, byte_len, source, ingested_at)
         FROM documents WHERE project_id = 1 AND archived_at IS NULL ORDER BY id",
    )?;
    let mut rows = stmt.query([])?;
    while let Some(row) = rows.next()? {
        let row_id: i64 = row.get(0)?;
        let path: String = row.get(1)?;
        let title: String = row.get(2)?;
        let ingested_hash: String = row.get(3)?;
        let state: String = row.get(4)?;
        let on_disk = document_file(root, Path::new(&path));
        let (reason, action) = match &on_disk {
            DocumentFile::Readable(hash) if *hash == ingested_hash => continue,
            DocumentFile::Readable(_) => (
                "File content changed since ingest.",
                Action::new(
                    "re_ingest",
                    ["doc", "add", &format!("--title={title}"), "--", &path],
                ),
            ),
            other => (
                match other {
                    DocumentFile::Missing => "Ingested file is missing.",
                    _ => {
                        "Ingested file is unreadable as a regular UTF-8 document at its recorded path."
                    }
                },
                Action::new(
                    "archive",
                    [
                        "doc",
                        "archive",
                        &row_id.to_string(),
                        "--reason",
                        "prune review: unavailable document",
                    ],
                ),
            ),
        };
        let category = Category::OutOfDateDocument;
        let id = identifier(category, (state, on_disk))?;
        let actions = vec![action, Action::keep(&id)];
        visit(Item {
            id,
            category,
            subjects: vec![Subject::Document { id: row_id, path }],
            reason: reason.into(),
            actions,
        });
    }
    Ok(())
}

#[derive(Serialize)]
enum DocumentFile {
    Readable(String),
    Missing,
    Unreadable(
        Option<i32>,
        Option<String>,
        Option<(u64, Option<std::time::SystemTime>)>,
    ),
}

fn document_file(root: &Path, path: &Path) -> DocumentFile {
    // Ingestion explicitly allows external files and records their canonical
    // path. That exact path is the boundary, not a new symlink target.
    let expected = if path.is_absolute() {
        path.to_path_buf()
    } else {
        let Ok(root) = fs::canonicalize(root) else {
            return DocumentFile::Unreadable(None, None, None);
        };
        if path
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return DocumentFile::Unreadable(None, None, None);
        }
        root.join(path)
    };
    let inspect = || -> std::io::Result<(PathBuf, fs::Metadata)> {
        let actual = fs::canonicalize(&expected)?;
        let meta = fs::symlink_metadata(&expected)?;
        Ok((actual, meta))
    };
    match inspect() {
        Ok((actual, meta)) => {
            let stamp = Some((meta.len(), meta.modified().ok()));
            if actual != expected || !meta.is_file() {
                return DocumentFile::Unreadable(None, None, stamp);
            }
            match fs::read(&actual) {
                Ok(bytes) if std::str::from_utf8(&bytes).is_ok() => {
                    DocumentFile::Readable(sha256_hex(&bytes))
                }
                Ok(bytes) => DocumentFile::Unreadable(None, Some(sha256_hex(&bytes)), stamp),
                Err(e) => DocumentFile::Unreadable(e.raw_os_error(), None, stamp),
            }
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => DocumentFile::Missing,
        Err(e) => DocumentFile::Unreadable(e.raw_os_error(), None, None),
    }
}
