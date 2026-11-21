//! Pull-based, repo-local cleanup review. Only explicit actions write.

mod backups;
pub(super) use backups::RETAIN as BACKUPS_RETENTION_COUNT;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags, TransactionBehavior};
use serde::Serialize;

use crate::config::ProjectConfig;
use crate::db::{self, ProjectPaths};
use crate::fs_identity::{
    FileIdentity, entry_identity, file_info_from_handle, is_link_or_reparse_point, open_direct_file,
};
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub backup_warning: Option<String>,
}

/// File effects already planned before a particular upgrade scan.
#[derive(Default)]
pub(super) struct UpgradeFiles {
    pub removed: Vec<PathBuf>,
    pub rewritten: BTreeMap<PathBuf, String>,
}

impl UpgradeFiles {
    pub(super) fn removes(&self, path: &Path) -> bool {
        self.removed.iter().any(|removed| path.starts_with(removed))
    }
}

pub fn scan(start: &Path) -> Result<Report> {
    scan_readonly(&db::discover_paths(start)?, false, &UpgradeFiles::default())
}

pub(super) fn preview_upgrade(start: &Path, files: &mut UpgradeFiles) -> Result<Report> {
    let paths = db::discover_paths(start)?;
    let config_path = &paths.config_path;
    if fs::symlink_metadata(config_path).is_err_and(|e| e.kind() == ErrorKind::NotFound) {
        let target = paths.memhub_dir.canonicalize()?.join(db::CONFIG_FILENAME);
        if let std::collections::btree_map::Entry::Vacant(seed) = files.rewritten.entry(target) {
            let name = paths
                .repo_root
                .file_name()
                .and_then(|name| name.to_str())
                .unwrap_or("memhub");
            seed.insert(db::config_seed_text(&paths.memhub_dir, name)?);
        }
    }
    scan_readonly(&paths, true, files)
}

fn scan_readonly(paths: &ProjectPaths, preview: bool, files: &UpgradeFiles) -> Result<Report> {
    let config = scan_config(paths, files)?;
    let mut conn = Connection::open_with_flags(&paths.db_path, OpenFlags::SQLITE_OPEN_READ_ONLY)?;
    // apply_all also initializes blank databases with no migration ledger.
    if preview {
        conn = db::migrated_memory_copy(&conn)?;
    }
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
    scan_in(&tx, paths, &config, has_dismissals, files)
}

fn scan_config(paths: &ProjectPaths, files: &UpgradeFiles) -> Result<ProjectConfig> {
    // Match maintained-open precedence without materializing a live config.
    // A broken link is not an absent config and must not trigger fallback.
    let example = paths.memhub_dir.join(db::CONFIG_EXAMPLE_FILENAME);
    if !files.rewritten.is_empty()
        && let Some(text) = paths
            .config_path
            .canonicalize()
            .ok()
            .or_else(|| {
                Some(
                    paths
                        .memhub_dir
                        .canonicalize()
                        .ok()?
                        .join(db::CONFIG_FILENAME),
                )
            })
            .and_then(|path| files.rewritten.get(&path))
    {
        return Ok(toml::from_str(text)?);
    }
    for path in [&paths.config_path, &example] {
        match fs::symlink_metadata(path) {
            Ok(_) => return ProjectConfig::load(path),
            Err(err) if err.kind() == ErrorKind::NotFound => {}
            Err(err) => return Err(err.into()),
        }
    }
    let name = paths
        .repo_root
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("memhub");
    Ok(ProjectConfig::default_for_repo_name(name))
}

/// Status already has a maintained open; reuse it without another open.
pub(super) fn scan_project(ctx: &db::ProjectContext) -> Result<Report> {
    let tx = ctx.conn.unchecked_transaction()?;
    scan_in(&tx, &ctx.paths, &ctx.config, true, &UpgradeFiles::default())
}

fn scan_in(
    conn: &Connection,
    paths: &ProjectPaths,
    config: &ProjectConfig,
    has_dismissals: bool,
    files: &UpgradeFiles,
) -> Result<Report> {
    let dismissed = if has_dismissals {
        conn.prepare("SELECT identifier FROM prune_dismissals WHERE project_id = 1")?
            .query_map([], |r| r.get::<_, String>(0))?
            .collect::<rusqlite::Result<HashSet<_>>>()?
    } else {
        HashSet::new()
    };
    let mut unmatched = if files.rewritten.is_empty() {
        HashSet::new()
    } else {
        dismissed.clone()
    };
    let mut report = Report {
        total: 0,
        counts: Category::ALL
            .into_iter()
            .map(|category| (category, CategoryCount::default()))
            .collect(),
        items: Vec::new(),
        backup_warning: None,
    };
    let mut new_documents = Vec::new();
    report.backup_warning = visit_candidates(conn, paths, config, files, |candidate| {
        let item = match candidate {
            Candidate::Existing(item) if dismissed.contains(&item.id) => {
                unmatched.remove(&item.id);
                return;
            }
            Candidate::Existing(item) => item,
            Candidate::AfterFileWrite {
                path,
                before,
                old_identity,
            } => {
                unmatched.remove(&before);
                if let Some(old_identity) = old_identity {
                    unmatched.remove(&old_identity);
                }
                if let Some(path) = path {
                    new_documents.push(path);
                }
                return;
            }
        };
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
    if let Some(path) = new_documents.first()
        && !unmatched.is_empty()
    {
        // Historical identifiers are opaque digests. Only the live write
        // can establish whether the new identity matches any of them.
        return Err(MemhubError::InvalidInput(format!(
            "cannot predict whether a historical dismissal applies after writing {path}"
        )));
    }
    let count = report
        .counts
        .get_mut(&Category::OutOfDateDocument)
        .expect("all categories");
    count.total += new_documents.len();
    count.overflow = count.total.saturating_sub(CATEGORY_LIMIT);
    report.total += new_documents.len();
    Ok(report)
}

pub fn dismiss(start: &Path, id: &str, actor: &str) -> Result<()> {
    let mut ctx = db::open_project(start)?;
    let tx = ctx
        .conn
        .transaction_with_behavior(TransactionBehavior::Immediate)?;
    let mut found = false;
    let warning = visit_candidates(
        &tx,
        &ctx.paths,
        &ctx.config,
        &UpgradeFiles::default(),
        |candidate| {
            if let Candidate::Existing(item) = candidate {
                found |= item.id == id;
            }
        },
    )?;
    if !found {
        let detail = warning.map_or_else(String::new, |warning| format!("; {warning}"));
        return Err(MemhubError::InvalidInput(format!(
            "no current prune candidate '{id}'; run `memhub prune scan` again{detail}"
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
    #[cfg(not(windows))]
    {
        let _ = (start, candidate);
        Err(MemhubError::InvalidInput(backups::REPORT_ONLY.into()))
    }
    #[cfg(windows)]
    {
        let candidate = candidate.ok_or_else(|| {
            MemhubError::InvalidInput(
                "backup deletion requires --candidate <identifier>; review `memhub prune scan` first"
                    .into(),
            )
        })?;
        backups::delete(&db::discover_paths(start)?, candidate)
    }
}

enum Candidate {
    Existing(Item),
    /// The new file state cannot reuse the currently observed identifier.
    /// Counts-only preview never constructs an identifier for a future file.
    AfterFileWrite {
        path: Option<String>,
        before: String,
        old_identity: Option<String>,
    },
}

fn visit_candidates(
    conn: &Connection,
    paths: &ProjectPaths,
    config: &ProjectConfig,
    files: &UpgradeFiles,
    mut visit: impl FnMut(Candidate),
) -> Result<Option<String>> {
    let mut existing = |item| visit(Candidate::Existing(item));
    facts(conn, config.retrieval.fact_stale_after_days, &mut existing)?;
    tasks(conn, &mut existing)?;
    documents(conn, &paths.repo_root, files, &mut visit)?;
    match backups::surplus(paths, &files.removed) {
        Ok((groups, warnings)) => {
            for group in groups {
                visit(Candidate::Existing(group.item()));
            }
            Ok(backups::warning(warnings))
        }
        Err(err) => Ok(Some(format!("Surplus backups not counted: {err}"))),
    }
}

#[derive(Serialize)]
#[serde(untagged)]
enum Timestamp {
    SinceEpoch(SystemTime),
    BeforeEpoch {
        secs_before_epoch: u64,
        nanos_before_epoch: u32,
    },
}

fn timestamp(time: SystemTime) -> Timestamp {
    // Preserve the existing fingerprint for ordinary timestamps. Serde
    // only needs a different representation for dates before the epoch.
    match time.duration_since(UNIX_EPOCH) {
        Ok(_) => Timestamp::SinceEpoch(time),
        Err(err) => Timestamp::BeforeEpoch {
            secs_before_epoch: err.duration().as_secs(),
            nanos_before_epoch: err.duration().subsec_nanos(),
        },
    }
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

fn documents(
    conn: &Connection,
    root: &Path,
    files: &UpgradeFiles,
    visit: &mut impl FnMut(Candidate),
) -> Result<()> {
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
        let expected = document_path(root, Path::new(&path));
        let on_disk = expected.as_deref().map_or_else(
            || DocumentFile::Unreadable(None, None, None, None),
            |expected| document_file(expected, files),
        );
        let category = Category::OutOfDateDocument;
        let (reason, action) = match &on_disk {
            DocumentFile::AfterWrite(hash) => {
                let previous = document_file(
                    expected.as_deref().expect("projected path"),
                    &UpgradeFiles::default(),
                );
                // Even if these bytes were dismissed on the current file
                // in the past, a replacement is not that same file.
                let old_identity = if let DocumentFile::Readable { identity, .. } = &previous {
                    Some(identifier(
                        category,
                        (
                            &state,
                            DocumentFile::Readable {
                                hash: hash.clone(),
                                identity: identity.clone(),
                            },
                        ),
                    )?)
                } else {
                    None
                };
                visit(Candidate::AfterFileWrite {
                    path: (*hash != ingested_hash).then_some(path),
                    before: identifier(category, (state, previous))?,
                    old_identity,
                });
                continue;
            }
            DocumentFile::Readable { hash, .. } if *hash == ingested_hash => continue,
            DocumentFile::Readable { .. } => (
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
        let id = identifier(category, (state, on_disk))?;
        let actions = vec![action, Action::keep(&id)];
        visit(Candidate::Existing(Item {
            id,
            category,
            subjects: vec![Subject::Document { id: row_id, path }],
            reason: reason.into(),
            actions,
        }));
    }
    Ok(())
}

#[derive(Serialize)]
enum DocumentFile {
    Readable {
        hash: String,
        identity: FileIdentity,
    },
    Missing,
    Linked(
        Vec<u8>,
        Option<Vec<u8>>,
        Option<Timestamp>,
        #[serde(skip_serializing_if = "Option::is_none")] Option<FileIdentity>,
    ),
    Unreadable(
        Option<i32>,
        Option<(String, FileIdentity)>,
        Option<(u64, Option<Timestamp>)>,
        // Used when there is no content hash and identity from a read handle.
        #[serde(skip_serializing_if = "Option::is_none")] Option<FileIdentity>,
    ),
    AfterWrite(String),
}

fn document_path(root: &Path, path: &Path) -> Option<PathBuf> {
    // Ingestion explicitly allows external files and records their canonical
    // path. That exact path is the boundary, not a new symlink target.
    if path.is_absolute() {
        if path
            .components()
            .any(|c| c == std::path::Component::ParentDir)
        {
            return None;
        }
        Some(path.to_path_buf())
    } else {
        if path
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return None;
        }
        Some(fs::canonicalize(root).ok()?.join(path))
    }
}

fn document_file(expected: &Path, files: &UpgradeFiles) -> DocumentFile {
    // Inspect from the root down. If a component is a link, record the
    // entry and its link text, not metadata from the destination.
    let ancestors: Vec<_> = expected.ancestors().filter(|p| p.has_root()).collect();
    for component in ancestors.into_iter().rev() {
        if files.removes(component) {
            return DocumentFile::Missing;
        }
        if component == expected
            && let Some(text) = files.rewritten.get(component)
        {
            return DocumentFile::AfterWrite(sha256_hex(text.as_bytes()));
        }
        let meta = match fs::symlink_metadata(component) {
            Ok(meta) => meta,
            Err(e) if e.kind() == ErrorKind::NotFound => return DocumentFile::Missing,
            Err(e) => return DocumentFile::Unreadable(e.raw_os_error(), None, None, None),
        };
        let modified = meta.modified().ok().map(timestamp);
        if is_link_or_reparse_point(&meta) {
            return DocumentFile::Linked(
                component.as_os_str().as_encoded_bytes().to_vec(),
                fs::read_link(component)
                    .ok()
                    .map(|target| target.as_os_str().as_encoded_bytes().to_vec()),
                modified,
                entry_identity(component, &meta),
            );
        }
        if component == expected {
            let stamp = Some((meta.len(), modified));
            // Keep the existing exact-path check before reading an ordinary file.
            if !meta.is_file() || fs::canonicalize(expected).ok().as_deref() != Some(expected) {
                return DocumentFile::Unreadable(
                    None,
                    None,
                    stamp,
                    entry_identity(component, &meta),
                );
            }
            let mut observed_identity = None;
            let mut read = || -> std::io::Result<_> {
                use std::io::Read;

                let mut file = open_direct_file(expected, &meta)?;
                let identity = file_info_from_handle(&file)?.identity;
                if fs::canonicalize(expected)? != expected {
                    return Err(std::io::Error::other(
                        "document path changed before reading",
                    ));
                }
                observed_identity = Some(identity.clone());
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)?;
                Ok((identity, bytes))
            };
            return match read() {
                Ok((identity, bytes)) if std::str::from_utf8(&bytes).is_ok() => {
                    DocumentFile::Readable {
                        hash: sha256_hex(&bytes),
                        identity,
                    }
                }
                Ok((identity, bytes)) => DocumentFile::Unreadable(
                    None,
                    Some((sha256_hex(&bytes), identity)),
                    stamp,
                    None,
                ),
                Err(e) => DocumentFile::Unreadable(
                    e.raw_os_error(),
                    None,
                    stamp,
                    observed_identity.or_else(|| entry_identity(component, &meta)),
                ),
            };
        }
    }
    DocumentFile::Unreadable(None, None, None, None)
}

#[cfg(test)]
mod tests {
    use super::DocumentFile;
    use serde_json::{json, to_value};

    #[test]
    fn missing_entry_identity_keeps_the_prior_fingerprint_shape() {
        assert_eq!(
            to_value(DocumentFile::Linked(vec![1], Some(vec![2]), None, None)).unwrap(),
            json!({"Linked": [[1], [2], null]})
        );
        assert_eq!(
            to_value(DocumentFile::Unreadable(Some(5), None, None, None)).unwrap(),
            json!({"Unreadable": [5, null, null]})
        );
    }
}
