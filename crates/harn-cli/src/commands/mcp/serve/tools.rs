use std::collections::BTreeMap;
use std::fs;
use std::io::{BufRead, BufReader, Read};
use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde::Deserialize;
use serde_json::{json, Value as JsonValue};
use tokio::sync::Notify;
use uuid::Uuid;

use harn_vm::event_log::{AnyEventLog, EventLog, SqliteEventLog};
use harn_vm::mcp_protocol;
use harn_vm::{append_secret_scan_audit, secret_scan_content, SecretFinding};

use crate::commands::orchestrator::common::{
    load_local_runtime, read_topic, synthetic_event_for_binding, trigger_fire, trigger_inspect_dlq,
    trigger_list, trigger_replay, TRIGGER_ATTEMPTS_TOPIC, TRIGGER_DLQ_TOPIC,
    TRIGGER_INBOX_CLAIMS_TOPIC, TRIGGER_INBOX_ENVELOPES_TOPIC, TRIGGER_INBOX_LEGACY_TOPIC,
    TRIGGER_INBOX_OBSERVABILITY_TOPIC, TRIGGER_OUTBOX_TOPIC,
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
                "harn.eval.inspect_run",
                "Build a read-only chain-of-custody dossier for a Harn/Burin eval run bundle, summary JSON, or event log.",
                json!({
                    "title": "Inspect Eval Run",
                    "readOnlyHint": true,
                    "destructiveHint": false,
                    "idempotentHint": true,
                    "openWorldHint": true,
                }),
                json!({
                    "type": "object",
                    "properties": {
                        "output_dir": { "type": "string" },
                        "summary_json": { "type": "string" },
                        "per_run_jsonl": { "type": "string" },
                        "run_record": { "type": "string" },
                        "events_dir": { "type": "string" },
                        "events_db": { "type": "string" },
                        "include_payloads": { "type": "boolean" },
                        "limit": { "type": "integer", "minimum": 1 },
                    },
                    "additionalProperties": false,
                }),
                Some(json!({
                    "type": "object",
                    "required": ["artifact_inventory", "event_topics", "verdict", "gaps", "next_commands"],
                    "properties": {
                        "artifact_inventory": { "type": "array" },
                        "event_topics": { "type": "array" },
                        "verdict": { "type": "object" },
                        "gaps": { "type": "array" },
                        "next_commands": { "type": "array" },
                    },
                })),
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
            "harn.eval.inspect_run" => self.tool_eval_inspect_run(arguments).await,
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

    pub(super) async fn tool_eval_inspect_run(
        &self,
        arguments: JsonValue,
    ) -> Result<JsonValue, String> {
        let request: EvalInspectRunRequest =
            serde_json::from_value(arguments).map_err(|error| error.to_string())?;
        inspect_eval_run(request).await
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

#[derive(Debug, Deserialize)]
struct EvalInspectRunRequest {
    output_dir: Option<PathBuf>,
    summary_json: Option<PathBuf>,
    per_run_jsonl: Option<PathBuf>,
    run_record: Option<PathBuf>,
    events_dir: Option<PathBuf>,
    events_db: Option<PathBuf>,
    include_payloads: Option<bool>,
    limit: Option<usize>,
}

async fn inspect_eval_run(request: EvalInspectRunRequest) -> Result<JsonValue, String> {
    let limit = request.limit.unwrap_or(250).clamp(1, 5_000);
    let include_payloads = request.include_payloads.unwrap_or(false);
    let output_dir = request.output_dir.or_else(|| {
        request
            .summary_json
            .as_ref()
            .and_then(|path| path.parent().map(Path::to_path_buf))
    });
    let summary_path = request.summary_json.or_else(|| {
        output_dir
            .as_ref()
            .map(|dir| dir.join("summary.json"))
            .filter(|path| path.exists())
    });
    let summary = summary_path
        .as_ref()
        .and_then(|path| load_json_file(path).ok());

    let events_dir = request
        .events_dir
        .or_else(|| path_from_summary(&summary, "event_log_dir"))
        .or_else(|| {
            output_dir
                .as_ref()
                .map(|dir| dir.join("events"))
                .filter(|path| path.exists())
        });
    let events_db = request.events_db.or_else(|| {
        output_dir
            .as_ref()
            .map(|dir| dir.join("events.sqlite"))
            .filter(|path| path.exists())
    });
    let per_run_jsonl = request.per_run_jsonl.or_else(|| {
        output_dir
            .as_ref()
            .map(|dir| dir.join("per_run.jsonl"))
            .filter(|path| path.exists())
    });
    let run_record = request
        .run_record
        .or_else(|| path_from_summary(&summary, "run_record_path"));
    let llm_transcript_dir = path_from_summary(&summary, "llm_transcript_dir").or_else(|| {
        output_dir
            .as_ref()
            .map(|dir| dir.join("llm"))
            .filter(|path| path.exists())
    });
    let final_diff = output_dir
        .as_ref()
        .map(|dir| dir.join("artifacts/final.diff"))
        .filter(|path| path.exists());

    let artifact_inventory = vec![
        artifact_report("output_dir", output_dir.as_deref())?,
        artifact_report("summary_json", summary_path.as_deref())?,
        artifact_report("per_run_jsonl", per_run_jsonl.as_deref())?,
        artifact_report("run_record", run_record.as_deref())?,
        artifact_report("events_dir", events_dir.as_deref())?,
        artifact_report("events_db", events_db.as_deref())?,
        artifact_report("llm_transcript_dir", llm_transcript_dir.as_deref())?,
        artifact_report("final_diff", final_diff.as_deref())?,
    ];

    let mut event_topics = Vec::new();
    if let Some(dir) = events_dir.as_deref() {
        event_topics.extend(scan_event_dir(dir, limit, include_payloads)?);
    }
    if let Some(db) = events_db.as_deref() {
        event_topics.extend(scan_sqlite_event_log(db, limit, include_payloads).await?);
    }

    let topic_names: Vec<String> = event_topics
        .iter()
        .filter_map(|topic| topic.get("topic").and_then(JsonValue::as_str))
        .map(str::to_string)
        .collect();
    let mut gaps = eval_artifact_gaps(
        &summary,
        summary_path.as_deref(),
        run_record.as_deref(),
        &event_topics,
        &topic_names,
    );
    if summary
        .as_ref()
        .and_then(|value| value.get("cross_check"))
        .and_then(|value| value.get("e2_reason"))
        .is_some()
        && summary
            .as_ref()
            .and_then(|value| value.pointer("/cross_check/e2_evidence"))
            .is_none()
    {
        gaps.push("summary has E2 text but no structured cross_check.e2_evidence".to_string());
    }

    Ok(json!({
        "artifact_inventory": artifact_inventory,
        "verdict": summary_verdict(summary.as_ref()),
        "event_topics": event_topics,
        "event_chain": event_chain_summary(&topic_names, &event_topics),
        "gaps": gaps,
        "next_commands": next_debug_commands(summary_path.as_deref(), run_record.as_deref(), events_db.as_deref()),
    }))
}

fn load_json_file(path: &Path) -> Result<JsonValue, String> {
    let content =
        fs::read_to_string(path).map_err(|error| format!("read {}: {error}", path.display()))?;
    serde_json::from_str(&content).map_err(|error| format!("parse {}: {error}", path.display()))
}

fn path_from_summary(summary: &Option<JsonValue>, key: &str) -> Option<PathBuf> {
    summary
        .as_ref()
        .and_then(|value| value.get(key))
        .and_then(JsonValue::as_str)
        .filter(|value| !value.trim().is_empty())
        .map(PathBuf::from)
}

fn artifact_report(label: &str, path: Option<&Path>) -> Result<JsonValue, String> {
    let Some(path) = path else {
        return Ok(json!({
            "label": label,
            "present": false,
            "reason": "not_provided",
        }));
    };
    let exists = path.exists();
    let metadata = fs::metadata(path).ok();
    let hash = if exists && metadata.as_ref().is_some_and(|meta| meta.is_file()) {
        Some(file_blake3(path)?)
    } else {
        None
    };
    Ok(json!({
        "label": label,
        "path": path.to_string_lossy(),
        "present": exists,
        "kind": metadata.as_ref().map(|meta| if meta.is_dir() { "dir" } else { "file" }),
        "bytes": metadata.as_ref().filter(|meta| meta.is_file()).map(std::fs::Metadata::len),
        "blake3": hash,
    }))
}

fn file_blake3(path: &Path) -> Result<String, String> {
    let mut file =
        fs::File::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
    let mut hasher = blake3::Hasher::new();
    let mut buffer = vec![0_u8; 64 * 1024].into_boxed_slice();
    loop {
        let n = file
            .read(&mut buffer)
            .map_err(|error| format!("read {}: {error}", path.display()))?;
        if n == 0 {
            break;
        }
        hasher.update(&buffer[..n]);
    }
    Ok(hasher.finalize().to_hex().to_string())
}

fn scan_event_dir(
    dir: &Path,
    limit: usize,
    include_payloads: bool,
) -> Result<Vec<JsonValue>, String> {
    let mut topics = Vec::new();
    let topic_dir = dir.join("topics");
    if topic_dir.is_dir() {
        let mut entries: Vec<_> = fs::read_dir(&topic_dir)
            .map_err(|error| format!("read {}: {error}", topic_dir.display()))?
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .filter(|path| path.extension().and_then(|ext| ext.to_str()) == Some("jsonl"))
            .collect();
        entries.sort();
        for path in entries {
            let topic = path
                .file_name()
                .and_then(|name| name.to_str())
                .and_then(|name| name.strip_suffix(".jsonl"))
                .unwrap_or("unknown");
            topics.push(scan_jsonl_topic(
                "file",
                topic,
                &path,
                limit,
                include_payloads,
            )?);
        }
    }

    let mut roots: Vec<_> = fs::read_dir(dir)
        .map_err(|error| format!("read {}: {error}", dir.display()))?
        .filter_map(Result::ok)
        .map(|entry| entry.path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("event_log-") && name.ends_with(".jsonl"))
        })
        .collect();
    roots.sort();
    for path in roots {
        let topic = path
            .file_stem()
            .and_then(|name| name.to_str())
            .unwrap_or("event_log");
        topics.push(scan_jsonl_topic(
            "file",
            topic,
            &path,
            limit,
            include_payloads,
        )?);
    }
    Ok(topics)
}

async fn scan_sqlite_event_log(
    path: &Path,
    limit: usize,
    include_payloads: bool,
) -> Result<Vec<JsonValue>, String> {
    let log = AnyEventLog::Sqlite(
        SqliteEventLog::open_read_only(path.to_path_buf(), 16)
            .map_err(|error| error.to_string())?,
    );
    let topics = log.topics().await.map_err(|error| error.to_string())?;
    let mut reports = Vec::new();
    for topic in topics {
        let latest = log
            .latest(&topic)
            .await
            .map_err(|error| error.to_string())?;
        let events = log
            .read_range(&topic, None, limit)
            .await
            .map_err(|error| error.to_string())?;
        let mut stats = EventTopicStats::default();
        for (id, event) in events {
            let value = json!({
                "id": id,
                "event": {
                    "kind": event.kind,
                    "payload": event.payload,
                    "headers": event.headers,
                    "occurred_at_ms": event.occurred_at_ms,
                }
            });
            stats.observe(&value, limit, include_payloads);
        }
        let counts_complete = latest.is_none_or(|latest_id| stats.last_id == Some(latest_id));
        reports.push(event_records_report(
            "sqlite",
            topic.as_str(),
            Some(path),
            latest,
            stats,
            counts_complete,
        ));
    }
    Ok(reports)
}

fn scan_jsonl_topic(
    backend: &str,
    topic: &str,
    path: &Path,
    limit: usize,
    include_payloads: bool,
) -> Result<JsonValue, String> {
    let file = fs::File::open(path).map_err(|error| format!("open {}: {error}", path.display()))?;
    let reader = BufReader::new(file);
    let mut stats = EventTopicStats::default();
    let mut invalid_lines = 0_u64;
    let mut total_lines = 0_u64;
    for line in reader.lines() {
        total_lines += 1;
        let line = line.map_err(|error| format!("read {}: {error}", path.display()))?;
        match serde_json::from_str::<JsonValue>(&line) {
            Ok(value) => stats.observe(&value, limit, include_payloads),
            Err(_) => invalid_lines += 1,
        }
    }
    let mut report = event_records_report(backend, topic, Some(path), None, stats, true);
    report["line_count"] = json!(total_lines);
    report["invalid_line_count"] = json!(invalid_lines);
    report["blake3"] = json!(file_blake3(path)?);
    Ok(report)
}

#[derive(Default)]
struct EventTopicStats {
    records_scanned: u64,
    sampled_event_count: u64,
    kinds: BTreeMap<String, u64>,
    payload_types: BTreeMap<String, u64>,
    roles: BTreeMap<String, u64>,
    first_id: Option<u64>,
    last_id: Option<u64>,
    first_sampled_id: Option<u64>,
    last_sampled_id: Option<u64>,
    previous_record_hash: Option<String>,
    provenance_breaks: u64,
    sample_provenance_breaks: u64,
    provenance_records: u64,
    samples: Vec<JsonValue>,
}

impl EventTopicStats {
    fn observe(&mut self, value: &JsonValue, sample_limit: usize, include_payloads: bool) {
        self.records_scanned += 1;
        let id = value.get("id").and_then(JsonValue::as_u64);
        self.first_id = self.first_id.or(id);
        self.last_id = id.or(self.last_id);

        // The sample is the first `sample_limit` records, so the sample-scoped
        // fields describe exactly that prefix window — distinct from the
        // full-scan `first_id`/`last_id` whenever a backend scans past the
        // limit (the JSONL reader walks every line; sqlite is already bounded).
        let is_sampled = self.sampled_event_count < sample_limit as u64;
        if is_sampled {
            self.sampled_event_count += 1;
            self.first_sampled_id = self.first_sampled_id.or(id);
            self.last_sampled_id = id.or(self.last_sampled_id);
            if include_payloads && self.samples.len() < 5 {
                self.samples.push(value.clone());
            }
        }

        let event = value.get("event").unwrap_or(value);
        if let Some(kind) = event.get("kind").and_then(JsonValue::as_str) {
            *self.kinds.entry(kind.to_string()).or_default() += 1;
        }
        if let Some(kind) = event
            .pointer("/payload/event/type")
            .or_else(|| event.pointer("/payload/type"))
            .and_then(JsonValue::as_str)
        {
            *self.payload_types.entry(kind.to_string()).or_default() += 1;
        }
        if let Some(role) = event
            .pointer("/payload/role")
            .or_else(|| event.pointer("/payload/message/role"))
            .and_then(JsonValue::as_str)
        {
            *self.roles.entry(role.to_string()).or_default() += 1;
        }
        let headers = event.get("headers").or_else(|| value.get("headers"));
        let record_hash = headers
            .and_then(|headers| headers.get("harn.provenance.record_hash"))
            .and_then(JsonValue::as_str);
        let prev_hash = headers
            .and_then(|headers| headers.get("harn.provenance.prev_hash"))
            .and_then(JsonValue::as_str);
        if let Some(record_hash) = record_hash {
            self.provenance_records += 1;
            if let Some(previous) = self.previous_record_hash.as_deref() {
                if prev_hash != Some(previous) {
                    self.provenance_breaks += 1;
                    if is_sampled {
                        self.sample_provenance_breaks += 1;
                    }
                }
            }
            self.previous_record_hash = Some(record_hash.to_string());
        }
    }
}

fn event_records_report(
    backend: &str,
    topic: &str,
    path: Option<&Path>,
    latest_id: Option<u64>,
    stats: EventTopicStats,
    counts_complete: bool,
) -> JsonValue {
    json!({
        "backend": backend,
        "topic": topic,
        "path": path.map(|path| path.to_string_lossy().into_owned()),
        "record_count": stats.records_scanned,
        "sampled_event_count": stats.sampled_event_count,
        "counts_complete": counts_complete,
        "latest_id": latest_id,
        "first_event_id": stats.first_id,
        "last_event_id": stats.last_id,
        "first_sampled_id": stats.first_sampled_id,
        "last_sampled_id": stats.last_sampled_id,
        "kinds": stats.kinds,
        "payload_event_types": stats.payload_types,
        "roles": stats.roles,
        "provenance": {
            "records_with_hash": stats.provenance_records,
            "chain_breaks": stats.provenance_breaks,
            "chain_ok": stats.provenance_breaks == 0,
            "chain_breaks_in_sample": stats.sample_provenance_breaks,
            "sample_chain_ok": stats.sample_provenance_breaks == 0,
        },
        "samples": stats.samples,
    })
}

fn summary_verdict(summary: Option<&JsonValue>) -> JsonValue {
    let Some(summary) = summary else {
        return json!({"summary_present": false});
    };
    json!({
        "summary_present": true,
        "task": summary.get("task"),
        "model": summary.get("model"),
        "result": summary.get("result"),
        "outcome_kind": summary.get("outcome_kind"),
        "verification_passed": summary.get("verification_passed"),
        "verify_timed_out": summary.get("verify_timed_out"),
        "verify_failure_excerpt": summary.get("verify_failure_excerpt"),
        "cross_check": {
            "e1_verdict": summary.pointer("/cross_check/e1_verdict"),
            "e2_verdict": summary.pointer("/cross_check/e2_verdict"),
            "e1_e2_aligned": summary.pointer("/cross_check/e1_e2_aligned"),
            "e2_reason": summary.pointer("/cross_check/e2_reason"),
            "judge_dead": summary.pointer("/cross_check/judge_dead"),
        },
        "completion_contract": {
            "contract_passed": summary.pointer("/completion_contract/contract_passed"),
            "run_ready_for_final": summary.pointer("/completion_contract/run_ready_for_final"),
            "verification_passed": summary.pointer("/completion_contract/verification_passed"),
            "warnings": summary.pointer("/completion_contract/completion_warnings"),
        },
    })
}

fn eval_artifact_gaps(
    summary: &Option<JsonValue>,
    summary_path: Option<&Path>,
    run_record: Option<&Path>,
    event_topics: &[JsonValue],
    topic_names: &[String],
) -> Vec<String> {
    let mut gaps = Vec::new();
    if summary_path.is_none_or(|path| !path.exists()) {
        gaps.push("missing summary_json".to_string());
    }
    if run_record.is_none_or(|path| !path.exists()) {
        gaps.push("missing run_record or run_record_path is null".to_string());
    }
    if event_topics.is_empty() {
        gaps.push("no event-log topics found".to_string());
    }
    if !topic_names
        .iter()
        .any(|name| name == "agent.transcript.llm")
    {
        gaps.push("missing agent.transcript.llm topic".to_string());
    }
    if !topic_names
        .iter()
        .any(|name| name.starts_with("observability.agent_events."))
    {
        gaps.push("missing observability.agent_events.<session-id> topic".to_string());
    }
    if summary
        .as_ref()
        .and_then(|value| value.get("verification"))
        .is_none()
    {
        gaps.push("summary lacks structured verification object".to_string());
    }
    gaps
}

fn event_chain_summary(topic_names: &[String], event_topics: &[JsonValue]) -> JsonValue {
    let mut aggregate_payload_types: BTreeMap<String, u64> = BTreeMap::new();
    for topic in event_topics {
        if let Some(map) = topic
            .get("payload_event_types")
            .and_then(JsonValue::as_object)
        {
            for (key, value) in map {
                if let Some(count) = value.as_u64() {
                    *aggregate_payload_types.entry(key.clone()).or_default() += count;
                }
            }
        }
    }
    // A topic can surface from both the JSONL dir and the sqlite db, so dedup
    // (and sort, for deterministic output) before reporting.
    let mut agent_event_topics: Vec<String> = topic_names
        .iter()
        .filter(|name| name.starts_with("observability.agent_events."))
        .cloned()
        .collect();
    agent_event_topics.sort();
    agent_event_topics.dedup();
    json!({
        "has_agent_transcript": topic_names.iter().any(|name| name == "agent.transcript.llm"),
        "agent_event_topics": agent_event_topics,
        "payload_event_types": aggregate_payload_types,
        "tool_call_events": aggregate_payload_types.get("tool_call").copied().unwrap_or(0),
        "tool_call_update_events": aggregate_payload_types
            .get("tool_call_update")
            .copied()
            .unwrap_or(0),
    })
}

fn next_debug_commands(
    summary_path: Option<&Path>,
    run_record: Option<&Path>,
    events_db: Option<&Path>,
) -> Vec<String> {
    let mut commands = Vec::new();
    if let Some(path) = summary_path {
        commands.push(format!(
            "jq '.cross_check,.verification,.completion_contract' {}",
            shell_quote_path(path)
        ));
    }
    if let Some(path) = run_record {
        commands.push(format!("harn runs view --json {}", shell_quote_path(path)));
    }
    if let Some(path) = events_db {
        commands.push(format!(
            "harn replay --events-db {} --json <add --session-id>",
            shell_quote_path(path)
        ));
    }
    commands
}

fn shell_quote_path(path: &Path) -> String {
    let raw = path.to_string_lossy();
    format!("'{}'", raw.replace('\'', "'\\''"))
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
        | "harn.eval.inspect_run"
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
