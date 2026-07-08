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
use crate::code_index;
use crate::code_index::locate::{LocateHit, LocateOptions, LocateResponse};
use crate::commands;
use crate::config::{PathMatcher, RetrievalMode};
use crate::models::{
    CommandRecord, Decision, Fact, PendingWriteRecord, RenderResult, SearchResult, StatusSummary,
    Task,
};
use crate::retrieval::{self, RecallHit, RecallOptions, RecallResponse, RecallWarning, SourceType};

/// Warm the bundled embed + rerank ONNX models: one `embed_one` plus a
/// tiny one-doc rerank. Called on a background thread from `serve` so the
/// first real recall/locate call doesn't pay the ~2-3s cold session-init
/// cost; never panics — a failure is logged and otherwise ignored
/// (issue #71).
fn warm_models() {
    if let Err(err) = retrieval::embed_one("memhub mcp warm-up") {
        log::warn!("mcp warm-up: embedding model failed to load: {err}");
        return;
    }
    if let Err(err) =
        retrieval::rerank::rerank("memhub mcp warm-up", &["memhub mcp warm-up".to_string()])
    {
        log::warn!("mcp warm-up: reranker model failed to load: {err}");
    }
}

pub fn serve(start: &Path) -> crate::Result<()> {
    // Independent of the tokio runtime below, so it never delays or
    // blocks serve startup.
    std::thread::spawn(warm_models);

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
        let id = commands::pending_write::propose_fact_scoped(
            &self.start,
            &params.key,
            &params.value,
            &params.rationale,
            params.global,
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
        let id = commands::pending_write::propose_decision_scoped(
            &self.start,
            &params.title,
            &params.rationale,
            params.global,
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

    async fn propose_supersede_impl(
        &self,
        Parameters(params): Parameters<ProposeSupersedeParams>,
        actor: ClientIdentity,
        provenance_json: String,
    ) -> std::result::Result<Json<ProposeSupersedeToolResponse>, McpError> {
        let id = commands::pending_write::propose_supersede(
            &self.start,
            &params.target_kind,
            &params.old,
            &params.new,
            &params.rationale,
            &actor.normalized,
            &actor.raw,
            &provenance_json,
        )
        .map_err(map_tool_error)?;

        Ok(Json(ProposeSupersedeToolResponse {
            id,
            status: "pending".to_string(),
            actor: actor.normalized,
            actor_raw: actor.raw,
            target_kind: params.target_kind,
            old: params.old,
            new: params.new,
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
        // MCP-only confinement gate (Wave-0 F11, decision Q39): doc_add is
        // the one write surface that hands memhub an entirely
        // agent-supplied filesystem path, so it is canonicalized and
        // checked against the repo root / [doc] allowed_dirs / deny_list
        // independently, BEFORE calling into the CLI-shared
        // `commands::doc::add` — which stays deliberately unrestricted for
        // the user-typed CLI path (see `commands::doc::prepare_doc`).
        let confined_path = {
            let ctx = crate::db::open_project(&self.start).map_err(map_tool_error)?;
            confine_doc_add_path(
                &ctx.paths.repo_root,
                &ctx.config.doc.allowed_dirs,
                &ctx.config.deny_list.patterns,
                std::path::Path::new(&params.file),
            )
            .map_err(map_tool_error)?
        };

        let outcome = commands::doc::add(
            &self.start,
            &confined_path,
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
            chunks: usize_to_i64(outcome.chunk_count),
            status: status.to_string(),
            enabled_default_recall: outcome.enabled_default_recall,
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

    async fn locate_impl(
        &self,
        Parameters(params): Parameters<LocateParams>,
    ) -> std::result::Result<Json<LocateToolResponse>, McpError> {
        let response = code_index::locate::locate(
            &self.start,
            LocateOptions {
                query: params.query,
                limit: params.limit.unwrap_or(0),
                use_reranker: params.rerank.unwrap_or(false),
                // `--no-refresh` is a CLI-only opt-in for tight repeat-call
                // loops (issue #67); the MCP tool always keeps the
                // lazy-freshness guarantee.
                no_refresh: false,
            },
        )
        .map_err(map_tool_error)?;
        Ok(Json(LocateToolResponse::from(response)))
    }

    /// Resolve a `sync_*` tool's target dir: an explicit `remote`
    /// override if given, else the canonical
    /// `<drive_subpath>/memhub/<project_id>` from config.
    fn resolve_sync_remote(&self, remote: Option<&str>) -> std::result::Result<PathBuf, McpError> {
        match remote {
            Some(p) if !p.trim().is_empty() => Ok(PathBuf::from(p)),
            _ => commands::sync::default_remote_dir(&self.start).map_err(map_tool_error),
        }
    }

    async fn sync_status_impl(
        &self,
    ) -> std::result::Result<Json<SyncStatusToolResponse>, McpError> {
        let s = commands::sync::enablement_status(&self.start).map_err(map_tool_error)?;
        Ok(Json(SyncStatusToolResponse::from(s)))
    }

    async fn sync_snapshot_impl(
        &self,
        Parameters(params): Parameters<SyncSnapshotParams>,
    ) -> std::result::Result<Json<SyncSnapshotToolResponse>, McpError> {
        let out_dir = self.resolve_sync_remote(params.remote.as_deref())?;
        // Push-side clobber gate (F12): without `force` the snapshot refuses
        // to overwrite a remote that is drive-ahead of or diverged from local.
        let summary = commands::sync::snapshot(&self.start, &out_dir, params.force.unwrap_or(false))
            .map_err(map_tool_error)?;
        Ok(Json(SyncSnapshotToolResponse::from(summary)))
    }

    async fn sync_check_impl(
        &self,
        Parameters(params): Parameters<SyncRemoteParams>,
    ) -> std::result::Result<Json<SyncCheckToolResponse>, McpError> {
        let remote = self.resolve_sync_remote(params.remote.as_deref())?;
        let report = commands::sync::check(&self.start, &remote).map_err(map_tool_error)?;
        Ok(Json(SyncCheckToolResponse::from((
            report,
            remote.display().to_string(),
        ))))
    }

    async fn sync_adopt_impl(
        &self,
        Parameters(params): Parameters<SyncAdoptParams>,
    ) -> std::result::Result<Json<SyncAdoptToolResponse>, McpError> {
        let remote = self.resolve_sync_remote(params.remote.as_deref())?;
        // Destructive overwrite of the local DB. The MCP gate is an
        // explicit `confirm: true` (maps to the CLI `--yes`); without
        // it, return the would-change verdict so the agent surfaces it
        // to the user before any swap (decision 103: operator-gated).
        if !params.confirm.unwrap_or(false) {
            let report = commands::sync::check(&self.start, &remote).map_err(map_tool_error)?;
            return Ok(Json(SyncAdoptToolResponse {
                adopted: false,
                reason: Some(
                    "adopt overwrites the local DB with the Drive snapshot; re-call with \
                     confirm=true after the user approves"
                        .to_string(),
                ),
                verdict: Some(report.verdict.as_str().to_string()),
                project_id_mismatch: report.project_id_mismatch,
                schema_blocks_adopt: Some(report.schema_blocks_adopt),
                project_id: None,
                adopted_from_machine: None,
                previous_schema: None,
                new_schema: None,
                baseline: None,
                backup_path: None,
            }));
        }
        let summary = commands::sync::adopt(&self.start, &remote, true).map_err(map_tool_error)?;
        Ok(Json(SyncAdoptToolResponse::from(summary)))
    }

    async fn sync_commit_impl(
        &self,
        Parameters(params): Parameters<SyncRemoteParams>,
    ) -> std::result::Result<Json<SyncCommitToolResponse>, McpError> {
        let remote = self.resolve_sync_remote(params.remote.as_deref())?;
        let summary = commands::sync::commit(&self.start, &remote).map_err(map_tool_error)?;
        Ok(Json(SyncCommitToolResponse::from(summary)))
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
        description = "Stage a proposed fact — a durable, user-confirmable truth about this repo (a build/test/run command, a toolchain version, a standing constraint) — into pending_writes for human review. NEVER durable until the user runs `memhub review accept`. Use when you've inferred something worth persisting beyond the session, not for a transient observation or a to-do."
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
        description = "Stage a proposed decision — a durable record of a choice and the rationale behind it (what was decided and WHY) — into pending_writes for human review. NEVER durable until the user runs `memhub review accept`. Use to capture an architectural or design decision worth preserving."
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
        name = "propose_supersede",
        description = "Stage a proposed SUPERSESSION — retire an outdated fact or decision by linking it to the row that replaces it — into pending_writes for human review. NEVER writes durably: the demote-with-link (the old row is kept, tagged, and penalized in recall, never deleted) happens only when the user runs `memhub review accept`. Set target_kind to \"fact\" or \"decision\"; `old` and `new` identify the retired row and its replacement (fact: numeric id or exact key; decision: numeric id). Use when you find a durable fact/decision that a newer one has clearly replaced — you are proposing the retirement, not performing it (untrusted-writer guardrail)."
    )]
    async fn propose_supersede(
        &self,
        params: Parameters<ProposeSupersedeParams>,
        request_context: RequestContext<RoleServer>,
    ) -> std::result::Result<Json<ProposeSupersedeToolResponse>, McpError> {
        let actor = current_client_identity(&request_context);
        let provenance_json = current_pending_write_provenance(&request_context);
        self.propose_supersede_impl(params, actor, provenance_json)
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
        description = "Create a task directly in the durable tasks table — a concrete future to-do or follow-up for this repo. Tasks are intent, not truth claims: a direct write with no review gate; the user prunes. Use to record work to be done later, not to assert that something is already true (propose_fact) or to capture why a choice was made (propose_decision)."
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
        description = "Ingest (or re-ingest) a local markdown file as an external reference document, chunked and RAG-searchable. Direct write: a doc is a user-pointed artifact, not an agent claim, so no review gate. Docs are OPT-IN to recall — query them with recall(source_types=[\"doc\"]) to scope to docs alone; after a repo's first doc add they also join the default recall bundle when a chunk clears the relevance floor (decision 90). Unchanged content (same hash) is a no-op; changed content replaces every chunk. The path must canonicalize under the repo root or a configured [doc] allowed_dirs entry and must not match the deny-list; otherwise this refuses with a clear error (Wave-0 F11)."
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
        description = "Retrieve relevant facts, decisions, and tasks via SQL+RAG hybrid recall (FTS5 + brute-force cosine when hybrid mode is configured). Read-only; prefer this over reading PROJECT_LEDGER.md mid-session. Ingested reference docs are opt-in (add one with doc_add); once added they join the default bundle when a chunk clears the relevance floor (decision 90), and source_types=[\"doc\"] scopes a query to docs alone. The response's `available_docs` counts ingested doc chunks that did NOT surface this call — when it is non-zero and the question is design/spec/architecture-flavored, consider a follow-up recall scoped to docs (use judgment; not every turn)."
    )]
    async fn recall(
        &self,
        params: Parameters<RecallParams>,
    ) -> std::result::Result<Json<RecallToolResponse>, McpError> {
        self.recall_impl(params).await
    }

    #[tool(
        name = "locate",
        description = "Locate code in this repo by intent (M11). SQL+RAG hybrid search (FTS5 BM25 + cosine when hybrid mode is configured) over a sibling code index at .memhub/code_index.sqlite, lazily refreshed to the working tree on each call. Returns ranked breadcrumbs — {path, start_line, end_line, symbol, kind, score, snippet} — where `snippet` is a CLIPPED excerpt (≤6 lines), never the full chunk. Read-only: never returns whole files and never edits. Use this to find WHERE code lives before reading it with your own file tools. `rerank` runs the bundled cross-encoder over the candidate pool (off by default. Fusion (reranker off) is the default and wins Recall@3; rerank wins single-best-guess Recall@1 — decisions 122/123)."
    )]
    async fn locate(
        &self,
        params: Parameters<LocateParams>,
    ) -> std::result::Result<Json<LocateToolResponse>, McpError> {
        self.locate_impl(params).await
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

    #[tool(
        name = "sync_status",
        description = "Cross-machine Drive sync: show enablement, the Drive-folder project id, the resolved remote dir (<drive_subpath>/memhub/<project_id>), the local logical version, and the last-sync marker. No Drive comparison. memhub stays offline; the synced folder (Google Drive for Desktop) is the transport."
    )]
    async fn sync_status(&self) -> std::result::Result<Json<SyncStatusToolResponse>, McpError> {
        self.sync_status_impl().await
    }

    #[tool(
        name = "sync_snapshot",
        description = "Write a consistent single-file DB snapshot + manifest into the synced Drive folder — the push; follow with sync_commit to record the baseline. Refuses when the remote is drive-ahead of or diverged from local (would clobber newer state) — pull first, or pass `force=true` for last-writer-wins. Defaults to the configured remote dir; pass `remote` to override. Requires `memhub sync enable` for this repo."
    )]
    async fn sync_snapshot(
        &self,
        params: Parameters<SyncSnapshotParams>,
    ) -> std::result::Result<Json<SyncSnapshotToolResponse>, McpError> {
        self.sync_snapshot_impl(params).await
    }

    #[tool(
        name = "sync_check",
        description = "Compare the local DB against the Drive snapshot and report the fast-forward verdict (up-to-date / local-ahead / drive-ahead / diverged / no-remote). Reads only the manifest, never the multi-MB snapshot. Surface `project_id_mismatch` and `schema_blocks_adopt` to the user — both block a safe adopt. Defaults to the configured remote dir; pass `remote` to override."
    )]
    async fn sync_check(
        &self,
        params: Parameters<SyncRemoteParams>,
    ) -> std::result::Result<Json<SyncCheckToolResponse>, McpError> {
        self.sync_check_impl(params).await
    }

    #[tool(
        name = "sync_adopt",
        description = "Replace the local DB with the Drive snapshot (the pull). DESTRUCTIVE and lossy on a diverged history. Gated: without `confirm=true` it returns the would-change verdict and refuses — surface that to the user and only re-call with confirm=true after they approve. Hard refusals regardless of confirm: project-id mismatch, a snapshot schema newer than this binary (run `memhub upgrade`), or a checksum that disagrees with the manifest. Defaults to the configured remote dir; pass `remote` to override."
    )]
    async fn sync_adopt(
        &self,
        params: Parameters<SyncAdoptParams>,
    ) -> std::result::Result<Json<SyncAdoptToolResponse>, McpError> {
        self.sync_adopt_impl(params).await
    }

    #[tool(
        name = "sync_commit",
        description = "Record that the local DB now equals the just-pushed snapshot, so the next sync_check reads up-to-date. Call after sync_snapshot. Defaults to the configured remote dir; pass `remote` to override."
    )]
    async fn sync_commit(
        &self,
        params: Parameters<SyncRemoteParams>,
    ) -> std::result::Result<Json<SyncCommitToolResponse>, McpError> {
        self.sync_commit_impl(params).await
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for MemhubServer {
    fn get_info(&self) -> ServerInfo {
        ServerInfo::new(ServerCapabilities::builder().enable_tools().build())
            .with_server_info(Implementation::new("memhub", env!("CARGO_PKG_VERSION")))
            .with_instructions(
                r#"memhub: local-first per-repo project memory. Routing rules below are ABSOLUTE when user intent matches.

INTENT → TOOL (always start here; do not fall through to Grep/Read/manual scan):
• past decisions, status of work, "is there a fact/task about X" → recall
• find code by what it does, "where is X", "I want to change Y" → locate
• token usage, context cost, recall savings → metrics
• ingest a markdown spec/design doc as searchable reference → doc_add
• new task / mark task done → task_add / task_done
• cross-machine pull/push of memhub state → sync_check, sync_snapshot, sync_adopt, sync_commit
• session start (turn 1 ONLY) → read .memhub/rendered/PROJECT.md once

OTHER (direct, use when explicitly needed): status, search, list_tasks, list_decisions, list_facts, list_pending_writes, get_command, render (regenerate PROJECT.md), sync_status, log_session_note (write-only scratch).

NEVER:
• Grep for code by intent before `locate` has narrowed candidates. Grep is for confirming inside files `locate` returned.
• Read PROJECT_LEDGER.md before trying `recall`. The ledger is fallback — when recall is empty or the user asks for it.
• Re-read PROJECT.md after turn 1 unless the user asks or after `render`.
• Write facts/decisions directly, or retire one yourself. Stage via `propose_fact` / `propose_decision` / `propose_supersede`; durable on `memhub review accept`.

OUT OF SCOPE (use other tools):
• External library / framework docs → your docs tool (e.g., context7) or web search.
• World knowledge / general programming concepts.
"#,
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
    /// Propose for the machine-global store instead of this repo. Still
    /// staged in pending_writes; durable in `~/.memhub/global.sqlite`
    /// only after human `memhub review accept` — the agent never writes
    /// global directly.
    #[serde(default)]
    global: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ProposeDecisionParams {
    title: String,
    rationale: String,
    /// Propose for the machine-global store instead of this repo,
    /// staged the same way; durable only after human
    /// `memhub review accept`.
    #[serde(default)]
    global: bool,
}

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct ProposeSupersedeParams {
    /// `"fact"` or `"decision"` — which durable table the supersession
    /// targets.
    target_kind: String,
    /// The row being retired. Fact: numeric id or exact key. Decision:
    /// numeric id. Resolved when the human accepts, not at propose time.
    old: String,
    /// The row that replaces it, same identifier rules as `old`.
    new: String,
    /// Why the old row should be retired in favor of the new one.
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
    deny_patterns: i64,
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
            deny_patterns: usize_to_i64(value.deny_patterns),
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
    /// `Some(new_decision_id)` once superseded (Wave 3 L3). Paired with
    /// `status == "superseded"`; the row is kept, not deleted.
    superseded_by: Option<i64>,
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
            superseded_by: value.superseded_by,
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

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct ProposeSupersedeToolResponse {
    id: i64,
    status: String,
    actor: String,
    actor_raw: String,
    target_kind: String,
    old: String,
    new: String,
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
    /// Path to the local markdown file to ingest. Must canonicalize to
    /// somewhere under the repo root or a configured `[doc] allowed_dirs`
    /// entry, and must not match the deny-list — otherwise this call
    /// refuses.
    file: String,
    /// Optional title override (defaults to first heading or file name).
    title: Option<String>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct DocAddToolResponse {
    id: i64,
    title: String,
    path: String,
    chunks: i64,
    /// `created` | `updated` | `unchanged`.
    status: String,
    /// True when this call flipped on default-bundle doc recall for
    /// the repo (first doc ingested). After this, strong topical doc
    /// matches surface in plain `recall`; `source_types=["doc"]` still
    /// scopes to docs only.
    enabled_default_recall: bool,
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
    source: String,
    verified_at: Option<String>,
    created_at: String,
    is_stale: bool,
    /// `Some(new_fact_id)` once superseded (Wave 3 L3). The fact is kept
    /// (demote-with-link); the link points at its replacement.
    superseded_by: Option<i64>,
}

impl From<Fact> for FactToolRecord {
    fn from(value: Fact) -> Self {
        Self {
            id: value.id,
            key: value.key,
            value: value.value,
            source: value.source,
            verified_at: value.verified_at,
            created_at: value.created_at,
            is_stale: value.is_stale,
            superseded_by: value.superseded_by,
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
    candidate_count: i64,
    returned_count: i64,
    /// Ingested doc chunks that exist but were NOT searched because the
    /// call did not scope to `doc`. Non-zero is a cue to consider a
    /// follow-up `recall(..., source_types=["doc"])` when the question is
    /// design/spec/architecture-flavored. After a repo's first doc add,
    /// doc chunks join the default bundle when they clear the floor (decision 90).
    available_docs: i64,
    warnings: Vec<RecallToolWarning>,
    provenance: RecallToolProvenance,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct RecallToolHit {
    source_type: String,
    /// `"repo"` or `"global"` (M9). Repo-local always wins on
    /// conflict; a `"global"` hit may be overridden by repo memory.
    scope: String,
    source_id: i64,
    title: String,
    body: String,
    stale: bool,
    /// `Some(new_id)` when this row was superseded by another of the same
    /// source type (Wave 3 L3). The hit is demoted, not dropped — repo
    /// memory stays no-loss. Only facts/decisions can carry it.
    superseded_by: Option<i64>,
    source: String,
    created_at: String,
    /// Cross-encoder relevance logit that decided this hit's place in
    /// `results` (array order is final rank; there is no separate rank
    /// field on this path). `Some` when the re-ranker ran for this query
    /// (hybrid mode + re-ranker enabled), `None` otherwise — the raw
    /// fusion/FTS/vector scores are diagnostic-only and not surfaced here
    /// (issue #72; see `memhub recall --json` for the full breakdown).
    rerank_score: Option<f32>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct RecallToolWarning {
    kind: String,
    stale_count: i64,
    total_count: i64,
    reason: String,
    fix: String,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct RecallToolProvenance {
    matcher: String,
    elapsed_ms: i64,
}

impl From<RecallHit> for RecallToolHit {
    fn from(value: RecallHit) -> Self {
        Self {
            source_type: value.source_type,
            scope: value.scope,
            source_id: value.source_id,
            title: value.title,
            body: value.body,
            stale: value.stale,
            superseded_by: value.superseded_by,
            source: value.source,
            created_at: value.created_at,
            rerank_score: value.rerank_score,
        }
    }
}

impl From<RecallWarning> for RecallToolWarning {
    fn from(value: RecallWarning) -> Self {
        Self {
            kind: value.kind,
            stale_count: usize_to_i64(value.stale_count),
            total_count: usize_to_i64(value.total_count),
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
            candidate_count: usize_to_i64(value.candidate_count),
            returned_count: usize_to_i64(value.returned_count),
            available_docs: usize_to_i64(value.available_docs),
            warnings: value
                .warnings
                .into_iter()
                .map(RecallToolWarning::from)
                .collect(),
            provenance: RecallToolProvenance {
                matcher: value.matcher,
                elapsed_ms: u128_to_i64(value.elapsed_ms),
            },
        }
    }
}

// ── Code locator (M11) tool shapes ──────────────────────────────────

#[derive(Debug, Deserialize, schemars::JsonSchema)]
struct LocateParams {
    /// Natural-language description of the code you're looking for.
    query: String,
    /// Max results. 0 / omitted uses the locator default (10).
    limit: Option<usize>,
    /// Run the bundled cross-encoder over the candidate pool. Off by
    /// default; ignored in `fts` mode (no embed pool to reorder).
    rerank: Option<bool>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct LocateToolResponse {
    query: String,
    mode: String,
    results: Vec<LocateToolHit>,
    /// Distinct chunks that matched before truncation to `limit`.
    candidate_count: i64,
    returned_count: i64,
    /// Whether the cross-encoder actually ran this call.
    reranked: bool,
    /// Files / chunks in the index after the pre-query refresh.
    files_total: i64,
    chunks_total: i64,
    /// Indexed `HEAD` after the refresh, if resolvable.
    head: Option<String>,
    elapsed_ms: i64,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct LocateToolHit {
    rank: i64,
    /// Repo-relative, forward-slashed path.
    path: String,
    /// 1-indexed inclusive line range of the chunk.
    start_line: i64,
    end_line: i64,
    /// Symbol name when symbol-aware; `null` for line windows.
    symbol: Option<String>,
    /// Chunk kind tag (`function`, `struct`, `line-window`, …).
    kind: String,
    /// Blended fusion score.
    score: f64,
    fts_score: f64,
    vector_score: f64,
    /// Cross-encoder relevance logit when `rerank` ran, else `null`.
    rerank_score: Option<f32>,
    /// Clipped excerpt of the chunk body — NOT the full chunk.
    snippet: String,
}

impl From<LocateHit> for LocateToolHit {
    fn from(value: LocateHit) -> Self {
        Self {
            rank: usize_to_i64(value.rank),
            path: value.path,
            start_line: usize_to_i64(value.start_line),
            end_line: usize_to_i64(value.end_line),
            symbol: value.symbol,
            kind: value.kind,
            score: value.score,
            fts_score: value.fts_score,
            vector_score: value.vector_score,
            rerank_score: value.rerank_score,
            snippet: value.snippet,
        }
    }
}

impl From<LocateResponse> for LocateToolResponse {
    fn from(value: LocateResponse) -> Self {
        let mode = match value.mode {
            RetrievalMode::Fts => "fts".to_string(),
            RetrievalMode::Hybrid => "hybrid".to_string(),
        };
        Self {
            query: value.query,
            mode,
            results: value.results.into_iter().map(LocateToolHit::from).collect(),
            candidate_count: usize_to_i64(value.candidate_count),
            returned_count: usize_to_i64(value.returned_count),
            reranked: value.reranked,
            files_total: usize_to_i64(value.files_total),
            chunks_total: usize_to_i64(value.chunks_total),
            head: value.head,
            elapsed_ms: u128_to_i64(value.elapsed_ms),
        }
    }
}

// ── Cross-machine Drive sync (M10) tool shapes ──────────────────────

/// Logical content version, flattened for the MCP schema.
#[derive(Debug, Serialize, schemars::JsonSchema)]
struct LogicalVersionJson {
    writes_log_max_id: i64,
    writes_log_count: i64,
    digest: String,
}

impl From<commands::sync::LogicalVersion> for LogicalVersionJson {
    fn from(v: commands::sync::LogicalVersion) -> Self {
        Self {
            writes_log_max_id: v.writes_log_max_id,
            writes_log_count: v.writes_log_count,
            digest: v.digest,
        }
    }
}

/// Optional explicit remote dir. Omit to use the canonical
/// `<drive_subpath>/memhub/<project_id>` derived from config.
#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
struct SyncRemoteParams {
    remote: Option<String>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
struct SyncSnapshotParams {
    remote: Option<String>,
    /// Overwrite the remote even when it is drive-ahead of or diverged
    /// from local (last-writer-wins). Without it the push refuses rather
    /// than clobber newer remote state — surface the refusal to the user.
    force: Option<bool>,
}

#[derive(Debug, Deserialize, schemars::JsonSchema, Default)]
struct SyncAdoptParams {
    remote: Option<String>,
    /// Must be `true` to perform the destructive overwrite. Without it
    /// the tool returns the would-change verdict and refuses.
    confirm: Option<bool>,
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct SyncStatusToolResponse {
    enabled: bool,
    project_id: Option<String>,
    project_id_error: Option<String>,
    drive_subpath: String,
    /// Canonical `<drive_subpath>/memhub/<project_id>` push/pull dir.
    remote_dir: Option<String>,
    remote_dir_error: Option<String>,
    local_schema: String,
    local_logical: LogicalVersionJson,
    last_sync_at: Option<String>,
    last_sync_action: Option<String>,
}

impl From<commands::sync::SyncStatus> for SyncStatusToolResponse {
    fn from(s: commands::sync::SyncStatus) -> Self {
        let (project_id, project_id_error) = match s.project_id {
            Ok(id) => (Some(id), None),
            Err(e) => (None, Some(e)),
        };
        let (remote_dir, remote_dir_error) = match s.remote_dir {
            Ok(d) => (Some(d), None),
            Err(e) => (None, Some(e)),
        };
        Self {
            enabled: s.enabled,
            project_id,
            project_id_error,
            drive_subpath: s.drive_subpath,
            remote_dir,
            remote_dir_error,
            local_schema: s.local_schema,
            local_logical: s.local_logical.into(),
            last_sync_at: s.marker.as_ref().map(|m| m.synced_at.clone()),
            last_sync_action: s.marker.map(|m| m.last_action),
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct SyncSnapshotToolResponse {
    project_id: String,
    snapshot_path: String,
    manifest_path: String,
    schema_version: String,
    logical_version: LogicalVersionJson,
    file_sha256: String,
    bytes: i64,
}

impl From<commands::sync::SnapshotSummary> for SyncSnapshotToolResponse {
    fn from(s: commands::sync::SnapshotSummary) -> Self {
        Self {
            project_id: s.project_id,
            snapshot_path: s.snapshot_path.display().to_string(),
            manifest_path: s.manifest_path.display().to_string(),
            schema_version: s.schema_version,
            logical_version: s.logical_version.into(),
            file_sha256: s.file_sha256,
            bytes: s.bytes as i64,
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct SyncCheckToolResponse {
    /// `up-to-date` | `local-ahead` | `drive-ahead` | `diverged` | `no-remote`.
    verdict: String,
    baseline_present: bool,
    project_id: String,
    remote_dir: String,
    local_schema: String,
    local_logical: LogicalVersionJson,
    remote_schema: Option<String>,
    remote_logical: Option<LogicalVersionJson>,
    /// Snapshot schema is newer than this binary — adopt is refused;
    /// run `memhub upgrade` first.
    schema_blocks_adopt: bool,
    /// Set when the snapshot is for a different project — do NOT adopt.
    project_id_mismatch: Option<String>,
    remote_machine_id: Option<String>,
    remote_created_at: Option<String>,
}

impl From<(commands::sync::CheckReport, String)> for SyncCheckToolResponse {
    fn from((r, remote_dir): (commands::sync::CheckReport, String)) -> Self {
        Self {
            verdict: r.verdict.as_str().to_string(),
            baseline_present: r.baseline_present,
            project_id: r.project_id,
            remote_dir,
            local_schema: r.local_schema,
            local_logical: r.local_logical.into(),
            remote_schema: r.remote_schema,
            remote_logical: r.remote_logical.map(Into::into),
            schema_blocks_adopt: r.schema_blocks_adopt,
            project_id_mismatch: r.project_id_mismatch,
            remote_machine_id: r.remote_machine_id,
            remote_created_at: r.remote_created_at,
        }
    }
}

/// Adopt result. `adopted=false` is the confirm-gate refusal (carries
/// the would-change verdict); `adopted=true` carries the swap summary.
#[derive(Debug, Serialize, schemars::JsonSchema)]
struct SyncAdoptToolResponse {
    adopted: bool,
    reason: Option<String>,
    verdict: Option<String>,
    project_id_mismatch: Option<String>,
    schema_blocks_adopt: Option<bool>,
    project_id: Option<String>,
    adopted_from_machine: Option<String>,
    previous_schema: Option<String>,
    new_schema: Option<String>,
    baseline: Option<LogicalVersionJson>,
    backup_path: Option<String>,
}

impl From<commands::sync::AdoptSummary> for SyncAdoptToolResponse {
    fn from(s: commands::sync::AdoptSummary) -> Self {
        Self {
            adopted: true,
            reason: None,
            verdict: None,
            project_id_mismatch: None,
            schema_blocks_adopt: None,
            project_id: Some(s.project_id),
            adopted_from_machine: Some(s.adopted_from_machine),
            previous_schema: Some(s.previous_schema),
            new_schema: Some(s.new_schema),
            baseline: Some(s.baseline.into()),
            backup_path: Some(s.backup_path.display().to_string()),
        }
    }
}

#[derive(Debug, Serialize, schemars::JsonSchema)]
struct SyncCommitToolResponse {
    project_id: String,
    baseline: LogicalVersionJson,
}

impl From<commands::sync::CommitSummary> for SyncCommitToolResponse {
    fn from(s: commands::sync::CommitSummary) -> Self {
        Self {
            project_id: s.project_id,
            baseline: s.baseline.into(),
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
    current_client_identity_from_initialize(request_context.peer.peer_info().as_deref())
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
        request_context.peer.peer_info().as_deref(),
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
        // `codex-mcp-client` is Codex's actual MCP client name — without it,
        // Codex writes were bucketed under the raw string, escaping the
        // agent rollups (F15).
        "codex" | "codex-cli" | "openai-codex" | "codex-mcp-client" => "codex".to_string(),
        "opencode" | "open-code" | "opencode-cli" => "opencode".to_string(),
        other => {
            // An unmapped client still works (it buckets under its own
            // name); the warning surfaces the missing alias so it can be
            // added rather than silently fragmenting the metrics.
            log::warn!("mcp: unmapped client name '{}' — bucketing verbatim", sanitize_for_log(other));
            other.to_string()
        }
    }
}

fn sanitize_for_log(value: &str) -> String {
    value.chars().flat_map(char::escape_default).collect()
}

fn usize_to_i64(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

fn u128_to_i64(value: u128) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
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

// ── doc_add path confinement (Wave-0 F11, decision Q39) ─────────────────────

/// MCP-only confinement gate for `doc_add`.
///
/// `doc_add` is the one MCP write surface that hands memhub an entirely
/// agent-supplied filesystem path with no user keystroke behind it — a
/// prompt-injected tool call could otherwise point it at
/// `~/.aws/credentials` (or any other file) and have it durably
/// ingested into recall. This canonicalizes `candidate` (resolving
/// symlinks) and requires the result to land under `repo_root` or one
/// of `allowed_dirs`, then re-applies the repo's own deny-list — a
/// deny-listed path inside the repo root still refuses. Every failure
/// returns a plain `MemhubError::InvalidInput` naming the reason
/// (mapped to a clean `McpError::invalid_params` by `map_tool_error`);
/// nothing here panics.
///
/// Fails closed: an unresolvable `candidate` or `repo_root` is a
/// refusal, never a silent fallback to the raw, uncanonicalized path.
/// An `allowed_dirs` entry that itself fails to canonicalize (e.g. a
/// configured directory that does not exist on this machine) simply
/// cannot match anything — also fail closed, not a hard error, since a
/// bad allowlist entry is a config data issue, not grounds to refuse
/// every other call.
///
/// Deliberately NOT reused by `commands::doc::prepare_doc`, which stays
/// unrestricted for the user-typed CLI `doc add` path.
fn confine_doc_add_path(
    repo_root: &Path,
    allowed_dirs: &[PathBuf],
    deny_patterns: &[String],
    candidate: &Path,
) -> std::result::Result<PathBuf, MemhubError> {
    let canonical = candidate.canonicalize().map_err(|err| {
        MemhubError::InvalidInput(format!(
            "doc_add: cannot resolve '{}': {err}",
            candidate.display()
        ))
    })?;

    let repo_canonical = repo_root.canonicalize().map_err(|err| {
        MemhubError::InvalidInput(format!(
            "doc_add: cannot resolve repo root '{}': {err}",
            repo_root.display()
        ))
    })?;

    let matched_root = if canonical.starts_with(&repo_canonical) {
        Some(repo_canonical)
    } else {
        allowed_dirs.iter().find_map(|dir| {
            let dir_canonical = dir.canonicalize().ok()?;
            canonical
                .starts_with(&dir_canonical)
                .then_some(dir_canonical)
        })
    };

    let Some(root) = matched_root else {
        return Err(MemhubError::InvalidInput(format!(
            "doc_add: '{}' is outside the repo root and not listed in [doc] allowed_dirs",
            candidate.display()
        )));
    };

    let matcher = PathMatcher::from_patterns(deny_patterns)?;
    let relative = canonical.strip_prefix(&root).unwrap_or(&canonical);
    let relative_str = relative.to_string_lossy().replace('\\', "/");
    if matcher.is_denied(&relative_str) {
        return Err(MemhubError::InvalidInput(format!(
            "doc_add: '{}' matches the deny-list",
            candidate.display()
        )));
    }

    Ok(canonical)
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
    fn warm_models_completes_without_panicking() {
        // Exercises the exact call `serve` spawns on a background thread
        // (issue #71): one embed_one + a tiny one-doc rerank. Both models
        // are bundled, so this should succeed and leave the embedding
        // model's lazily-initialized handle ready for reuse.
        warm_models();
        assert!(
            retrieval::embed_one("post warm-up sanity check").is_ok(),
            "embedding model should be warm and reusable after warm_models()"
        );
    }

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
        assert_eq!(normalize_client_name("codex-mcp-client"), "codex");
        assert_eq!(normalize_client_name("Codex-MCP-Client"), "codex");
        assert_eq!(normalize_client_name("OpenCode"), "opencode");
        assert_eq!(normalize_client_name("opencode-cli"), "opencode");
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
                    global: false,
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
                    global: false,
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
                    global: false,
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
    fn opencode_mcp_identity_stages_pending_writes_as_opencode() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");

        let server = MemhubServer::new(temp.path().to_path_buf());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let raw = "OpenCode";
        let actor = ClientIdentity {
            normalized: normalize_client_name(raw),
            raw: raw.to_string(),
        };
        let provenance_json = pending_write_provenance_json(
            &NumberOrString::String("req-opencode".into()),
            &Meta::default(),
            Some(&InitializeRequestParams::new(
                Default::default(),
                Implementation::new(raw, "1.0.0"),
            )),
        );

        let result = runtime
            .block_on(server.propose_fact_impl(
                Parameters(ProposeFactParams {
                    key: "test-command".to_string(),
                    value: "cargo test".to_string(),
                    rationale: "OpenCode proposed a verified command candidate.".to_string(),
                    global: false,
                }),
                actor,
                provenance_json,
            ))
            .expect("propose fact");

        assert_eq!(result.0.actor, "opencode");

        let ctx = db::open_project(temp.path()).expect("open project");
        let staged: (String, String) = ctx
            .conn
            .query_row(
                "SELECT actor, actor_raw FROM pending_writes ORDER BY id DESC LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .expect("staged write");
        assert_eq!(staged.0, "opencode");
        assert_eq!(staged.1, "OpenCode");
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

    /// Run a git command in `repo`, asserting success. Locate's lazy
    /// refresh is git-aware, so the code-index test needs a real repo.
    fn git_in(repo: &Path, args: &[&str]) {
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(repo)
            .args(args)
            .status()
            .expect("spawn git");
        assert!(status.success(), "git {args:?} failed");
    }

    #[test]
    fn mcp_locate_returns_ranked_breadcrumbs() {
        let temp = tempdir().expect("tempdir");
        let root = temp.path();
        git_in(root, &["init"]);
        std::fs::create_dir_all(root.join("src")).expect("mkdir");
        std::fs::write(
            root.join("src/parser.rs"),
            "pub fn parse_manifest() -> bool { true }\n",
        )
        .expect("write");
        std::fs::write(root.join("src/render.rs"), "pub fn draw_widget() {}\n").expect("write");
        init::run(root).expect("init");
        git_in(root, &["add", "-A"]);
        git_in(
            root,
            &[
                "-c",
                "user.email=t@t",
                "-c",
                "user.name=t",
                "commit",
                "-m",
                "init",
            ],
        );

        let server = MemhubServer::new(root.to_path_buf());
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");

        let response = runtime
            .block_on(server.locate_impl(Parameters(LocateParams {
                query: "parse manifest".to_string(),
                limit: Some(5),
                rerank: None,
            })))
            .expect("locate");

        // Lazy refresh built the index on the fly and found the symbol.
        assert!(response.0.chunks_total >= 1);
        assert!(!response.0.results.is_empty(), "should locate the symbol");
        let top = &response.0.results[0];
        assert_eq!(top.path, "src/parser.rs");
        assert_eq!(top.symbol.as_deref(), Some("parse_manifest"));
        assert_eq!(top.kind, "function");
        assert!(top.start_line >= 1);
        // Breadcrumb only — a clipped snippet, never the full file.
        assert!(top.snippet.contains("parse_manifest"));
        // Reranker is off by default (PR3 contract).
        assert!(!response.0.reranked);
        assert!(top.rerank_score.is_none());
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

    /// Enable sync with a pinned project id + drive subpath so the
    /// remote dir resolves without a git remote.
    fn enable_sync_for(temp: &std::path::Path, drive: &std::path::Path) {
        let ctx = db::open_project(temp).expect("open");
        let mut cfg = ctx.config.clone();
        cfg.sync.enabled = true;
        cfg.sync.project_id = "mcp-test-proj".to_string();
        cfg.sync.drive_subpath = drive.display().to_string();
        cfg.save(&ctx.paths.config_path).expect("save config");
    }

    #[test]
    fn mcp_sync_status_reports_resolved_remote_dir() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let drive = temp.path().join("drive");
        enable_sync_for(temp.path(), &drive);

        let server = MemhubServer::new(temp.path().to_path_buf());
        let resp = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(server.sync_status_impl())
            .expect("sync_status");

        assert!(resp.0.enabled);
        let remote = resp.0.remote_dir.expect("remote_dir resolved");
        assert!(
            remote.ends_with(&format!("memhub{}mcp-test-proj", std::path::MAIN_SEPARATOR)),
            "remote dir is <drive>/memhub/<project_id>: {remote}"
        );
    }

    #[test]
    fn mcp_sync_adopt_refuses_without_confirm() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let drive = temp.path().join("drive");
        enable_sync_for(temp.path(), &drive);

        let server = MemhubServer::new(temp.path().to_path_buf());
        let resp = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(server.sync_adopt_impl(Parameters(SyncAdoptParams {
                remote: None,
                confirm: None,
            })))
            .expect("sync_adopt");

        assert!(!resp.0.adopted, "must refuse without confirm=true");
        assert!(resp.0.reason.is_some(), "refusal carries a reason");
        // No snapshot in the drive folder yet → verdict is no-remote.
        assert_eq!(resp.0.verdict.as_deref(), Some("no-remote"));
    }

    fn doc_add_actor() -> ClientIdentity {
        ClientIdentity {
            normalized: "claude-code".to_string(),
            raw: "claude-code".to_string(),
        }
    }

    #[test]
    fn mcp_doc_add_accepts_path_inside_repo_root() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let doc = temp.path().join("spec.md");
        std::fs::write(&doc, "# Spec\n\nbody\n").expect("write doc");

        let server = MemhubServer::new(temp.path().to_path_buf());
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(server.doc_add_impl(
                Parameters(DocAddParams {
                    file: doc.to_string_lossy().into_owned(),
                    title: None,
                }),
                doc_add_actor(),
            ))
            .expect("in-repo doc_add should succeed");

        assert_eq!(result.0.status, "created");
    }

    #[test]
    fn mcp_doc_add_refuses_path_outside_repo_root() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let outside = tempdir().expect("outside tempdir");
        let doc = outside.path().join("spec.md");
        std::fs::write(&doc, "# Spec\n\nbody\n").expect("write doc");

        let server = MemhubServer::new(temp.path().to_path_buf());
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(server.doc_add_impl(
                Parameters(DocAddParams {
                    file: doc.to_string_lossy().into_owned(),
                    title: None,
                }),
                doc_add_actor(),
            ));

        let err = match result {
            Ok(_) => panic!("path outside repo root and outside allowed_dirs must refuse"),
            Err(e) => e,
        };
        let message = err.message.to_string();
        assert!(
            message.contains("outside the repo root"),
            "unexpected error: {message}"
        );
    }

    #[test]
    fn mcp_doc_add_accepts_allowlisted_dir() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let outside = tempdir().expect("outside tempdir");
        let doc = outside.path().join("ext-spec.md");
        std::fs::write(&doc, "# External Spec\n\nbody\n").expect("write doc");

        let ctx = db::open_project(temp.path()).expect("open");
        let mut cfg = ctx.config.clone();
        cfg.doc.allowed_dirs = vec![outside.path().to_path_buf()];
        cfg.save(&ctx.paths.config_path).expect("save config");

        let server = MemhubServer::new(temp.path().to_path_buf());
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(server.doc_add_impl(
                Parameters(DocAddParams {
                    file: doc.to_string_lossy().into_owned(),
                    title: None,
                }),
                doc_add_actor(),
            ))
            .expect("allowlisted external path should succeed");

        assert_eq!(result.0.status, "created");
    }

    #[test]
    fn mcp_doc_add_refuses_deny_listed_path_inside_repo() {
        let temp = tempdir().expect("tempdir");
        init::run(temp.path()).expect("init");
        let doc = temp.path().join(".env");
        std::fs::write(&doc, "SECRET=1\n").expect("write doc");

        let server = MemhubServer::new(temp.path().to_path_buf());
        let result = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
            .block_on(server.doc_add_impl(
                Parameters(DocAddParams {
                    file: doc.to_string_lossy().into_owned(),
                    title: None,
                }),
                doc_add_actor(),
            ));

        let err = match result {
            Ok(_) => panic!("deny-listed path inside repo root must refuse"),
            Err(e) => e,
        };
        let message = err.message.to_string();
        assert!(message.contains("deny-list"), "unexpected error: {message}");
    }
}
