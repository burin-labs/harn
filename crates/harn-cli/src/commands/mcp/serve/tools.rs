use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use serde_json::{json, Value as JsonValue};
use tokio::sync::Notify;
use uuid::Uuid;

use harn_vm::mcp_protocol;
use harn_vm::{append_secret_scan_audit, secret_scan_content, SecretFinding};

use crate::commands::orchestrator::common::{
    load_local_runtime, read_topic, synthetic_event_for_binding, trigger_fire, trigger_inspect_dlq,
    trigger_list, trigger_replay, TRIGGER_ATTEMPTS_TOPIC, TRIGGER_DLQ_TOPIC,
    TRIGGER_INBOX_CLAIMS_TOPIC, TRIGGER_INBOX_ENVELOPES_TOPIC, TRIGGER_INBOX_LEGACY_TOPIC,
    TRIGGER_OUTBOX_TOPIC,
};
use crate::commands::orchestrator::inspect_data::collect_orchestrator_inspect_data;

use super::protocol::paginated_list_response;
use super::types::{
    ConnectionState, DlqRetryRequest, InspectPayload, McpListChangeKind, McpOrchestratorService,
    McpTaskRecord, McpTaskState, QueueSnapshot, SecretScanRequest, TopicPreview,
    TriggerFireRequest, TriggerListEntry, TriggerReplayRequest, TrustQueryRequest,
};
use super::util::{
    handler_json, inject_trace_headers, merge_json_object, now_rfc3339,
    parse_trust_query_timestamp, preview_events, report_milestone, trigger_kind_name,
    trigger_replay_steering_from_request,
};
use super::{DEFAULT_TASK_TTL_MS, MAX_TASK_TTL_MS};

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
                mcp_protocol::McpToolTaskSupport::Forbidden,
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
                mcp_protocol::McpToolTaskSupport::Optional,
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
                mcp_protocol::McpToolTaskSupport::Forbidden,
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
                mcp_protocol::McpToolTaskSupport::Optional,
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
                mcp_protocol::McpToolTaskSupport::Forbidden,
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
                mcp_protocol::McpToolTaskSupport::Forbidden,
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
                mcp_protocol::McpToolTaskSupport::Optional,
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
                mcp_protocol::McpToolTaskSupport::Forbidden,
            ),
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
                mcp_protocol::McpToolTaskSupport::Forbidden,
            ),
        ];
        paginated_list_response(id, "tools/list", "tools", params, tools)
    }

    pub(super) async fn handle_tools_call(
        &self,
        id: JsonValue,
        session: &ConnectionState,
        params: &JsonValue,
    ) -> JsonValue {
        if !session.authenticated {
            return harn_vm::jsonrpc::error_response(id, -32001, "unauthorized");
        }

        let name = params
            .get("name")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        if mcp_protocol::requests_task_augmentation(params) {
            if let Err(response) = validate_taskable_tool(id.clone(), name) {
                return response;
            }
            let task_ttl = match parse_task_ttl(params) {
                Ok(ttl) => ttl,
                Err(error) => return harn_vm::jsonrpc::error_response(id, -32602, &error),
            };
            return self.create_tool_task(id, session, name.to_string(), params.clone(), task_ttl);
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
            .record_tool_call(name, &trace_id, &session.client_identity, &result)
            .await;
        if result.is_ok() && tool_call_changes_resources(name) {
            self.notify_list_changed(&[McpListChangeKind::Resources]);
        }

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
        match name {
            "harn.secret_scan" | "harn::secret_scan" => self.tool_secret_scan(arguments).await,
            "harn.trigger.fire" => self.tool_trigger_fire(session, trace_id, arguments).await,
            "harn.trigger.list" => self.tool_trigger_list(arguments).await,
            "harn.trigger.replay" => self.tool_trigger_replay(arguments).await,
            "harn.orchestrator.queue" => self.tool_orchestrator_queue(arguments).await,
            "harn.orchestrator.dlq.list" => self.tool_orchestrator_dlq_list(arguments).await,
            "harn.orchestrator.dlq.retry" => self.tool_orchestrator_dlq_retry(arguments).await,
            "harn.orchestrator.inspect" => self.tool_orchestrator_inspect(arguments).await,
            "harn.trust.query" => self.tool_trust_query(arguments).await,
            _ => Err(format!("unknown tool '{name}'")),
        }
    }

    pub(super) fn create_tool_task(
        &self,
        id: JsonValue,
        session: &ConnectionState,
        name: String,
        params: JsonValue,
        ttl: Option<u64>,
    ) -> JsonValue {
        let task_id = Uuid::now_v7().to_string();
        let now = now_rfc3339();
        let task = McpTaskState {
            task_id: task_id.clone(),
            owner: session.client_identity.clone(),
            status: mcp_protocol::McpTaskStatus::Working,
            status_message: Some("The operation is now in progress.".to_string()),
            created_at: now.clone(),
            last_updated_at: now,
            ttl,
            poll_interval: Some(mcp_protocol::DEFAULT_TASK_POLL_INTERVAL_MS),
        };
        let notify = Arc::new(Notify::new());
        self.tasks.lock().expect("MCP tasks poisoned").insert(
            task_id.clone(),
            McpTaskRecord {
                task: task.clone(),
                result: None,
                notify,
            },
        );
        let _ = self.task_notify_tx.send(task.notification());

        let service = self.clone();
        let task_session = session.clone();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build MCP task runtime");
            runtime.block_on(async move {
                service
                    .run_tool_task(task_id, task_session, name, params)
                    .await;
            });
        });

        harn_vm::jsonrpc::response(
            id,
            json!({
                "task": task.to_json(),
                "_meta": {
                    "io.modelcontextprotocol/model-immediate-response": "The requested Harn tool is running as an MCP task.",
                }
            }),
        )
    }

    pub(super) async fn run_tool_task(
        &self,
        task_id: String,
        session: ConnectionState,
        name: String,
        params: JsonValue,
    ) {
        let arguments = params
            .get("arguments")
            .cloned()
            .unwrap_or_else(|| json!({}));
        let trace_id = format!("mcp_{}", Uuid::now_v7().simple());
        let result = self
            .execute_tool_call(&name, &session, &trace_id, arguments)
            .await;
        let _ = self
            .record_tool_call(&name, &trace_id, &session.client_identity, &result)
            .await;
        if result.is_ok() && tool_call_changes_resources(&name) {
            self.notify_list_changed(&[McpListChangeKind::Resources]);
        }
        self.complete_task(&task_id, result);
    }

    pub(super) fn complete_task(&self, task_id: &str, result: Result<JsonValue, String>) {
        let Some((notification, wake)) = ({
            let mut tasks = self.tasks.lock().expect("MCP tasks poisoned");
            let Some(record) = tasks.get_mut(task_id) else {
                return;
            };
            if record.task.status == mcp_protocol::McpTaskStatus::Cancelled {
                return;
            }
            let wake = record.notify.clone();
            let now = now_rfc3339();
            record.task.last_updated_at = now;
            match result {
                Ok(value) => {
                    record.task.status = mcp_protocol::McpTaskStatus::Completed;
                    record.task.status_message =
                        Some("The task completed successfully.".to_string());
                    record.result = Some(tool_call_result_json(value, false));
                }
                Err(error) => {
                    record.task.status = mcp_protocol::McpTaskStatus::Failed;
                    record.task.status_message = Some(format!("Tool execution failed: {error}"));
                    record.result = Some(tool_call_result_json(json!(error), true));
                }
            }
            Some((record.task.notification(), wake))
        }) else {
            return;
        };
        let _ = self.task_notify_tx.send(notification);
        wake.notify_waiters();
    }

    pub(super) fn handle_tasks_get(
        &self,
        id: JsonValue,
        session: &ConnectionState,
        params: &JsonValue,
    ) -> JsonValue {
        match self.task_record_for_session(session, params) {
            Ok(record) => harn_vm::jsonrpc::response(id, record.task.to_json()),
            Err(error) => harn_vm::jsonrpc::error_response(id, -32602, &error),
        }
    }

    pub(super) async fn handle_tasks_result(
        &self,
        id: JsonValue,
        session: &ConnectionState,
        params: &JsonValue,
    ) -> JsonValue {
        let task_id = match params.get("taskId").and_then(JsonValue::as_str) {
            Some(task_id) if !task_id.is_empty() => task_id.to_string(),
            _ => {
                return harn_vm::jsonrpc::error_response(
                    id,
                    -32602,
                    "Failed to retrieve task: missing taskId",
                )
            }
        };

        loop {
            let notify = {
                let tasks = self.tasks.lock().expect("MCP tasks poisoned");
                let Some(record) = tasks.get(&task_id) else {
                    return harn_vm::jsonrpc::error_response(
                        id,
                        -32602,
                        "Failed to retrieve task: task not found",
                    );
                };
                if record.task.owner != session.client_identity {
                    return harn_vm::jsonrpc::error_response(
                        id,
                        -32602,
                        "Failed to retrieve task: task not found",
                    );
                }
                if record.task.status.is_terminal() {
                    let Some(result) = record.result.clone() else {
                        return harn_vm::jsonrpc::error_response(
                            id,
                            -32603,
                            "Failed to retrieve task: terminal task has no result",
                        );
                    };
                    return harn_vm::jsonrpc::response(
                        id,
                        attach_related_task_meta(result, &task_id),
                    );
                }
                record.notify.clone()
            };
            tokio::select! {
                _ = notify.notified() => {}
                _ = tokio::time::sleep(std::time::Duration::from_millis(
                    mcp_protocol::DEFAULT_TASK_POLL_INTERVAL_MS,
                )) => {}
            }
        }
    }

    pub(super) fn handle_tasks_list(
        &self,
        id: JsonValue,
        session: &ConnectionState,
        params: &JsonValue,
    ) -> JsonValue {
        let matching = self
            .tasks
            .lock()
            .expect("MCP tasks poisoned")
            .values()
            .filter(|record| record.task.owner == session.client_identity)
            .map(|record| record.task.to_json())
            .collect::<Vec<_>>();
        paginated_list_response(
            id,
            mcp_protocol::METHOD_TASKS_LIST,
            "tasks",
            params,
            matching,
        )
    }

    pub(super) fn handle_tasks_cancel(
        &self,
        id: JsonValue,
        session: &ConnectionState,
        params: &JsonValue,
    ) -> JsonValue {
        let task_id = match params.get("taskId").and_then(JsonValue::as_str) {
            Some(task_id) if !task_id.is_empty() => task_id,
            _ => {
                return harn_vm::jsonrpc::error_response(
                    id,
                    -32602,
                    "Cannot cancel task: missing taskId",
                )
            }
        };
        let (task, notify) = {
            let mut tasks = self.tasks.lock().expect("MCP tasks poisoned");
            let Some(record) = tasks.get_mut(task_id) else {
                return harn_vm::jsonrpc::error_response(
                    id,
                    -32602,
                    "Cannot cancel task: task not found",
                );
            };
            if record.task.owner != session.client_identity {
                return harn_vm::jsonrpc::error_response(
                    id,
                    -32602,
                    "Cannot cancel task: task not found",
                );
            }
            if record.task.status.is_terminal() {
                return harn_vm::jsonrpc::error_response(
                    id,
                    -32602,
                    &format!(
                        "Cannot cancel task: already in terminal status '{}'",
                        record.task.status.as_str()
                    ),
                );
            }
            record.task.status = mcp_protocol::McpTaskStatus::Cancelled;
            record.task.status_message = Some("The task was cancelled by request.".to_string());
            record.task.last_updated_at = now_rfc3339();
            record.result = Some(json!({
                "content": [{
                    "type": "text",
                    "text": "Task was cancelled by request.",
                }],
                "isError": true,
            }));
            (record.task.clone(), record.notify.clone())
        };
        let _ = self.task_notify_tx.send(task.notification());
        notify.notify_waiters();
        harn_vm::jsonrpc::response(id, task.to_json())
    }

    pub(super) fn task_record_for_session(
        &self,
        session: &ConnectionState,
        params: &JsonValue,
    ) -> Result<McpTaskRecord, String> {
        let task_id = params
            .get("taskId")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| "Failed to retrieve task: missing taskId".to_string())?;
        let tasks = self.tasks.lock().expect("MCP tasks poisoned");
        let record = tasks
            .get(task_id)
            .ok_or_else(|| "Failed to retrieve task: task not found".to_string())?;
        if record.task.owner != session.client_identity {
            return Err("Failed to retrieve task: task not found".to_string());
        }
        Ok(record.clone())
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
        inject_trace_headers(&mut event, &session.client_identity, trace_id);
        report_milestone(0.5, "firing trigger");
        let handle = trigger_fire(&mut ctx, &request.trigger_id, event).await?;
        report_milestone(0.95, "trigger complete");
        self.notify_topic_resource_changed(TRIGGER_OUTBOX_TOPIC);
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
        self.notify_topic_resource_changed(TRIGGER_OUTBOX_TOPIC);
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
                        .chain(inbox_envelopes)
                        .chain(inbox_legacy)
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
        self.notify_topic_resource_changed(TRIGGER_OUTBOX_TOPIC);
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
        let ctx = load_local_runtime(&self.local_args()).await?;
        let records = harn_vm::query_trust_records(&ctx.event_log, &filters)
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

pub(super) fn tool_def(
    name: &str,
    description: &str,
    annotations: JsonValue,
    input_schema: JsonValue,
    output_schema: Option<JsonValue>,
    task_support: mcp_protocol::McpToolTaskSupport,
) -> JsonValue {
    let mut value = json!({
        "name": name,
        "description": description,
        "annotations": annotations,
        "inputSchema": input_schema,
        "execution": mcp_protocol::tool_execution(task_support),
    });
    if let Some(title) = value["annotations"].get("title").cloned() {
        value["title"] = title;
    }
    if let Some(output_schema) = output_schema {
        value["outputSchema"] = output_schema;
    }
    value
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

pub(super) fn task_support_for_tool(name: &str) -> Option<mcp_protocol::McpToolTaskSupport> {
    match name {
        "harn.trigger.fire" | "harn.trigger.replay" | "harn.orchestrator.dlq.retry" => {
            Some(mcp_protocol::McpToolTaskSupport::Optional)
        }
        "harn.secret_scan"
        | "harn::secret_scan"
        | "harn.trigger.list"
        | "harn.orchestrator.queue"
        | "harn.orchestrator.dlq.list"
        | "harn.orchestrator.inspect"
        | "harn.trust.query" => Some(mcp_protocol::McpToolTaskSupport::Forbidden),
        _ => None,
    }
}

pub(super) fn validate_taskable_tool(id: JsonValue, name: &str) -> Result<(), JsonValue> {
    match task_support_for_tool(name) {
        Some(mcp_protocol::McpToolTaskSupport::Optional)
        | Some(mcp_protocol::McpToolTaskSupport::Required) => Ok(()),
        Some(mcp_protocol::McpToolTaskSupport::Forbidden) => {
            Err(mcp_protocol::task_augmentation_error_response(
                id,
                "tools/call",
                -32602,
                "Tool does not support MCP task-augmented execution",
                &format!("Tool '{name}' advertises execution.taskSupport=\"forbidden\"."),
            ))
        }
        None => Err(harn_vm::jsonrpc::error_response(
            id,
            -32602,
            &format!("unknown tool '{name}'"),
        )),
    }
}

pub(super) fn parse_task_ttl(params: &JsonValue) -> Result<Option<u64>, String> {
    let task = params
        .get("task")
        .ok_or_else(|| "missing task params".to_string())?;
    let Some(object) = task.as_object() else {
        return Err("task must be an object".to_string());
    };
    let Some(ttl) = object.get("ttl") else {
        return Ok(Some(DEFAULT_TASK_TTL_MS));
    };
    let Some(ttl) = ttl.as_u64() else {
        return Err("task.ttl must be an unsigned integer number of milliseconds".to_string());
    };
    Ok(Some(ttl.min(MAX_TASK_TTL_MS)))
}

pub(super) fn tool_call_result_json(value: JsonValue, is_error: bool) -> JsonValue {
    if is_error {
        return json!({
            "content": [{
                "type": "text",
                "text": value.as_str().unwrap_or("Tool execution failed"),
            }],
            "isError": true,
        });
    }
    json!({
        "content": [{
            "type": "text",
            "text": serde_json::to_string_pretty(&value).unwrap_or_else(|_| value.to_string()),
        }],
        "structuredContent": value,
        "isError": false,
    })
}

pub(super) fn attach_related_task_meta(mut result: JsonValue, task_id: &str) -> JsonValue {
    let related = mcp_protocol::related_task_meta(task_id);
    if let Some(result_object) = result.as_object_mut() {
        let meta = result_object.entry("_meta").or_insert_with(|| json!({}));
        if let Some(meta_object) = meta.as_object_mut() {
            if let Some(related_object) = related.as_object() {
                for (key, value) in related_object {
                    meta_object.insert(key.clone(), value.clone());
                }
            }
        } else {
            result_object.insert("_meta".to_string(), related);
        }
    }
    result
}

pub(super) fn tool_call_changes_resources(name: &str) -> bool {
    matches!(
        name,
        "harn.trigger.fire" | "harn.trigger.replay" | "harn.orchestrator.dlq.retry"
    )
}
