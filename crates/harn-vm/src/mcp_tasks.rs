//! One owner for the MCP tasks extension.
//!
//! Three Harn surfaces speak MCP: the orchestrator server (`harn serve
//! orchestrator`), the script-driven server (`mcp_tools(registry)`), and the
//! export server (`pub fn` entrypoints). All three advertise the same
//! `io.modelcontextprotocol/tasks` extension to clients, so all three owe
//! clients the same `tasks/get`, `tasks/update`, and `tasks/cancel` behavior.
//!
//! Before this module only the orchestrator implemented it. The script-driven
//! server advertised the capability and answered every one of the three methods
//! with `task not found` — a client that read the capability and polled was
//! told, truthfully-looking, that its task had vanished. Advertising a
//! capability a server cannot serve is worse than not advertising it: the
//! client has no way to tell the difference between "this server does not do
//! tasks" and "your task is gone".
//!
//! So the lifecycle lives here, once, and a server supplies only the part that
//! is actually its own: how to run the work. Everything a client can observe —
//! ids, bearer access, status transitions, terminal-status rules, the JSON
//! projections, the wake-up on completion — is decided in this file, which is
//! what makes the three surfaces answer the same way by construction rather
//! than by three sets of matching tests.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use tokio::sync::Notify;
use ts_rs::TS;
use uuid::Uuid;

use crate::mcp_protocol;

/// How long a created task stays retrievable, in milliseconds.
///
/// Long enough that a client which drops its connection mid-run can reconnect
/// and still collect the result, short enough that an abandoned poll loop does
/// not pin memory for the life of the server.
pub const DEFAULT_TASK_TTL_MS: u64 = 10 * 60 * 1000;

/// Whether one tool may be invoked as a task, as `tools/list` reports it.
///
/// MCP lets a server declare this per tool rather than server-wide, which is
/// what makes an honest partial implementation possible: a server can serve the
/// extension for the tools it can actually run that way and say `forbidden` for
/// the rest, instead of advertising a blanket capability and failing whichever
/// calls it cannot honor.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq, Serialize, Deserialize, JsonSchema, TS)]
#[serde(rename_all = "lowercase")]
#[schemars(rename = "ToolTaskSupport")]
#[ts(rename = "ToolTaskSupport")]
pub enum McpTaskSupport {
    /// The client must not ask for a task. This is the default: a tool has to
    /// opt in, so adding the extension cannot change how an existing tool
    /// behaves.
    #[default]
    Forbidden,
    /// The client may ask for a task; a plain call still works.
    Optional,
    /// The tool is only invocable as a task.
    Required,
}

impl McpTaskSupport {
    pub fn wire_name(self) -> &'static str {
        match self {
            Self::Forbidden => "forbidden",
            Self::Optional => "optional",
            Self::Required => "required",
        }
    }

    /// Parse a script-declared `execution: {taskSupport: "..."}` value.
    ///
    /// An unrecognized spelling reads as `Forbidden` rather than as the
    /// permissive value: a typo should cost the tool its task support, not
    /// silently grant a lifecycle the author did not ask for.
    pub fn from_wire(value: &str) -> Self {
        match value {
            "optional" => Self::Optional,
            "required" => Self::Required,
            _ => Self::Forbidden,
        }
    }

    pub fn allows_task(self) -> bool {
        matches!(self, Self::Optional | Self::Required)
    }
}

/// The observable state of one task, as `tasks/*` and the creating
/// `tools/call` project it.
#[derive(Clone, Debug)]
pub struct McpTaskState {
    pub task_id: String,
    pub status: mcp_protocol::McpTaskStatus,
    pub status_message: Option<String>,
    pub created_at: String,
    pub last_updated_at: String,
    pub ttl: Option<u64>,
    pub poll_interval: Option<u64>,
}

impl McpTaskState {
    pub fn to_json(&self) -> JsonValue {
        let mut value = json!({
            "taskId": self.task_id,
            "status": mcp_protocol::mcp_task_status_wire_name(self.status),
            "createdAt": self.created_at,
            "lastUpdatedAt": self.last_updated_at,
            "ttlMs": self.ttl,
        });
        if let Some(message) = &self.status_message {
            value["statusMessage"] = json!(message);
        }
        if let Some(poll_interval) = self.poll_interval {
            value["pollIntervalMs"] = json!(poll_interval);
        }
        value
    }
}

/// A task plus whatever it has produced, and the handle waiters park on.
#[derive(Clone, Debug)]
pub struct McpTaskRecord {
    pub task: McpTaskState,
    pub result: Option<JsonValue>,
    pub notify: Arc<Notify>,
}

impl McpTaskRecord {
    pub fn to_detailed_json(&self) -> JsonValue {
        let mut value = self.task.to_json();
        value["resultType"] = json!(mcp_protocol::RESULT_TYPE_COMPLETE);
        match self.task.status {
            mcp_protocol::McpTaskStatus::Completed => {
                value["result"] = self.result.clone().unwrap_or_else(|| json!({}));
            }
            mcp_protocol::McpTaskStatus::Failed => {
                value["error"] = json!({
                    "code": -32603,
                    "message": self.task.status_message.as_deref().unwrap_or("Task failed"),
                });
            }
            mcp_protocol::McpTaskStatus::Working
            | mcp_protocol::McpTaskStatus::InputRequired
            | mcp_protocol::McpTaskStatus::Cancelled => {}
            _ => unreachable!("Harn only creates MCP task statuses it handles"),
        }
        value
    }
}

/// Every task one MCP server is holding, and the whole lifecycle over them.
#[derive(Default)]
pub struct McpTaskStore {
    tasks: Mutex<BTreeMap<String, McpTaskRecord>>,
}

impl std::fmt::Debug for McpTaskStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self.tasks.lock().map(|tasks| tasks.len()).unwrap_or(0);
        formatter
            .debug_struct("McpTaskStore")
            .field("tasks", &count)
            .finish()
    }
}

impl McpTaskStore {
    pub fn new() -> Self {
        Self::default()
    }

    /// Register a new working task and return its unguessable bearer handle.
    ///
    /// The caller then runs the work however that surface runs work, and
    /// reports back through [`McpTaskStore::complete`]. The store deliberately
    /// does not own execution: a script tool runs on the VM thread, an
    /// orchestrator tool can enter a child VM, and pretending one scheduler
    /// fits both is how the two implementations diverged in the first place.
    pub fn create(&self, ttl: Option<u64>) -> McpTaskState {
        let now = now_rfc3339();
        let task = McpTaskState {
            task_id: Uuid::now_v7().to_string(),
            status: mcp_protocol::McpTaskStatus::Working,
            status_message: Some("The operation is now in progress.".to_string()),
            created_at: now.clone(),
            last_updated_at: now,
            ttl,
            poll_interval: Some(mcp_protocol::DEFAULT_TASK_POLL_INTERVAL_MS),
        };
        self.tasks.lock().expect("MCP tasks poisoned").insert(
            task.task_id.clone(),
            McpTaskRecord {
                task: task.clone(),
                result: None,
                notify: Arc::new(Notify::new()),
            },
        );
        task
    }

    /// Record a finished task and wake anything waiting on it.
    ///
    /// A cancelled task is left alone: the client has already been told the
    /// terminal status, and letting late work overwrite it would make cancel
    /// mean "maybe".
    pub fn complete(&self, task_id: &str, result: Result<JsonValue, String>) {
        let Some(wake) = ({
            let mut tasks = self.tasks.lock().expect("MCP tasks poisoned");
            let Some(record) = tasks.get_mut(task_id) else {
                return;
            };
            if record.task.status == mcp_protocol::McpTaskStatus::Cancelled {
                return;
            }
            let wake = record.notify.clone();
            record.task.last_updated_at = now_rfc3339();
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
            Some(wake)
        }) else {
            return;
        };
        wake.notify_waiters();
    }

    /// Record a completed task whose result is already an MCP `tools/call`
    /// result rather than a bare tool return value.
    ///
    /// A server that projects its own result -- the export adapter builds MCP
    /// content blocks before it knows whether the call was a task -- has
    /// nothing left for [`McpTaskStore::complete`] to wrap, and wrapping it
    /// again nests `content` inside `content`. Handing the projection over
    /// intact is also lossless: a multi-block or non-object result survives,
    /// where reconstructing a bare value from the blocks would not. `isError`
    /// belongs to the completed tool-call result; only failure to produce a
    /// result transitions the task itself to `failed`.
    pub fn complete_with_tool_result(&self, task_id: &str, result: JsonValue) {
        let Some(wake) = ({
            let mut tasks = self.tasks.lock().expect("MCP tasks poisoned");
            let Some(record) = tasks.get_mut(task_id) else {
                return;
            };
            if record.task.status == mcp_protocol::McpTaskStatus::Cancelled {
                return;
            }
            record.task.last_updated_at = now_rfc3339();
            record.task.status = mcp_protocol::McpTaskStatus::Completed;
            record.task.status_message = Some("The task produced a tool result.".to_string());
            record.result = Some(result);
            Some(record.notify.clone())
        }) else {
            return;
        };
        wake.notify_waiters();
    }

    /// A handle that fires when the named task reaches a terminal status.
    ///
    /// A caller that wants the result rather than a poll loop takes this
    /// *before* its first `tasks/get`, so a task that finishes between the read
    /// and the wait still wakes it.
    pub fn notifier(&self, task_id: &str) -> Option<Arc<Notify>> {
        self.tasks
            .lock()
            .expect("MCP tasks poisoned")
            .get(task_id)
            .map(|record| record.notify.clone())
    }

    /// The record named by the unguessable bearer handle in `params.taskId`.
    pub fn record_for_task(&self, params: &JsonValue) -> Result<McpTaskRecord, String> {
        let task_id = params
            .get("taskId")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| "Failed to retrieve task: missing taskId".to_string())?;
        let tasks = self.tasks.lock().expect("MCP tasks poisoned");
        let record = tasks
            .get(task_id)
            .ok_or_else(|| "Failed to retrieve task: task not found".to_string())?;
        Ok(record.clone())
    }

    pub fn handle_get(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        match self.record_for_task(params) {
            Ok(record) => crate::jsonrpc::response(id, record.to_detailed_json()),
            Err(error) => crate::jsonrpc::error_response(id, -32602, &error),
        }
    }

    /// `tasks/update` supplies responses to a task that asked for input.
    ///
    /// No Harn surface creates `input-required` tasks yet, so every update is
    /// answered as "nothing outstanding" — but the shape of the refusal still
    /// distinguishes a malformed call from a well-formed one against a task
    /// that simply is not waiting, which is what a client needs to know.
    pub fn handle_update(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        if let Err(error) = self.record_for_task(params) {
            return crate::jsonrpc::error_response(id, -32602, &error);
        }
        let supplied = params
            .get("inputResponses")
            .and_then(JsonValue::as_object)
            .is_some_and(|responses| !responses.is_empty());
        let message = if supplied {
            "Task has no outstanding input requests"
        } else {
            "tasks/update requires at least one input response"
        };
        crate::jsonrpc::error_response(id, -32602, message)
    }

    pub fn handle_cancel(&self, id: JsonValue, params: &JsonValue) -> JsonValue {
        let task_id = match params.get("taskId").and_then(JsonValue::as_str) {
            Some(task_id) if !task_id.is_empty() => task_id.to_string(),
            _ => {
                return crate::jsonrpc::error_response(
                    id,
                    -32602,
                    "Cannot cancel task: missing taskId",
                );
            }
        };
        let notify = {
            let mut tasks = self.tasks.lock().expect("MCP tasks poisoned");
            let Some(record) = tasks.get_mut(&task_id) else {
                return crate::jsonrpc::error_response(
                    id,
                    -32602,
                    "Cannot cancel task: task not found",
                );
            };
            if record.task.status.is_terminal() {
                return crate::jsonrpc::response(id, json!({}));
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
            record.notify.clone()
        };
        notify.notify_waiters();
        crate::jsonrpc::response(id, json!({}))
    }
}

/// The `tools/call` response that hands a client a task instead of a result.
pub fn task_created_response(id: JsonValue, task: &McpTaskState, note: &str) -> JsonValue {
    let mut result = task.to_json();
    result["resultType"] = json!("task");
    result["_meta"] = json!({
        crate::tool_registry::HARN_MCP_TOOL_CONTRACT_META_KEY: {
            "immediateResponse": note,
        },
    });
    crate::jsonrpc::response(id, result)
}

/// Project a tool's return value as an MCP `tools/call` result.
pub fn tool_call_result_json(value: JsonValue, is_error: bool) -> JsonValue {
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

fn now_rfc3339() -> String {
    crate::clock::system_now_rfc3339()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(task_id: &str) -> JsonValue {
        json!({ "taskId": task_id })
    }

    #[test]
    fn a_created_task_is_retrievable_by_its_unguessable_bearer_handle() {
        let store = McpTaskStore::new();
        let task = store.create(Some(DEFAULT_TASK_TTL_MS));

        let read = store.handle_get(json!(1), &params(&task.task_id));
        assert_eq!(read["result"]["taskId"], json!(task.task_id));
        assert_eq!(read["result"]["status"], json!("working"));
        assert_eq!(
            uuid::Uuid::parse_str(&task.task_id)
                .unwrap()
                .get_version_num(),
            7
        );
    }

    #[test]
    fn completing_a_task_publishes_its_result() {
        let store = McpTaskStore::new();
        let task = store.create(None);
        store.complete(&task.task_id, Ok(json!({ "answer": 42 })));

        let read = store.handle_get(json!(1), &params(&task.task_id));
        assert_eq!(read["result"]["status"], json!("completed"));
        assert_eq!(
            read["result"]["result"]["structuredContent"],
            json!({ "answer": 42 })
        );
    }

    #[test]
    fn a_completed_tool_error_remains_a_typed_result() {
        let store = McpTaskStore::new();
        let task = store.create(None);
        let result = json!({
            "content": [{"type": "text", "text": "NotFound"}],
            "isError": true,
            "_meta": {
                crate::tool_registry::HARN_MCP_TOOL_CONTRACT_META_KEY: {
                    "applicationError": {"tool": "lookup"},
                },
            },
        });
        store.complete_with_tool_result(&task.task_id, result.clone());

        let read = store.handle_get(json!(1), &params(&task.task_id));
        assert_eq!(read["result"]["status"], "completed");
        assert_eq!(read["result"]["result"], result);
    }

    #[test]
    fn a_failed_task_reports_an_error_rather_than_an_empty_result() {
        let store = McpTaskStore::new();
        let task = store.create(None);
        store.complete(&task.task_id, Err("boom".to_string()));

        let read = store.handle_get(json!(1), &params(&task.task_id));
        assert_eq!(read["result"]["status"], json!("failed"));
        assert_eq!(read["result"]["error"]["code"], json!(-32603));
        assert!(read["result"]["error"]["message"]
            .as_str()
            .expect("failed tasks carry a message")
            .contains("boom"));
    }

    #[test]
    fn cancel_is_terminal_and_late_work_cannot_overwrite_it() {
        let store = McpTaskStore::new();
        let task = store.create(None);

        let cancelled = store.handle_cancel(json!(1), &params(&task.task_id));
        assert_eq!(cancelled["result"], json!({}));

        // The work was already in flight and finishes after the cancel. If it
        // won, `tasks/cancel` would mean "maybe", and a client that cancelled a
        // destructive operation would still see it succeed.
        store.complete(&task.task_id, Ok(json!({ "answer": 42 })));
        let read = store.handle_get(json!(2), &params(&task.task_id));
        assert_eq!(read["result"]["status"], json!("cancelled"));

        let again = store.handle_cancel(json!(3), &params(&task.task_id));
        assert_eq!(again["result"], json!({}));
    }

    #[test]
    fn an_unknown_task_id_is_not_found_rather_than_a_silent_success() {
        let store = McpTaskStore::new();
        for response in [
            store.handle_get(json!(1), &params("missing")),
            store.handle_update(json!(2), &params("missing")),
        ] {
            assert_eq!(
                response["error"]["message"],
                json!("Failed to retrieve task: task not found")
            );
        }
        assert_eq!(
            store.handle_cancel(json!(3), &params("missing"))["error"]["message"],
            json!("Cannot cancel task: task not found")
        );
    }

    #[test]
    fn update_separates_a_malformed_call_from_a_task_that_is_not_waiting() {
        let store = McpTaskStore::new();
        let task = store.create(None);

        let empty = store.handle_update(json!(1), &params(&task.task_id));
        assert_eq!(
            empty["error"]["message"],
            json!("tasks/update requires at least one input response")
        );

        let supplied = store.handle_update(
            json!(2),
            &json!({ "taskId": task.task_id, "inputResponses": { "q": "a" } }),
        );
        assert_eq!(
            supplied["error"]["message"],
            json!("Task has no outstanding input requests")
        );
    }
}
