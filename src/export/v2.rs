use serde::{Deserialize, Serialize};

use super::v1;

pub const EXPORT_VERSION: u32 = 2;

/// V2 keeps the v1 record layout, adding lifecycle state to facts and tasks.
#[derive(Debug, Serialize, Deserialize)]
pub struct Export {
    pub memhub_export_version: u32,
    pub exported_at: String,
    pub exported_by: String,
    pub source_schema_version: String,
    pub project: v1::ProjectMeta,
    pub facts: Vec<Fact>,
    pub decisions: Vec<v1::Decision>,
    pub tasks: Vec<Task>,
    pub commands: Vec<v1::Command>,
    pub pending_writes: Vec<v1::PendingWrite>,
    pub writes_log: Vec<v1::WriteLogEntry>,
    #[serde(default)]
    pub session_notes: Vec<v1::SessionNote>,
    #[serde(default)]
    pub project_state: Vec<v1::NarrativeEntry>,
    #[serde(default)]
    pub project_arch: Vec<v1::NarrativeEntry>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Fact {
    #[serde(flatten)]
    pub base: v1::Fact,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retired_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub retired_reason: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Task {
    #[serde(flatten)]
    pub base: v1::Task,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub dropped_reason: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_drop_status: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_drop_updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub pre_keep_updated_at: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub kept_at: Option<String>,
}

impl From<v1::Export> for Export {
    fn from(old: v1::Export) -> Self {
        Self {
            memhub_export_version: EXPORT_VERSION,
            exported_at: old.exported_at,
            exported_by: old.exported_by,
            source_schema_version: old.source_schema_version,
            project: old.project,
            facts: old
                .facts
                .into_iter()
                .map(|base| Fact {
                    base,
                    retired_at: None,
                    retired_reason: None,
                })
                .collect(),
            decisions: old.decisions,
            tasks: old
                .tasks
                .into_iter()
                .map(|base| Task {
                    base,
                    dropped_reason: None,
                    pre_drop_status: None,
                    pre_drop_updated_at: None,
                    pre_keep_updated_at: None,
                    kept_at: None,
                })
                .collect(),
            commands: old.commands,
            pending_writes: old.pending_writes,
            writes_log: old.writes_log,
            session_notes: old.session_notes,
            project_state: old.project_state,
            project_arch: old.project_arch,
        }
    }
}
