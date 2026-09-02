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
//! ids, authorization binding, status transitions, terminal-status rules, the JSON
//! projections, the wake-up on completion — is decided in this file, which is
//! what makes the three surfaces answer the same way by construction rather
//! than by three sets of matching tests.

use std::collections::BTreeMap;
use std::num::NonZeroUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, Weak};

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

/// Maximum task records retained by one server process.
///
/// The TTL bounds ordinary result retention. This hard ceiling also covers
/// callers that explicitly request an unlimited lifetime and protects a
/// server that receives no later request with which to run an expiry sweep.
pub const DEFAULT_MAX_TASK_RECORDS: usize = 1_024;

/// Opaque authorization context bound to a task at creation.
///
/// Authenticated adapters derive this from stable, non-secret principal
/// fields. The store keeps only the fingerprint, never a credential or a
/// customer-facing identity. An unscoped context is reserved for transports
/// where no authentication context exists, such as one local stdio peer.
#[derive(Clone, Debug, Eq, PartialEq)]
pub struct McpTaskAccess {
    fingerprint: Option<blake3::Hash>,
}

impl McpTaskAccess {
    pub fn unscoped() -> Self {
        Self { fingerprint: None }
    }

    pub fn authenticated(scheme: &str, subject: &str, tenant: Option<&str>) -> Self {
        let mut hasher = blake3::Hasher::new();
        hasher.update(b"harn.mcp.task-access.v1\0");
        for part in [scheme, subject, tenant.unwrap_or("")] {
            hasher.update(&(part.len() as u64).to_le_bytes());
            hasher.update(part.as_bytes());
        }
        hasher.update(&[u8::from(tenant.is_some())]);
        Self {
            fingerprint: Some(hasher.finalize()),
        }
    }
}

/// Resource policy for one task store.
#[derive(Clone, Debug)]
struct McpTaskPolicy {
    max_records: NonZeroUsize,
}

impl Default for McpTaskPolicy {
    fn default() -> Self {
        Self {
            max_records: NonZeroUsize::new(DEFAULT_MAX_TASK_RECORDS)
                .expect("default MCP task capacity is non-zero"),
        }
    }
}

#[derive(Clone, Debug, Eq, PartialEq)]
pub enum McpTaskAdmissionError {
    Capacity {
        limit: usize,
        active: usize,
        retained_terminal: usize,
    },
}

impl std::fmt::Display for McpTaskAdmissionError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Capacity {
                limit,
                active,
                retained_terminal,
            } => write!(
                formatter,
                "MCP task capacity exhausted (limit={limit}, active={active}, retained_terminal={retained_terminal})"
            ),
        }
    }
}

impl std::error::Error for McpTaskAdmissionError {}

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
struct McpTaskRecord {
    task: McpTaskState,
    result: Option<JsonValue>,
    notify: Arc<Notify>,
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

#[derive(Debug)]
struct StoredTask {
    record: McpTaskRecord,
    access: McpTaskAccess,
    cancel_token: Arc<AtomicBool>,
    cancel_notify: Arc<Notify>,
    expires_at_ms: Option<i64>,
    sequence: u64,
}

#[derive(Debug, Default)]
struct McpTaskStoreState {
    tasks: BTreeMap<String, StoredTask>,
    next_sequence: u64,
}

#[derive(Debug)]
struct McpTaskStoreInner {
    state: Mutex<McpTaskStoreState>,
    clock: Arc<dyn harn_clock::Clock>,
    policy: McpTaskPolicy,
}

/// Every task one MCP server is holding, and the whole lifecycle over them.
///
/// The store is the single owner of authorization binding, cancellation,
/// retention, and terminal-state transitions. Adapters receive a lease rather
/// than a bare id so the cancellation token installed in execution is exactly
/// the token signalled by `tasks/cancel`.
#[derive(Clone)]
pub struct McpTaskStore {
    inner: Arc<McpTaskStoreInner>,
}

impl Default for McpTaskStore {
    fn default() -> Self {
        Self::with_clock_and_policy(harn_clock::RealClock::arc(), McpTaskPolicy::default())
    }
}

/// Exclusive completion authority for one running task.
///
/// Dropping a live lease records failure, so an adapter cannot accidentally
/// strand a task in `working` by returning early or panicking across its own
/// execution boundary.
pub struct McpTaskLease {
    task_id: Option<String>,
    task: McpTaskState,
    cancel_token: Arc<AtomicBool>,
    cancel_notify: Arc<Notify>,
    store: Weak<McpTaskStoreInner>,
}

impl std::fmt::Debug for McpTaskLease {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("McpTaskLease")
            .field("task_id", &self.task.task_id)
            .field("cancel_requested", &self.cancel_requested())
            .finish()
    }
}

impl McpTaskLease {
    pub fn task(&self) -> &McpTaskState {
        &self.task
    }

    pub fn cancel_token(&self) -> Arc<AtomicBool> {
        self.cancel_token.clone()
    }

    pub fn cancel_requested(&self) -> bool {
        self.cancel_token.load(Ordering::Acquire)
    }

    pub async fn cancelled(&self) {
        loop {
            let notified = self.cancel_notify.notified();
            if self.cancel_requested() {
                return;
            }
            notified.await;
        }
    }

    pub fn complete(mut self, result: Result<JsonValue, String>, uses_result_envelope: bool) {
        let Some(task_id) = self.task_id.take() else {
            return;
        };
        let Some(store) = self.store.upgrade() else {
            return;
        };
        let outcome = match result {
            Ok(value) => TaskOutcome::Value {
                value,
                uses_result_envelope,
            },
            Err(error) => TaskOutcome::Failed {
                error,
                uses_result_envelope,
            },
        };
        finish_task(&store, &task_id, outcome);
    }

    pub fn complete_with_tool_result(mut self, result: JsonValue, uses_result_envelope: bool) {
        let Some(task_id) = self.task_id.take() else {
            return;
        };
        let Some(store) = self.store.upgrade() else {
            return;
        };
        finish_task(
            &store,
            &task_id,
            TaskOutcome::ToolResult {
                result,
                uses_result_envelope,
            },
        );
    }

    pub fn cancel(mut self) {
        let Some(task_id) = self.task_id.take() else {
            return;
        };
        let Some(store) = self.store.upgrade() else {
            return;
        };
        finish_task(&store, &task_id, TaskOutcome::Cancelled);
    }
}

impl Drop for McpTaskLease {
    fn drop(&mut self) {
        let Some(task_id) = self.task_id.take() else {
            return;
        };
        let Some(store) = self.store.upgrade() else {
            return;
        };
        finish_task(
            &store,
            &task_id,
            TaskOutcome::Failed {
                error: "Task execution ended without reporting an outcome".to_string(),
                uses_result_envelope: false,
            },
        );
    }
}

enum TaskOutcome {
    Value {
        value: JsonValue,
        uses_result_envelope: bool,
    },
    ToolResult {
        result: JsonValue,
        uses_result_envelope: bool,
    },
    Failed {
        error: String,
        uses_result_envelope: bool,
    },
    Cancelled,
}

impl std::fmt::Debug for McpTaskStore {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let count = self
            .inner
            .state
            .lock()
            .map(|state| state.tasks.len())
            .unwrap_or(0);
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

    fn with_clock_and_policy(clock: Arc<dyn harn_clock::Clock>, policy: McpTaskPolicy) -> Self {
        Self {
            inner: Arc::new(McpTaskStoreInner {
                state: Mutex::new(McpTaskStoreState::default()),
                clock,
                policy,
            }),
        }
    }

    /// Register a new working task and return its exclusive execution lease.
    pub fn begin(
        &self,
        access: McpTaskAccess,
        ttl: Option<u64>,
    ) -> Result<McpTaskLease, McpTaskAdmissionError> {
        let now = harn_clock::now_rfc3339(self.inner.clock.as_ref());
        let now_ms = self.inner.clock.monotonic_ms();
        let task = McpTaskState {
            task_id: Uuid::now_v7().to_string(),
            status: mcp_protocol::McpTaskStatus::Working,
            status_message: Some("The operation is now in progress.".to_string()),
            created_at: now.clone(),
            last_updated_at: now,
            ttl,
            poll_interval: Some(mcp_protocol::DEFAULT_TASK_POLL_INTERVAL_MS),
        };
        let cancel_token = Arc::new(AtomicBool::new(false));
        let cancel_notify = Arc::new(Notify::new());
        let expires_at_ms =
            ttl.map(|ttl_ms| now_ms.saturating_add(i64::try_from(ttl_ms).unwrap_or(i64::MAX)));
        {
            let mut state = self.inner.state.lock().expect("MCP tasks poisoned");
            sweep_expired(&mut state, now_ms);
            make_capacity(&mut state, self.inner.policy.max_records.get())?;
            let sequence = state.next_sequence;
            state.next_sequence = state.next_sequence.wrapping_add(1);
            state.tasks.insert(
                task.task_id.clone(),
                StoredTask {
                    record: McpTaskRecord {
                        task: task.clone(),
                        result: None,
                        notify: Arc::new(Notify::new()),
                    },
                    access,
                    cancel_token: cancel_token.clone(),
                    cancel_notify: cancel_notify.clone(),
                    expires_at_ms,
                    sequence,
                },
            );
        }
        Ok(McpTaskLease {
            task_id: Some(task.task_id.clone()),
            task,
            cancel_token,
            cancel_notify,
            store: Arc::downgrade(&self.inner),
        })
    }

    /// A handle that fires when the named task reaches a terminal status.
    ///
    /// A caller that wants the result rather than a poll loop takes this
    /// *before* its first `tasks/get`, so a task that finishes between the read
    /// and the wait still wakes it.
    pub fn notifier(&self, access: &McpTaskAccess, task_id: &str) -> Option<Arc<Notify>> {
        self.lookup(access, task_id).map(|record| record.notify)
    }

    fn lookup(&self, access: &McpTaskAccess, task_id: &str) -> Option<McpTaskRecord> {
        let now_ms = self.inner.clock.monotonic_ms();
        let mut state = self.inner.state.lock().expect("MCP tasks poisoned");
        sweep_expired(&mut state, now_ms);
        let stored = state.tasks.get(task_id)?;
        if &stored.access != access {
            return None;
        }
        Some(stored.record.clone())
    }

    /// The record named by `params.taskId`, only in its creation context.
    fn record_for_task(
        &self,
        access: &McpTaskAccess,
        params: &JsonValue,
    ) -> Result<McpTaskRecord, String> {
        let task_id = params
            .get("taskId")
            .and_then(JsonValue::as_str)
            .ok_or_else(|| "Failed to retrieve task: missing taskId".to_string())?;
        self.lookup(access, task_id)
            .ok_or_else(|| "Failed to retrieve task: task not found".to_string())
    }

    pub fn handle_get(
        &self,
        access: &McpTaskAccess,
        id: JsonValue,
        params: &JsonValue,
    ) -> JsonValue {
        match self.record_for_task(access, params) {
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
    pub fn handle_update(
        &self,
        access: &McpTaskAccess,
        id: JsonValue,
        params: &JsonValue,
    ) -> JsonValue {
        if let Err(error) = self.record_for_task(access, params) {
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

    pub fn handle_cancel(
        &self,
        access: &McpTaskAccess,
        id: JsonValue,
        params: &JsonValue,
    ) -> JsonValue {
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
        {
            let now_ms = self.inner.clock.monotonic_ms();
            let mut state = self.inner.state.lock().expect("MCP tasks poisoned");
            sweep_expired(&mut state, now_ms);
            let Some(stored) = state.tasks.get_mut(&task_id) else {
                return crate::jsonrpc::error_response(
                    id,
                    -32602,
                    "Cannot cancel task: task not found",
                );
            };
            if &stored.access != access {
                return crate::jsonrpc::error_response(
                    id,
                    -32602,
                    "Cannot cancel task: task not found",
                );
            }
            if stored.record.task.status.is_terminal() {
                return crate::jsonrpc::response(id, json!({}));
            }
            stored.cancel_token.store(true, Ordering::Release);
            stored.cancel_notify.notify_waiters();
            stored.record.task.status_message = Some("Cancellation requested.".to_string());
            stored.record.task.last_updated_at = harn_clock::now_rfc3339(self.inner.clock.as_ref());
        }
        crate::jsonrpc::response(id, json!({}))
    }
}

fn sweep_expired(state: &mut McpTaskStoreState, now_ms: i64) {
    let expired = state
        .tasks
        .iter()
        .filter(|(_, stored)| {
            stored
                .expires_at_ms
                .is_some_and(|deadline| now_ms >= deadline)
        })
        .map(|(task_id, _)| task_id.clone())
        .collect::<Vec<_>>();
    for task_id in expired {
        if let Some(stored) = state.tasks.remove(&task_id) {
            stored.cancel_token.store(true, Ordering::Release);
            stored.cancel_notify.notify_waiters();
            stored.record.notify.notify_waiters();
        }
    }
}

fn make_capacity(state: &mut McpTaskStoreState, limit: usize) -> Result<(), McpTaskAdmissionError> {
    while state.tasks.len() >= limit {
        let oldest_terminal = state
            .tasks
            .iter()
            .filter(|(_, stored)| stored.record.task.status.is_terminal())
            .min_by_key(|(_, stored)| stored.sequence)
            .map(|(task_id, _)| task_id.clone());
        let Some(task_id) = oldest_terminal else {
            let active = state
                .tasks
                .values()
                .filter(|stored| !stored.record.task.status.is_terminal())
                .count();
            return Err(McpTaskAdmissionError::Capacity {
                limit,
                active,
                retained_terminal: state.tasks.len().saturating_sub(active),
            });
        };
        state.tasks.remove(&task_id);
    }
    Ok(())
}

fn finish_task(store: &McpTaskStoreInner, task_id: &str, outcome: TaskOutcome) {
    let wake = {
        let now_ms = store.clock.monotonic_ms();
        let mut state = store.state.lock().expect("MCP tasks poisoned");
        sweep_expired(&mut state, now_ms);
        let Some(stored) = state.tasks.get_mut(task_id) else {
            return;
        };
        if stored.record.task.status.is_terminal() {
            return;
        }
        // `tasks/cancel` acknowledges the request while holding this same
        // lock. Once it returns, a completion that was ready in the same
        // scheduler turn must not overwrite the accepted cancellation. If
        // completion acquired the lock first, the terminal-status check above
        // preserves that already-linearized result.
        let outcome = if stored.cancel_token.load(Ordering::Acquire) {
            TaskOutcome::Cancelled
        } else {
            outcome
        };
        stored.record.task.last_updated_at = harn_clock::now_rfc3339(store.clock.as_ref());
        match outcome {
            TaskOutcome::Value {
                value,
                uses_result_envelope,
            } => {
                stored.record.task.status = mcp_protocol::McpTaskStatus::Completed;
                stored.record.task.status_message =
                    Some("The task completed successfully.".to_string());
                let mut result = tool_call_result_json(value, false);
                if uses_result_envelope {
                    mcp_protocol::apply_result_envelope(&mut result, None);
                }
                stored.record.result = Some(result);
            }
            TaskOutcome::ToolResult {
                mut result,
                uses_result_envelope,
            } => {
                if uses_result_envelope {
                    mcp_protocol::apply_result_envelope(&mut result, None);
                }
                stored.record.task.status = mcp_protocol::McpTaskStatus::Completed;
                stored.record.task.status_message =
                    Some("The task produced a tool result.".to_string());
                stored.record.result = Some(result);
            }
            TaskOutcome::Failed {
                error,
                uses_result_envelope,
            } => {
                stored.record.task.status = mcp_protocol::McpTaskStatus::Failed;
                stored.record.task.status_message = Some(format!("Tool execution failed: {error}"));
                let mut result = tool_call_result_json(json!(error), true);
                if uses_result_envelope {
                    mcp_protocol::apply_result_envelope(&mut result, None);
                }
                stored.record.result = Some(result);
            }
            TaskOutcome::Cancelled => {
                stored.record.task.status = mcp_protocol::McpTaskStatus::Cancelled;
                stored.record.task.status_message =
                    Some("The task was cancelled by request.".to_string());
                stored.record.result = Some(tool_call_result_json(
                    json!("Task was cancelled by request."),
                    true,
                ));
            }
        }
        stored.record.notify.clone()
    };
    wake.notify_waiters();
}

/// The `tools/call` response that hands a client a task instead of a result.
pub fn task_created_response(id: JsonValue, task: &McpTaskState, note: &str) -> JsonValue {
    let mut result = task.to_json();
    result["resultType"] = json!("task");
    result["_meta"] = json!({});
    result["_meta"][crate::tool_registry::HARN_MCP_TOOL_CONTRACT_META_KEY] = json!({
        "immediateResponse": note,
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    fn unscoped() -> McpTaskAccess {
        McpTaskAccess::unscoped()
    }

    fn begin(store: &McpTaskStore, ttl: Option<u64>) -> McpTaskLease {
        store.begin(unscoped(), ttl).expect("task admitted")
    }

    fn params(task_id: &str) -> JsonValue {
        json!({ "taskId": task_id })
    }

    #[test]
    fn a_created_task_is_retrievable_by_its_unguessable_bearer_handle() {
        let store = McpTaskStore::new();
        let lease = begin(&store, Some(DEFAULT_TASK_TTL_MS));
        let task = lease.task().clone();

        let read = store.handle_get(&unscoped(), json!(1), &params(&task.task_id));
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
    fn task_creation_note_uses_only_the_harn_vendor_namespace() {
        let store = McpTaskStore::new();
        let lease = begin(&store, None);
        let response = task_created_response(json!(1), lease.task(), "working");
        assert_eq!(
            response["result"]["_meta"][crate::tool_registry::HARN_MCP_TOOL_CONTRACT_META_KEY]
                ["immediateResponse"],
            json!("working")
        );
        assert!(response["result"]["_meta"]
            .get("io.modelcontextprotocol/model-immediate-response")
            .is_none());
    }

    #[test]
    fn completing_a_task_publishes_its_result() {
        let store = McpTaskStore::new();
        let lease = begin(&store, None);
        let task_id = lease.task().task_id.clone();
        lease.complete(Ok(json!({ "answer": 42 })), false);

        let read = store.handle_get(&unscoped(), json!(1), &params(&task_id));
        assert_eq!(read["result"]["status"], json!("completed"));
        assert_eq!(
            read["result"]["result"]["structuredContent"],
            json!({ "answer": 42 })
        );
    }

    #[test]
    fn a_completed_tool_error_remains_a_typed_result() {
        let store = McpTaskStore::new();
        let lease = begin(&store, None);
        let task_id = lease.task().task_id.clone();
        let result = json!({
            "content": [{"type": "text", "text": "NotFound"}],
            "isError": true,
            "_meta": {
                crate::tool_registry::HARN_MCP_TOOL_CONTRACT_META_KEY: {
                    "applicationError": {"tool": "lookup"},
                },
            },
        });
        lease.complete_with_tool_result(result.clone(), false);

        let read = store.handle_get(&unscoped(), json!(1), &params(&task_id));
        assert_eq!(read["result"]["status"], "completed");
        assert_eq!(read["result"]["result"], result);
    }

    #[test]
    fn stable_completed_tool_result_gets_the_stable_envelope_only_when_requested() {
        let store = McpTaskStore::new();
        let stable_task = begin(&store, None);
        let standard_task = begin(&store, None);
        let stable_bare_task = begin(&store, None);
        let stable_id = stable_task.task().task_id.clone();
        let standard_id = standard_task.task().task_id.clone();
        let stable_bare_id = stable_bare_task.task().task_id.clone();
        let result = json!({
            "content": [{"type": "text", "text": "ok"}],
            "isError": false,
        });
        stable_task.complete_with_tool_result(result.clone(), true);
        standard_task.complete_with_tool_result(result, false);
        stable_bare_task.complete(Ok(json!({"value": "ok"})), true);

        let stable = store.handle_get(&unscoped(), json!(1), &params(&stable_id));
        let standard = store.handle_get(&unscoped(), json!(2), &params(&standard_id));
        let stable_bare = store.handle_get(&unscoped(), json!(3), &params(&stable_bare_id));
        assert_eq!(stable["result"]["result"]["resultType"], "complete");
        assert!(standard["result"]["result"].get("resultType").is_none());
        assert_eq!(stable_bare["result"]["result"]["resultType"], "complete");
    }

    #[test]
    fn a_failed_task_reports_an_error_rather_than_an_empty_result() {
        let store = McpTaskStore::new();
        let lease = begin(&store, None);
        let task_id = lease.task().task_id.clone();
        lease.complete(Err("boom".to_string()), false);

        let read = store.handle_get(&unscoped(), json!(1), &params(&task_id));
        assert_eq!(read["result"]["status"], json!("failed"));
        assert_eq!(read["result"]["error"]["code"], json!(-32603));
        assert!(read["result"]["error"]["message"]
            .as_str()
            .expect("failed tasks carry a message")
            .contains("boom"));
    }

    #[test]
    fn cancel_requests_cooperative_stop_and_execution_owns_terminal_truth() {
        let store = McpTaskStore::new();
        let lease = begin(&store, None);
        let task_id = lease.task().task_id.clone();
        let token = lease.cancel_token();

        let cancelled = store.handle_cancel(&unscoped(), json!(1), &params(&task_id));
        assert_eq!(cancelled["result"], json!({}));
        assert!(token.load(Ordering::Acquire));
        let pending = store.handle_get(&unscoped(), json!(2), &params(&task_id));
        assert_eq!(pending["result"]["status"], json!("working"));
        assert_eq!(
            pending["result"]["statusMessage"],
            json!("Cancellation requested.")
        );

        lease.cancel();
        let read = store.handle_get(&unscoped(), json!(3), &params(&task_id));
        assert_eq!(read["result"]["status"], json!("cancelled"));

        let again = store.handle_cancel(&unscoped(), json!(4), &params(&task_id));
        assert_eq!(again["result"], json!({}));
    }

    #[test]
    fn acknowledged_cancel_wins_over_same_turn_completion() {
        let store = McpTaskStore::new();
        let lease = begin(&store, None);
        let task_id = lease.task().task_id.clone();

        store.handle_cancel(&unscoped(), json!(1), &params(&task_id));
        lease.complete(Ok(json!({"committed": true})), false);

        let read = store.handle_get(&unscoped(), json!(2), &params(&task_id));
        assert_eq!(read["result"]["status"], "cancelled");
        assert!(read["result"].get("result").is_none());
    }

    #[test]
    fn completion_that_linearizes_before_cancel_stays_completed() {
        let store = McpTaskStore::new();
        let lease = begin(&store, None);
        let task_id = lease.task().task_id.clone();

        lease.complete(Ok(json!({"committed": true})), false);
        store.handle_cancel(&unscoped(), json!(1), &params(&task_id));

        let read = store.handle_get(&unscoped(), json!(2), &params(&task_id));
        assert_eq!(read["result"]["status"], "completed");
        assert_eq!(
            read["result"]["result"]["structuredContent"]["committed"],
            true
        );
    }

    #[test]
    fn an_unknown_task_id_is_not_found_rather_than_a_silent_success() {
        let store = McpTaskStore::new();
        for response in [
            store.handle_get(&unscoped(), json!(1), &params("missing")),
            store.handle_update(&unscoped(), json!(2), &params("missing")),
        ] {
            assert_eq!(
                response["error"]["message"],
                json!("Failed to retrieve task: task not found")
            );
        }
        assert_eq!(
            store.handle_cancel(&unscoped(), json!(3), &params("missing"))["error"]["message"],
            json!("Cannot cancel task: task not found")
        );
    }

    #[test]
    fn update_separates_a_malformed_call_from_a_task_that_is_not_waiting() {
        let store = McpTaskStore::new();
        let lease = begin(&store, None);
        let task_id = lease.task().task_id.clone();

        let empty = store.handle_update(&unscoped(), json!(1), &params(&task_id));
        assert_eq!(
            empty["error"]["message"],
            json!("tasks/update requires at least one input response")
        );

        let supplied = store.handle_update(
            &unscoped(),
            json!(2),
            &json!({ "taskId": task_id, "inputResponses": { "q": "a" } }),
        );
        assert_eq!(
            supplied["error"]["message"],
            json!("Task has no outstanding input requests")
        );
    }

    #[test]
    fn authenticated_task_ids_do_not_cross_principal_boundaries() {
        let store = McpTaskStore::new();
        let owner = McpTaskAccess::authenticated("bearer", "subject-a", Some("tenant-a"));
        let stranger = McpTaskAccess::authenticated("bearer", "subject-b", Some("tenant-b"));
        assert_ne!(
            McpTaskAccess::authenticated("bearer", "subject-a", None),
            McpTaskAccess::authenticated("bearer", "subject-a", Some("")),
            "missing tenant context must not collapse into a measured empty tenant"
        );
        let lease = store.begin(owner.clone(), None).expect("task admitted");
        let task_id = lease.task().task_id.clone();

        assert_eq!(
            store.handle_get(&owner, json!(1), &params(&task_id))["result"]["status"],
            "working"
        );
        for response in [
            store.handle_get(&stranger, json!(2), &params(&task_id)),
            store.handle_update(&stranger, json!(3), &params(&task_id)),
            store.handle_cancel(&stranger, json!(4), &params(&task_id)),
        ] {
            assert!(response["error"]["message"]
                .as_str()
                .expect("error message")
                .contains("task not found"));
        }
        assert!(!lease.cancel_requested());
    }

    #[test]
    fn ttl_expiry_cancels_work_and_cannot_be_resurrected() {
        let clock = harn_clock::PausedClock::new(time::OffsetDateTime::UNIX_EPOCH);
        let store = McpTaskStore::with_clock_and_policy(clock.clone(), McpTaskPolicy::default());
        let lease = store.begin(unscoped(), Some(10)).expect("task admitted");
        let task_id = lease.task().task_id.clone();
        let token = lease.cancel_token();

        clock.advance(Duration::from_millis(10));
        let expired = store.handle_get(&unscoped(), json!(1), &params(&task_id));
        assert_eq!(
            expired["error"]["message"],
            "Failed to retrieve task: task not found"
        );
        assert!(token.load(Ordering::Acquire));

        lease.complete(Ok(json!({"late": true})), false);
        assert_eq!(
            store.handle_get(&unscoped(), json!(2), &params(&task_id))["error"]["message"],
            "Failed to retrieve task: task not found"
        );
    }

    #[test]
    fn capacity_evicts_oldest_terminal_but_never_live_work() {
        let policy = McpTaskPolicy {
            max_records: NonZeroUsize::new(2).unwrap(),
        };
        let store = McpTaskStore::with_clock_and_policy(harn_clock::RealClock::arc(), policy);
        let first = begin(&store, None);
        let first_id = first.task().task_id.clone();
        first.complete(Ok(json!(1)), false);
        let _second = begin(&store, None);
        let _third = begin(&store, None);
        assert!(store.lookup(&unscoped(), &first_id).is_none());

        let error = store
            .begin(unscoped(), None)
            .expect_err("two live tasks exhaust capacity");
        assert_eq!(
            error,
            McpTaskAdmissionError::Capacity {
                limit: 2,
                active: 2,
                retained_terminal: 0,
            }
        );
    }

    #[test]
    fn dropping_a_lease_cannot_strand_working_state() {
        let store = McpTaskStore::new();
        let lease = begin(&store, None);
        let task_id = lease.task().task_id.clone();
        drop(lease);

        let read = store.handle_get(&unscoped(), json!(1), &params(&task_id));
        assert_eq!(read["result"]["status"], "failed");
        assert!(read["result"]["error"]["message"]
            .as_str()
            .expect("failure message")
            .contains("without reporting an outcome"));
    }
}
