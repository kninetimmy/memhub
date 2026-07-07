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
    /// Added after the initial v1 schema. `#[serde(default)]` lets older
    /// exports that predate these fields import cleanly as empty arrays.
    #[serde(default)]
    pub session_notes: Vec<SessionNote>,
    #[serde(default)]
    pub project_state: Vec<NarrativeEntry>,
    #[serde(default)]
    pub project_arch: Vec<NarrativeEntry>,
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
    /// Added in migration 0018 (Wave 3 L3). `#[serde(default)]` keeps
    /// exports written before facts could be superseded importable cleanly
    /// as `None`. Mirrors `Decision::superseded_by` so a superseded fact's
    /// demote-with-link survives cross-machine transfer, not just decisions.
    #[serde(default)]
    pub superseded_by: Option<i64>,
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
    /// Optional natural-language paraphrase. Added in migration 0011
    /// (decision 72 / task #23). `#[serde(default)]` keeps older exports
    /// importable cleanly.
    #[serde(default)]
    pub summary: Option<String>,
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

#[derive(Debug, Serialize, Deserialize)]
pub struct SessionNote {
    pub id: i64,
    pub actor: String,
    pub actor_raw: String,
    pub text: String,
    pub created_at: String,
}

/// Shared shape for `project_state` and `project_arch`. Both tables hold
/// append-only narrative history with the same columns.
#[derive(Debug, Serialize, Deserialize)]
pub struct NarrativeEntry {
    pub id: i64,
    pub body: String,
    pub actor: String,
    pub actor_raw: String,
    pub created_at: String,
}
