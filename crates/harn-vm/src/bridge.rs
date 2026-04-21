//! JSON-RPC 2.0 bridge for host communication.
//!
//! When `harn run --bridge` is used, the VM delegates builtins (llm_call,
//! file I/O, tool execution) to a host process over stdin/stdout JSON-RPC.
//! The host application handles these requests using its own providers.

use std::collections::{BTreeMap, HashMap, VecDeque};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::io::AsyncBufReadExt;
use tokio::sync::{oneshot, Mutex};

use crate::orchestration::MutationSessionRecord;
use crate::value::{ErrorCategory, VmClosure, VmError, VmValue};
use crate::visible_text::VisibleTextState;
use crate::vm::Vm;

/// Default timeout for bridge calls (5 minutes).
const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// A JSON-RPC 2.0 bridge to a host process over stdin/stdout.
///
/// The bridge sends requests to the host on stdout and receives responses
/// on stdin. A background task reads stdin and dispatches responses to
/// waiting callers by request ID. All stdout writes are serialized through
/// a mutex to prevent interleaving.
pub struct HostBridge {
    next_id: AtomicU64,
    /// Pending request waiters, keyed by JSON-RPC id.
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
    /// Whether the host has sent a cancel notification.
    cancelled: Arc<AtomicBool>,
    /// Mutex protecting stdout writes to prevent interleaving.
    stdout_lock: Arc<std::sync::Mutex<()>>,
    /// ACP session ID (set in ACP mode for session-scoped notifications).
    session_id: std::sync::Mutex<String>,
    /// Name of the currently executing Harn script (without .harn suffix).
    script_name: std::sync::Mutex<String>,
    /// User messages injected by the host while a run is active.
    queued_user_messages: Arc<Mutex<VecDeque<QueuedUserMessage>>>,
    /// Host-triggered resume signal for daemon agents.
    resume_requested: Arc<AtomicBool>,
    /// Host-triggered skill-registry invalidation signal. Set when the
    /// host sends a `skills/update` notification; consumed by the CLI
    /// between runs (watch mode, long-running agents) to rebuild the
    /// layered skill catalog from its current filesystem + host state.
    skills_reload_requested: Arc<AtomicBool>,
    /// Whether the current daemon-mode agent loop is blocked in idle wait.
    daemon_idle: Arc<AtomicBool>,
    /// Per-call visible assistant text state for call_progress notifications.
    visible_call_states: std::sync::Mutex<HashMap<String, VisibleTextState>>,
    /// Whether an LLM call's deltas should be exposed to end users while streaming.
    visible_call_streams: std::sync::Mutex<HashMap<String, bool>>,
    /// Optional in-process host-module backend used by `harn playground`.
    in_process: Option<InProcessHost>,
    #[cfg(test)]
    recorded_notifications: Arc<std::sync::Mutex<Vec<serde_json::Value>>>,
}

struct InProcessHost {
    module_path: PathBuf,
    exported_functions: BTreeMap<String, Rc<VmClosure>>,
    vm: Vm,
}

impl InProcessHost {
    async fn dispatch(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, VmError> {
        match method {
            "builtin_call" => {
                let name = params
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or_default();
                let args = params
                    .get("args")
                    .and_then(|value| value.as_array())
                    .cloned()
                    .unwrap_or_default()
                    .into_iter()
                    .map(|value| json_result_to_vm_value(&value))
                    .collect::<Vec<_>>();
                self.invoke_export(name, &args).await
            }
            "host/tools/list" => self
                .invoke_optional_export("host_tools_list", &[])
                .await
                .map(|value| value.unwrap_or_else(|| serde_json::json!({ "tools": [] }))),
            "session/request_permission" => self.request_permission(params).await,
            other => Err(VmError::Runtime(format!(
                "playground host backend does not implement bridge method '{other}'"
            ))),
        }
    }

    async fn invoke_export(
        &self,
        name: &str,
        args: &[VmValue],
    ) -> Result<serde_json::Value, VmError> {
        let Some(closure) = self.exported_functions.get(name) else {
            return Err(VmError::Runtime(format!(
                "Playground host is missing capability '{name}'. Define `pub fn {name}(...)` in {}",
                self.module_path.display()
            )));
        };

        let mut vm = self.vm.child_vm_for_host();
        let result = vm.call_closure_pub(closure, args).await?;
        Ok(crate::llm::vm_value_to_json(&result))
    }

    async fn invoke_optional_export(
        &self,
        name: &str,
        args: &[VmValue],
    ) -> Result<Option<serde_json::Value>, VmError> {
        if !self.exported_functions.contains_key(name) {
            return Ok(None);
        }
        self.invoke_export(name, args).await.map(Some)
    }

    async fn request_permission(
        &self,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, VmError> {
        let Some(closure) = self.exported_functions.get("request_permission") else {
            return Ok(serde_json::json!({ "granted": true }));
        };

        let tool_name = params
            .get("toolCall")
            .and_then(|tool_call| tool_call.get("toolName"))
            .and_then(|value| value.as_str())
            .unwrap_or_default();
        let tool_args = params
            .get("toolCall")
            .and_then(|tool_call| tool_call.get("rawInput"))
            .map(json_result_to_vm_value)
            .unwrap_or(VmValue::Nil);
        let full_payload = json_result_to_vm_value(&params);

        let arg_count = closure.func.params.len();
        let args = if arg_count >= 3 {
            vec![
                VmValue::String(Rc::from(tool_name.to_string())),
                tool_args,
                full_payload,
            ]
        } else if arg_count == 2 {
            vec![VmValue::String(Rc::from(tool_name.to_string())), tool_args]
        } else if arg_count == 1 {
            vec![full_payload]
        } else {
            Vec::new()
        };

        let mut vm = self.vm.child_vm_for_host();
        let result = vm.call_closure_pub(closure, &args).await?;
        let payload = match result {
            VmValue::Bool(granted) => serde_json::json!({ "granted": granted }),
            VmValue::String(reason) if !reason.is_empty() => {
                serde_json::json!({ "granted": false, "reason": reason.to_string() })
            }
            other => {
                let json = crate::llm::vm_value_to_json(&other);
                if json
                    .get("granted")
                    .and_then(|value| value.as_bool())
                    .is_some()
                    || json.get("outcome").is_some()
                {
                    json
                } else {
                    serde_json::json!({ "granted": other.is_truthy() })
                }
            }
        };
        Ok(payload)
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum QueuedUserMessageMode {
    InterruptImmediate,
    FinishStep,
    WaitForCompletion,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeliveryCheckpoint {
    InterruptImmediate,
    AfterCurrentOperation,
    EndOfInteraction,
}

impl QueuedUserMessageMode {
    fn from_str(value: &str) -> Self {
        match value {
            "interrupt_immediate" | "interrupt" => Self::InterruptImmediate,
            "finish_step" | "after_current_operation" => Self::FinishStep,
            _ => Self::WaitForCompletion,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedUserMessage {
    pub content: String,
    pub mode: QueuedUserMessageMode,
}

// Default doesn't apply — new() spawns async tasks requiring a tokio LocalSet.
#[allow(clippy::new_without_default)]
impl HostBridge {
    /// Create a new bridge and spawn the stdin reader task.
    ///
    /// Must be called within a tokio LocalSet (uses spawn_local for the
    /// stdin reader since it's single-threaded).
    pub fn new() -> Self {
        let pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>> =
            Arc::new(Mutex::new(HashMap::new()));
        let cancelled = Arc::new(AtomicBool::new(false));
        let queued_user_messages: Arc<Mutex<VecDeque<QueuedUserMessage>>> =
            Arc::new(Mutex::new(VecDeque::new()));
        let resume_requested = Arc::new(AtomicBool::new(false));
        let skills_reload_requested = Arc::new(AtomicBool::new(false));
        let daemon_idle = Arc::new(AtomicBool::new(false));

        // Stdin reader: reads JSON-RPC lines and dispatches responses
        let pending_clone = pending.clone();
        let cancelled_clone = cancelled.clone();
        let queued_clone = queued_user_messages.clone();
        let resume_clone = resume_requested.clone();
        let skills_reload_clone = skills_reload_requested.clone();
        tokio::task::spawn_local(async move {
            let stdin = tokio::io::stdin();
            let reader = tokio::io::BufReader::new(stdin);
            let mut lines = reader.lines();

            while let Ok(Some(line)) = lines.next_line().await {
                let line = line.trim().to_string();
                if line.is_empty() {
                    continue;
                }

                let msg: serde_json::Value = match serde_json::from_str(&line) {
                    Ok(v) => v,
                    Err(_) => continue,
                };

                // Notifications have no id; responses have one.
                if msg.get("id").is_none() {
                    if let Some(method) = msg["method"].as_str() {
                        if method == "cancel" {
                            cancelled_clone.store(true, Ordering::SeqCst);
                        } else if method == "agent/resume" {
                            resume_clone.store(true, Ordering::SeqCst);
                        } else if method == "skills/update" {
                            skills_reload_clone.store(true, Ordering::SeqCst);
                        } else if method == "user_message"
                            || method == "session/input"
                            || method == "agent/user_message"
                        {
                            let params = &msg["params"];
                            let content = params
                                .get("content")
                                .and_then(|v| v.as_str())
                                .unwrap_or("")
                                .to_string();
                            if !content.is_empty() {
                                let mode = QueuedUserMessageMode::from_str(
                                    params
                                        .get("mode")
                                        .and_then(|v| v.as_str())
                                        .unwrap_or("wait_for_completion"),
                                );
                                queued_clone
                                    .lock()
                                    .await
                                    .push_back(QueuedUserMessage { content, mode });
                            }
                        }
                    }
                    continue;
                }

                if let Some(id) = msg["id"].as_u64() {
                    let mut pending = pending_clone.lock().await;
                    if let Some(sender) = pending.remove(&id) {
                        let _ = sender.send(msg);
                    }
                }
            }

            // stdin closed: drop pending senders to cancel waiters.
            let mut pending = pending_clone.lock().await;
            pending.clear();
        });

        Self {
            next_id: AtomicU64::new(1),
            pending,
            cancelled,
            stdout_lock: Arc::new(std::sync::Mutex::new(())),
            session_id: std::sync::Mutex::new(String::new()),
            script_name: std::sync::Mutex::new(String::new()),
            queued_user_messages,
            resume_requested,
            skills_reload_requested,
            daemon_idle,
            visible_call_states: std::sync::Mutex::new(HashMap::new()),
            visible_call_streams: std::sync::Mutex::new(HashMap::new()),
            in_process: None,
            #[cfg(test)]
            recorded_notifications: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// Create a bridge from pre-existing shared state.
    ///
    /// Unlike `new()`, does **not** spawn a stdin reader — the caller is
    /// responsible for dispatching responses into `pending`.  This is used
    /// by ACP mode which already has its own stdin reader.
    pub fn from_parts(
        pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
        cancelled: Arc<AtomicBool>,
        stdout_lock: Arc<std::sync::Mutex<()>>,
        start_id: u64,
    ) -> Self {
        Self {
            next_id: AtomicU64::new(start_id),
            pending,
            cancelled,
            stdout_lock,
            session_id: std::sync::Mutex::new(String::new()),
            script_name: std::sync::Mutex::new(String::new()),
            queued_user_messages: Arc::new(Mutex::new(VecDeque::new())),
            resume_requested: Arc::new(AtomicBool::new(false)),
            skills_reload_requested: Arc::new(AtomicBool::new(false)),
            daemon_idle: Arc::new(AtomicBool::new(false)),
            visible_call_states: std::sync::Mutex::new(HashMap::new()),
            visible_call_streams: std::sync::Mutex::new(HashMap::new()),
            in_process: None,
            #[cfg(test)]
            recorded_notifications: Arc::new(std::sync::Mutex::new(Vec::new())),
        }
    }

    /// Create an in-process host bridge backed by exported functions from a
    /// Harn module. Used by `harn playground` to avoid JSON-RPC boilerplate.
    pub async fn from_harn_module(mut vm: Vm, module_path: &Path) -> Result<Self, VmError> {
        let exported_functions = vm.load_module_exports(module_path).await?;
        Ok(Self {
            next_id: AtomicU64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            cancelled: Arc::new(AtomicBool::new(false)),
            stdout_lock: Arc::new(std::sync::Mutex::new(())),
            session_id: std::sync::Mutex::new(String::new()),
            script_name: std::sync::Mutex::new(String::new()),
            queued_user_messages: Arc::new(Mutex::new(VecDeque::new())),
            resume_requested: Arc::new(AtomicBool::new(false)),
            skills_reload_requested: Arc::new(AtomicBool::new(false)),
            daemon_idle: Arc::new(AtomicBool::new(false)),
            visible_call_states: std::sync::Mutex::new(HashMap::new()),
            visible_call_streams: std::sync::Mutex::new(HashMap::new()),
            in_process: Some(InProcessHost {
                module_path: module_path.to_path_buf(),
                exported_functions,
                vm,
            }),
            #[cfg(test)]
            recorded_notifications: Arc::new(std::sync::Mutex::new(Vec::new())),
        })
    }

    /// Set the ACP session ID for session-scoped notifications.
    pub fn set_session_id(&self, id: &str) {
        *self.session_id.lock().unwrap_or_else(|e| e.into_inner()) = id.to_string();
    }

    /// Set the currently executing script name (without .harn suffix).
    pub fn set_script_name(&self, name: &str) {
        *self.script_name.lock().unwrap_or_else(|e| e.into_inner()) = name.to_string();
    }

    /// Get the current script name.
    fn get_script_name(&self) -> String {
        self.script_name
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Get the session ID.
    fn get_session_id(&self) -> String {
        self.session_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Write a complete JSON-RPC line to stdout, serialized through a mutex.
    fn write_line(&self, line: &str) -> Result<(), VmError> {
        let _guard = self.stdout_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut stdout = std::io::stdout().lock();
        stdout
            .write_all(line.as_bytes())
            .map_err(|e| VmError::Runtime(format!("Bridge write error: {e}")))?;
        stdout
            .write_all(b"\n")
            .map_err(|e| VmError::Runtime(format!("Bridge write error: {e}")))?;
        stdout
            .flush()
            .map_err(|e| VmError::Runtime(format!("Bridge flush error: {e}")))?;
        Ok(())
    }

    /// Send a JSON-RPC request to the host and wait for the response.
    /// Times out after 5 minutes to prevent deadlocks.
    pub async fn call(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, VmError> {
        if let Some(in_process) = &self.in_process {
            return in_process.dispatch(method, params).await;
        }

        if self.is_cancelled() {
            return Err(VmError::Runtime("Bridge: operation cancelled".into()));
        }

        let id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let request = crate::jsonrpc::request(id, method, params);

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id, tx);
        }

        let line = serde_json::to_string(&request)
            .map_err(|e| VmError::Runtime(format!("Bridge serialization error: {e}")))?;
        if let Err(e) = self.write_line(&line) {
            let mut pending = self.pending.lock().await;
            pending.remove(&id);
            return Err(e);
        }

        let response = match tokio::time::timeout(DEFAULT_TIMEOUT, rx).await {
            Ok(Ok(msg)) => msg,
            Ok(Err(_)) => {
                // Sender dropped: host closed or stdin reader exited.
                return Err(VmError::Runtime(
                    "Bridge: host closed connection before responding".into(),
                ));
            }
            Err(_) => {
                let mut pending = self.pending.lock().await;
                pending.remove(&id);
                return Err(VmError::Runtime(format!(
                    "Bridge: host did not respond to '{method}' within {}s",
                    DEFAULT_TIMEOUT.as_secs()
                )));
            }
        };

        if let Some(error) = response.get("error") {
            let message = error["message"].as_str().unwrap_or("Unknown host error");
            let code = error["code"].as_i64().unwrap_or(-1);
            // JSON-RPC -32001 signals the host rejected the tool (not permitted / not in allowlist).
            if code == -32001 {
                return Err(VmError::CategorizedError {
                    message: message.to_string(),
                    category: ErrorCategory::ToolRejected,
                });
            }
            return Err(VmError::Runtime(format!("Host error ({code}): {message}")));
        }

        Ok(response["result"].clone())
    }

    /// Send a JSON-RPC notification to the host (no response expected).
    /// Serialized through the stdout mutex to prevent interleaving.
    pub fn notify(&self, method: &str, params: serde_json::Value) {
        let notification = crate::jsonrpc::notification(method, params);
        #[cfg(test)]
        self.recorded_notifications
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(notification.clone());
        if self.in_process.is_some() {
            return;
        }
        if let Ok(line) = serde_json::to_string(&notification) {
            let _ = self.write_line(&line);
        }
    }

    #[cfg(test)]
    pub(crate) fn recorded_notifications(&self) -> Vec<serde_json::Value> {
        self.recorded_notifications
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Check if the host has sent a cancel notification.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }

    pub fn take_resume_signal(&self) -> bool {
        self.resume_requested.swap(false, Ordering::SeqCst)
    }

    pub fn signal_resume(&self) {
        self.resume_requested.store(true, Ordering::SeqCst);
    }

    pub fn set_daemon_idle(&self, idle: bool) {
        self.daemon_idle.store(idle, Ordering::SeqCst);
    }

    pub fn is_daemon_idle(&self) -> bool {
        self.daemon_idle.load(Ordering::SeqCst)
    }

    /// Consume any pending `skills/update` signal the host has sent.
    /// Returns `true` exactly once per notification, letting callers
    /// trigger a layered-discovery rebuild without polling false
    /// positives. See issue #73 for the hot-reload contract.
    pub fn take_skills_reload_signal(&self) -> bool {
        self.skills_reload_requested.swap(false, Ordering::SeqCst)
    }

    /// Manually mark the skill catalog as stale. Used by tests and by
    /// the CLI when an internal event (e.g. `harn install`) should
    /// trigger the same rebuild a `skills/update` notification would.
    pub fn signal_skills_reload(&self) {
        self.skills_reload_requested.store(true, Ordering::SeqCst);
    }

    /// Call the host's `skills/list` RPC and return the raw JSON array
    /// it responded with. Shape:
    /// `[{ "id": "...", "name": "...", "description": "...", "source": "..." }, ...]`.
    /// The CLI adapter converts each entry into a
    /// [`crate::skills::SkillManifestRef`].
    pub async fn list_host_skills(&self) -> Result<Vec<serde_json::Value>, VmError> {
        let result = self.call("skills/list", serde_json::json!({})).await?;
        match result {
            serde_json::Value::Array(items) => Ok(items),
            serde_json::Value::Object(map) => match map.get("skills") {
                Some(serde_json::Value::Array(items)) => Ok(items.clone()),
                _ => Err(VmError::Runtime(
                    "skills/list: host response must be an array or { skills: [...] }".into(),
                )),
            },
            _ => Err(VmError::Runtime(
                "skills/list: unexpected response shape".into(),
            )),
        }
    }

    /// Call the host's `host/tools/list` RPC and return normalized tool
    /// descriptors. Shape:
    /// `[{ "name": "...", "description": "...", "schema": {...}, "deprecated": false }, ...]`.
    /// The bridge also accepts `{ "tools": [...] }` and
    /// `{ "result": { "tools": [...] } }` wrappers for lenient hosts.
    pub async fn list_host_tools(&self) -> Result<Vec<serde_json::Value>, VmError> {
        let result = self.call("host/tools/list", serde_json::json!({})).await?;
        parse_host_tools_list_response(result)
    }

    /// Call the host's `skills/fetch` RPC for one skill id. Returns the
    /// raw JSON body so the CLI can inspect both the frontmatter fields
    /// and the skill markdown body in whatever shape the host sends.
    pub async fn fetch_host_skill(&self, id: &str) -> Result<serde_json::Value, VmError> {
        self.call("skills/fetch", serde_json::json!({ "id": id }))
            .await
    }

    pub async fn push_queued_user_message(&self, content: String, mode: &str) {
        self.queued_user_messages
            .lock()
            .await
            .push_back(QueuedUserMessage {
                content,
                mode: QueuedUserMessageMode::from_str(mode),
            });
    }

    pub async fn take_queued_user_messages(
        &self,
        include_interrupt_immediate: bool,
        include_finish_step: bool,
        include_wait_for_completion: bool,
    ) -> Vec<QueuedUserMessage> {
        let mut queue = self.queued_user_messages.lock().await;
        let mut selected = Vec::new();
        let mut retained = VecDeque::new();
        while let Some(message) = queue.pop_front() {
            let should_take = match message.mode {
                QueuedUserMessageMode::InterruptImmediate => include_interrupt_immediate,
                QueuedUserMessageMode::FinishStep => include_finish_step,
                QueuedUserMessageMode::WaitForCompletion => include_wait_for_completion,
            };
            if should_take {
                selected.push(message);
            } else {
                retained.push_back(message);
            }
        }
        *queue = retained;
        selected
    }

    pub async fn take_queued_user_messages_for(
        &self,
        checkpoint: DeliveryCheckpoint,
    ) -> Vec<QueuedUserMessage> {
        match checkpoint {
            DeliveryCheckpoint::InterruptImmediate => {
                self.take_queued_user_messages(true, false, false).await
            }
            DeliveryCheckpoint::AfterCurrentOperation => {
                self.take_queued_user_messages(false, true, false).await
            }
            DeliveryCheckpoint::EndOfInteraction => {
                self.take_queued_user_messages(false, false, true).await
            }
        }
    }

    /// Send an output notification (for log/print in bridge mode).
    pub fn send_output(&self, text: &str) {
        self.notify("output", serde_json::json!({"text": text}));
    }

    /// Send a progress notification with optional numeric progress and structured data.
    pub fn send_progress(
        &self,
        phase: &str,
        message: &str,
        progress: Option<i64>,
        total: Option<i64>,
        data: Option<serde_json::Value>,
    ) {
        let mut payload = serde_json::json!({"phase": phase, "message": message});
        if let Some(p) = progress {
            payload["progress"] = serde_json::json!(p);
        }
        if let Some(t) = total {
            payload["total"] = serde_json::json!(t);
        }
        if let Some(d) = data {
            payload["data"] = d;
        }
        self.notify("progress", payload);
    }

    /// Send a structured log notification.
    pub fn send_log(&self, level: &str, message: &str, fields: Option<serde_json::Value>) {
        let mut payload = serde_json::json!({"level": level, "message": message});
        if let Some(f) = fields {
            payload["fields"] = f;
        }
        self.notify("log", payload);
    }

    /// Send a `session/update` with `call_start` — signals the beginning of
    /// an LLM call, tool call, or builtin call for observability.
    pub fn send_call_start(
        &self,
        call_id: &str,
        call_type: &str,
        name: &str,
        metadata: serde_json::Value,
    ) {
        let session_id = self.get_session_id();
        let script = self.get_script_name();
        let stream_publicly = metadata
            .get("stream_publicly")
            .and_then(|value| value.as_bool())
            .unwrap_or(true);
        self.visible_call_streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(call_id.to_string(), stream_publicly);
        self.notify(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "call_start",
                    "content": {
                        "toolCallId": call_id,
                        "call_type": call_type,
                        "name": name,
                        "script": script,
                        "metadata": metadata,
                    },
                },
            }),
        );
    }

    /// Send a `session/update` with `call_progress` — a streaming token delta
    /// from an in-flight LLM call.
    pub fn send_call_progress(
        &self,
        call_id: &str,
        delta: &str,
        accumulated_tokens: u64,
        user_visible: bool,
    ) {
        let session_id = self.get_session_id();
        let (visible_text, visible_delta) = {
            let stream_publicly = self
                .visible_call_streams
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(call_id)
                .copied()
                .unwrap_or(true);
            let mut states = self
                .visible_call_states
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            let state = states.entry(call_id.to_string()).or_default();
            state.push(delta, stream_publicly)
        };
        self.notify(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "call_progress",
                    "content": {
                        "toolCallId": call_id,
                        "delta": delta,
                        "accumulated_tokens": accumulated_tokens,
                        "visible_text": visible_text,
                        "visible_delta": visible_delta,
                        "user_visible": user_visible,
                    },
                },
            }),
        );
    }

    /// Send a `session/update` with `call_end` — signals completion of a call.
    pub fn send_call_end(
        &self,
        call_id: &str,
        call_type: &str,
        name: &str,
        duration_ms: u64,
        status: &str,
        metadata: serde_json::Value,
    ) {
        let session_id = self.get_session_id();
        let script = self.get_script_name();
        self.visible_call_states
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(call_id);
        self.visible_call_streams
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(call_id);
        self.notify(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "call_end",
                    "content": {
                        "toolCallId": call_id,
                        "call_type": call_type,
                        "name": name,
                        "script": script,
                        "duration_ms": duration_ms,
                        "status": status,
                        "metadata": metadata,
                    },
                },
            }),
        );
    }

    /// Send a worker lifecycle update for delegated/background execution.
    pub fn send_worker_update(
        &self,
        worker_id: &str,
        worker_name: &str,
        status: &str,
        metadata: serde_json::Value,
        audit: Option<&MutationSessionRecord>,
    ) {
        let session_id = self.get_session_id();
        let script = self.get_script_name();
        let started_at = metadata.get("started_at").cloned().unwrap_or_default();
        let finished_at = metadata.get("finished_at").cloned().unwrap_or_default();
        let snapshot_path = metadata.get("snapshot_path").cloned().unwrap_or_default();
        let run_id = metadata.get("child_run_id").cloned().unwrap_or_default();
        let run_path = metadata.get("child_run_path").cloned().unwrap_or_default();
        let lifecycle = serde_json::json!({
            "event": status,
            "worker_id": worker_id,
            "worker_name": worker_name,
            "started_at": started_at,
            "finished_at": finished_at,
        });
        self.notify(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "worker_update",
                    "content": {
                        "worker_id": worker_id,
                        "worker_name": worker_name,
                        "status": status,
                        "script": script,
                        "started_at": started_at,
                        "finished_at": finished_at,
                        "snapshot_path": snapshot_path,
                        "run_id": run_id,
                        "run_path": run_path,
                        "lifecycle": lifecycle,
                        "audit": audit,
                        "metadata": metadata,
                    },
                },
            }),
        );
    }
}

/// Convert a serde_json::Value to a VmValue.
pub fn json_result_to_vm_value(val: &serde_json::Value) -> VmValue {
    crate::stdlib::json_to_vm_value(val)
}

fn parse_host_tools_list_response(
    result: serde_json::Value,
) -> Result<Vec<serde_json::Value>, VmError> {
    let tools = match result {
        serde_json::Value::Array(items) => items,
        serde_json::Value::Object(map) => match map.get("tools").cloned().or_else(|| {
            map.get("result")
                .and_then(|value| value.get("tools"))
                .cloned()
        }) {
            Some(serde_json::Value::Array(items)) => items,
            _ => {
                return Err(VmError::Runtime(
                    "host/tools/list: host response must be an array or { tools: [...] }".into(),
                ));
            }
        },
        _ => {
            return Err(VmError::Runtime(
                "host/tools/list: unexpected response shape".into(),
            ));
        }
    };

    let mut normalized = Vec::with_capacity(tools.len());
    for tool in tools {
        let serde_json::Value::Object(map) = tool else {
            return Err(VmError::Runtime(
                "host/tools/list: every tool must be an object".into(),
            ));
        };
        let Some(name) = map.get("name").and_then(|value| value.as_str()) else {
            return Err(VmError::Runtime(
                "host/tools/list: every tool must include a string `name`".into(),
            ));
        };
        let description = map
            .get("description")
            .and_then(|value| value.as_str())
            .or_else(|| {
                map.get("short_description")
                    .and_then(|value| value.as_str())
            })
            .unwrap_or_default();
        let schema = map
            .get("schema")
            .cloned()
            .or_else(|| map.get("parameters").cloned())
            .or_else(|| map.get("input_schema").cloned())
            .unwrap_or(serde_json::Value::Null);
        let deprecated = map
            .get("deprecated")
            .and_then(|value| value.as_bool())
            .unwrap_or(false);
        normalized.push(serde_json::json!({
            "name": name,
            "description": description,
            "schema": schema,
            "deprecated": deprecated,
        }));
    }
    Ok(normalized)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_json_rpc_request_format() {
        let request = crate::jsonrpc::request(
            1,
            "llm_call",
            serde_json::json!({
                "prompt": "Hello",
                "system": "Be helpful",
            }),
        );
        let s = serde_json::to_string(&request).unwrap();
        assert!(s.contains("\"jsonrpc\":\"2.0\""));
        assert!(s.contains("\"id\":1"));
        assert!(s.contains("\"method\":\"llm_call\""));
    }

    #[test]
    fn test_json_rpc_notification_format() {
        let notification =
            crate::jsonrpc::notification("output", serde_json::json!({"text": "[harn] hello\n"}));
        let s = serde_json::to_string(&notification).unwrap();
        assert!(s.contains("\"method\":\"output\""));
        assert!(!s.contains("\"id\""));
    }

    #[test]
    fn test_json_rpc_error_response_parsing() {
        let response = crate::jsonrpc::error_response(1, -32600, "Invalid request");
        assert!(response.get("error").is_some());
        assert_eq!(
            response["error"]["message"].as_str().unwrap(),
            "Invalid request"
        );
    }

    #[test]
    fn test_json_rpc_success_response_parsing() {
        let response = crate::jsonrpc::response(
            1,
            serde_json::json!({
                "text": "Hello world",
                "input_tokens": 10,
                "output_tokens": 5,
            }),
        );
        assert!(response.get("result").is_some());
        assert_eq!(response["result"]["text"].as_str().unwrap(), "Hello world");
    }

    #[test]
    fn test_cancelled_flag() {
        let cancelled = Arc::new(AtomicBool::new(false));
        assert!(!cancelled.load(Ordering::SeqCst));
        cancelled.store(true, Ordering::SeqCst);
        assert!(cancelled.load(Ordering::SeqCst));
    }

    #[test]
    fn queued_messages_are_filtered_by_delivery_mode() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        runtime.block_on(async {
            let bridge = HostBridge::from_parts(
                Arc::new(Mutex::new(HashMap::new())),
                Arc::new(AtomicBool::new(false)),
                Arc::new(std::sync::Mutex::new(())),
                1,
            );
            bridge
                .push_queued_user_message("first".to_string(), "finish_step")
                .await;
            bridge
                .push_queued_user_message("second".to_string(), "wait_for_completion")
                .await;

            let finish_step = bridge.take_queued_user_messages(false, true, false).await;
            assert_eq!(finish_step.len(), 1);
            assert_eq!(finish_step[0].content, "first");

            let turn_end = bridge.take_queued_user_messages(false, false, true).await;
            assert_eq!(turn_end.len(), 1);
            assert_eq!(turn_end[0].content, "second");
        });
    }

    #[test]
    fn test_json_result_to_vm_value_string() {
        let val = serde_json::json!("hello");
        let vm_val = json_result_to_vm_value(&val);
        assert_eq!(vm_val.display(), "hello");
    }

    #[test]
    fn test_json_result_to_vm_value_dict() {
        let val = serde_json::json!({"name": "test", "count": 42});
        let vm_val = json_result_to_vm_value(&val);
        let VmValue::Dict(d) = &vm_val else {
            unreachable!("Expected Dict, got {:?}", vm_val);
        };
        assert_eq!(d.get("name").unwrap().display(), "test");
        assert_eq!(d.get("count").unwrap().display(), "42");
    }

    #[test]
    fn test_json_result_to_vm_value_null() {
        let val = serde_json::json!(null);
        let vm_val = json_result_to_vm_value(&val);
        assert!(matches!(vm_val, VmValue::Nil));
    }

    #[test]
    fn test_json_result_to_vm_value_nested() {
        let val = serde_json::json!({
            "text": "response",
            "tool_calls": [
                {"id": "tc_1", "name": "read_file", "arguments": {"path": "foo.rs"}}
            ],
            "input_tokens": 100,
            "output_tokens": 50,
        });
        let vm_val = json_result_to_vm_value(&val);
        let VmValue::Dict(d) = &vm_val else {
            unreachable!("Expected Dict, got {:?}", vm_val);
        };
        assert_eq!(d.get("text").unwrap().display(), "response");
        let VmValue::List(list) = d.get("tool_calls").unwrap() else {
            unreachable!("Expected List for tool_calls");
        };
        assert_eq!(list.len(), 1);
    }

    #[test]
    fn parse_host_tools_list_accepts_object_wrapper() {
        let tools = parse_host_tools_list_response(serde_json::json!({
            "tools": [
                {
                    "name": "Read",
                    "description": "Read a file",
                    "schema": {"type": "object"},
                }
            ]
        }))
        .expect("tool list");

        assert_eq!(tools.len(), 1);
        assert_eq!(tools[0]["name"], "Read");
        assert_eq!(tools[0]["deprecated"], false);
    }

    #[test]
    fn parse_host_tools_list_accepts_compat_fields() {
        let tools = parse_host_tools_list_response(serde_json::json!({
            "result": {
                "tools": [
                    {
                        "name": "Edit",
                        "short_description": "Apply an edit",
                        "input_schema": {"type": "object"},
                        "deprecated": true,
                    }
                ]
            }
        }))
        .expect("tool list");

        assert_eq!(tools[0]["description"], "Apply an edit");
        assert_eq!(tools[0]["schema"]["type"], "object");
        assert_eq!(tools[0]["deprecated"], true);
    }

    #[test]
    fn parse_host_tools_list_requires_tool_names() {
        let err = parse_host_tools_list_response(serde_json::json!({
            "tools": [
                {"description": "missing name"}
            ]
        }))
        .expect_err("expected error");
        assert!(err
            .to_string()
            .contains("host/tools/list: every tool must include a string `name`"));
    }

    #[test]
    fn test_timeout_duration() {
        assert_eq!(DEFAULT_TIMEOUT.as_secs(), 300);
    }
}
