use serde::{Deserialize, Serialize};

pub const EXPORT_VERSION: u32 = 1;

/// Top-level shape of a `memhub` export file.
///
/// Stable across schema changes: the version-tagged JSON is the durable contract,
/// while the SQLite schema may evolve. New format versions add a sibling module
/// (`v2`, ...) and translate older versions on import.
#[derive(Debug, Serialize, Deserialize)]
pub struct Export {
    pub memhub_export_version: u32,
    pub exported_at: String,
    pub exported_by: String,
    pub source_schema_version: String,
    pub project: ProjectMeta,
    pub facts: Vec<Fact>,
    pub decisions: Vec<Decision>,
    pub tasks: Vec<Task>,
    pub commands: Vec<Command>,
    pub pending_writes: Vec<PendingWrite>,
    pub writes_log: Vec<WriteLogEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct ProjectMeta {
    pub root_path_at_export: String,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Fact {
    pub id: i64,
    pub key: String,
    pub value: String,
    pub confidence: f64,
    pub source: String,
    pub verified_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Decision {
    pub id: i64,
    pub title: String,
    pub rationale: String,
    pub status: String,
    pub decided_at: String,
    pub superseded_by: Option<i64>,
    #[serde(default = "default_decision_source")]
    pub source: String,
}

fn default_decision_source() -> String {
    "user".to_string()
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Task {
    pub id: i64,
    pub title: String,
    pub status: String,
    pub notes: Option<String>,
    pub created_at: String,
    pub updated_at: String,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Command {
    pub id: i64,
    pub kind: String,
    pub cmdline: String,
    pub last_exit_code: Option<i64>,
    pub last_run_at: Option<String>,
    pub success_count: i64,
    pub fail_count: i64,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct PendingWrite {
    pub id: i64,
    pub kind: String,
    pub payload_json: String,
    pub rationale: String,
    pub status: String,
    pub actor: String,
    pub actor_raw: String,
    pub created_at: String,
    pub provenance_json: String,
    /// Added in migration 0005. `#[serde(default)]` lets exports written before
    /// this field existed import cleanly as `None`.
    #[serde(default)]
    pub reviewed_at: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct WriteLogEntry {
    pub id: i64,
    pub actor: String,
    pub table_name: String,
    pub row_id: Option<i64>,
    pub action: String,
    pub reason: Option<String>,
    pub at: String,
}
