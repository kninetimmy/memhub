//! Single-item apply for wrap-up. Requests carry data, never commands or paths.

use std::io::Read;

use rusqlite::{OptionalExtension, TransactionBehavior};

use super::*;
use crate::commands::{doc, fact, lifecycle, pending_write, task};

mod binding;
use binding::Binding;

const VERSION: u8 = 1;
const MAX_REQUEST_BYTES: u64 = 1024 * 1024;

#[derive(Clone, Copy, Debug, clap::ValueEnum)]
pub enum Host {
    Claude,
    Codex,
    Opencode,
    Antigravity,
}

impl Host {
    fn attribution(self) -> (&'static str, &'static str) {
        match self {
            Self::Claude => ("claude-code", "claude:wrap-up"),
            Self::Codex => ("codex", "codex:wrap-up"),
            Self::Opencode => ("opencode", "opencode:wrap-up"),
            Self::Antigravity => ("antigravity", "antigravity:wrap-up"),
        }
    }
}

#[derive(Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
enum Target {
    Fact {
        id: i64,
    },
    Task {
        id: i64,
    },
    Document {
        id: i64,
    },
    Backups,
    Pair {
        category: Category,
        first: SourceRef,
        second: SourceRef,
    },
}

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SourceRef {
    kind: SourceKind,
    id: i64,
}

#[derive(Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum SourceKind {
    Fact,
    Decision,
    Task,
    ArchSection,
    DocChunk,
}

impl SourceKind {
    fn name(&self) -> &'static str {
        match self {
            Self::Fact => "fact",
            Self::Decision => "decision",
            Self::Task => "task",
            Self::ArchSection => "arch_section",
            Self::DocChunk => "doc_chunk",
        }
    }
}

impl SourceRef {
    fn from_subject(subject: &Subject) -> Self {
        let (kind, id) = match subject {
            Subject::Fact { id, .. } => (SourceKind::Fact, *id),
            Subject::Decision { id, .. } => (SourceKind::Decision, *id),
            Subject::Task { id, .. } => (SourceKind::Task, *id),
            Subject::ArchitectureSection { id, .. } => (SourceKind::ArchSection, *id),
            Subject::DocumentChunk { id, .. } => (SourceKind::DocChunk, *id),
            _ => unreachable!("only similarity sources"),
        };
        Self { kind, id }
    }

    fn locator(&self) -> (&'static str, i64) {
        (self.kind.name(), self.id)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
enum Effect {
    Keep,
    Drop,
    Done,
    Archive,
    ReIngest,
    Dismiss,
    Verify,
    UpdateValue,
    Delete,
}

impl Effect {
    fn offered(self, item: &Item) -> bool {
        let name = match self {
            Self::Keep => "keep",
            Self::Drop => "drop",
            Self::Done => "done",
            Self::Archive => "archive",
            Self::ReIngest => "re_ingest",
            Self::Dismiss => "dismiss",
            Self::Verify => "verify",
            Self::UpdateValue => "update_value",
            Self::Delete => "delete",
        };
        item.actions.iter().any(|a| a.action == name)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Request {
    version: u8,
    project: String,
    candidate: String,
    target: Target,
    action: Effect,
    replacement: Option<String>,
}

#[derive(Serialize)]
pub struct WrapUpReport {
    version: u8,
    project: String,
    total: usize,
    counts: BTreeMap<Category, CategoryCount>,
    items: Vec<ReviewItem>,
    backup_warning: Option<String>,
    similarity_warning: Option<String>,
}

#[derive(Serialize)]
struct ReviewItem {
    id: String,
    category: Category,
    subjects: Vec<Subject>,
    reason: String,
    excerpts: Option<PairExcerpts>,
    offer: Offer,
}

#[derive(Serialize)]
struct Offer {
    target: Target,
    actions: Vec<Effect>,
}

impl Offer {
    fn for_item(item: &Item) -> Self {
        use Effect::*;
        let (target, actions): (_, &[Effect]) = match item.subjects.as_slice() {
            [Subject::Fact { id, .. }] => (Target::Fact { id: *id }, &[Verify, UpdateValue]),
            [Subject::Task { id, .. }] => (Target::Task { id: *id }, &[Keep, Drop, Done]),
            [Subject::Document { id, .. }] => {
                (Target::Document { id: *id }, &[Keep, Archive, ReIngest])
            }
            [Subject::BackupFiles { .. }] => (Target::Backups, &[Keep, Delete]),
            [first, second] => (
                Target::Pair {
                    category: item.category,
                    first: SourceRef::from_subject(first),
                    second: SourceRef::from_subject(second),
                },
                &[Dismiss],
            ),
            _ => unreachable!("unexpected scanner subjects"),
        };
        Self {
            target,
            actions: actions
                .iter()
                .copied()
                .filter(|a| a.offered(item))
                .collect(),
        }
    }
}

#[derive(Serialize)]
pub struct Outcome {
    candidate: String,
    action: Effect,
    #[serde(skip_serializing_if = "Option::is_none")]
    pending_id: Option<i64>,
    #[serde(skip_serializing_if = "Option::is_none")]
    removed: Option<usize>,
}

fn invalid(message: impl Into<String>) -> MemhubError {
    MemhubError::InvalidInput(message.into())
}

fn local_paths(start: &Path) -> Result<ProjectPaths> {
    // No ancestor discovery, project creation or global-store open.
    Ok(ProjectPaths::for_repo_root(&fs::canonicalize(start)?))
}

fn require_project(conn: &Connection, paths: &ProjectPaths, binding: &Binding) -> Result<()> {
    binding.require_connection(conn, paths)?;
    if db::ledger_schema(conn)?.as_deref() != Some(db::latest_schema_version()) {
        return Err(invalid(
            "wrap-up prune needs a current local database schema",
        ));
    }
    let root: Option<String> =
        conn.query_row("SELECT root_path FROM projects WHERE id = 1", [], |r| {
            r.get(0)
        })?;
    if root.and_then(|root| fs::canonicalize(root).ok()).as_ref() != Some(&paths.repo_root) {
        return Err(invalid(
            "wrap-up prune database does not belong to this project root",
        ));
    }
    Ok(())
}

pub fn scan_for_wrap_up(start: &Path) -> Result<WrapUpReport> {
    let paths = local_paths(start)?;
    let binding = Binding::read(&paths)?;
    let config = scan_config(&paths)?;
    let mut conn = Connection::open_with_flags(
        &paths.db_path,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    let tx = conn.transaction()?;
    require_project(&tx, &paths, &binding)?;
    let report = scan_in(
        &tx,
        &paths,
        &config,
        true,
        true,
        None,
        &binding.protected_files(&paths)?,
    )?;
    binding.require_connection(&tx, &paths)?;
    let items = report
        .items
        .into_iter()
        .map(|item| ReviewItem {
            offer: Offer::for_item(&item),
            id: item.id,
            category: item.category,
            subjects: item.subjects,
            reason: item.reason,
            excerpts: item.excerpts,
        })
        .collect();
    Ok(WrapUpReport {
        version: VERSION,
        project: binding.fingerprint,
        total: report.total,
        counts: report.counts,
        items,
        backup_warning: report.backup_warning,
        similarity_warning: report.similarity_warning,
    })
}

fn hex_id(value: &str, len: usize) -> bool {
    value.len() == len
        && value
            .bytes()
            .all(|c| matches!(c, b'0'..=b'9' | b'a'..=b'f'))
}

fn read_request(paths: &ProjectPaths, name: &str) -> Result<Request> {
    if !hex_id(name, 32) {
        return Err(invalid(
            "prune request name must be 32 lowercase hexadecimal characters",
        ));
    }
    let path = paths.memhub_dir.join(format!("prune-apply-{name}.json"));
    let file = open_direct_file(&path, &fs::symlink_metadata(&path)?)?;
    if file_info_from_handle(&file)?.links != 1 {
        return Err(invalid("prune request must be an unshared regular file"));
    }
    let mut bytes = Vec::new();
    file.take(MAX_REQUEST_BYTES + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_REQUEST_BYTES {
        return Err(invalid("prune request exceeds 1 MiB"));
    }
    if bytes.iter().find(|b| !b.is_ascii_whitespace()) != Some(&b'{') {
        return Err(invalid("prune request must be one JSON object"));
    }
    let request: Request = serde_json::from_slice(&bytes)?;
    if request.version != VERSION
        || !hex_id(&request.candidate, 64)
        || !hex_id(&request.project, 64)
        || (request.action == Effect::UpdateValue) != request.replacement.is_some()
    {
        return Err(invalid(
            "invalid prune request version, identifier or replacement",
        ));
    }
    Ok(request)
}

fn matches_offer(request: &Request, item: &Item) -> bool {
    item.id == request.candidate && request.action.offered(item)
}

pub fn apply(start: &Path, name: &str, host: Host) -> Result<Outcome> {
    let paths = local_paths(start)?;
    let binding = Binding::read(&paths)?;
    let request = read_request(&paths, name)?;
    if request.project != binding.fingerprint {
        return Err(invalid(
            "prune request does not match this local project/database",
        ));
    }
    let (proposal_actor, actor) = host.attribution();
    let id = request.candidate.as_str();
    let stale = || {
        invalid(format!(
            "candidate {id} is missing, changed, dismissed or does not offer this action; review again"
        ))
    };
    let config = scan_config(&paths)?;
    let mut conn = Connection::open_with_flags(
        &paths.db_path,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_NOFOLLOW,
    )?;
    conn.execute_batch("PRAGMA foreign_keys = ON; PRAGMA recursive_triggers = OFF;")?;
    let mut document = None;
    let mut pair = None;
    let mut group = None;
    // Filesystem checks and pair scoring don't need the SQLite writer lock.
    {
        let snapshot = conn.transaction()?;
        require_project(&snapshot, &paths, &binding)?;
        if dismissal::is_dismissed(&snapshot, id)? {
            return Err(stale());
        }
        match &request.target {
            Target::Document { id } => visit_documents(
                &snapshot,
                &paths.repo_root,
                Some(*id),
                request.action == Effect::ReIngest,
                &binding.protected_files(&paths)?,
                |item, state, title, bytes| {
                    if matches_offer(&request, &item)
                        && let [Subject::Document { path, .. }] = item.subjects.as_slice()
                    {
                        document = Some((state, title, path.clone(), bytes));
                    }
                },
            )?,
            Target::Pair {
                category,
                first,
                second,
            } => {
                if request.action != Effect::Dismiss || !config.retrieval.use_reranker {
                    return Err(stale());
                }
                pair = similarity::Pair::prepare_targeted(
                    &snapshot,
                    &paths.repo_root,
                    *category,
                    first.locator(),
                    second.locator(),
                    id,
                    &binding.protected_files(&paths)?,
                )?;
            }
            Target::Backups => {
                let current = backups::find_group(&paths, id, &binding.protected_files(&paths)?)?;
                if !matches_offer(&request, &current.item()) {
                    return Err(stale());
                }
                group = Some(current);
            }
            Target::Fact { .. } | Target::Task { .. } => {}
        }
    }

    binding.require_connection(&conn, &paths)?;
    let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
    require_project(&tx, &paths, &binding)?;
    if dismissal::is_dismissed(&tx, id)? {
        return Err(stale());
    }
    let mut outcome = Outcome {
        candidate: id.to_owned(),
        action: request.action,
        pending_id: None,
        removed: None,
    };
    match request.target {
        Target::Fact { id: row } => {
            let mut key = None;
            facts(
                &tx,
                config.retrieval.fact_stale_after_days,
                Some(row),
                &mut |item| {
                    if matches_offer(&request, &item)
                        && let [Subject::Fact { key: found, .. }] = item.subjects.as_slice()
                    {
                        key = Some(found.clone());
                    }
                },
            )?;
            let key = key.ok_or_else(stale)?;
            if let Some(value) = request.replacement {
                let kind: Option<String> = tx.query_row(
                    "SELECT kind FROM facts WHERE project_id = 1 AND id = ?1",
                    [row],
                    |r| r.get(0),
                )?;
                outcome.pending_id = Some(pending_write::propose_prune_fact_in_tx(
                    &tx,
                    &key,
                    &value,
                    kind.as_deref(),
                    proposal_actor,
                    actor,
                    id,
                )?);
            } else {
                fact::verify_in_tx(&tx, row, &row.to_string(), actor)?;
            }
        }
        Target::Task { id: row } => {
            let mut current = false;
            tasks(&tx, Some(row), &mut |item| {
                current = matches_offer(&request, &item)
            })?;
            if !current {
                return Err(stale());
            }
            match request.action {
                Effect::Keep => lifecycle::drop_or_keep_task_in_tx(&tx, row, None, actor)?,
                Effect::Drop => {
                    lifecycle::drop_or_keep_task_in_tx(&tx, row, Some(DROP_REASON), actor)?
                }
                Effect::Done => task::done_in_tx(&tx, row, actor)?,
                _ => return Err(stale()),
            }
        }
        Target::Document { id: row } => {
            let (state, title, path, bytes) = document.ok_or_else(stale)?;
            let current: Option<String> = tx
                .query_row(&document_query(true), [row], |r| r.get(4))
                .optional()?;
            if current.as_ref() != Some(&state) {
                return Err(stale());
            }
            match request.action {
                Effect::Keep => dismissal::record(&tx, id, actor)?,
                Effect::Archive => {
                    lifecycle::set_doc_archive_in_tx(&tx, row, Some(ARCHIVE_REASON), actor)?
                }
                Effect::ReIngest => doc::reingest_in_tx(
                    &tx,
                    row,
                    path,
                    title,
                    bytes.ok_or_else(stale)?,
                    actor,
                    config.retrieval.mode,
                )?,
                _ => return Err(stale()),
            }
        }
        Target::Pair { .. } => {
            if !pair.ok_or_else(stale)?.is_current(
                &tx,
                &paths.repo_root,
                id,
                &binding.protected_files(&paths)?,
            )? {
                return Err(stale());
            }
            dismissal::record(&tx, id, actor)?;
        }
        Target::Backups => {
            let group = group.ok_or_else(stale)?;
            match request.action {
                Effect::Keep => {
                    if !backups::group_is_current(
                        &paths,
                        &group.directory,
                        id,
                        &binding.protected_files(&paths)?,
                    )? {
                        return Err(stale());
                    }
                    dismissal::record(&tx, id, actor)?;
                }
                #[cfg(windows)]
                Effect::Delete => {
                    outcome.removed = Some(group.delete()?);
                    // The writer lock protected the dismissal check but there
                    // is no DB mutation to commit after the filesystem commit.
                    return Ok(outcome);
                }
                _ => return Err(stale()),
            }
        }
    }
    tx.commit()?;
    Ok(outcome)
}
