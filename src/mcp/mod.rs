use std::future::{self, Future};
use std::path::{Path, PathBuf};

use log::info;
use rmcp::ErrorData as McpError;
use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{
    Implementation, InitializeRequestParams, InitializeResult, ServerCapabilities, ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{
    Json, RoleServer, ServerHandler, ServiceExt, schemars, tool, tool_router, transport::stdio,
};
use serde::{Deserialize, Serialize};

use crate::commands;
use crate::models::{CommandRecord, Decision, SearchResult, StatusSummary, Task};
use crate::{MemhubError, Result};

pub fn serve(start: &Path) -> Result<()> {
    let server = MemhubServer::new(start.to_path_buf());
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()?;

    runtime.block_on(async move {
        let service = server
            .serve(stdio())
            .await
            .map_err(|err| MemhubError::Mcp(err.to_string()))?;
        service
            .waiting()
            .await
            .map_err(|err| MemhubError::Mcp(err.to_string()))?;
        Ok(())
    })
}

#[derive(Clone)]
struct MemhubServer {
    start: PathBuf,
    #[allow(dead_code)]
    tool_router: ToolRouter<Self>,
}

impl MemhubServer {
    fn new(start: PathBuf) -> Self {
        Self {
            start,
            tool_router: Self::tool_router(),
        }
    }

    async fn status_impl(&self) -> std::result::Result<Json<StatusToolResponse>, McpError> {
        let summary = commands::status::run(&self.start).map_err(map_tool_error)?;
        Ok(Json(StatusToolResponse::from(summary)))
    }

    async fn search_impl(
        &self,
        Parameters(params): Parameters<SearchParams>,
    ) -> std::result::Result<Json<SearchToolResponse>, McpError> {
        let limit = params.limit.unwrap_or(10);
        let response =
            commands::search::run(&self.start, &params.query, limit).map_err(map_tool_error)?;
        Ok(Json(SearchToolResponse {
            matcher: response.matcher,
            query: response.query,
            results: response.results.into_iter().map(SearchHit::from).collect(),
        }))
    }

    async fn list_tasks_impl(
        &self,
        Parameters(params): Parameters<ListTasksParams>,
    ) -> std::result::Result<Json<ListTasksToolResponse>, McpError> {
        let status = params.status.unwrap_or_else(|| "open".to_string());
        let limit = params.limit.unwrap_or(25);
        let tasks =
            commands::task::list_by_status(&self.start, &status, limit).map_err(map_tool_error)?;
        Ok(Json(ListTasksToolResponse {
            status,
            tasks: tasks.into_iter().map(TaskToolRecord::from).collect(),
        }))
    }

    async fn list_decisions_impl(
        &self,
        Parameters(params): Parameters<ListDecisionsParams>,
    ) -> std::result::Result<Json<ListDecisionsToolResponse>, McpError> {
        let limit = params.limit.unwrap_or(10);
        let decisions =
            commands::decision::list_active_recent(&self.start, limit).map_err(map_tool_error)?;
        Ok(Json(ListDecisionsToolResponse {
            decisions: decisions
                .into_iter()
                .map(DecisionToolRecord::from)
                .collect(),
        }))
    }

    async fn get_command_impl(
        &self,
        Parameters(params): Parameters<GetCommandParams>,
    ) -> std::result::Result<Json<GetCommandToolResponse>, McpError> {
        let command =
            commands::command::latest_by_kind(&self.start, &params.kind).map_err(map_tool_error)?;
        Ok(Json(GetCommandToolResponse {
            command: command.map(CommandToolRecord::from),
        }))
    }

    async fn record_command_impl(
        &self,
        Parameters(params): Parameters<RecordCommandParams>,
    ) -> std::result::Result<Json<RecordCommandToolResponse>, McpError> {
        let (id, created) =
            commands::command::verify(&self.start, &params.kind, &params.cmdline, params.exit_code)
                .map_err(map_tool_error)?;

        Ok(Json(RecordCommandToolResponse {
            id,
            created,
            kind: params.kind,
            cmdline: params.cmdline,
            exit_code: params.exit_code,
        }))
    }

    async fn propose_fact_impl(
        &self,
        Parameters(params): Parameters<ProposeFactParams>,
        actor: ClientIdentity,
    ) -> std::result::Result<Json<ProposeFactToolResponse>, McpError> {
        let id = commands::pending_write::propose_fact(
            &self.start,
            &params.key,
            &params.value,
            &params.rationale,
            &actor.normalized,
            &actor.raw,
        )
        .map_err(map_tool_error)?;

        Ok(Json(ProposeFactToolResponse {
            id,
            status: "pending".to_string(),
            actor: actor.normalized,
            actor_raw: actor.raw,
            key: params.key,
        }))
    }

    async fn propose_decision_impl(
        &self,
        Parameters(params): Parameters<ProposeDecisionParams>,
        actor: ClientIdentity,
    ) -> std::result::Result<Json<ProposeDecisionToolResponse>, McpError> {
        let id = commands::pending_write::propose_decision(
            &self.start,
            &params.title,
            &params.rationale,
            &actor.normalized,
            &actor.raw,
        )
        .map_err(map_tool_error)?;

        Ok(Json(ProposeDecisionToolResponse {
            id,
            status: "pending".to_string(),
            actor: actor.normalized,
            actor_raw: actor.raw,
            title: params.title,
        }))
    }
}

#[tool_router(router = tool_router)]
impl MemhubServer {
    #[tool(
        name = "status",
        description = "Return a summary of the current memhub project and stored record counts."
    )]
    async fn status(&self) -> std::result::Result<Json<StatusToolResponse>, McpError> {
        self.status_impl().await
    }

    #[tool(
        name = "search",
        description = "Search indexed memhub data. Supports file-history lookups and decision text search."
    )]
    async fn search(
        &self,
        params: Parameters<SearchParams>,
    ) -> std::result::Result<Json<SearchToolResponse>, McpError> {
        self.search_impl(params).await
    }

    #[tool(
        name = "list_tasks",
        description = "List tasks by status using indexed task lookups. Defaults to open tasks."
    )]
    async fn list_tasks(
        &self,
        params: Parameters<ListTasksParams>,
    ) -> std::result::Result<Json<ListTasksToolResponse>, McpError> {
        self.list_tasks_impl(params).await
    }

    #[tool(
        name = "list_decisions",
        description = "List recent active decisions from memhub."
    )]
    async fn list_decisions(
        &self,
        params: Parameters<ListDecisionsParams>,
    ) -> std::result::Result<Json<ListDecisionsToolResponse>, McpError> {
        self.list_decisions_impl(params).await
    }

    #[tool(
        name = "get_command",
        description = "Return the latest recorded command for a given kind such as build or test."
    )]
    async fn get_command(
        &self,
        params: Parameters<GetCommandParams>,
    ) -> std::result::Result<Json<GetCommandToolResponse>, McpError> {
        self.get_command_impl(params).await
    }

    #[tool(
        name = "record_command",
        description = "Record a verified command outcome using the existing explicit command verification path."
    )]
    async fn record_command(
        &self,
        params: Parameters<RecordCommandParams>,
    ) -> std::result::Result<Json<RecordCommandToolResponse>, McpError> {
        self.record_command_impl(params).await
    }

    #[tool(
        name = "propose_fact",
        description = "Stage a proposed fact write for later review instead of writing directly to durable facts."
    )]
    async fn propose_fact(
        &self,
        params: Parameters<ProposeFactParams>,
        request_context: RequestContext<RoleServer>,
    ) -> std::result::Result<Json<ProposeFactToolResponse>, McpError> {
        let actor = current_client_identity(&request_context);
        self.propose_fact_impl(params, actor).await
    }

    #[tool(
        name = "propose_decision",
        description = "Stage a proposed decision write for later review instead of writing directly to durable decisions."
    )]
    async fn propose_decision(
        &self,
        params: Parameters<ProposeDecisionParams>,
        request_context: RequestContext<RoleServer>,
    ) -> std::result::Result<Json<ProposeDecisionToolResponse>, McpError> {
        let actor = current_client_identity(&request_context);
        self.propose_decision_impl(params, actor).await
    }
}

impl ServerHandler for MemhubServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("memhub", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Local-first per-repo project memory. Read tools are direct; writes are limited to explicit verified command recording plus staged fact and decision proposals.",
            )
    }

    fn initialize(
        &self,
        request: InitializeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> impl Future<Output = std::result::Result<InitializeResult, McpError>> + '_ {
        if context.peer.peer_info().is_none() {
            context.peer.set_peer_info(request.clone());
        }

        let actor = current_client_identity_from_initialize(Some(&request));
        info!(
            "mcp client initialized: normalized={} raw={}",
            actor.normalized, actor.raw
        );

        future::ready(Ok(self.get_info()))
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
struct SearchParams {
    query: String,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
struct ListTasksParams {
    status: Option<String>,
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
struct ListDecisionsParams {
    limit: Option<usize>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct GetCommandParams {
    kind: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RecordCommandParams {
    kind: String,
    cmdline: String,
    exit_code: i64,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ProposeFactParams {
    key: String,
    value: String,
    rationale: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ProposeDecisionParams {
    title: String,
    rationale: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct StatusToolResponse {
    project_name: String,
    repo_root: String,
    db_path: String,
    config_path: String,
    schema_version: String,
    facts: i64,
    decisions: i64,
    tasks_open: i64,
    tasks_total: i64,
    commands: i64,
    commits: i64,
    files: i64,
    chunks: i64,
    pending_writes: i64,
    writes_logged: i64,
}

impl From<StatusSummary> for StatusToolResponse {
    fn from(value: StatusSummary) -> Self {
        Self {
            project_name: value.project_name,
            repo_root: value.repo_root.display().to_string(),
            db_path: value.db_path.display().to_string(),
            config_path: value.config_path.display().to_string(),
            schema_version: value.schema_version,
            facts: value.facts,
            decisions: value.decisions,
            tasks_open: value.tasks_open,
            tasks_total: value.tasks_total,
            commands: value.commands,
            commits: value.commits,
            files: value.files,
            chunks: value.chunks,
            pending_writes: value.pending_writes,
            writes_logged: value.writes_logged,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct SearchToolResponse {
    matcher: String,
    query: String,
    results: Vec<SearchHit>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
#[serde(tag = "type", rename_all = "snake_case")]
enum SearchHit {
    FileHistory {
        path: String,
        commit_sha: String,
        author: String,
        committed_at: String,
        message: String,
        change_type: String,
    },
    Decision {
        decision_id: i64,
        title: String,
        rationale: String,
        decided_at: String,
        score: f64,
    },
}

impl From<SearchResult> for SearchHit {
    fn from(value: SearchResult) -> Self {
        match value {
            SearchResult::FileHistory(hit) => Self::FileHistory {
                path: hit.path,
                commit_sha: hit.commit_sha,
                author: hit.author,
                committed_at: hit.committed_at,
                message: hit.message,
                change_type: hit.change_type,
            },
            SearchResult::Decision(hit) => Self::Decision {
                decision_id: hit.decision_id,
                title: hit.title,
                rationale: hit.rationale,
                decided_at: hit.decided_at,
                score: hit.score,
            },
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct ListTasksToolResponse {
    status: String,
    tasks: Vec<TaskToolRecord>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct TaskToolRecord {
    id: i64,
    title: String,
    status: String,
    notes: Option<String>,
    created_at: String,
    updated_at: String,
}

impl From<Task> for TaskToolRecord {
    fn from(value: Task) -> Self {
        Self {
            id: value.id,
            title: value.title,
            status: value.status,
            notes: value.notes,
            created_at: value.created_at,
            updated_at: value.updated_at,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct ListDecisionsToolResponse {
    decisions: Vec<DecisionToolRecord>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct DecisionToolRecord {
    id: i64,
    title: String,
    rationale: String,
    status: String,
    decided_at: String,
}

impl From<Decision> for DecisionToolRecord {
    fn from(value: Decision) -> Self {
        Self {
            id: value.id,
            title: value.title,
            rationale: value.rationale,
            status: value.status,
            decided_at: value.decided_at,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct GetCommandToolResponse {
    command: Option<CommandToolRecord>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct CommandToolRecord {
    id: i64,
    kind: String,
    cmdline: String,
    last_exit_code: Option<i64>,
    last_run_at: Option<String>,
    success_count: i64,
    fail_count: i64,
}

impl From<CommandRecord> for CommandToolRecord {
    fn from(value: CommandRecord) -> Self {
        Self {
            id: value.id,
            kind: value.kind,
            cmdline: value.cmdline,
            last_exit_code: value.last_exit_code,
            last_run_at: value.last_run_at,
            success_count: value.success_count,
            fail_count: value.fail_count,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct RecordCommandToolResponse {
    id: i64,
    created: bool,
    kind: String,
    cmdline: String,
    exit_code: i64,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct ProposeFactToolResponse {
    id: i64,
    status: String,
    actor: String,
    actor_raw: String,
    key: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct ProposeDecisionToolResponse {
    id: i64,
    status: String,
    actor: String,
    actor_raw: String,
    title: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClientIdentity {
    normalized: String,
    raw: String,
}

fn current_client_identity(request_context: &RequestContext<RoleServer>) -> ClientIdentity {
    current_client_identity_from_initialize(request_context.peer.peer_info())
}

fn current_client_identity_from_initialize(
    peer_info: Option<&InitializeRequestParams>,
) -> ClientIdentity {
    match peer_info.map(|info| info.client_info.name.trim()) {
        Some(name) if !name.is_empty() => ClientIdentity {
            normalized: normalize_client_name(name),
            raw: name.to_string(),
        },
        _ => ClientIdentity {
            normalized: "unknown".to_string(),
            raw: "unknown".to_string(),
        },
    }
}

fn normalize_client_name(name: &str) -> String {
    match name.trim().to_ascii_lowercase().as_str() {
        "claude-ai" | "claude code" | "claude-code" => "claude-code".to_string(),
        "codex" | "codex-cli" | "openai-codex" => "codex".to_string(),
        other => other.to_string(),
    }
}

fn map_tool_error(err: MemhubError) -> McpError {
    match err {
        MemhubError::InvalidInput(message)
        | MemhubError::InvalidManagedMarkdown {
            path: _,
            reason: message,
        } => McpError::invalid_params(message, None),
        other => McpError::internal_error(other.to_string(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{command, decision, init, status, task};
    use crate::db;
    use rusqlite::params;
    use tempfile::tempdir;

    #[test]
    fn mcp_status_reads_project_summary() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");

        let server = MemhubServer::new(temp.path().to_path_buf());
        let status = server.status_impl();
        let status = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(status)
            .expect("status");

        assert!(!status.0.project_name.is_empty());
        assert_eq!(status.0.tasks_open, 0);
        assert_eq!(status.0.commands, 0);
        assert_eq!(status.0.pending_writes, 0);
    }

    #[test]
    fn mcp_tools_read_existing_records() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        decision::add(
            temp.path(),
            "Use indexed MCP lookups",
            "Keep reads predictable.",
        )
        .expect("decision");
        task::add(temp.path(), "Ship MCP server", Some("Milestone 3")).expect("task");
        command::verify(temp.path(), "build", "cargo build", 0).expect("command");

        let server = MemhubServer::new(temp.path().to_path_buf());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let tasks = runtime
            .block_on(server.list_tasks_impl(Parameters(ListTasksParams::default())))
            .expect("list tasks");
        let decisions = runtime
            .block_on(server.list_decisions_impl(Parameters(ListDecisionsParams::default())))
            .expect("list decisions");
        let command = runtime
            .block_on(server.get_command_impl(Parameters(GetCommandParams {
                kind: "build".to_string(),
            })))
            .expect("get command");

        assert_eq!(tasks.0.tasks.len(), 1);
        assert_eq!(tasks.0.tasks[0].title, "Ship MCP server");
        assert_eq!(decisions.0.decisions.len(), 1);
        assert_eq!(decisions.0.decisions[0].title, "Use indexed MCP lookups");
        assert_eq!(
            command.0.command.expect("command record").cmdline,
            "cargo build"
        );
    }

    #[test]
    fn mcp_record_command_reuses_verified_write_path() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");

        let server = MemhubServer::new(temp.path().to_path_buf());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let result = runtime
            .block_on(server.record_command_impl(Parameters(RecordCommandParams {
                kind: "test".to_string(),
                cmdline: "cargo test".to_string(),
                exit_code: 0,
            })))
            .expect("record command");

        assert!(result.0.created);
        let stored = command::latest_by_kind(temp.path(), "test")
            .expect("get command")
            .expect("command row");
        assert_eq!(stored.cmdline, "cargo test");
        assert_eq!(stored.last_exit_code, Some(0));
    }

    #[test]
    fn normalize_client_aliases() {
        assert_eq!(normalize_client_name("claude-ai"), "claude-code");
        assert_eq!(normalize_client_name("Claude Code"), "claude-code");
        assert_eq!(normalize_client_name("openai-codex"), "codex");
        assert_eq!(normalize_client_name("CustomClient"), "customclient");
    }

    #[test]
    fn mcp_proposal_tools_stage_pending_writes() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");

        let server = MemhubServer::new(temp.path().to_path_buf());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let fact_result = runtime
            .block_on(server.propose_fact_impl(
                Parameters(ProposeFactParams {
                    key: "build-command".to_string(),
                    value: "cargo build".to_string(),
                    rationale: "Observed in this repo and should be reviewed.".to_string(),
                }),
                ClientIdentity {
                    normalized: "codex".to_string(),
                    raw: "openai-codex".to_string(),
                },
            ))
            .expect("propose fact");
        let decision_result = runtime
            .block_on(server.propose_decision_impl(
                Parameters(ProposeDecisionParams {
                    title: "Keep staged writes narrow".to_string(),
                    rationale: "Avoid direct agent writes before review exists.".to_string(),
                }),
                ClientIdentity {
                    normalized: "claude-code".to_string(),
                    raw: "claude-ai".to_string(),
                },
            ))
            .expect("propose decision");

        assert_eq!(fact_result.0.status, "pending");
        assert_eq!(fact_result.0.actor, "codex");
        assert_eq!(decision_result.0.status, "pending");
        assert_eq!(decision_result.0.actor, "claude-code");

        let ctx = db::open_project(temp.path()).expect("open project");
        let pending_count: i64 = ctx
            .conn
            .query_row(
                "SELECT COUNT(*) FROM pending_writes WHERE status = 'pending'",
                [],
                |row| row.get(0),
            )
            .expect("pending count");
        let staged_fact: (String, String, String, String) = ctx
            .conn
            .query_row(
                "SELECT payload_json, rationale, actor, actor_raw
                 FROM pending_writes
                 WHERE kind = 'fact'
                 LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .expect("staged fact");
        let staged_decision: String = ctx
            .conn
            .query_row(
                "SELECT payload_json FROM pending_writes WHERE kind = 'decision' LIMIT 1",
                [],
                |row| row.get(0),
            )
            .expect("staged decision");
        let durable_fact_count: i64 = ctx
            .conn
            .query_row("SELECT COUNT(*) FROM facts", [], |row| row.get(0))
            .expect("fact count");
        let durable_decision_count: i64 = ctx
            .conn
            .query_row("SELECT COUNT(*) FROM decisions", [], |row| row.get(0))
            .expect("decision count");
        let summary = status::run(temp.path()).expect("status");

        assert_eq!(pending_count, 2);
        assert!(staged_fact.0.contains("\"key\":\"build-command\""));
        assert_eq!(
            staged_fact.1,
            "Observed in this repo and should be reviewed."
        );
        assert_eq!(staged_fact.2, "codex");
        assert_eq!(staged_fact.3, "openai-codex");
        assert!(staged_decision.contains("\"title\":\"Keep staged writes narrow\""));
        assert_eq!(durable_fact_count, 0);
        assert_eq!(durable_decision_count, 0);
        assert_eq!(summary.pending_writes, 2);
    }

    #[test]
    fn mcp_pending_writes_are_logged() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");

        let server = MemhubServer::new(temp.path().to_path_buf());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        runtime
            .block_on(server.propose_fact_impl(
                Parameters(ProposeFactParams {
                    key: "lint-command".to_string(),
                    value: "cargo fmt --check".to_string(),
                    rationale: "Candidate command for future review.".to_string(),
                }),
                ClientIdentity {
                    normalized: "codex".to_string(),
                    raw: "codex".to_string(),
                },
            ))
            .expect("propose fact");

        let ctx = db::open_project(temp.path()).expect("open project");
        let writes_log_row: (String, String) = ctx
            .conn
            .query_row(
                "SELECT actor, reason
                 FROM writes_log
                 WHERE table_name = 'pending_writes'
                 ORDER BY id DESC
                 LIMIT 1",
                params![],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("writes log row");

        assert_eq!(writes_log_row.0, "codex");
        assert_eq!(writes_log_row.1, "mcp propose_fact");
    }
}
