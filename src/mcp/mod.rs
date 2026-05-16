use std::future::{self, Future};
use std::path::{Path, PathBuf};

use log::info;
use rmcp::ErrorData as McpError;
use rmcp::handler::server::{router::tool::ToolRouter, wrapper::Parameters};
use rmcp::model::{
    Implementation, InitializeRequestParams, InitializeResult, Meta, RequestId, ServerCapabilities,
    ServerInfo,
};
use rmcp::service::RequestContext;
use rmcp::{
    Json, RoleServer, ServerHandler, ServiceExt, schemars, tool, tool_handler, tool_router,
    transport::stdio,
};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::MemhubError;
use crate::commands;
use crate::config::RetrievalMode;
use crate::models::{
    CommandRecord, Decision, Fact, PendingWriteRecord, RenderResult, SearchResult, StatusSummary,
    Task,
};
use crate::retrieval::{self, RecallHit, RecallOptions, RecallResponse, RecallWarning, SourceType};

pub fn serve(start: &Path) -> crate::Result<()> {
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
        actor: ClientIdentity,
    ) -> std::result::Result<Json<RecordCommandToolResponse>, McpError> {
        let (id, created) = commands::command::verify(
            &self.start,
            &params.kind,
            &params.cmdline,
            params.exit_code,
            &actor.normalized,
        )
        .map_err(map_tool_error)?;

        Ok(Json(RecordCommandToolResponse {
            id,
            created,
            kind: params.kind,
            cmdline: params.cmdline,
            exit_code: params.exit_code,
        }))
    }

    async fn list_pending_writes_impl(
        &self,
        Parameters(params): Parameters<ListPendingWritesParams>,
    ) -> std::result::Result<Json<ListPendingWritesToolResponse>, McpError> {
        let status = params.status.as_deref();
        let limit = params.limit.unwrap_or(commands::review::DEFAULT_LIST_LIMIT);
        let rows = commands::review::list(&self.start, status, limit).map_err(map_tool_error)?;
        Ok(Json(ListPendingWritesToolResponse {
            status: status.map(|s| s.to_string()),
            pending_writes: rows.into_iter().map(PendingWriteToolRecord::from).collect(),
        }))
    }

    async fn propose_fact_impl(
        &self,
        Parameters(params): Parameters<ProposeFactParams>,
        actor: ClientIdentity,
        provenance_json: String,
    ) -> std::result::Result<Json<ProposeFactToolResponse>, McpError> {
        let id = commands::pending_write::propose_fact(
            &self.start,
            &params.key,
            &params.value,
            &params.rationale,
            &actor.normalized,
            &actor.raw,
            &provenance_json,
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

    async fn log_session_note_impl(
        &self,
        Parameters(params): Parameters<LogSessionNoteParams>,
        actor: ClientIdentity,
    ) -> std::result::Result<Json<LogSessionNoteToolResponse>, McpError> {
        let note =
            commands::session_note::add(&self.start, &params.text, &actor.normalized, &actor.raw)
                .map_err(map_tool_error)?;

        Ok(Json(LogSessionNoteToolResponse {
            id: note.id,
            actor: note.actor,
            actor_raw: note.actor_raw,
            created_at: note.created_at,
        }))
    }

    async fn propose_decision_impl(
        &self,
        Parameters(params): Parameters<ProposeDecisionParams>,
        actor: ClientIdentity,
        provenance_json: String,
    ) -> std::result::Result<Json<ProposeDecisionToolResponse>, McpError> {
        let id = commands::pending_write::propose_decision(
            &self.start,
            &params.title,
            &params.rationale,
            &actor.normalized,
            &actor.raw,
            &provenance_json,
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

    async fn task_add_impl(
        &self,
        Parameters(params): Parameters<TaskAddParams>,
        actor: ClientIdentity,
    ) -> std::result::Result<Json<TaskAddToolResponse>, McpError> {
        let id = commands::task::add(
            &self.start,
            &params.title,
            params.notes.as_deref(),
            &actor.normalized,
        )
        .map_err(map_tool_error)?;

        Ok(Json(TaskAddToolResponse {
            id,
            title: params.title,
            status: "open".to_string(),
            actor: actor.normalized,
            actor_raw: actor.raw,
        }))
    }

    async fn task_done_impl(
        &self,
        Parameters(params): Parameters<TaskDoneParams>,
        actor: ClientIdentity,
    ) -> std::result::Result<Json<TaskDoneToolResponse>, McpError> {
        commands::task::done(&self.start, params.id, &actor.normalized).map_err(map_tool_error)?;

        Ok(Json(TaskDoneToolResponse {
            id: params.id,
            status: "done".to_string(),
            actor: actor.normalized,
            actor_raw: actor.raw,
        }))
    }

    async fn doc_add_impl(
        &self,
        Parameters(params): Parameters<DocAddParams>,
        actor: ClientIdentity,
    ) -> std::result::Result<Json<DocAddToolResponse>, McpError> {
        let outcome = commands::doc::add(
            &self.start,
            std::path::Path::new(&params.file),
            params.title.as_deref(),
            &actor.normalized,
        )
        .map_err(map_tool_error)?;

        let status = match outcome.status {
            commands::doc::IngestStatus::Created => "created",
            commands::doc::IngestStatus::Updated => "updated",
            commands::doc::IngestStatus::Unchanged => "unchanged",
        };
        Ok(Json(DocAddToolResponse {
            id: outcome.doc_id,
            title: outcome.title,
            path: outcome.path,
            chunks: outcome.chunk_count,
            status: status.to_string(),
            actor: actor.normalized,
            actor_raw: actor.raw,
        }))
    }

    async fn list_facts_impl(
        &self,
        Parameters(params): Parameters<ListFactsParams>,
    ) -> std::result::Result<Json<ListFactsToolResponse>, McpError> {
        let mut facts = commands::fact::list(&self.start).map_err(map_tool_error)?;
        if let Some(limit) = params.limit {
            facts.truncate(limit);
        }
        Ok(Json(ListFactsToolResponse {
            facts: facts.into_iter().map(FactToolRecord::from).collect(),
        }))
    }

    async fn render_impl(
        &self,
        actor: ClientIdentity,
    ) -> std::result::Result<Json<RenderToolResponse>, McpError> {
        let result =
            commands::render::run(&self.start, &actor.normalized).map_err(map_tool_error)?;
        Ok(Json(RenderToolResponse::from(result)))
    }

    async fn metrics_impl(&self) -> std::result::Result<Json<MetricsToolResponse>, McpError> {
        let data = commands::metrics::query_tool_data(&self.start).map_err(map_tool_error)?;
        Ok(Json(MetricsToolResponse::from(data)))
    }

    async fn recall_impl(
        &self,
        Parameters(params): Parameters<RecallParams>,
    ) -> std::result::Result<Json<RecallToolResponse>, McpError> {
        let mode = match params.mode.as_deref() {
            Some("fts") => Some(RetrievalMode::Fts),
            Some("hybrid") => Some(RetrievalMode::Hybrid),
            Some(other) => {
                return Err(McpError::invalid_params(
                    format!("invalid mode '{other}'; expected 'fts' or 'hybrid'"),
                    None,
                ));
            }
            None => None,
        };
        let mut source_types = Vec::new();
        for raw in params.source_types.as_deref().unwrap_or(&[]) {
            match raw.as_str() {
                "fact" => source_types.push(SourceType::Fact),
                "decision" => source_types.push(SourceType::Decision),
                "task" => source_types.push(SourceType::Task),
                "doc" | "doc_chunk" => source_types.push(SourceType::DocChunk),
                other => {
                    return Err(McpError::invalid_params(
                        format!(
                            "invalid source_type '{other}'; expected fact, decision, task, or doc"
                        ),
                        None,
                    ));
                }
            }
        }
        let response = retrieval::recall(
            &self.start,
            RecallOptions {
                query: params.query,
                mode,
                max_results: params.max_results.unwrap_or(0),
                source_types,
                include_stale: params.include_stale,
                accepted_only: params.accepted_only,
                use_reranker: None,
                min_rerank_score: None,
                log_metrics: true,
            },
        )
        .map_err(map_tool_error)?;
        Ok(Json(RecallToolResponse::from(response)))
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
        request_context: RequestContext<RoleServer>,
    ) -> std::result::Result<Json<RecordCommandToolResponse>, McpError> {
        let actor = current_client_identity(&request_context);
        self.record_command_impl(params, actor).await
    }

    #[tool(
        name = "list_pending_writes",
        description = "List staged agent-originated proposals from pending_writes. Filter by status; defaults to pending."
    )]
    async fn list_pending_writes(
        &self,
        params: Parameters<ListPendingWritesParams>,
    ) -> std::result::Result<Json<ListPendingWritesToolResponse>, McpError> {
        self.list_pending_writes_impl(params).await
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
        let provenance_json = current_pending_write_provenance(&request_context);
        self.propose_fact_impl(params, actor, provenance_json).await
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
        let provenance_json = current_pending_write_provenance(&request_context);
        self.propose_decision_impl(params, actor, provenance_json)
            .await
    }

    #[tool(
        name = "log_session_note",
        description = "Record a free-form session note. Notes are write-only scratch space and never promote to facts or decisions."
    )]
    async fn log_session_note(
        &self,
        params: Parameters<LogSessionNoteParams>,
        request_context: RequestContext<RoleServer>,
    ) -> std::result::Result<Json<LogSessionNoteToolResponse>, McpError> {
        let actor = current_client_identity(&request_context);
        self.log_session_note_impl(params, actor).await
    }

    #[tool(
        name = "task_add",
        description = "Create a task directly in the durable tasks table. Tasks are intent, not claims; the user prunes."
    )]
    async fn task_add(
        &self,
        params: Parameters<TaskAddParams>,
        request_context: RequestContext<RoleServer>,
    ) -> std::result::Result<Json<TaskAddToolResponse>, McpError> {
        let actor = current_client_identity(&request_context);
        self.task_add_impl(params, actor).await
    }

    #[tool(
        name = "doc_add",
        description = "Ingest (or re-ingest) a local markdown file as an external reference document, chunked and RAG-searchable. Direct write: a doc is a user-pointed artifact, not an agent claim, so no review gate. Docs are OPT-IN to recall — query them with recall(source_types=[\"doc\"]); they never appear in the default bundle. Unchanged content (same hash) is a no-op; changed content replaces every chunk."
    )]
    async fn doc_add(
        &self,
        params: Parameters<DocAddParams>,
        request_context: RequestContext<RoleServer>,
    ) -> std::result::Result<Json<DocAddToolResponse>, McpError> {
        let actor = current_client_identity(&request_context);
        self.doc_add_impl(params, actor).await
    }

    #[tool(
        name = "task_done",
        description = "Mark an existing task as done by id."
    )]
    async fn task_done(
        &self,
        params: Parameters<TaskDoneParams>,
        request_context: RequestContext<RoleServer>,
    ) -> std::result::Result<Json<TaskDoneToolResponse>, McpError> {
        let actor = current_client_identity(&request_context);
        self.task_done_impl(params, actor).await
    }

    #[tool(
        name = "list_facts",
        description = "List durable facts. Use this to look up build/test/run commands or other key-value records the user has confirmed."
    )]
    async fn list_facts(
        &self,
        params: Parameters<ListFactsParams>,
    ) -> std::result::Result<Json<ListFactsToolResponse>, McpError> {
        self.list_facts_impl(params).await
    }

    #[tool(
        name = "render",
        description = "Regenerate the configured local PROJECT.md and PROJECT_LEDGER.md render outputs from the current DB state. Prior files are backed up automatically."
    )]
    async fn render(
        &self,
        request_context: RequestContext<RoleServer>,
    ) -> std::result::Result<Json<RenderToolResponse>, McpError> {
        let actor = current_client_identity(&request_context);
        self.render_impl(actor).await
    }

    #[tool(
        name = "recall",
        description = "Retrieve relevant facts, decisions, and tasks via SQL+RAG hybrid recall (FTS5 + brute-force cosine when hybrid mode is configured). Read-only; prefer this over reading PROJECT_LEDGER.md mid-session. Ingested reference docs are OPT-IN: pass source_types=[\"doc\"] to search them. The response's `available_docs` counts ingested doc chunks you did NOT search — when it is non-zero and the question is design/spec/architecture-flavored, consider a follow-up recall scoped to docs (use judgment; not every turn)."
    )]
    async fn recall(
        &self,
        params: Parameters<RecallParams>,
    ) -> std::result::Result<Json<RecallToolResponse>, McpError> {
        self.recall_impl(params).await
    }

    #[tool(
        name = "metrics",
        description = "Return token-accounting totals and a pre-rendered dashboard panel. \
                       When metrics are disabled returns {enabled:false} only. \
                       When enabled, returns 7-day and 30-day recall/session token aggregates \
                       plus up to 10 recent sessions and a rendered_panel string the /metrics \
                       skill prints verbatim."
    )]
    async fn metrics(&self) -> std::result::Result<Json<MetricsToolResponse>, McpError> {
        self.metrics_impl().await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for MemhubServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("memhub", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                "Local-first per-repo project memory. Read tools are direct (status, search, recall, list_tasks, list_decisions, list_facts, list_pending_writes, get_command, metrics). Prefer `recall` over reading PROJECT_LEDGER.md mid-session — it does SQL+RAG hybrid retrieval across facts, decisions, and tasks. Tasks write directly (task_add, task_done) since tasks are intent. `doc_add` ingests a user-pointed markdown file as opt-in reference material (search it with recall source_types=[\"doc\"]; it never enters the default bundle, and recall's `available_docs` signals when ingested docs went unsearched). Facts and decisions stage via propose_fact / propose_decision and require human acceptance through `memhub review accept`. Session notes are write-only scratch. `render` regenerates the configured local PROJECT.md from the DB. `metrics` returns token-accounting totals and a pre-rendered dashboard panel for the /metrics skill.",
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
            sanitize_for_log(&actor.normalized),
            sanitize_for_log(&actor.raw),
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

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
struct ListPendingWritesParams {
    status: Option<String>,
    limit: Option<usize>,
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
    stale_facts: i64,
    decisions: i64,
    tasks_open: i64,
    tasks_total: i64,
    commands: i64,
    commits: i64,
    files: i64,
    chunks: i64,
    pending_writes: i64,
    writes_logged: i64,
    deny_patterns: usize,
    k9_detected: bool,
    k9_enabled: bool,
    k9_agent_docs_path: String,
    k9_drift: Option<String>,
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
            stale_facts: value.stale_facts,
            decisions: value.decisions,
            tasks_open: value.tasks_open,
            tasks_total: value.tasks_total,
            commands: value.commands,
            commits: value.commits,
            files: value.files,
            chunks: value.chunks,
            pending_writes: value.pending_writes,
            writes_logged: value.writes_logged,
            deny_patterns: value.deny_patterns,
            k9_detected: value.k9_detected,
            k9_enabled: value.k9_enabled,
            k9_agent_docs_path: value.k9_agent_docs_path,
            k9_drift: value.k9_drift,
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
    source: String,
}

impl From<Decision> for DecisionToolRecord {
    fn from(value: Decision) -> Self {
        Self {
            id: value.id,
            title: value.title,
            rationale: value.rationale,
            status: value.status,
            decided_at: value.decided_at,
            source: value.source,
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
    confidence: Option<f64>,
}

impl From<CommandRecord> for CommandToolRecord {
    fn from(value: CommandRecord) -> Self {
        let confidence = value.confidence();
        Self {
            id: value.id,
            kind: value.kind,
            cmdline: value.cmdline,
            last_exit_code: value.last_exit_code,
            last_run_at: value.last_run_at,
            success_count: value.success_count,
            fail_count: value.fail_count,
            confidence,
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
struct ListPendingWritesToolResponse {
    status: Option<String>,
    pending_writes: Vec<PendingWriteToolRecord>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct PendingWriteToolRecord {
    id: i64,
    kind: String,
    status: String,
    actor: String,
    actor_raw: String,
    rationale: String,
    payload_json: String,
    provenance_json: String,
    created_at: String,
    reviewed_at: Option<String>,
}

impl From<PendingWriteRecord> for PendingWriteToolRecord {
    fn from(value: PendingWriteRecord) -> Self {
        Self {
            id: value.id,
            kind: value.kind,
            status: value.status,
            actor: value.actor,
            actor_raw: value.actor_raw,
            rationale: value.rationale,
            payload_json: value.payload_json,
            provenance_json: value.provenance_json,
            created_at: value.created_at,
            reviewed_at: value.reviewed_at,
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LogSessionNoteParams {
    text: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct LogSessionNoteToolResponse {
    id: i64,
    actor: String,
    actor_raw: String,
    created_at: String,
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

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TaskAddParams {
    title: String,
    notes: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct TaskAddToolResponse {
    id: i64,
    title: String,
    status: String,
    actor: String,
    actor_raw: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct DocAddParams {
    /// Path to the local markdown file to ingest.
    file: String,
    /// Optional title override (defaults to first heading or file name).
    title: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct DocAddToolResponse {
    id: i64,
    title: String,
    path: String,
    chunks: usize,
    /// `created` | `updated` | `unchanged`.
    status: String,
    actor: String,
    actor_raw: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct TaskDoneParams {
    id: i64,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct TaskDoneToolResponse {
    id: i64,
    status: String,
    actor: String,
    actor_raw: String,
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
struct ListFactsParams {
    limit: Option<usize>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct ListFactsToolResponse {
    facts: Vec<FactToolRecord>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct FactToolRecord {
    id: i64,
    key: String,
    value: String,
    confidence: f64,
    source: String,
    verified_at: Option<String>,
    created_at: String,
    is_stale: bool,
}

impl From<Fact> for FactToolRecord {
    fn from(value: Fact) -> Self {
        Self {
            id: value.id,
            key: value.key,
            value: value.value,
            confidence: value.confidence,
            source: value.source,
            verified_at: value.verified_at,
            created_at: value.created_at,
            is_stale: value.is_stale,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct RenderToolResponse {
    project_md_path: String,
    ledger_md_path: String,
    written_files: Vec<String>,
    backup_files: Vec<String>,
}

impl From<RenderResult> for RenderToolResponse {
    fn from(value: RenderResult) -> Self {
        Self {
            project_md_path: value.project_md_path.display().to_string(),
            ledger_md_path: value.ledger_md_path.display().to_string(),
            written_files: value
                .written_files
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
            backup_files: value
                .backup_files
                .iter()
                .map(|p| p.display().to_string())
                .collect(),
        }
    }
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct RecallParams {
    query: String,
    mode: Option<String>,
    max_results: Option<usize>,
    source_types: Option<Vec<String>>,
    accepted_only: Option<bool>,
    include_stale: Option<bool>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct RecallToolResponse {
    query: String,
    mode: String,
    results: Vec<RecallToolHit>,
    candidate_count: usize,
    returned_count: usize,
    /// Ingested doc chunks that exist but were NOT searched because the
    /// call did not scope to `doc`. Non-zero is a cue to consider a
    /// follow-up `recall(..., source_types=["doc"])` when the question is
    /// design/spec/architecture-flavored. Docs are opt-in and never in
    /// the default bundle.
    available_docs: usize,
    warnings: Vec<RecallToolWarning>,
    provenance: RecallToolProvenance,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct RecallToolHit {
    rank: usize,
    source_type: String,
    source_id: i64,
    title: String,
    body: String,
    score: f64,
    fts_score: f64,
    vector_score: f64,
    confidence: f64,
    stale: bool,
    source: String,
    created_at: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct RecallToolWarning {
    kind: String,
    stale_count: usize,
    total_count: usize,
    reason: String,
    fix: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct RecallToolProvenance {
    matcher: String,
    elapsed_ms: u128,
}

impl From<RecallHit> for RecallToolHit {
    fn from(value: RecallHit) -> Self {
        Self {
            rank: value.rank,
            source_type: value.source_type,
            source_id: value.source_id,
            title: value.title,
            body: value.body,
            score: value.score,
            fts_score: value.fts_score,
            vector_score: value.vector_score,
            confidence: value.confidence,
            stale: value.stale,
            source: value.source,
            created_at: value.created_at,
        }
    }
}

impl From<RecallWarning> for RecallToolWarning {
    fn from(value: RecallWarning) -> Self {
        Self {
            kind: value.kind,
            stale_count: value.stale_count,
            total_count: value.total_count,
            reason: value.reason,
            fix: value.fix,
        }
    }
}

impl From<RecallResponse> for RecallToolResponse {
    fn from(value: RecallResponse) -> Self {
        let mode = match value.mode {
            RetrievalMode::Fts => "fts".to_string(),
            RetrievalMode::Hybrid => "hybrid".to_string(),
        };
        Self {
            query: value.query,
            mode,
            results: value.results.into_iter().map(RecallToolHit::from).collect(),
            candidate_count: value.candidate_count,
            returned_count: value.returned_count,
            available_docs: value.available_docs,
            warnings: value
                .warnings
                .into_iter()
                .map(RecallToolWarning::from)
                .collect(),
            provenance: RecallToolProvenance {
                matcher: value.matcher,
                elapsed_ms: value.elapsed_ms,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ClientIdentity {
    normalized: String,
    raw: String,
}

#[derive(Debug, Serialize)]
struct PendingWriteProvenance {
    source: &'static str,
    request_id: Value,
    request_meta: Option<Value>,
    protocol_version: String,
    client_name: String,
    client_version: String,
    initialize_meta: Option<Value>,
}

fn current_client_identity(request_context: &RequestContext<RoleServer>) -> ClientIdentity {
    current_client_identity_from_initialize(request_context.peer.peer_info())
}

fn current_client_identity_from_initialize(
    peer_info: Option<&InitializeRequestParams>,
) -> ClientIdentity {
    match peer_info {
        Some(info) => {
            let raw = info.client_info.name.as_str();
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                ClientIdentity {
                    normalized: "unknown".to_string(),
                    raw: raw.to_string(),
                }
            } else {
                ClientIdentity {
                    normalized: normalize_client_name(trimmed),
                    raw: raw.to_string(),
                }
            }
        }
        _ => ClientIdentity {
            normalized: "unknown".to_string(),
            raw: "unknown".to_string(),
        },
    }
}

fn current_pending_write_provenance(request_context: &RequestContext<RoleServer>) -> String {
    pending_write_provenance_json(
        &request_context.id,
        &request_context.meta,
        request_context.peer.peer_info(),
    )
}

fn pending_write_provenance_json(
    request_id: &RequestId,
    request_meta: &Meta,
    peer_info: Option<&InitializeRequestParams>,
) -> String {
    let provenance = match peer_info {
        Some(info) => PendingWriteProvenance {
            source: "mcp",
            request_id: request_id.clone().into_json_value(),
            request_meta: optional_meta_value(Some(request_meta)),
            protocol_version: info.protocol_version.to_string(),
            client_name: info.client_info.name.clone(),
            client_version: info.client_info.version.clone(),
            initialize_meta: optional_meta_value(info.meta.as_ref()),
        },
        None => PendingWriteProvenance {
            source: "mcp",
            request_id: request_id.clone().into_json_value(),
            request_meta: optional_meta_value(Some(request_meta)),
            protocol_version: "unknown".to_string(),
            client_name: "unknown".to_string(),
            client_version: "unknown".to_string(),
            initialize_meta: None,
        },
    };

    serde_json::to_string(&provenance).expect("pending write provenance should serialize")
}

fn optional_meta_value(meta: Option<&Meta>) -> Option<Value> {
    meta.filter(|meta| !meta.0.is_empty())
        .map(|meta| serde_json::to_value(meta).expect("mcp meta should serialize"))
}

fn normalize_client_name(name: &str) -> String {
    match name.trim().to_ascii_lowercase().as_str() {
        "claude-ai" | "claude code" | "claude-code" => "claude-code".to_string(),
        "codex" | "codex-cli" | "openai-codex" => "codex".to_string(),
        other => other.to_string(),
    }
}

fn sanitize_for_log(value: &str) -> String {
    value.chars().flat_map(char::escape_default).collect()
}

// ── MetricsToolResponse ──────────────────────────────────────────────────────

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct MetricsToolResponse {
    enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    components: Option<MetricsComponents>,
    #[serde(skip_serializing_if = "Option::is_none")]
    transcripts_dir: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_scrape_ts: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    totals_7d: Option<PeriodTotalsResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    totals_30d: Option<PeriodTotalsResponse>,
    #[serde(skip_serializing_if = "Option::is_none")]
    last_sessions: Option<Vec<SessionToolRecord>>,
    /// Pre-rendered text panel; print verbatim in the /metrics skill.
    /// Absent when enabled=false.
    #[serde(skip_serializing_if = "Option::is_none")]
    rendered_panel: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct MetricsComponents {
    recall_proxy: bool,
    session_accounting: bool,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct PeriodTotalsResponse {
    recalls: i64,
    bundle_tokens: i64,
    ledger_tokens: i64,
    sessions: i64,
    input_tokens: i64,
    output_tokens: i64,
    cache_read_tokens: i64,
    cache_creation_tokens: i64,
    context_offset_pct: Option<f64>,
}

impl From<crate::metrics::formatter::PeriodTotals> for PeriodTotalsResponse {
    fn from(t: crate::metrics::formatter::PeriodTotals) -> Self {
        let pct = t.context_offset_pct();
        Self {
            recalls: t.recalls,
            bundle_tokens: t.bundle_tokens,
            ledger_tokens: t.ledger_tokens,
            sessions: t.sessions,
            input_tokens: t.input_tokens,
            output_tokens: t.output_tokens,
            cache_read_tokens: t.cache_read_tokens,
            cache_creation_tokens: t.cache_creation_tokens,
            context_offset_pct: pct,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct SessionToolRecord {
    session_id: String,
    agent: String,
    started_at: String,
    ended_at: Option<String>,
    /// Total input tokens for this session.
    #[serde(rename = "input")]
    input_tokens: i64,
    /// Total output tokens for this session.
    #[serde(rename = "output")]
    output_tokens: i64,
    recall_calls: i64,
}

impl From<crate::metrics::formatter::SessionSummary> for SessionToolRecord {
    fn from(s: crate::metrics::formatter::SessionSummary) -> Self {
        Self {
            session_id: s.session_id,
            agent: s.agent,
            started_at: s.started_at,
            ended_at: s.ended_at,
            input_tokens: s.input_tokens,
            output_tokens: s.output_tokens,
            recall_calls: s.recall_calls,
        }
    }
}

impl From<commands::metrics::MetricsToolData> for MetricsToolResponse {
    fn from(d: commands::metrics::MetricsToolData) -> Self {
        if !d.enabled {
            return Self {
                enabled: false,
                components: None,
                transcripts_dir: None,
                last_scrape_ts: None,
                totals_7d: None,
                totals_30d: None,
                last_sessions: None,
                rendered_panel: None,
            };
        }
        Self {
            enabled: true,
            components: Some(MetricsComponents {
                recall_proxy: d.recall_proxy,
                session_accounting: d.session_accounting,
            }),
            transcripts_dir: d.claude_transcripts_dir,
            last_scrape_ts: d.last_scrape_ts,
            totals_7d: Some(PeriodTotalsResponse::from(d.totals_7d)),
            totals_30d: Some(PeriodTotalsResponse::from(d.totals_30d)),
            last_sessions: Some(
                d.last_sessions
                    .into_iter()
                    .map(SessionToolRecord::from)
                    .collect(),
            ),
            rendered_panel: Some(d.rendered_panel),
        }
    }
}

fn map_tool_error(err: MemhubError) -> McpError {
    match err {
        MemhubError::InvalidInput(message) => McpError::invalid_params(message, None),
        other => McpError::internal_error(other.to_string(), None),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::commands::{command, decision, init, status, task};
    use crate::db;
    use rmcp::model::NumberOrString;
    use rusqlite::params;
    use serde_json::Value;
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
        assert!(!status.0.k9_detected);
        assert!(!status.0.k9_enabled);
        assert!(status.0.k9_drift.is_none());
    }

    #[test]
    fn mcp_status_reports_k9_state_when_detected_and_enabled() {
        let temp = tempdir().expect("tempdir");
        let docs = temp.path().join("agent_docs");
        std::fs::create_dir_all(&docs).expect("create agent_docs");
        std::fs::write(docs.join("project_state.md"), "# state").expect("marker");
        init::run(temp.path()).expect("init");

        let server = MemhubServer::new(temp.path().to_path_buf());
        let status = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(server.status_impl())
            .expect("status");

        assert!(status.0.k9_detected);
        assert!(status.0.k9_enabled);
        assert_eq!(status.0.k9_agent_docs_path, "agent_docs");
        assert!(status.0.k9_drift.is_none());
    }

    #[test]
    fn mcp_tools_read_existing_records() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        decision::add(
            temp.path(),
            "Use indexed MCP lookups",
            "Keep reads predictable.",
            "user",
            "cli:user",
        )
        .expect("decision");
        task::add(
            temp.path(),
            "Ship MCP server",
            Some("Milestone 3"),
            "cli:user",
        )
        .expect("task");
        command::verify(temp.path(), "build", "cargo build", 0, "cli:user").expect("command");

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
        let actor = ClientIdentity {
            normalized: "codex".to_string(),
            raw: "openai-codex".to_string(),
        };
        let result = runtime
            .block_on(server.record_command_impl(
                Parameters(RecordCommandParams {
                    kind: "test".to_string(),
                    cmdline: "cargo test".to_string(),
                    exit_code: 0,
                }),
                actor,
            ))
            .expect("record command");

        assert!(result.0.created);
        let stored = command::latest_by_kind(temp.path(), "test")
            .expect("get command")
            .expect("command row");
        assert_eq!(stored.cmdline, "cargo test");
        assert_eq!(stored.last_exit_code, Some(0));

        let ctx = crate::db::open_project(temp.path()).expect("open");
        let actor_logged: String = ctx
            .conn
            .query_row(
                "SELECT actor FROM writes_log
                 WHERE table_name = 'commands'
                 ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .expect("query writes_log");
        assert_eq!(actor_logged, "codex");
    }

    #[test]
    fn normalize_client_aliases() {
        assert_eq!(normalize_client_name("claude-ai"), "claude-code");
        assert_eq!(normalize_client_name("Claude Code"), "claude-code");
        assert_eq!(normalize_client_name("openai-codex"), "codex");
        assert_eq!(normalize_client_name("CustomClient"), "customclient");
    }

    #[test]
    fn current_client_identity_preserves_exact_raw_name() {
        let identity =
            current_client_identity_from_initialize(Some(&InitializeRequestParams::new(
                Default::default(),
                Implementation::new(" openai-codex ", "1.2.3"),
            )));

        assert_eq!(identity.normalized, "codex");
        assert_eq!(identity.raw, " openai-codex ");
    }

    #[test]
    fn sanitize_for_log_escapes_control_characters() {
        let sanitized = sanitize_for_log("codex\ncli\t\u{7}");

        assert!(!sanitized.contains('\n'));
        assert!(!sanitized.contains('\t'));
        assert!(!sanitized.contains('\u{7}'));
        assert!(sanitized.contains("\\n"));
        assert!(sanitized.contains("\\t"));
    }

    #[test]
    fn pending_write_provenance_captures_available_mcp_context() {
        let mut initialize = InitializeRequestParams::new(
            Default::default(),
            Implementation::new(" openai-codex ", "1.2.3"),
        )
        .with_protocol_version(rmcp::model::ProtocolVersion::V_2025_06_18);
        initialize.meta = Some(Meta(
            [("workspace".to_string(), Value::String("repo".to_string()))]
                .into_iter()
                .collect(),
        ));
        let request_meta = Meta(
            [(
                "progressToken".to_string(),
                Value::String("progress-1".to_string()),
            )]
            .into_iter()
            .collect(),
        );

        let provenance_json = pending_write_provenance_json(
            &NumberOrString::String("req-7".into()),
            &request_meta,
            Some(&initialize),
        );
        let provenance: Value = serde_json::from_str(&provenance_json).expect("json");

        assert_eq!(provenance["source"], "mcp");
        assert_eq!(provenance["request_id"], "req-7");
        assert_eq!(provenance["request_meta"]["progressToken"], "progress-1");
        assert_eq!(provenance["protocol_version"], "2025-06-18");
        assert_eq!(provenance["client_name"], " openai-codex ");
        assert_eq!(provenance["client_version"], "1.2.3");
        assert_eq!(provenance["initialize_meta"]["workspace"], "repo");
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

        let fact_provenance = pending_write_provenance_json(
            &NumberOrString::String("req-fact".into()),
            &Meta::default(),
            Some(&InitializeRequestParams::new(
                Default::default(),
                Implementation::new(" openai-codex ", "1.2.3"),
            )),
        );
        let decision_provenance = pending_write_provenance_json(
            &NumberOrString::String("req-decision".into()),
            &Meta::default(),
            Some(&InitializeRequestParams::new(
                Default::default(),
                Implementation::new("claude-ai", "2.0.0"),
            )),
        );

        let fact_result = runtime
            .block_on(server.propose_fact_impl(
                Parameters(ProposeFactParams {
                    key: "build-command".to_string(),
                    value: "cargo build".to_string(),
                    rationale: "Observed in this repo and should be reviewed.".to_string(),
                }),
                ClientIdentity {
                    normalized: "codex".to_string(),
                    raw: " openai-codex ".to_string(),
                },
                fact_provenance.clone(),
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
                decision_provenance.clone(),
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
        let staged_fact: (String, String, String, String, String) = ctx
            .conn
            .query_row(
                "SELECT payload_json, rationale, actor, actor_raw, provenance_json
                 FROM pending_writes
                 WHERE kind = 'fact'
                 LIMIT 1",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
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
        assert_eq!(staged_fact.3, " openai-codex ");
        assert_eq!(staged_fact.4, fact_provenance);
        assert!(staged_decision.contains("\"title\":\"Keep staged writes narrow\""));
        assert_eq!(durable_fact_count, 0);
        assert_eq!(durable_decision_count, 0);
        assert_eq!(summary.pending_writes, 2);
    }

    #[test]
    fn mcp_log_session_note_persists_with_client_identity() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");

        let server = MemhubServer::new(temp.path().to_path_buf());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let response = runtime
            .block_on(server.log_session_note_impl(
                Parameters(LogSessionNoteParams {
                    text: "experimented with router fallback, no clear winner yet".to_string(),
                }),
                ClientIdentity {
                    normalized: "claude-code".to_string(),
                    raw: "claude-ai".to_string(),
                },
            ))
            .expect("log session note");

        assert!(response.0.id > 0);
        assert_eq!(response.0.actor, "claude-code");
        assert_eq!(response.0.actor_raw, "claude-ai");
        assert!(!response.0.created_at.is_empty());

        let ctx = db::open_project(temp.path()).expect("open");
        let stored_text: String = ctx
            .conn
            .query_row(
                "SELECT text FROM session_notes WHERE id = ?1",
                params![response.0.id],
                |row| row.get(0),
            )
            .expect("note row exists");
        assert!(stored_text.contains("router fallback"));

        let (audit_actor, audit_table, audit_action): (String, String, String) = ctx
            .conn
            .query_row(
                "SELECT actor, table_name, action FROM writes_log
                 WHERE table_name = 'session_notes'
                 ORDER BY id DESC LIMIT 1",
                params![],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .expect("audit row exists");
        assert_eq!(audit_actor, "claude-code");
        assert_eq!(audit_table, "session_notes");
        assert_eq!(audit_action, "insert");
    }

    #[test]
    fn mcp_list_pending_writes_returns_staged_proposals() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");

        crate::commands::pending_write::propose_fact(
            temp.path(),
            "build-command",
            "cargo build",
            "Observed in repo.",
            "codex",
            "openai-codex",
            "{\"source\":\"mcp\"}",
        )
        .expect("propose fact");
        crate::commands::pending_write::propose_decision(
            temp.path(),
            "Adopt the kraken pattern",
            "Sea creatures organize concurrent workloads cleanly.",
            "claude-code",
            "claude-ai",
            "{\"source\":\"mcp\"}",
        )
        .expect("propose decision");

        let server = MemhubServer::new(temp.path().to_path_buf());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let response = runtime
            .block_on(
                server.list_pending_writes_impl(Parameters(ListPendingWritesParams::default())),
            )
            .expect("list pending writes");

        assert_eq!(response.0.pending_writes.len(), 2);
        let kinds: Vec<_> = response
            .0
            .pending_writes
            .iter()
            .map(|p| p.kind.as_str())
            .collect();
        assert!(kinds.contains(&"fact"));
        assert!(kinds.contains(&"decision"));
        assert!(
            response
                .0
                .pending_writes
                .iter()
                .all(|p| p.status == "pending")
        );
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

        let provenance_json = pending_write_provenance_json(
            &NumberOrString::Number(7),
            &Meta::default(),
            Some(&InitializeRequestParams::new(
                Default::default(),
                Implementation::new("codex", "1.0.0"),
            )),
        );

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
                provenance_json,
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

    #[test]
    fn mcp_task_add_writes_directly_with_agent_attribution() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");

        let server = MemhubServer::new(temp.path().to_path_buf());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let result = runtime
            .block_on(server.task_add_impl(
                Parameters(TaskAddParams {
                    title: "Refactor cache layer".to_string(),
                    notes: Some("Noted mid-session".to_string()),
                }),
                ClientIdentity {
                    normalized: "codex".to_string(),
                    raw: "openai-codex".to_string(),
                },
            ))
            .expect("task_add");

        assert_eq!(result.0.title, "Refactor cache layer");
        assert_eq!(result.0.status, "open");
        assert_eq!(result.0.actor, "codex");

        let tasks = task::list(temp.path(), Some("open")).expect("list tasks");
        assert_eq!(tasks.len(), 1);
        assert_eq!(tasks[0].title, "Refactor cache layer");
    }

    #[test]
    fn mcp_task_done_marks_existing_task() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let task_id = task::add(temp.path(), "Wire MCP server", None, "cli:user").expect("seed");

        let server = MemhubServer::new(temp.path().to_path_buf());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let result = runtime
            .block_on(server.task_done_impl(
                Parameters(TaskDoneParams { id: task_id }),
                ClientIdentity {
                    normalized: "claude-code".to_string(),
                    raw: "claude-ai".to_string(),
                },
            ))
            .expect("task_done");

        assert_eq!(result.0.id, task_id);
        assert_eq!(result.0.status, "done");

        let remaining = task::list(temp.path(), Some("open")).expect("open tasks");
        assert!(remaining.is_empty());
    }

    #[test]
    fn mcp_list_facts_returns_durable_rows() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        crate::commands::fact::add(
            temp.path(),
            "build-command",
            "cargo build",
            "user",
            "cli:user",
        )
        .expect("seed fact");

        let server = MemhubServer::new(temp.path().to_path_buf());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let result = runtime
            .block_on(server.list_facts_impl(Parameters(ListFactsParams::default())))
            .expect("list_facts");

        assert_eq!(result.0.facts.len(), 1);
        assert_eq!(result.0.facts[0].key, "build-command");
        assert_eq!(result.0.facts[0].value, "cargo build");
        assert_eq!(result.0.facts[0].source, "user");
        assert!(!result.0.facts[0].is_stale);
    }

    #[test]
    fn mcp_recall_returns_ranked_hits_and_provenance() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        decision::add(
            temp.path(),
            "Stage agent-originated writes before promotion",
            "Agents propose facts and decisions but durable rows require human review.",
            "user+agent:claude-code",
            "cli:user",
        )
        .expect("decision");
        task::add(
            temp.path(),
            "Ship recall surface",
            Some("PR4 of M8 rolls out recall CLI plus MCP tool."),
            "cli:user",
        )
        .expect("task");

        let server = MemhubServer::new(temp.path().to_path_buf());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let response = runtime
            .block_on(server.recall_impl(Parameters(RecallParams {
                query: "agent originated writes review".to_string(),
                mode: Some("fts".to_string()),
                max_results: Some(3),
                source_types: None,
                accepted_only: None,
                include_stale: None,
            })))
            .expect("recall");

        assert_eq!(response.0.mode, "fts");
        assert!(!response.0.results.is_empty());
        assert_eq!(response.0.results[0].source_type, "decision");
        assert!(response.0.provenance.matcher.starts_with("recall:"));
    }

    #[test]
    fn mcp_recall_rejects_invalid_mode() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let server = MemhubServer::new(temp.path().to_path_buf());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let result = runtime.block_on(server.recall_impl(Parameters(RecallParams {
            query: "anything".to_string(),
            mode: Some("turbo".to_string()),
            max_results: None,
            source_types: None,
            accepted_only: None,
            include_stale: None,
        })));
        let err = match result {
            Ok(_) => panic!("invalid mode should error"),
            Err(e) => e,
        };
        let message = err.message.to_string();
        assert!(message.contains("turbo"), "unexpected error: {message}");
    }

    #[test]
    fn mcp_render_regenerates_local_docs_from_db() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");

        let server = MemhubServer::new(temp.path().to_path_buf());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let actor = ClientIdentity {
            normalized: "claude-code".to_string(),
            raw: "claude-ai".to_string(),
        };
        let result = runtime.block_on(server.render_impl(actor)).expect("render");

        assert!(
            std::path::Path::new(&result.0.project_md_path).exists(),
            "PROJECT.md should be written"
        );
        assert!(
            std::path::Path::new(&result.0.ledger_md_path).exists(),
            "PROJECT_LEDGER.md should be written"
        );

        let ctx = crate::db::open_project(temp.path()).expect("open");
        let actor_logged: String = ctx
            .conn
            .query_row(
                "SELECT actor FROM writes_log
                 WHERE table_name = 'render'
                 ORDER BY id DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .expect("query writes_log");
        assert_eq!(actor_logged, "claude-code");
    }

    fn run_metrics(server: &MemhubServer) -> Json<MetricsToolResponse> {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(server.metrics_impl())
            .expect("metrics")
    }

    #[test]
    fn mcp_metrics_returns_disabled_state_when_not_enabled() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        // metrics.enabled defaults to false — no additional setup needed.
        let server = MemhubServer::new(temp.path().to_path_buf());
        let resp = run_metrics(&server);
        assert!(!resp.0.enabled);
        assert!(resp.0.totals_7d.is_none());
        assert!(resp.0.rendered_panel.is_none());
    }

    #[test]
    fn mcp_metrics_returns_no_data_panel_when_enabled_but_empty() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        crate::commands::metrics::enable(temp.path()).expect("enable");

        let server = MemhubServer::new(temp.path().to_path_buf());
        let resp = run_metrics(&server);
        assert!(resp.0.enabled);
        let panel = resp.0.rendered_panel.expect("rendered_panel");
        assert!(
            panel.contains("no recall or session data"),
            "unexpected panel: {panel}"
        );
        assert!(resp.0.totals_7d.is_some());
    }

    #[test]
    fn mcp_metrics_returns_full_panel_when_session_data_present() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        crate::commands::metrics::enable(temp.path()).expect("enable");

        // Inject a synthetic session row so the panel has data.
        {
            let ctx = crate::db::open_project(temp.path()).expect("open");
            ctx.conn
                .execute(
                    "INSERT INTO session_metrics \
                     (session_id, agent, started_at, ended_at, \
                      input_tokens, output_tokens, cache_read_tokens, \
                      cache_creation_tokens, recall_calls) \
                     VALUES ('test-session-001', 'claude-code', \
                             datetime('now', '-1 hour'), datetime('now'), \
                             1000, 500, 200, 100, 3)",
                    [],
                )
                .expect("insert session");
        }

        let server = MemhubServer::new(temp.path().to_path_buf());
        let resp = run_metrics(&server);
        assert!(resp.0.enabled);
        let panel = resp.0.rendered_panel.expect("rendered_panel");
        assert!(panel.contains("Last 7 days"), "panel: {panel}");
        assert!(panel.contains("Last 30 days"), "panel: {panel}");
        let sessions = resp.0.last_sessions.expect("last_sessions");
        assert_eq!(sessions.len(), 1);
        assert_eq!(sessions[0].input_tokens, 1000);
        assert_eq!(sessions[0].recall_calls, 3);
    }
}
