//! Pull-based, repo-local cleanup review. Only explicit actions write.

mod backups;
mod dismissal;
mod similarity;
pub(super) use backups::RETAIN as BACKUPS_RETENTION_COUNT;
pub use dismissal::dismiss;

use std::collections::{BTreeMap, HashSet};
use std::fs;
use std::io::ErrorKind;
use std::path::Path;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{Connection, OpenFlags};
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
    DuplicateFacts,
    UnlinkedDecisions,
    CorrectionSweep,
}

impl Category {
    const ALL: [Self; 4] = [
        Self::StaleFact,
        Self::IdleTask,
        Self::OutOfDateDocument,
        Self::SurplusBackups,
    ];
    const SIMILARITY: [Self; 3] = [
        Self::DuplicateFacts,
        Self::UnlinkedDecisions,
        Self::CorrectionSweep,
    ];

    pub fn heading(self) -> &'static str {
        match self {
            Self::StaleFact => "Stale facts",
            Self::IdleTask => "Idle tasks",
            Self::OutOfDateDocument => "Out-of-date documents",
            Self::SurplusBackups => "Surplus backups",
            Self::DuplicateFacts => "Near-duplicate facts",
            Self::UnlinkedDecisions => "Unlinked decisions",
            Self::CorrectionSweep => "Correction sweep",
        }
    }
}

#[derive(Debug, Clone, Serialize)]
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
    Decision {
        id: i64,
        title: String,
    },
    ArchitectureSection {
        id: i64,
        heading: String,
    },
    DocumentChunk {
        id: i64,
        document_id: i64,
        path: String,
        title: String,
        heading: String,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub excerpts: Option<CorrectionExcerpts>,
}

#[derive(Debug, Clone, Serialize)]
pub struct SourceExcerpt {
    pub source_type: &'static str,
    pub source_id: i64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub recorded_at: Option<String>,
    /// Stored text, not a model-truncated or paraphrased snippet.
    pub text: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub architecture_source: Option<ArchitectureSource>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ArchitectureSource {
    pub architecture_id: i64,
    /// Zero-based ordinal of the matched stored section, including legacy layouts.
    pub section_ordinal: i64,
    /// Original architecture Markdown, not a reconstructed heading breadcrumb.
    pub text: String,
}

#[derive(Debug, Serialize)]
pub struct CorrectionExcerpts {
    pub old_claim: SourceExcerpt,
    pub possible_restatement: SourceExcerpt,
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
    #[serde(skip_serializing_if = "Option::is_none")]
    pub similarity_warning: Option<String>,
}

pub fn scan(start: &Path) -> Result<Report> {
    scan_readonly(start, None)
}

/// Report only the named old row's correction sweep after a committed change.
/// Uses the same read-only open as scan: no maintenance, expiry or query logs.
pub fn correction_sweep(start: &Path, source_type: &str, id: i64) -> Result<Report> {
    scan_readonly(start, Some((source_type, id)))
}

fn scan_readonly(start: &Path, old: Option<(&str, i64)>) -> Result<Report> {
    let paths = db::discover_paths(start)?;
    let config = scan_config(&paths)?;
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
    scan_in(&tx, &paths, &config, has_dismissals, true, old)
}

fn scan_config(paths: &ProjectPaths) -> Result<ProjectConfig> {
    // Match maintained-open precedence without materializing a live config.
    // A broken link is not an absent config and must not trigger fallback.
    let example = paths.memhub_dir.join(db::CONFIG_EXAMPLE_FILENAME);
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
    scan_in(&tx, &ctx.paths, &ctx.config, true, false, None)
}

fn scan_in(
    conn: &Connection,
    paths: &ProjectPaths,
    config: &ProjectConfig,
    has_dismissals: bool,
    include_similarity: bool,
    old: Option<(&str, i64)>,
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
            .filter(|_| old.is_none())
            .chain(Category::SIMILARITY.into_iter().filter(|category| {
                include_similarity && (old.is_none() || *category == Category::CorrectionSweep)
            }))
            .map(|category| (category, CategoryCount::default()))
            .collect(),
        items: Vec::new(),
        backup_warning: None,
        similarity_warning: None,
    };
    let mut visit = |item: Item| {
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
    };
    let backup_warning = if old.is_none() {
        visit_candidates(conn, paths, config, &mut visit)?
    } else {
        None
    };
    if include_similarity && config.retrieval.use_reranker {
        similarity::visit(conn, &paths.repo_root, old, &mut visit)?;
    }
    report.backup_warning = backup_warning;
    if include_similarity && !config.retrieval.use_reranker {
        report.similarity_warning =
            Some("Similarity checks skipped: [retrieval] use_reranker is disabled.".into());
    }
    Ok(report)
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

fn visit_candidates(
    conn: &Connection,
    paths: &ProjectPaths,
    config: &ProjectConfig,
    mut visit: impl FnMut(Item),
) -> Result<Option<String>> {
    facts(
        conn,
        config.retrieval.fact_stale_after_days,
        None,
        &mut visit,
    )?;
    tasks(conn, None, &mut visit)?;
    documents(conn, &paths.repo_root, None, &mut visit)?;
    match backups::surplus(paths) {
        Ok((groups, warnings)) => {
            for group in groups {
                visit(group.item());
            }
            Ok(backups::warning(warnings))
        }
        Err(err) => Ok(Some(format!("Surplus backups not counted: {err}"))),
    }
}

#[derive(Clone, Serialize)]
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

fn facts(
    conn: &Connection,
    horizon: i64,
    selected: Option<i64>,
    visit: &mut dyn FnMut(Item),
) -> Result<()> {
    let filter = if selected.is_some() {
        "AND id = ?2"
    } else {
        ""
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT id, key, kind, verified_at,
                json_array(id, key, value, confidence, source, verified_at, created_at, kind)
         FROM facts WHERE project_id = 1 AND superseded_by IS NULL AND retired_at IS NULL
           AND (verified_at IS NULL OR julianday('now') - julianday(verified_at) > ?1)
           {filter}
         ORDER BY verified_at IS NOT NULL, verified_at, id",
    ))?;
    let mut rows = stmt.query(rusqlite::params_from_iter(
        [horizon].into_iter().chain(selected),
    ))?;
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
            excerpts: None,
        });
    }
    Ok(())
}

fn tasks(conn: &Connection, selected: Option<i64>, visit: &mut dyn FnMut(Item)) -> Result<()> {
    let filter = if selected.is_some() {
        "AND id = ?2"
    } else {
        ""
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT id, title, status, updated_at,
                json_array(id, title, status, notes, created_at, updated_at, dropped_reason,
                           pre_drop_status, pre_drop_updated_at, pre_keep_updated_at, kept_at)
         FROM tasks WHERE project_id = 1 AND status IN ('open', 'blocked')
           AND julianday('now') - julianday(updated_at) > ?1
           {filter}
         ORDER BY updated_at, id",
    ))?;
    let mut rows = stmt.query(rusqlite::params_from_iter(
        [IDLE_TASK_DAYS].into_iter().chain(selected),
    ))?;
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
            excerpts: None,
        });
    }
    Ok(())
}

fn documents(
    conn: &Connection,
    root: &Path,
    selected: Option<i64>,
    visit: &mut dyn FnMut(Item),
) -> Result<()> {
    let filter = if selected.is_some() {
        "AND id = ?1"
    } else {
        ""
    };
    let mut stmt = conn.prepare(&format!(
        "SELECT id, path, title, content_hash,
                json_array(id, path, title, content_hash, byte_len, source, ingested_at)
         FROM documents WHERE project_id = 1 AND archived_at IS NULL {filter} ORDER BY id",
    ))?;
    let mut rows = stmt.query(rusqlite::params_from_iter(selected))?;
    while let Some(row) = rows.next()? {
        let row_id: i64 = row.get(0)?;
        let path: String = row.get(1)?;
        let title: String = row.get(2)?;
        let ingested_hash: String = row.get(3)?;
        let state: String = row.get(4)?;
        let on_disk = document_file(root, Path::new(&path));
        let (reason, action) = match &on_disk {
            DocumentFile::Readable { hash, .. } if *hash == ingested_hash => continue,
            DocumentFile::Readable { .. } => {
                let reason = "File content changed since ingest.";
                match re_ingest_action("re_ingest", &on_disk, &path, &title) {
                    Ok(action) => (reason.into(), Some(action)),
                    Err(why) => (format!("{reason} Re-ingest unavailable: {why}"), None),
                }
            }
            other => (
                match other {
                    DocumentFile::Missing => "Ingested file is missing.",
                    _ => {
                        "Ingested file is unreadable as a regular UTF-8 document at its recorded path."
                    }
                }.into(),
                Some(Action::new(
                    "archive",
                    [
                        "doc",
                        "archive",
                        &row_id.to_string(),
                        "--reason",
                        "prune review: unavailable document",
                    ],
                )),
            ),
        };
        let category = Category::OutOfDateDocument;
        let id = identifier(category, (state, on_disk))?;
        let actions = action.into_iter().chain([Action::keep(&id)]).collect();
        visit(Item {
            id,
            category,
            subjects: vec![Subject::Document { id: row_id, path }],
            reason,
            actions,
            excerpts: None,
        });
    }
    Ok(())
}

fn re_ingest_action(
    action: &'static str,
    file: &DocumentFile,
    recorded: &str,
    title: &str,
) -> std::result::Result<Action, &'static str> {
    let path = Path::new(recorded);
    if !path.is_absolute() {
        return Err(
            "the stored path is relative; `doc add` resolves against the caller's directory and uses a canonical path as its row key.",
        );
    }
    if !matches!(file, DocumentFile::Readable { .. }) {
        return Err(
            "the recorded path is missing, unreadable, or redirected. Restore a regular UTF-8 file at that path and rescan; review evidence is from the stored row.",
        );
    }
    // Ingest looks up the canonical path by exact SQL string equality, not
    // filesystem identity or Path equality (which ignores some spelling).
    if !matches!(
        file,
        DocumentFile::Readable {
            ingest_key: true,
            ..
        }
    ) {
        return Err(
            "the recorded path does not match the canonical row key `doc add` would use. Review the stored row before re-ingesting.",
        );
    }
    Ok(Action::new(
        action,
        ["doc", "add", &format!("--title={title}"), "--", recorded],
    ))
}

#[derive(Clone, Serialize)]
enum DocumentFile {
    Readable {
        hash: String,
        identity: FileIdentity,
        // Action eligibility is not part of the on-disk fingerprint.
        #[serde(skip)]
        ingest_key: bool,
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
}

fn document_file(root: &Path, path: &Path) -> DocumentFile {
    // Ingestion explicitly allows external files and records their canonical
    // path. That exact path is the boundary, not a new symlink target.
    let expected = if path.is_absolute() {
        if path
            .components()
            .any(|c| c == std::path::Component::ParentDir)
        {
            return DocumentFile::Unreadable(None, None, None, None);
        }
        path.to_path_buf()
    } else {
        let Ok(root) = fs::canonicalize(root) else {
            return DocumentFile::Unreadable(None, None, None, None);
        };
        if path
            .components()
            .any(|c| !matches!(c, std::path::Component::Normal(_)))
        {
            return DocumentFile::Unreadable(None, None, None, None);
        }
        root.join(path)
    };
    // Inspect from the root down. If a component is a link, record the
    // entry and its link text, not metadata from the destination.
    let ancestors: Vec<_> = expected.ancestors().filter(|p| p.has_root()).collect();
    for component in ancestors.into_iter().rev() {
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
            if !meta.is_file() || fs::canonicalize(&expected).ok().as_ref() != Some(&expected) {
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

                let mut file = open_direct_file(&expected, &meta)?;
                let identity = file_info_from_handle(&file)?.identity;
                let canonical = fs::canonicalize(&expected)?;
                if canonical != expected {
                    return Err(std::io::Error::other(
                        "document path changed before reading",
                    ));
                }
                let ingest_key = canonical.as_os_str() == path.as_os_str();
                observed_identity = Some(identity.clone());
                let mut bytes = Vec::new();
                file.read_to_end(&mut bytes)?;
                Ok((identity, bytes, ingest_key))
            };
            return match read() {
                Ok((identity, bytes, ingest_key)) if std::str::from_utf8(&bytes).is_ok() => {
                    DocumentFile::Readable {
                        hash: sha256_hex(&bytes),
                        identity,
                        ingest_key,
                    }
                }
                Ok((identity, bytes, _)) => DocumentFile::Unreadable(
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
