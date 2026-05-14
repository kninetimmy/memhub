use std::path::PathBuf;

#[derive(Debug)]
pub struct InitResult {
    pub repo_root: PathBuf,
    pub db_path: PathBuf,
    pub config_created: bool,
    pub gitignore_updated: bool,
    pub memhub_preexisting: bool,
    pub migrations_applied: Vec<String>,
}

pub const FACT_STALE_AFTER_DAYS: i64 = 90;

#[derive(Debug)]
pub struct Fact {
    pub id: i64,
    pub key: String,
    pub value: String,
    pub confidence: f64,
    pub source: String,
    pub verified_at: Option<String>,
    pub created_at: String,
    pub is_stale: bool,
}

#[derive(Debug)]
pub struct Decision {
    pub id: i64,
    pub title: String,
    pub rationale: String,
    pub status: String,
    pub decided_at: String,
    pub source: String,
    /// Optional natural-language paraphrase. Prepended to the embed text
    /// and the cross-encoder re-rank input so jargon-titled decisions
    /// surface for plain-English queries. See decision 72.
    pub summary: Option<String>,
}

#[derive(Debug)]
pub struct Task {
    pub id: i64,
    pub title: String,
    pub status: String,
    pub notes: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug)]
pub struct CommandRecord {
    pub id: i64,
    pub kind: String,
    pub cmdline: String,
    pub last_exit_code: Option<i64>,
    pub last_run_at: Option<String>,
    pub success_count: i64,
    pub fail_count: i64,
}

impl CommandRecord {
    pub fn confidence(&self) -> Option<f64> {
        let total = self.success_count + self.fail_count;
        if total <= 0 {
            None
        } else {
            Some(self.success_count as f64 / total as f64)
        }
    }
}

#[derive(Debug)]
pub struct GitIngestSummary {
    pub since: Option<String>,
    pub commits_seen: usize,
    pub unique_files_seen: usize,
    pub commit_file_links_seen: usize,
    pub denied_files_skipped: usize,
}

#[derive(Debug)]
pub struct FileHistoryHit {
    pub path: String,
    pub commit_sha: String,
    pub author: String,
    pub committed_at: String,
    pub message: String,
    pub change_type: String,
}

#[derive(Debug)]
pub struct DecisionSearchHit {
    pub decision_id: i64,
    pub title: String,
    pub rationale: String,
    pub decided_at: String,
    pub score: f64,
}

#[derive(Debug)]
pub enum SearchResult {
    FileHistory(FileHistoryHit),
    Decision(DecisionSearchHit),
}

#[derive(Debug)]
pub struct SearchResponse {
    pub matcher: String,
    pub query: String,
    pub results: Vec<SearchResult>,
}

#[derive(Debug)]
pub struct MarkdownSyncResult {
    pub updated_files: Vec<PathBuf>,
    pub backup_files: Vec<PathBuf>,
}

#[derive(Debug)]
pub struct RenderResult {
    pub output_dir: PathBuf,
    pub project_md_path: PathBuf,
    pub ledger_md_path: PathBuf,
    pub written_files: Vec<PathBuf>,
    pub backup_files: Vec<PathBuf>,
}

#[derive(Debug)]
pub struct PendingWriteRecord {
    pub id: i64,
    pub kind: String,
    pub payload_json: String,
    pub rationale: String,
    pub status: String,
    pub actor: String,
    pub actor_raw: String,
    pub provenance_json: String,
    pub created_at: String,
    pub reviewed_at: Option<String>,
}

#[derive(Debug)]
pub struct ReviewExpireSummary {
    pub older_than_days: i64,
    pub expired: usize,
}

#[derive(Debug)]
pub struct SessionNote {
    pub id: i64,
    pub actor: String,
    pub actor_raw: String,
    pub text: String,
    pub created_at: String,
}

#[derive(Debug, Clone, Copy)]
pub enum NarrativeKind {
    State,
    Arch,
}

impl NarrativeKind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Self::State => "state",
            Self::Arch => "arch",
        }
    }

    pub fn table(&self) -> &'static str {
        match self {
            Self::State => "project_state",
            Self::Arch => "project_arch",
        }
    }
}

#[derive(Debug)]
pub struct NarrativeEntry {
    pub id: i64,
    pub body: String,
    pub actor: String,
    pub actor_raw: String,
    pub created_at: String,
}

#[derive(Debug)]
pub struct CountByLabel {
    pub label: String,
    pub count: i64,
}

#[derive(Debug)]
pub struct TopCommandKind {
    pub kind: String,
    pub cmdline: String,
    pub success_count: i64,
    pub fail_count: i64,
    pub confidence: Option<f64>,
    pub last_run_at: Option<String>,
}

#[derive(Debug)]
pub struct RecentFactKey {
    pub key: String,
    pub verified_at: Option<String>,
    pub is_stale: bool,
}

#[derive(Debug)]
pub struct StatsSummary {
    pub project_name: String,
    pub repo_root: PathBuf,
    pub window_label: String,
    pub window_days: Option<i64>,
    pub facts: i64,
    pub stale_facts: i64,
    pub stale_ratio: Option<f64>,
    pub decisions: i64,
    pub tasks_total: i64,
    pub tasks_open: i64,
    pub commands: i64,
    pub commits: i64,
    pub files: i64,
    pub chunks: i64,
    pub pending_writes_now: i64,
    pub writes_logged_total: i64,
    pub writes_in_window: i64,
    pub writes_by_actor: Vec<CountByLabel>,
    pub writes_by_table: Vec<CountByLabel>,
    pub pending_created_in_window: i64,
    pub pending_reviewed_in_window: i64,
    pub review_rate: Option<f64>,
    pub pending_by_status: Vec<CountByLabel>,
    pub top_command_kinds: Vec<TopCommandKind>,
    pub recent_facts: Vec<RecentFactKey>,
}

#[derive(Debug)]
pub struct StatusSummary {
    pub project_name: String,
    pub repo_root: PathBuf,
    pub db_path: PathBuf,
    pub config_path: PathBuf,
    pub schema_version: String,
    pub facts: i64,
    pub stale_facts: i64,
    pub decisions: i64,
    pub tasks_open: i64,
    pub tasks_total: i64,
    pub commands: i64,
    pub commits: i64,
    pub files: i64,
    pub chunks: i64,
    pub pending_writes: i64,
    pub writes_logged: i64,
    pub deny_patterns: usize,
    pub k9_detected: bool,
    pub k9_enabled: bool,
    pub k9_agent_docs_path: String,
    pub k9_drift: Option<String>,
}
