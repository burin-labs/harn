//! Agent Client Protocol (ACP) server implementation.
//!
//! Implements the ACP specification (<https://agentclientprotocol.com>) so that
//! harn can act as an agent runtime accessible from any host application
//! (IDEs, CLI tools, web apps, etc.).  Communication is JSON-RPC 2.0 over stdin/stdout, following the same
//! structural pattern as the existing `--bridge` mode.

mod builtins;
mod events;
mod execute;
mod io;

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use harn_vm::agent_events::{clear_session_sinks, register_sink};
use harn_vm::visible_text::{sanitize_visible_assistant_text, VisibleTextState};
use tokio::io::AsyncBufReadExt;
use tokio::sync::{mpsc, oneshot, Mutex};

use events::AcpAgentEventSink;
use io::send_json_response;

fn verbose_bridge_logs_enabled() -> bool {
    matches!(
        std::env::var("HARN_ACP_VERBOSE").ok().as_deref(),
        Some("1" | "true" | "TRUE" | "yes" | "YES")
    ) || matches!(
        std::env::var("HARN_ACP_TRACE_CALLS").ok().as_deref(),
        Some("1")
    )
}

fn host_call_timeout(method: &str) -> std::time::Duration {
    let configured = std::env::var("HARN_HOST_CALL_TIMEOUT_SECS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0);
    if let Some(seconds) = configured {
        return std::time::Duration::from_secs(seconds);
    }
    if method == "host/call" {
        return std::time::Duration::from_secs(300);
    }
    std::time::Duration::from_secs(60)
}

fn suppress_default_info_log(message: &str) -> bool {
    if verbose_bridge_logs_enabled() {
        return false;
    }
    [
        "ACP_BOOT:",
        "span_end ",
        "WORKFLOW_POLICY:",
        "HINTS:",
        "AGENT_CONTEXT:",
        "SIBLING_OUTLINES:",
        "PROVIDERS: count=",
        "AUTO: base context start",
        "AUTO: base context done",
    ]
    .iter()
    .any(|prefix| message.starts_with(prefix))
}

#[derive(Clone, Default)]
struct SessionInfo {
    title: Option<String>,
    meta: serde_json::Map<String, serde_json::Value>,
}

struct Session {
    cwd: PathBuf,
    /// If a cancel was requested for the current prompt execution.
    cancelled: Arc<AtomicBool>,
    /// Active host bridge for queued input / daemon resume while a prompt runs.
    host_bridge: Option<Rc<harn_vm::bridge::HostBridge>>,
    info: SessionInfo,
}

#[derive(Clone)]
pub(super) enum AcpOutput {
    Stdout(Arc<std::sync::Mutex<()>>),
    Channel(mpsc::UnboundedSender<String>),
}

impl AcpOutput {
    fn stdout() -> Self {
        Self::Stdout(Arc::new(std::sync::Mutex::new(())))
    }

    pub(super) fn write_line(&self, line: &str) {
        match self {
            Self::Stdout(lock) => {
                let _guard = lock.lock().unwrap_or_else(|e| e.into_inner());
                let mut stdout = std::io::stdout().lock();
                let _ = stdout.write_all(line.as_bytes());
                let _ = stdout.write_all(b"\n");
                let _ = stdout.flush();
            }
            Self::Channel(tx) => {
                let _ = tx.send(line.to_string());
            }
        }
    }
}

/// ACP server that reads JSON-RPC requests from a transport and writes
/// responses / notifications back to that same transport.
pub struct AcpServer {
    /// Optional pipeline file to execute on each `session/prompt`.
    pipeline: Option<String>,
    /// Active sessions keyed by session ID.
    sessions: HashMap<String, Session>,
    /// Monotonically increasing JSON-RPC request ID for outgoing requests.
    next_id: AtomicU64,
    /// Pending outgoing request waiters, keyed by JSON-RPC id.
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
    /// Transport output sink.
    output: AcpOutput,
}

impl AcpServer {
    fn new(pipeline: Option<String>) -> Self {
        Self::new_with_output(pipeline, AcpOutput::stdout())
    }

    fn new_with_output(pipeline: Option<String>, output: AcpOutput) -> Self {
        Self {
            pipeline,
            sessions: HashMap::new(),
            next_id: AtomicU64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            output,
        }
    }

    /// Write a complete JSON-RPC message to the current transport.
    fn write_line(&self, line: &str) {
        self.output.write_line(line);
    }

    /// Send a JSON-RPC success response.
    fn send_response(&self, id: &serde_json::Value, result: serde_json::Value) {
        let response = harn_vm::jsonrpc::response(id.clone(), result);
        if let Ok(line) = serde_json::to_string(&response) {
            self.write_line(&line);
        }
    }

    /// Send a JSON-RPC error response.
    fn send_error(&self, id: &serde_json::Value, code: i64, message: &str) {
        let response = harn_vm::jsonrpc::error_response(id.clone(), code, message);
        if let Ok(line) = serde_json::to_string(&response) {
            self.write_line(&line);
        }
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    #[allow(dead_code)]
    fn send_notification(&self, method: &str, params: serde_json::Value) {
        let notification = harn_vm::jsonrpc::notification(method, params);
        if let Ok(line) = serde_json::to_string(&notification) {
            self.write_line(&line);
        }
    }

    /// Send a `session/update` notification with an agent message chunk.
    #[allow(dead_code)]
    fn send_update(&self, session_id: &str, text: &str) {
        let visible_text = sanitize_visible_assistant_text(text, true);
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {
                        "type": "text",
                        "text": text,
                        "visible_text": visible_text.clone(),
                        "visible_delta": visible_text,
                    },
                },
            }),
        );
    }

    fn send_prompt_error(&self, session_id: &str, id: &serde_json::Value, message: &str) {
        self.send_update(session_id, &format!("Error: {message}\n"));
        self.send_error(id, -32000, message);
        eprintln!("{message}");
    }

    /// Generate a unique session ID.
    fn next_session_id(&mut self) -> String {
        uuid::Uuid::new_v4().to_string()
    }

    fn handle_initialize(&self, id: &serde_json::Value) {
        self.send_response(
            id,
            serde_json::json!({
                "protocolVersion": 1,
                "agentCapabilities": {
                    "promptCapabilities": {},
                    "sessionCapabilities": {
                        "fork": {},
                        "load": {},
                    },
                },
                "agentInfo": {
                    "name": "harn",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            }),
        );
    }

    fn insert_session(&mut self, session_id: String, cwd: PathBuf, info: SessionInfo) {
        self.sessions.insert(
            session_id.clone(),
            Session {
                cwd,
                cancelled: Arc::new(AtomicBool::new(false)),
                host_bridge: None,
                info,
            },
        );
        harn_vm::agent_sessions::open_or_create(Some(session_id));
    }

    fn handle_session_new(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let cwd = params
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        let session_id = self.next_session_id();
        self.insert_session(session_id.clone(), cwd, SessionInfo::default());

        self.send_response(
            id,
            serde_json::json!({
                "sessionId": session_id,
                "modes": [{
                    "id": "default",
                    "name": "Default",
                    "description": "Execute harn pipelines",
                }],
            }),
        );
    }

    fn emit_session_info_update(&self, session_id: &str, info: &SessionInfo) {
        let mut update = serde_json::json!({
            "sessionUpdate": "session_info_update",
        });
        if let Some(title) = &info.title {
            update["title"] = serde_json::json!(title);
        }
        if !info.meta.is_empty() {
            update["_meta"] = serde_json::Value::Object(info.meta.clone());
        }
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": update,
            }),
        );
    }

    fn handle_session_fork(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let src_id = params
            .get("session_id")
            .or_else(|| params.get("sessionId"))
            .and_then(|value| value.as_str())
            .map(str::to_string);
        let Some(src_id) = src_id else {
            self.send_error(id, -32602, "Missing session_id");
            return;
        };
        let Some(src_cwd) = self
            .sessions
            .get(&src_id)
            .map(|session| session.cwd.clone())
        else {
            self.send_error(id, -32602, &format!("Unknown session: {src_id}"));
            return;
        };

        if !harn_vm::agent_sessions::exists(&src_id) {
            harn_vm::agent_sessions::open_or_create(Some(src_id.clone()));
        }

        let keep_first = match params.get("keep_first").and_then(|value| value.as_i64()) {
            Some(value) if value < 0 => {
                self.send_error(id, -32602, "Invalid keep_first: must be >= 0");
                return;
            }
            Some(value) => Some(value as usize),
            None => None,
        };
        let dst_id = params
            .get("id")
            .and_then(|value| value.as_str())
            .map(str::to_string);
        if let Some(dst_id) = dst_id.as_deref() {
            if self.sessions.contains_key(dst_id) {
                self.send_error(id, -32602, &format!("Session already exists: {dst_id}"));
                return;
            }
            if harn_vm::agent_sessions::exists(dst_id) {
                self.send_error(id, -32602, &format!("Session already exists: {dst_id}"));
                return;
            }
        }
        let branch_name = params
            .get("branch_name")
            .and_then(|value| value.as_str())
            .map(str::to_string);

        let new_session_id = match keep_first {
            Some(keep_first) => harn_vm::agent_sessions::fork_at(&src_id, keep_first, dst_id),
            None => harn_vm::agent_sessions::fork(&src_id, dst_id),
        };
        let Some(new_session_id) = new_session_id else {
            self.send_error(id, -32000, &format!("Failed to fork session: {src_id}"));
            return;
        };

        let snapshot = harn_vm::agent_sessions::snapshot(&new_session_id)
            .and_then(|value| serde_json::to_value(harn_vm::llm::vm_value_to_json(&value)).ok())
            .unwrap_or_else(|| serde_json::json!({}));
        let branched_at = snapshot
            .get("branched_at_event_index")
            .cloned()
            .unwrap_or(serde_json::Value::Null);

        let mut meta = serde_json::Map::new();
        meta.insert("state".to_string(), serde_json::json!("forked"));
        meta.insert("parent_id".to_string(), serde_json::json!(src_id.clone()));
        meta.insert("branched_at".to_string(), branched_at.clone());
        if let Some(branch_name) = &branch_name {
            meta.insert("branch_name".to_string(), serde_json::json!(branch_name));
        }
        let info = SessionInfo {
            title: branch_name,
            meta,
        };

        self.sessions.insert(
            new_session_id.clone(),
            Session {
                cwd: src_cwd,
                cancelled: Arc::new(AtomicBool::new(false)),
                host_bridge: None,
                info: info.clone(),
            },
        );
        self.emit_session_info_update(&new_session_id, &info);
        self.send_response(
            id,
            serde_json::json!({
                "sessionId": new_session_id,
                "state": "forked",
                "parent_id": src_id,
                "branched_at": branched_at,
            }),
        );
    }

    async fn handle_session_prompt(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let session_id = match params.get("sessionId").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                self.send_error(id, -32602, "Missing sessionId");
                return;
            }
        };

        let prompt_text = params
            .get("prompt")
            .and_then(|v| v.as_array())
            .map(|blocks| {
                blocks
                    .iter()
                    .filter_map(|b| {
                        if b.get("type").and_then(|t| t.as_str()) == Some("text") {
                            b.get("text").and_then(|t| t.as_str())
                        } else {
                            None
                        }
                    })
                    .collect::<Vec<_>>()
                    .join("\n")
            })
            .unwrap_or_default();

        let (cwd, cancelled) = match self.sessions.get_mut(&session_id) {
            Some(s) => {
                s.cancelled.store(false, Ordering::SeqCst);
                s.host_bridge = None;
                (s.cwd.clone(), s.cancelled.clone())
            }
            None => {
                self.send_error(id, -32602, &format!("Unknown session: {session_id}"));
                return;
            }
        };
        harn_vm::agent_sessions::open_or_create(Some(session_id.clone()));
        let _session_guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());

        let (source, source_path) = if let Some(ref pipeline_path) = self.pipeline {
            let full_path = if std::path::Path::new(pipeline_path).is_absolute() {
                PathBuf::from(pipeline_path)
            } else {
                cwd.join(pipeline_path)
            };
            match std::fs::read_to_string(&full_path) {
                Ok(src) => (src, Some(full_path)),
                Err(e) => {
                    let message = format!("Failed to read pipeline {}: {e}", full_path.display());
                    self.send_prompt_error(&session_id, id, &message);
                    return;
                }
            }
        } else {
            // Wrap inline prompt source in a pipeline so the compiler has
            // an entry point.
            let wrapped = format!("pipeline main() {{\n{prompt_text}\n}}");
            (wrapped, None)
        };

        let output = self.output.clone();
        let pending = self.pending.clone();
        let next_id = &self.next_id;
        let sid = session_id.clone();

        // Translate AgentEvents into ACP session/update notifications so
        // the client observes tool lifecycle on the wire.
        register_sink(
            session_id.clone(),
            Arc::new(AcpAgentEventSink::new(output.clone())),
        );

        let bridge = Rc::new(AcpBridge {
            session_id: sid.clone(),
            output: output.clone(),
            pending: pending.clone(),
            next_id_counter: AtomicU64::new(next_id.fetch_add(1000, Ordering::SeqCst)),
            cancelled: cancelled.clone(),
            script_name: std::sync::Mutex::new(String::new()),
            assistant_state: std::sync::Mutex::new(VisibleTextState::default()),
        });
        let bridge_output = output.clone();
        let host_bridge = Rc::new(harn_vm::bridge::HostBridge::from_parts_with_writer(
            bridge.pending.clone(),
            Arc::new(AtomicBool::new(false)),
            Arc::new(move |line| {
                bridge_output.write_line(line);
                Ok(())
            }),
            bridge.next_id_counter.fetch_add(10_000, Ordering::SeqCst),
        ));
        host_bridge.set_session_id(&bridge.session_id);

        let compile_started = Instant::now();
        let chunk = match harn_vm::compile_source(&source) {
            Ok(c) => c,
            Err(e) => {
                self.send_prompt_error(&session_id, id, &format!("Compilation error: {e}"));
                return;
            }
        };
        let compile_ms = compile_started.elapsed().as_millis() as u64;
        bridge.send_log(
            "info",
            &format!("ACP_BOOT: compile_ms={compile_ms}"),
            Some(serde_json::json!({
                "compile_ms": compile_ms,
                "pipeline": source_path
                    .as_ref()
                    .map(|p| p.display().to_string())
                    .unwrap_or_else(|| "<inline>".to_string()),
            })),
        );
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.host_bridge = Some(host_bridge.clone());
        }

        let id_owned = id.clone();
        let send_output = self.output.clone();
        let result = execute::execute_chunk(
            chunk,
            bridge.clone(),
            host_bridge,
            &prompt_text,
            source_path.as_deref(),
            &cwd,
        )
        .await;
        if let Some(session) = self.sessions.get_mut(&session_id) {
            session.host_bridge = None;
        }

        // Unregister so a session reusing this id can't receive stale
        // events routed to a dropped stdout lock.
        clear_session_sinks(&session_id);

        match result {
            Ok(output) => {
                if !output.is_empty() {
                    bridge.send_update(&output);
                }
                send_json_response(
                    &send_output,
                    &id_owned,
                    serde_json::json!({"stopReason": "completed"}),
                );
            }
            Err(e) => {
                if cancelled.load(Ordering::SeqCst) {
                    send_json_response(
                        &send_output,
                        &id_owned,
                        serde_json::json!({"stopReason": "cancelled"}),
                    );
                } else {
                    self.send_prompt_error(&sid, &id_owned, &e);
                }
            }
        }
    }

    fn handle_session_cancel(&mut self, params: &serde_json::Value) {
        if let Some(session_id) = params.get("sessionId").and_then(|v| v.as_str()) {
            if let Some(session) = self.sessions.get(session_id) {
                session.cancelled.store(true, Ordering::SeqCst);
            }
        }
    }

    async fn handle_session_input(&self, params: &serde_json::Value) {
        let session_id = params
            .get("sessionId")
            .and_then(|v| v.as_str())
            .or_else(|| self.sessions.keys().next().map(|s| s.as_str()));
        let Some(session_id) = session_id else {
            return;
        };
        let Some(content) = params.get("content").and_then(|v| v.as_str()) else {
            return;
        };
        let mode = params
            .get("mode")
            .and_then(|v| v.as_str())
            .unwrap_or("wait_for_completion");
        if let Some(bridge) = self
            .sessions
            .get(session_id)
            .and_then(|session| session.host_bridge.clone())
        {
            bridge
                .push_queued_user_message(content.to_string(), mode)
                .await;
        }
    }

    fn handle_agent_resume(&self, params: &serde_json::Value) {
        let session_id = params
            .get("sessionId")
            .and_then(|v| v.as_str())
            .or_else(|| self.sessions.keys().next().map(|s| s.as_str()));
        let Some(session_id) = session_id else {
            return;
        };
        if let Some(bridge) = self
            .sessions
            .get(session_id)
            .and_then(|session| session.host_bridge.clone())
        {
            bridge.signal_resume();
        }
    }

    fn handle_session_list(&self, id: &serde_json::Value) {
        let sessions: Vec<serde_json::Value> = self
            .sessions
            .iter()
            .map(|(sid, session)| {
                let mut item = serde_json::json!({
                    "sessionId": sid,
                    "cwd": session.cwd,
                });
                if let Some(title) = &session.info.title {
                    item["title"] = serde_json::json!(title);
                }
                if !session.info.meta.is_empty() {
                    item["_meta"] = serde_json::Value::Object(session.info.meta.clone());
                }
                item
            })
            .collect();
        self.send_response(id, serde_json::json!({"sessions": sessions}));
    }

    async fn handle_hitl_respond(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let session_cwd = params
            .get("sessionId")
            .and_then(|value| value.as_str())
            .and_then(|session_id| self.sessions.get(session_id))
            .map(|session| session.cwd.as_path());
        let fallback_cwd = self
            .sessions
            .values()
            .next()
            .map(|session| session.cwd.as_path());
        let cwd = session_cwd.or(fallback_cwd);
        let response: harn_vm::HitlHostResponse = match serde_json::from_value(params.clone()) {
            Ok(response) => response,
            Err(error) => {
                self.send_error(
                    id,
                    -32602,
                    &format!("Invalid harn.hitl.respond params: {error}"),
                );
                return;
            }
        };
        match harn_vm::append_hitl_response(cwd, response).await {
            Ok(_) => self.send_response(id, serde_json::json!({"ok": true})),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    fn workflow_base_dir_for<'a>(&'a self, params: &'a serde_json::Value) -> Option<&'a PathBuf> {
        params
            .get("sessionId")
            .and_then(|value| value.as_str())
            .and_then(|session_id| self.sessions.get(session_id))
            .map(|session| &session.cwd)
            .or_else(|| self.sessions.values().next().map(|session| &session.cwd))
    }

    async fn handle_workflow_signal(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(workflow_id) = params.get("workflowId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/signal: missing workflowId");
            return;
        };
        let Some(name) = params.get("name").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/signal: missing name");
            return;
        };
        let Some(base_dir) = self.workflow_base_dir_for(params) else {
            self.send_error(id, -32602, "workflow/signal: no session cwd available");
            return;
        };
        let payload = params
            .get("payload")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        match harn_vm::workflow_signal_for_base(base_dir, workflow_id, name, payload) {
            Ok(result) => self.send_response(id, result),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    fn handle_workflow_query(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(workflow_id) = params.get("workflowId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/query: missing workflowId");
            return;
        };
        let Some(name) = params.get("name").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/query: missing name");
            return;
        };
        let Some(base_dir) = self.workflow_base_dir_for(params) else {
            self.send_error(id, -32602, "workflow/query: no session cwd available");
            return;
        };
        match harn_vm::workflow_query_for_base(base_dir, workflow_id, name) {
            Ok(result) => self.send_response(id, result),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    async fn handle_workflow_update(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(workflow_id) = params.get("workflowId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/update: missing workflowId");
            return;
        };
        let Some(name) = params.get("name").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/update: missing name");
            return;
        };
        let Some(base_dir) = self.workflow_base_dir_for(params) else {
            self.send_error(id, -32602, "workflow/update: no session cwd available");
            return;
        };
        let payload = params
            .get("payload")
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        let timeout_ms = params
            .get("timeoutMs")
            .and_then(|value| value.as_u64())
            .unwrap_or(30_000);
        match harn_vm::workflow_update_for_base(
            base_dir,
            workflow_id,
            name,
            payload,
            std::time::Duration::from_millis(timeout_ms),
        )
        .await
        {
            Ok(result) => self.send_response(id, result),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    fn handle_workflow_pause(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(workflow_id) = params.get("workflowId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/pause: missing workflowId");
            return;
        };
        let Some(base_dir) = self.workflow_base_dir_for(params) else {
            self.send_error(id, -32602, "workflow/pause: no session cwd available");
            return;
        };
        match harn_vm::workflow_pause_for_base(base_dir, workflow_id) {
            Ok(result) => self.send_response(id, result),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    fn handle_workflow_resume(&self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(workflow_id) = params.get("workflowId").and_then(|value| value.as_str()) else {
            self.send_error(id, -32602, "workflow/resume: missing workflowId");
            return;
        };
        let Some(base_dir) = self.workflow_base_dir_for(params) else {
            self.send_error(id, -32602, "workflow/resume: no session cwd available");
            return;
        };
        match harn_vm::workflow_resume_for_base(base_dir, workflow_id) {
            Ok(result) => self.send_response(id, result),
            Err(error) => self.send_error(id, -32000, &error),
        }
    }

    fn handle_session_load(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let Some(session_id) = params
            .get("sessionId")
            .or_else(|| params.get("session_id"))
            .and_then(serde_json::Value::as_str)
        else {
            self.send_error(id, -32602, "session/load requires sessionId");
            return;
        };

        let Some(session) = self.sessions.get(session_id) else {
            self.send_error(id, -32004, &format!("Session not found: {session_id}"));
            return;
        };

        let mut session_value = serde_json::json!({
            "sessionId": session_id,
            "cwd": session.cwd.display().to_string(),
        });
        if let Some(title) = session.info.title.as_ref() {
            session_value["title"] = serde_json::json!(title);
        }
        if !session.info.meta.is_empty() {
            session_value["_meta"] = serde_json::Value::Object(session.info.meta.clone());
        }

        self.send_response(
            id,
            serde_json::json!({
                "session": session_value,
                "replayed": [],
            }),
        );
    }

    async fn handle_incoming_message(&mut self, msg: serde_json::Value) {
        if msg.get("method").is_none() && msg.get("id").is_some() {
            if let Some(id) = msg["id"].as_u64() {
                let mut pending = self.pending.lock().await;
                if let Some(sender) = pending.remove(&id) {
                    let _ = sender.send(msg);
                }
            }
            return;
        }

        let method = match msg.get("method").and_then(|v| v.as_str()) {
            Some(m) => m.to_string(),
            None => return,
        };
        let id = msg.get("id").cloned().unwrap_or(serde_json::Value::Null);
        let params = msg.get("params").cloned().unwrap_or(serde_json::json!({}));

        match method.as_str() {
            "initialize" => {
                self.handle_initialize(&id);
            }
            "session/new" => {
                self.handle_session_new(&id, &params);
            }
            "session/load" => {
                self.handle_session_load(&id, &params);
            }
            "session/fork" => {
                self.handle_session_fork(&id, &params);
            }
            "session/prompt" => {
                self.handle_session_prompt(&id, &params).await;
            }
            "session/cancel" => {
                self.handle_session_cancel(&params);
            }
            "session/input" | "user_message" | "agent/user_message" => {
                self.handle_session_input(&params).await;
            }
            "agent/resume" => {
                self.handle_agent_resume(&params);
            }
            "harn.hitl.respond" => {
                self.handle_hitl_respond(&id, &params).await;
            }
            "workflow/signal" | "harn.workflow.signal" => {
                self.handle_workflow_signal(&id, &params).await;
            }
            "workflow/query" | "harn.workflow.query" => {
                self.handle_workflow_query(&id, &params);
            }
            "workflow/update" | "harn.workflow.update" => {
                self.handle_workflow_update(&id, &params).await;
            }
            "workflow/pause" | "harn.workflow.pause" => {
                self.handle_workflow_pause(&id, &params);
            }
            "workflow/resume" | "harn.workflow.resume" => {
                self.handle_workflow_resume(&id, &params);
            }
            "session/list" => {
                self.handle_session_list(&id);
            }
            _ => {
                if !id.is_null() {
                    self.send_error(&id, -32601, &format!("Method not found: {method}"));
                }
            }
        }
    }
}

pub(crate) async fn run_acp_channel_server(
    pipeline: Option<String>,
    mut request_rx: mpsc::UnboundedReceiver<serde_json::Value>,
    response_tx: mpsc::UnboundedSender<String>,
) {
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let mut server = AcpServer::new_with_output(pipeline, AcpOutput::Channel(response_tx));
            let pending_clone = server.pending.clone();
            let (routed_tx, mut routed_rx) =
                tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();

            tokio::task::spawn_local(async move {
                while let Some(msg) = request_rx.recv().await {
                    if msg.get("method").is_none() && msg.get("id").is_some() {
                        if let Some(id) = msg["id"].as_u64() {
                            let mut pending = pending_clone.lock().await;
                            if let Some(sender) = pending.remove(&id) {
                                let _ = sender.send(msg);
                            }
                        }
                        continue;
                    }

                    let _ = routed_tx.send(msg);
                }

                let mut pending = pending_clone.lock().await;
                pending.clear();
            });

            while let Some(msg) = routed_rx.recv().await {
                server.handle_incoming_message(msg).await;
            }
        })
        .await;
}

/// Shared state that bridge-style builtins use to communicate with the
/// ACP client (editor) over JSON-RPC.
pub(super) struct AcpBridge {
    pub(super) session_id: String,
    pub(super) output: AcpOutput,
    pub(super) pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
    pub(super) next_id_counter: AtomicU64,
    pub(super) cancelled: Arc<AtomicBool>,
    /// Name of the currently executing Harn script (without .harn suffix).
    pub(super) script_name: std::sync::Mutex<String>,
    pub(super) assistant_state: std::sync::Mutex<VisibleTextState>,
}

impl AcpBridge {
    /// Write a complete JSON-RPC line to stdout.
    fn write_line(&self, line: &str) {
        self.output.write_line(line);
    }

    /// Send a JSON-RPC notification.
    fn send_notification(&self, method: &str, params: serde_json::Value) {
        let notification = harn_vm::jsonrpc::notification(method, params);
        if let Ok(line) = serde_json::to_string(&notification) {
            self.write_line(&line);
        }
    }

    /// Send a `session/update` with agent_message_chunk.
    pub(super) fn send_update(&self, text: &str) {
        let (visible_text, visible_delta) = self
            .assistant_state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(text, true);
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": self.session_id,
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {
                        "type": "text",
                        "text": text,
                        "visible_text": visible_text,
                        "visible_delta": visible_delta,
                    },
                },
            }),
        );
    }

    /// Send a structured `session/update` with progress phase, message,
    /// and data. `progress` is a harn vendor-extension session-update
    /// variant; canonical ACP has no progress-phase concept.
    pub(super) fn send_progress(
        &self,
        phase: &str,
        message: &str,
        progress: Option<i64>,
        total: Option<i64>,
        data: Option<serde_json::Value>,
    ) {
        let mut update = serde_json::json!({
            "sessionUpdate": "progress",
            "phase": phase,
            "message": message,
        });
        if let Some(p) = progress {
            update["progress"] = serde_json::json!(p);
        }
        if let Some(t) = total {
            update["total"] = serde_json::json!(t);
        }
        if let Some(d) = data {
            update["data"] = d;
        }
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": self.session_id,
                "update": update,
            }),
        );
    }

    /// Send a structured `session/update` with log level, message, and
    /// fields. `log` is a harn vendor-extension; canonical ACP has no
    /// log channel on the session-update stream.
    pub(super) fn send_log(&self, level: &str, message: &str, fields: Option<serde_json::Value>) {
        if level == "info" && suppress_default_info_log(message) {
            return;
        }
        let mut update = serde_json::json!({
            "sessionUpdate": "log",
            "level": level,
            "message": message,
        });
        if let Some(f) = fields {
            update["fields"] = f;
        }
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": self.session_id,
                "update": update,
            }),
        );
    }

    /// Set the currently executing script name (without .harn suffix).
    fn set_script_name(&self, name: &str) {
        *self.script_name.lock().unwrap_or_else(|e| e.into_inner()) = name.to_string();
    }

    /// Get the current script name.
    #[allow(dead_code)]
    fn get_script_name(&self) -> String {
        self.script_name
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Send a JSON-RPC request to the client and await the response.
    pub(super) async fn call_client(
        &self,
        method: &str,
        params: serde_json::Value,
    ) -> Result<serde_json::Value, harn_vm::VmError> {
        if self.cancelled.load(Ordering::SeqCst) {
            return Err(harn_vm::VmError::Runtime("Cancelled".into()));
        }

        let id = self.next_id_counter.fetch_add(1, Ordering::SeqCst);

        let request = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "method": method,
            "params": params,
        });

        let (tx, rx) = oneshot::channel();
        {
            let mut pending = self.pending.lock().await;
            pending.insert(id, tx);
        }

        if let Ok(line) = serde_json::to_string(&request) {
            self.write_line(&line);
        }

        let timeout = host_call_timeout(method);
        match tokio::time::timeout(timeout, rx).await {
            Ok(Ok(msg)) => {
                if let Some(error) = msg.get("error") {
                    let message = error["message"].as_str().unwrap_or("Unknown client error");
                    Err(harn_vm::VmError::Runtime(format!(
                        "Client error: {message}"
                    )))
                } else {
                    Ok(msg["result"].clone())
                }
            }
            Ok(Err(_)) => Err(harn_vm::VmError::Runtime("Client closed connection".into())),
            Err(_) => {
                let mut pending = self.pending.lock().await;
                pending.remove(&id);
                Err(harn_vm::VmError::Runtime(format!(
                    "Client did not respond to '{method}' within {timeout:?}"
                )))
            }
        }
    }
}

/// Start the ACP server. Reads JSON-RPC from stdin, writes to stdout.
pub async fn run_acp_server(pipeline: Option<&str>) {
    let local = tokio::task::LocalSet::new();
    let pipeline_owned = pipeline.map(|s| s.to_string());

    local
        .run_until(async move {
            let mut server = AcpServer::new(pipeline_owned);

            // stdin dispatcher: routes responses to pending waiters, and
            // requests/notifications onto the request channel.
            let pending_clone = server.pending.clone();
            let (request_tx, mut request_rx) =
                tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();

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

                    if msg.get("method").is_none() && msg.get("id").is_some() {
                        if let Some(id) = msg["id"].as_u64() {
                            let mut pending = pending_clone.lock().await;
                            if let Some(sender) = pending.remove(&id) {
                                let _ = sender.send(msg);
                            }
                        }
                        continue;
                    }

                    let _ = request_tx.send(msg);
                }

                // stdin closed — clean up pending.
                let mut pending = pending_clone.lock().await;
                pending.clear();
            });

            while let Some(msg) = request_rx.recv().await {
                server.handle_incoming_message(msg).await;
            }
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::builtins::normalize_host_capability_manifest;
    use super::sanitize_visible_assistant_text;
    use harn_vm::VmValue;
    use std::collections::BTreeMap;
    use std::rc::Rc;

    #[test]
    fn normalize_host_capabilities_wraps_array_entries_in_ops_dicts() {
        let mut root = BTreeMap::new();
        root.insert(
            "project".to_string(),
            VmValue::List(Rc::new(vec![VmValue::String(Rc::from(
                "scope_test_command",
            ))])),
        );

        let normalized = normalize_host_capability_manifest(VmValue::Dict(Rc::new(root)));
        let manifest = normalized.as_dict().expect("dict manifest");
        let project = manifest
            .get("project")
            .and_then(|value| value.as_dict())
            .expect("project capability dict");
        let ops = project
            .get("ops")
            .and_then(|value| match value {
                VmValue::List(list) => Some(list),
                _ => None,
            })
            .expect("ops list");

        assert!(ops
            .iter()
            .any(|value| value.display() == "scope_test_command"));
    }

    #[test]
    fn sanitize_visible_assistant_text_strips_internal_markers() {
        let raw = "hello\n##DONE##\nDONE\n[result of read]\nsecret\n[end of read result]\nworld";
        assert_eq!(
            sanitize_visible_assistant_text(raw, false),
            "hello\n\nworld"
        );
    }

    #[test]
    fn sanitize_visible_assistant_text_keeps_normal_code_fences() {
        let raw = "```ts\nconst x = 1\n```";
        assert_eq!(sanitize_visible_assistant_text(raw, false), raw);
    }

    #[test]
    fn sanitize_visible_assistant_text_drops_internal_json_fences() {
        let raw = "```json\n{\"plan\":[{\"tool_name\":\"read\"}]}\n```\n\nVisible";
        assert_eq!(sanitize_visible_assistant_text(raw, false), "Visible");
    }

    #[test]
    fn sanitize_visible_assistant_text_drops_inline_planner_json() {
        let raw = "{\"mode\":\"ask_user\",\"direction\":\"Need one decision\",\"targets\":[\"src\"],\"tasks\":[\"Clarify scope\"],\"unknowns\":[\"Which one?\"]}\n\nVisible";
        assert_eq!(sanitize_visible_assistant_text(raw, false), "Visible");
    }

    #[test]
    fn sanitize_visible_assistant_text_drops_partial_inline_planner_json() {
        let raw = "Visible\n{\"mode\":\"plan_then_execute\",\"direction\":\"Patch the file\"";
        assert_eq!(sanitize_visible_assistant_text(raw, true), "Visible");
    }

    #[test]
    fn sanitize_visible_assistant_text_keeps_normal_json() {
        let raw = "{\"status\":\"ok\",\"message\":\"Visible\"}";
        assert_eq!(sanitize_visible_assistant_text(raw, false), raw);
    }
}
