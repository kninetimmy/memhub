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

#[derive(Debug)]
pub struct Fact {
    pub id: i64,
    pub key: String,
    pub value: String,
    pub confidence: f64,
    pub source: String,
    pub verified_at: Option<String>,
    pub created_at: String,
}

#[derive(Debug)]
pub struct Decision {
    pub id: i64,
    pub title: String,
    pub rationale: String,
    pub status: String,
    pub decided_at: String,
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

#[derive(Debug)]
pub struct StatusSummary {
    pub project_name: String,
    pub repo_root: PathBuf,
    pub db_path: PathBuf,
    pub config_path: PathBuf,
    pub schema_version: String,
    pub facts: i64,
    pub decisions: i64,
    pub tasks_open: i64,
    pub tasks_total: i64,
    pub commands: i64,
    pub writes_logged: i64,
}
