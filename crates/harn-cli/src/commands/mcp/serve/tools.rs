use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::Deserialize;
use serde_json::{json, Value as JsonValue};
use uuid::Uuid;

use harn_vm::mcp_protocol;
use harn_vm::{append_secret_scan_audit, secret_scan_content, SecretFinding};

use super::protocol::paginated_list_response;
use super::types::{
    ConnectionState, DlqRetryRequest, InspectPayload, McpOrchestratorService, QueueSnapshot,
    SecretScanRequest, TopicPreview, TriggerFireRequest, TriggerListEntry, TriggerReplayRequest,
    TrustQueryRequest,
};
use super::util::{
    handler_json, inject_trace_headers, merge_json_object, parse_trust_query_timestamp,
    preview_events, report_milestone, trigger_kind_name, trigger_replay_steering_from_request,
};
use super::DEFAULT_TASK_TTL_MS;
use crate::commands::orchestrator::common::{
    load_local_runtime, read_topic, synthetic_event_for_binding, trigger_fire, trigger_inspect_dlq,
    trigger_list, trigger_replay, TRIGGER_ATTEMPTS_TOPIC, TRIGGER_DLQ_TOPIC,
    TRIGGER_INBOX_CLAIMS_TOPIC, TRIGGER_INBOX_ENVELOPES_TOPIC, TRIGGER_INBOX_LEGACY_TOPIC,
    TRIGGER_INBOX_OBSERVABILITY_TOPIC, TRIGGER_OUTBOX_TOPIC,
};
use crate::commands::orchestrator::inspect_data::collect_orchestrator_inspect_data;

impl McpOrchestratorService {
    pub(super) fn handle_tools_list(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        let tools = vec![
            tool_def(
                "harn.secret_scan",
                "Scan content for high-signal secrets before commit or PR-open flows. The `harn::secret_scan` alias is also accepted.",
                read_only_tool_annotations("Secret Scan"),
                json!({
                    "type": "object",
                    "required": ["content"],
                    "properties": {
                        "content": { "type": "string" },
                    },
                    "additionalProperties": false,
                }),
                Some(json!({
                    "type": "array",
                    "items": {
                        "type": "object",
                        "required": [
                            "detector",
                            "source",
                            "title",
                            "line",
                            "column_start",
                            "column_end",
                            "start_offset",
                            "end_offset",
                            "redacted",
                            "fingerprint"
                        ],
                        "properties": {
                            "detector": { "type": "string" },
                            "source": { "type": "string" },
                            "title": { "type": "string" },
                            "line": { "type": "integer" },
                            "column_start": { "type": "integer" },
                            "column_end": { "type": "integer" },
                            "start_offset": { "type": "integer" },
                            "end_offset": { "type": "integer" },
                            "redacted": { "type": "string" },
                            "fingerprint": { "type": "string" },
                        },
                    },
                })),
            ),
            tool_def(
                "harn.trigger.fire",
                "Dispatch a trigger inline and return its event id plus terminal status.",
                mutating_open_world_tool_annotations("Fire Trigger"),
                json!({
                    "type": "object",
                    "required": ["trigger_id", "payload"],
                    "properties": {
                        "trigger_id": { "type": "string" },
                        "payload": {},
                    },
                    "additionalProperties": false,
                }),
                Some(json!({
                    "type": "object",
                    "required": ["event_id", "status"],
                    "properties": {
                        "event_id": { "type": "string" },
                        "status": { "type": "string" },
                    },
                })),
            ),
            tool_def(
                "harn.trigger.list",
                "List registered triggers and their kind/provider/when/handler metadata.",
                read_only_tool_annotations("List Triggers"),
                json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                }),
                None,
            ),
            tool_def(
                "harn.trigger.replay",
                "Replay an existing trigger event, optionally resolving bindings as of a historical timestamp or recording a teaching correction.",
                mutating_open_world_tool_annotations("Replay Trigger"),
                json!({
                    "type": "object",
                    "required": ["event_id"],
                    "properties": {
                        "event_id": { "type": "string" },
                        "as_of": { "type": "string" },
                        "steer_from": { "type": "string" },
                        "to_decision": {},
                        "reason": { "type": "string" },
                        "applied_by": { "type": "string" },
                        "scope": {
                            "type": "string",
                            "enum": ["this_run", "this_persona", "all"],
                        },
                    },
                    "additionalProperties": false,
                }),
                None,
            ),
            tool_def(
                "harn.orchestrator.queue",
                "Return inbox/outbox/attempt/DLQ counts plus recent previews.",
                read_only_tool_annotations("Inspect Orchestrator Queue"),
                json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                }),
                None,
            ),
            tool_def(
                "harn.orchestrator.dlq.list",
                "List pending dead-letter queue entries.",
                read_only_tool_annotations("List Dead Letter Queue"),
                json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                }),
                None,
            ),
            tool_def(
                "harn.orchestrator.dlq.retry",
                "Replay a pending dead-letter queue entry.",
                mutating_open_world_tool_annotations("Retry Dead Letter Queue Entry"),
                json!({
                    "type": "object",
                    "required": ["entry_id"],
                    "properties": {
                        "entry_id": { "type": "string" },
                    },
                    "additionalProperties": false,
                }),
                None,
            ),
            tool_def(
                "harn.orchestrator.inspect",
                "Snapshot dispatcher state, triggers, flow-control state, and recent dispatches.",
                read_only_tool_annotations("Inspect Orchestrator"),
                json!({
                    "type": "object",
                    "properties": {},
                    "additionalProperties": false,
                }),
                None,
            ),
            run_report_tool_def(
                "harn.run.report",
                "Run Report",
                "Build a versioned, read-only report from a root run record and its delegated children.",
            ),
            run_review_tool_def(),
            tool_def(
                "harn.trust.query",
                "Query trust-graph records with the same filters exposed by trust_query(filters).",
                read_only_tool_annotations("Query Trust Records"),
                json!({
                    "type": "object",
                    "properties": {
                        "agent": { "type": "string" },
                        "action": { "type": "string" },
                        "since": { "type": "string" },
                        "until": { "type": "string" },
                        "tier": {
                            "type": "string",
                            "enum": ["shadow", "suggest", "act_with_approval", "act_auto"]
                        },
                        "outcome": {
                            "type": "string",
                            "enum": ["success", "failure", "denied", "timeout"]
                        },
                        "limit": { "type": "integer", "minimum": 0 },
                        "grouped_by_trace": { "type": "boolean" }
                    },
                    "additionalProperties": false,
                }),
                Some(json!({
                    "type": "object",
                    "required": ["grouped_by_trace", "results"],
                    "properties": {
                        "grouped_by_trace": { "type": "boolean" },
                        "results": { "type": "array" },
                    },
                })),
            ),
        ];
        paginated_list_response(id, "tools/list", "tools", params, tools)
    }

    pub(super) async fn handle_tools_call(
        &self,
        id: JsonValue,
        session: &ConnectionState,
        params: &JsonValue,
        uses_result_envelope: bool,
    ) -> JsonValue {
        if !session.authenticated {
            return harn_vm::jsonrpc::error_response(id, -32001, "unauthorized");
        }

        let name = params
            .get("name")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        if mcp_protocol::client_supports_tasks(params)
            && matches!(task_policy_for_tool(name), Some(TaskPolicy::Async))
        {
            return self.create_tool_task(
                id,
                session,
                name.to_string(),
                params.clone(),
                Some(DEFAULT_TASK_TTL_MS),
                uses_result_envelope,
            );
        }
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let trace_id = format!("mcp_{}", Uuid::now_v7().simple());

        // Bind the request's progressToken to the active outbound bus
        // (installed by the transport) for the duration of the tool
        // call. Built-in tool implementations and any nested Harn
        // handlers can then call `mcp_report_progress(...)` without
        // taking a token argument.
        let progress_ctx = params
            .pointer("/_meta/progressToken")
            .cloned()
            .filter(harn_vm::mcp_progress::is_valid_progress_token)
            .and_then(|token| {
                harn_vm::mcp_progress::active_bus()
                    .map(|bus| harn_vm::mcp_progress::ProgressContext::new(bus, token))
            });

        // Box-pin the tool-call future before scoping it: handle_tools_call
        // is a deep async state machine and adding another async wrapper
        // grew the stack frame past the test runtime's 2 MiB budget.
        let result = harn_vm::mcp_progress::scope_context(
            progress_ctx,
            Box::pin(self.execute_tool_call(name, session, &trace_id, arguments)),
        )
        .await;

        let _ = self
            .record_tool_call(name, &trace_id, session.mcp.client_identity(), &result)
            .await;
        match result {
            Ok(value) => harn_vm::jsonrpc::response(
                id,
                json!({
                    "content": [{
                        "type": "text",
                        "text": serde_json::to_string_pretty(&value)
                            .unwrap_or_else(|_| value.to_string()),
                    }],
                    "structuredContent": value,
                    "isError": false,
                }),
            ),
            Err(error) => harn_vm::jsonrpc::response(
                id,
                json!({
                    "content": [{ "type": "text", "text": error }],
                    "isError": true,
                }),
            ),
        }
    }

    pub(super) async fn execute_tool_call(
        &self,
        name: &str,
        session: &ConnectionState,
        trace_id: &str,
        arguments: JsonValue,
    ) -> Result<JsonValue, String> {
        // Each arm is boxed rather than awaited inline. An inline `.await`
        // embeds every arm's future in this one future, so the frame is the
        // *sum* of all eleven — several hundred kilobytes that grows whenever
        // any tool's call graph gains a field, with a stack overflow as the
        // failure mode. Boxing makes the frame the size of a pointer per arm,
        // at one allocation on a path that is already doing I/O.
        let future: std::pin::Pin<Box<dyn std::future::Future<Output = _> + Send + '_>> = match name
        {
            "harn.secret_scan" | "harn::secret_scan" => Box::pin(self.tool_secret_scan(arguments)),
            "harn.trigger.fire" => Box::pin(self.tool_trigger_fire(session, trace_id, arguments)),
            "harn.trigger.list" => Box::pin(self.tool_trigger_list(arguments)),
            "harn.trigger.replay" => Box::pin(self.tool_trigger_replay(arguments)),
            "harn.orchestrator.queue" => Box::pin(self.tool_orchestrator_queue(arguments)),
            "harn.orchestrator.dlq.list" => Box::pin(self.tool_orchestrator_dlq_list(arguments)),
            "harn.orchestrator.dlq.retry" => Box::pin(self.tool_orchestrator_dlq_retry(arguments)),
            "harn.orchestrator.inspect" => Box::pin(self.tool_orchestrator_inspect(arguments)),
            "harn.run.report" => Box::pin(self.tool_run_report(arguments)),
            "harn.run.review" => Box::pin(self.tool_run_review(arguments)),
            "harn.trust.query" => Box::pin(self.tool_trust_query(arguments)),
            _ => return Err(format!("unknown tool '{name}'")),
        };
        future.await
    }

    pub(super) fn create_tool_task(
        &self,
        id: JsonValue,
        session: &ConnectionState,
        name: String,
        params: JsonValue,
        ttl: Option<u64>,
        uses_result_envelope: bool,
    ) -> JsonValue {
        let lease = match self
            .tasks
            .begin(harn_vm::mcp_tasks::McpTaskAccess::unscoped(), ttl)
        {
            Ok(lease) => lease,
            Err(error) => return harn_vm::jsonrpc::error_response(id, -32000, &error.to_string()),
        };
        let task = lease.task().clone();
        let service = self.clone();
        let task_session = session.clone();
        // The task runs a Harn tool on this thread, so it drives the VM.
        std::thread::Builder::new()
            .name("harn-mcp-task".to_string())
            .stack_size(crate::CLI_RUNTIME_STACK_SIZE)
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("build MCP task runtime");
                runtime.block_on(async move {
                    service
                        .run_tool_task(lease, task_session, name, params, uses_result_envelope)
                        .await;
                });
            })
            .expect("spawn MCP task thread");

        harn_vm::mcp_tasks::task_created_response(
            id,
            &task,
            "The requested Harn tool is running as an MCP task.",
        )
    }

    pub(super) async fn run_tool_task(
        &self,
        lease: harn_vm::mcp_tasks::McpTaskLease,
        session: ConnectionState,
        name: String,
        params: JsonValue,
        uses_result_envelope: bool,
    ) {
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let trace_id = format!("mcp_{}", Uuid::now_v7().simple());
        let result = tokio::select! {
            biased;
            result = self.execute_tool_call(&name, &session, &trace_id, arguments) => result,
            () = lease.cancelled() => {
                lease.cancel();
                return;
            }
        };
        let _ = self
            .record_tool_call(&name, &trace_id, session.mcp.client_identity(), &result)
            .await;
        lease.complete(result, uses_result_envelope);
    }

    pub(super) fn handle_tasks_get(
        &self,
        id: JsonValue,
        _session: &ConnectionState,
        params: &JsonValue,
    ) -> JsonValue {
        self.tasks
            .handle_get(&harn_vm::mcp_tasks::McpTaskAccess::unscoped(), id, params)
    }

    pub(super) fn handle_tasks_update(
        &self,
        id: JsonValue,
        _session: &ConnectionState,
        params: &JsonValue,
    ) -> JsonValue {
        self.tasks
            .handle_update(&harn_vm::mcp_tasks::McpTaskAccess::unscoped(), id, params)
    }

    pub(super) fn handle_tasks_cancel(
        &self,
        id: JsonValue,
        _session: &ConnectionState,
        params: &JsonValue,
    ) -> JsonValue {
        self.tasks
            .handle_cancel(&harn_vm::mcp_tasks::McpTaskAccess::unscoped(), id, params)
    }

    pub(super) async fn tool_secret_scan(&self, arguments: JsonValue) -> Result<JsonValue, String> {
        let request: SecretScanRequest =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        let findings: Vec<SecretFinding> = secret_scan_content(&request.content);
        let ctx = load_local_runtime(&self.local_args()).await?;
        append_secret_scan_audit(
            ctx.event_log.as_ref(),
            "mcp.harn.secret_scan",
            request.content.len(),
            &findings,
        )
        .await
        .map_err(|error| error.to_string())?;
        serde_json::to_value(findings).map_err(|error| error.to_string())
    }

    pub(super) async fn tool_trigger_fire(
        &self,
        session: &ConnectionState,
        trace_id: &str,
        arguments: JsonValue,
    ) -> Result<JsonValue, String> {
        let request: TriggerFireRequest =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        report_milestone(0.1, "loading runtime");
        let mut ctx = load_local_runtime(&self.local_args()).await?;
        report_milestone(0.3, "preparing event");
        let mut event = synthetic_event_for_binding(&ctx, &request.trigger_id)?;
        merge_json_object(&mut event, request.payload);
        inject_trace_headers(&mut event, session.mcp.client_identity(), trace_id);
        report_milestone(0.5, "firing trigger");
        let handle = trigger_fire(&mut ctx, &request.trigger_id, event).await?;
        report_milestone(0.95, "trigger complete");
        Ok(json!({
            "event_id": handle.event_id,
            "status": handle.status,
            "binding_id": handle.binding_id,
            "binding_version": handle.binding_version,
            "dlq_entry_id": handle.dlq_entry_id,
            "error": handle.error,
            "result": handle.result,
        }))
    }

    pub(super) async fn tool_trigger_list(
        &self,
        _arguments: JsonValue,
    ) -> Result<JsonValue, String> {
        let mut ctx = load_local_runtime(&self.local_args()).await?;
        let snapshots = trigger_list(&mut ctx).await?;
        let mut snapshots_by_id = BTreeMap::new();
        for snapshot in snapshots {
            snapshots_by_id.insert(snapshot.id.clone(), snapshot);
        }

        let mut triggers = Vec::new();
        for trigger in &ctx.collected_triggers {
            let Some(snapshot) = snapshots_by_id.get(&trigger.config.id) else {
                continue;
            };
            triggers.push(TriggerListEntry {
                trigger_id: trigger.config.id.clone(),
                kind: trigger_kind_name(trigger.config.kind).to_string(),
                provider: trigger.config.provider.as_str().to_string(),
                when: trigger.when.as_ref().map(|when| when.reference.raw.clone()),
                handler: handler_json(&trigger.handler),
                version: snapshot.version,
                state: snapshot.state.as_str().to_string(),
                metrics: snapshot.metrics.clone(),
            });
        }
        Ok(json!({ "triggers": triggers }))
    }

    pub(super) async fn tool_trigger_replay(
        &self,
        arguments: JsonValue,
    ) -> Result<JsonValue, String> {
        let request: TriggerReplayRequest =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        let steering = trigger_replay_steering_from_request(&request)?;
        if request.as_of.is_some() || steering.is_some() {
            let workspace_root = self
                .config_path
                .parent()
                .unwrap_or(Path::new("."))
                .to_path_buf();
            let ctx = load_local_runtime(&self.local_args()).await?;
            let report = crate::commands::trigger::replay::replay_report_for_event_log(
                ctx.event_log.clone(),
                &workspace_root,
                &request.event_id,
                request.as_of.as_deref(),
                false,
                steering.as_ref(),
            )
            .await?;
            return serde_json::to_value(report).map_err(|error| error.to_string());
        }

        let mut ctx = load_local_runtime(&self.local_args()).await?;
        let handle = trigger_replay(&mut ctx, &request.event_id).await?;
        serde_json::to_value(handle).map_err(|error| error.to_string())
    }

    pub(super) async fn tool_orchestrator_queue(
        &self,
        _arguments: JsonValue,
    ) -> Result<JsonValue, String> {
        let ctx = load_local_runtime(&self.local_args()).await?;
        let dispatcher = harn_vm::snapshot_dispatcher_stats();
        let inbox_claims = read_topic(&ctx.event_log, TRIGGER_INBOX_CLAIMS_TOPIC).await?;
        let inbox_envelopes = read_topic(&ctx.event_log, TRIGGER_INBOX_ENVELOPES_TOPIC).await?;
        let inbox_observability =
            read_topic(&ctx.event_log, TRIGGER_INBOX_OBSERVABILITY_TOPIC).await?;
        let inbox_legacy = read_topic(&ctx.event_log, TRIGGER_INBOX_LEGACY_TOPIC).await?;
        let outbox = read_topic(&ctx.event_log, TRIGGER_OUTBOX_TOPIC).await?;
        let attempts = read_topic(&ctx.event_log, TRIGGER_ATTEMPTS_TOPIC).await?;
        let dlq = read_topic(&ctx.event_log, TRIGGER_DLQ_TOPIC).await?;

        let queue = QueueSnapshot {
            dispatcher,
            inbox: TopicPreview {
                count: inbox_claims.len() + inbox_envelopes.len() + inbox_legacy.len(),
                head: preview_events(
                    inbox_claims
                        .into_iter()
                        .chain(inbox_observability)
                        .collect(),
                ),
            },
            outbox: TopicPreview {
                count: outbox.len(),
                head: preview_events(outbox),
            },
            attempts: TopicPreview {
                count: attempts.len(),
                head: preview_events(attempts),
            },
            dlq: TopicPreview {
                count: dlq.len(),
                head: preview_events(dlq),
            },
        };
        serde_json::to_value(queue).map_err(|error| error.to_string())
    }

    pub(super) async fn tool_orchestrator_dlq_list(
        &self,
        _arguments: JsonValue,
    ) -> Result<JsonValue, String> {
        let mut ctx = load_local_runtime(&self.local_args()).await?;
        let entries = trigger_inspect_dlq(&mut ctx).await?;
        Ok(json!({ "entries": entries }))
    }

    pub(super) async fn tool_orchestrator_dlq_retry(
        &self,
        arguments: JsonValue,
    ) -> Result<JsonValue, String> {
        let request: DlqRetryRequest =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        let mut ctx = load_local_runtime(&self.local_args()).await?;
        let entries = trigger_inspect_dlq(&mut ctx).await?;
        let entry = entries
            .iter()
            .find(|entry| entry.id == request.entry_id)
            .ok_or_else(|| format!("unknown pending DLQ entry '{}'", request.entry_id))?;
        let handle = trigger_replay(&mut ctx, &entry.event_id).await?;
        Ok(json!({
            "entry_id": entry.id,
            "handle": handle,
        }))
    }

    pub(super) async fn tool_orchestrator_inspect(
        &self,
        _arguments: JsonValue,
    ) -> Result<JsonValue, String> {
        let mut ctx = load_local_runtime(&self.local_args()).await?;
        let inspect = collect_orchestrator_inspect_data(&mut ctx).await?;
        let payload = InspectPayload {
            dispatcher: harn_vm::snapshot_dispatcher_stats(),
            inspect,
        };
        serde_json::to_value(payload).map_err(|error| error.to_string())
    }

    pub(super) async fn tool_run_report(&self, arguments: JsonValue) -> Result<JsonValue, String> {
        let request: McpRunReportRequest =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        let project_root = self.project_root();
        let run_record_path = resolve_report_path(&project_root, request.path);
        let events_db = request
            .events_db
            .map(|path| resolve_report_path(&project_root, path));
        let report =
            harn_vm::orchestration::build_run_report(harn_vm::orchestration::RunReportRequest {
                run_record_path,
                events_db,
                allowed_roots: vec![project_root.clone(), self.effective_state_dir()],
                source_root: Some(project_root),
            })
            .await
            .map_err(|error| error.to_string())?;
        serde_json::to_value(report).map_err(|error| error.to_string())
    }

    pub(super) async fn tool_run_review(&self, arguments: JsonValue) -> Result<JsonValue, String> {
        let request: McpRunReviewRequest =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        let project_root = self.project_root();
        let allowed_roots = vec![project_root.clone(), self.effective_state_dir()];
        let (input, rubric, model) = match request {
            McpRunReviewRequest::Report(McpRunReviewReportRequest {
                report_path,
                rubric,
                model,
            }) => (
                harn_vm::orchestration::RunReviewInput::Report {
                    path: resolve_report_path(&project_root, report_path),
                    allowed_roots,
                },
                rubric,
                model,
            ),
            McpRunReviewRequest::RunRecord(McpRunReviewRunRecordRequest {
                run_record_path,
                events_db,
                rubric,
                model,
            }) => (
                harn_vm::orchestration::RunReviewInput::RunRecord(
                    harn_vm::orchestration::RunReportRequest {
                        run_record_path: resolve_report_path(&project_root, run_record_path),
                        events_db: events_db.map(|path| resolve_report_path(&project_root, path)),
                        allowed_roots,
                        source_root: Some(project_root),
                    },
                ),
                rubric,
                model,
            ),
        };
        let review =
            harn_vm::orchestration::review_run_report(harn_vm::orchestration::RunReviewRequest {
                input,
                rubric: rubric.unwrap_or_else(|| {
                    harn_vm::orchestration::DEFAULT_RUN_REVIEW_RUBRIC.to_string()
                }),
                model,
            })
            .await
            .map_err(|error| error.to_string())?;
        serde_json::to_value(review).map_err(|error| error.to_string())
    }

    pub(super) async fn tool_trust_query(&self, arguments: JsonValue) -> Result<JsonValue, String> {
        let request: TrustQueryRequest =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        let filters = harn_vm::TrustQueryFilters {
            agent: request.agent,
            action: request.action,
            since: request
                .since
                .as_deref()
                .map(parse_trust_query_timestamp)
                .transpose()?,
            until: request
                .until
                .as_deref()
                .map(parse_trust_query_timestamp)
                .transpose()?,
            tier: request.tier,
            outcome: request.outcome,
            limit: request.limit,
            grouped_by_trace: request.grouped_by_trace,
        };
        let event_log = self.orchestrator_event_log()?;
        let records = harn_vm::query_trust_records(&event_log, &filters)
            .await
            .map_err(|error| error.to_string())?;
        let results = if filters.grouped_by_trace {
            serde_json::to_value(harn_vm::group_trust_records_by_trace(&records))
                .map_err(|error| error.to_string())?
        } else {
            serde_json::to_value(records).map_err(|error| error.to_string())?
        };
        Ok(json!({
            "grouped_by_trace": filters.grouped_by_trace,
            "results": results,
        }))
    }
}

#[derive(Debug, Deserialize)]
struct McpRunReportRequest {
    path: PathBuf,
    events_db: Option<PathBuf>,
}

#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum McpRunReviewRequest {
    Report(McpRunReviewReportRequest),
    RunRecord(McpRunReviewRunRecordRequest),
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpRunReviewReportRequest {
    report_path: PathBuf,
    rubric: Option<String>,
    model: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct McpRunReviewRunRecordRequest {
    run_record_path: PathBuf,
    events_db: Option<PathBuf>,
    rubric: Option<String>,
    model: Option<String>,
}

fn resolve_report_path(project_root: &Path, path: PathBuf) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        project_root.join(path)
    }
}

pub(super) fn tool_def(
    name: &str,
    description: &str,
    annotations: JsonValue,
    input_schema: JsonValue,
    output_schema: Option<JsonValue>,
) -> JsonValue {
    let mut value = json!({
        "name": name,
        "description": description,
        "annotations": annotations,
        "inputSchema": input_schema,
    });
    if let Some(title) = value["annotations"].get("title").cloned() {
        value["title"] = title;
    }
    if let Some(output_schema) = output_schema {
        value["outputSchema"] = output_schema;
    }
    value
}

fn run_report_tool_def(name: &str, title: &str, description: &str) -> JsonValue {
    tool_def(
        name,
        description,
        read_only_tool_annotations(title),
        json!({
            "type": "object",
            "required": ["path"],
            "properties": {
                "path": { "type": "string" },
                "events_db": { "type": "string" },
            },
            "additionalProperties": false,
        }),
        Some(json!({
            "type": "object",
            "required": [
                "schema",
                "schema_version",
                "producer",
                "projection",
                "root_run_id",
                "agents",
                "delegations",
                "llm_calls",
                "coordination",
                "timelines",
                "sources",
                "checks"
            ],
            "properties": {
                "schema": { "const": "harn.run_report.v1" },
                "schema_version": { "const": 1 },
                "producer": { "type": "object" },
                "projection": { "type": "object" },
                "root_run_id": { "type": "string" },
                "agents": { "type": "array" },
                "delegations": { "type": "array" },
                "llm_calls": { "type": "array" },
                "coordination": { "type": "object" },
                "timelines": { "type": "array" },
                "sources": { "type": "array" },
                "checks": { "type": "array" },
            },
            "additionalProperties": false,
        })),
    )
}

fn run_review_tool_def() -> JsonValue {
    tool_def(
        "harn.run.review",
        "Build or read a run report and assess it with one provenance-bound model call.",
        model_call_tool_annotations("Run Review"),
        json!({
            "oneOf": [
                {
                    "type": "object",
                    "required": ["report_path"],
                    "properties": {
                        "report_path": { "type": "string" },
                        "rubric": { "type": "string" },
                        "model": { "type": "string" }
                    },
                    "additionalProperties": false
                },
                {
                    "type": "object",
                    "required": ["run_record_path"],
                    "properties": {
                        "run_record_path": { "type": "string" },
                        "events_db": { "type": "string" },
                        "rubric": { "type": "string" },
                        "model": { "type": "string" }
                    },
                    "additionalProperties": false
                }
            ]
        }),
        Some(json!({
            "type": "object",
            "required": ["schema", "schema_version", "provenance", "lifecycle", "verdict"],
            "properties": {
                "schema": { "const": "harn.run_review.v1" },
                "schema_version": { "const": 1 },
                "provenance": { "type": "object" },
                "lifecycle": { "type": "object" },
                "verdict": { "enum": ["pass", "concerns", "fail"] }
            }
        })),
    )
}

pub(super) fn read_only_tool_annotations(title: &str) -> JsonValue {
    json!({
        "title": title,
        "readOnlyHint": true,
        "destructiveHint": false,
        "idempotentHint": true,
        "openWorldHint": false,
    })
}

pub(super) fn mutating_open_world_tool_annotations(title: &str) -> JsonValue {
    json!({
        "title": title,
        "readOnlyHint": false,
        "destructiveHint": true,
        "idempotentHint": false,
        "openWorldHint": true,
    })
}

fn model_call_tool_annotations(title: &str) -> JsonValue {
    json!({
        "title": title,
        "readOnlyHint": false,
        "destructiveHint": false,
        "idempotentHint": false,
        "openWorldHint": true,
    })
}

#[derive(Clone, Copy)]
pub(super) enum TaskPolicy {
    Inline,
    Async,
}

pub(super) fn task_policy_for_tool(name: &str) -> Option<TaskPolicy> {
    match name {
        "harn.trigger.fire"
        | "harn.trigger.replay"
        | "harn.orchestrator.dlq.retry"
        | "harn.run.review" => Some(TaskPolicy::Async),
        "harn.secret_scan"
        | "harn::secret_scan"
        | "harn.trigger.list"
        | "harn.orchestrator.queue"
        | "harn.orchestrator.dlq.list"
        | "harn.orchestrator.inspect"
        | "harn.run.report"
        | "harn.trust.query" => Some(TaskPolicy::Inline),
        _ => None,
    }
}
