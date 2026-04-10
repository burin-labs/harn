//! Agent Client Protocol (ACP) server implementation.
//!
//! Implements the ACP specification (<https://agentclientprotocol.com>) so that
//! harn can act as an agent runtime accessible from any host application
//! (IDEs, CLI tools, web apps, etc.).  Communication is JSON-RPC 2.0 over stdin/stdout, following the same
//! structural pattern as the existing `--bridge` mode.

use std::collections::{BTreeMap, HashMap};
use std::io::Write;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::Instant;

use harn_vm::visible_text::{sanitize_visible_assistant_text, VisibleTextState};
use tokio::io::AsyncBufReadExt;
use tokio::sync::{oneshot, Mutex};

// ---------------------------------------------------------------------------
// Session state
// ---------------------------------------------------------------------------

struct Session {
    cwd: PathBuf,
    /// If a cancel was requested for the current prompt execution.
    cancelled: Arc<AtomicBool>,
    /// Active host bridge for queued input / daemon resume while a prompt runs.
    host_bridge: Option<Rc<harn_vm::bridge::HostBridge>>,
}

// ---------------------------------------------------------------------------
// AcpServer
// ---------------------------------------------------------------------------

/// ACP server that reads JSON-RPC requests from stdin and writes
/// responses / notifications to stdout.
pub struct AcpServer {
    /// Optional pipeline file to execute on each `session/prompt`.
    pipeline: Option<String>,
    /// Active sessions keyed by session ID.
    sessions: HashMap<String, Session>,
    /// Counter for generating unique session IDs.
    session_counter: u64,
    /// Monotonically increasing JSON-RPC request ID for outgoing requests.
    next_id: AtomicU64,
    /// Pending outgoing request waiters, keyed by JSON-RPC id.
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
    /// Mutex protecting stdout writes to prevent interleaving.
    stdout_lock: Arc<std::sync::Mutex<()>>,
}

impl AcpServer {
    fn new(pipeline: Option<String>) -> Self {
        Self {
            pipeline,
            sessions: HashMap::new(),
            session_counter: 0,
            next_id: AtomicU64::new(1),
            pending: Arc::new(Mutex::new(HashMap::new())),
            stdout_lock: Arc::new(std::sync::Mutex::new(())),
        }
    }

    /// Write a complete JSON-RPC line to stdout, serialized through a mutex.
    fn write_line(&self, line: &str) {
        let _guard = self.stdout_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(line.as_bytes());
        let _ = stdout.write_all(b"\n");
        let _ = stdout.flush();
    }

    /// Send a JSON-RPC success response.
    fn send_response(&self, id: &serde_json::Value, result: serde_json::Value) {
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "result": result,
        });
        if let Ok(line) = serde_json::to_string(&response) {
            self.write_line(&line);
        }
    }

    /// Send a JSON-RPC error response.
    fn send_error(&self, id: &serde_json::Value, code: i64, message: &str) {
        let response = serde_json::json!({
            "jsonrpc": "2.0",
            "id": id,
            "error": {
                "code": code,
                "message": message,
            },
        });
        if let Ok(line) = serde_json::to_string(&response) {
            self.write_line(&line);
        }
    }

    /// Send a JSON-RPC notification (no id, no response expected).
    #[allow(dead_code)]
    fn send_notification(&self, method: &str, params: serde_json::Value) {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
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

    /// Generate a unique session ID.
    fn next_session_id(&mut self) -> String {
        self.session_counter += 1;
        format!(
            "{:08x}-{:04x}-{:04x}-{:04x}-{:012x}",
            self.session_counter,
            std::process::id() & 0xFFFF,
            0x4000 | (self.session_counter & 0x0FFF),
            0x8000 | ((self.session_counter >> 12) & 0x3FFF),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap_or_default()
                .as_nanos()
                & 0xFFFF_FFFF_FFFF
        )
    }

    // -----------------------------------------------------------------------
    // Request handlers
    // -----------------------------------------------------------------------

    fn handle_initialize(&self, id: &serde_json::Value) {
        self.send_response(
            id,
            serde_json::json!({
                "protocolVersion": 1,
                "agentCapabilities": {
                    "promptCapabilities": {},
                },
                "agentInfo": {
                    "name": "harn",
                    "version": env!("CARGO_PKG_VERSION"),
                },
            }),
        );
    }

    fn handle_session_new(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let cwd = params
            .get("cwd")
            .and_then(|v| v.as_str())
            .map(PathBuf::from)
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| PathBuf::from(".")));

        let session_id = self.next_session_id();

        self.sessions.insert(
            session_id.clone(),
            Session {
                cwd,
                cancelled: Arc::new(AtomicBool::new(false)),
                host_bridge: None,
            },
        );

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

    async fn handle_session_prompt(&mut self, id: &serde_json::Value, params: &serde_json::Value) {
        let session_id = match params.get("sessionId").and_then(|v| v.as_str()) {
            Some(s) => s.to_string(),
            None => {
                self.send_error(id, -32602, "Missing sessionId");
                return;
            }
        };

        // Extract the prompt text from the content blocks.
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

        // Look up the session.
        let (cwd, cancelled) = match self.sessions.get_mut(&session_id) {
            Some(s) => {
                // Reset cancel flag for new execution.
                s.cancelled.store(false, Ordering::SeqCst);
                s.host_bridge = None;
                (s.cwd.clone(), s.cancelled.clone())
            }
            None => {
                self.send_error(id, -32602, &format!("Unknown session: {session_id}"));
                return;
            }
        };

        let fatal_prompt_error = |message: String| -> ! {
            exit_after_fatal_prompt_error(&self.stdout_lock, &session_id, id, &message)
        };

        // Determine source to execute: either the configured pipeline file or
        // the prompt text treated as inline harn source.
        let (source, source_path) = if let Some(ref pipeline_path) = self.pipeline {
            let full_path = if std::path::Path::new(pipeline_path).is_absolute() {
                PathBuf::from(pipeline_path)
            } else {
                cwd.join(pipeline_path)
            };
            match std::fs::read_to_string(&full_path) {
                Ok(src) => (src, Some(full_path)),
                Err(e) => fatal_prompt_error(format!(
                    "Failed to read pipeline {}: {e}",
                    full_path.display()
                )),
            }
        } else {
            // Treat the prompt text as inline harn source code.
            // Wrap in a pipeline so the compiler has an entry point.
            let wrapped = format!("pipeline main() {{\n{prompt_text}\n}}");
            (wrapped, None)
        };

        // Build shared state for bridge-style builtins.
        let stdout_lock = self.stdout_lock.clone();
        let pending = self.pending.clone();
        let next_id = &self.next_id;
        let sid = session_id.clone();

        // We need to construct a lightweight struct that the builtins can use
        // to send notifications and make client requests. We package the
        // relevant Arc/Rc references into an AcpBridge.
        let bridge = Rc::new(AcpBridge {
            session_id: sid.clone(),
            stdout_lock: stdout_lock.clone(),
            pending: pending.clone(),
            next_id_counter: AtomicU64::new(next_id.fetch_add(1000, Ordering::SeqCst)),
            cancelled: cancelled.clone(),
            script_name: std::sync::Mutex::new(String::new()),
            assistant_state: std::sync::Mutex::new(VisibleTextState::default()),
        });
        let host_bridge = Rc::new(harn_vm::bridge::HostBridge::from_parts(
            bridge.pending.clone(),
            Arc::new(AtomicBool::new(false)),
            bridge.stdout_lock.clone(),
            bridge.next_id_counter.fetch_add(10_000, Ordering::SeqCst),
        ));
        host_bridge.set_session_id(&bridge.session_id);

        let compile_started = Instant::now();
        let chunk = match compile_source(&source, source_path.as_deref()) {
            Ok(c) => c,
            Err(e) => fatal_prompt_error(format!("Compilation error: {e}")),
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

        // Execute the compiled chunk.
        let id_owned = id.clone();
        let send_lock = self.stdout_lock.clone();
        let result = execute_chunk(
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

        match result {
            Ok(output) => {
                if !output.is_empty() {
                    // Send output as an update notification with cumulative
                    // visible assistant text for host UIs.
                    bridge.send_update(&output);
                }
                send_json_response(
                    &send_lock,
                    &id_owned,
                    serde_json::json!({"stopReason": "completed"}),
                );
            }
            Err(e) => {
                if cancelled.load(Ordering::SeqCst) {
                    send_json_response(
                        &send_lock,
                        &id_owned,
                        serde_json::json!({"stopReason": "cancelled"}),
                    );
                } else {
                    exit_after_fatal_prompt_error(&send_lock, &sid, &id_owned, &e);
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
            .keys()
            .map(|sid| serde_json::json!({"sessionId": sid}))
            .collect();
        self.send_response(id, serde_json::json!({"sessions": sessions}));
    }
}

// ---------------------------------------------------------------------------
// AcpBridge — lightweight handle shared with VM builtins
// ---------------------------------------------------------------------------

/// Shared state that bridge-style builtins use to communicate with the
/// ACP client (editor) over JSON-RPC.
struct AcpBridge {
    session_id: String,
    stdout_lock: Arc<std::sync::Mutex<()>>,
    pending: Arc<Mutex<HashMap<u64, oneshot::Sender<serde_json::Value>>>>,
    next_id_counter: AtomicU64,
    cancelled: Arc<AtomicBool>,
    /// Name of the currently executing Harn script (without .harn suffix).
    script_name: std::sync::Mutex<String>,
    assistant_state: std::sync::Mutex<VisibleTextState>,
}

impl AcpBridge {
    /// Write a complete JSON-RPC line to stdout.
    fn write_line(&self, line: &str) {
        let _guard = self.stdout_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(line.as_bytes());
        let _ = stdout.write_all(b"\n");
        let _ = stdout.flush();
    }

    /// Send a JSON-RPC notification.
    fn send_notification(&self, method: &str, params: serde_json::Value) {
        let notification = serde_json::json!({
            "jsonrpc": "2.0",
            "method": method,
            "params": params,
        });
        if let Ok(line) = serde_json::to_string(&notification) {
            self.write_line(&line);
        }
    }

    /// Send a `session/update` with agent_message_chunk.
    fn send_update(&self, text: &str) {
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

    /// Send a structured `session/update` with progress phase, message, and data.
    fn send_progress(
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

    /// Send a structured `session/update` with log level, message, and fields.
    fn send_log(&self, level: &str, message: &str, fields: Option<serde_json::Value>) {
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

    /// Send a `session/update` with `call_start` — signals the beginning of
    /// an LLM call, tool call, or builtin call for observability.
    #[allow(dead_code)]
    fn send_call_start(
        &self,
        call_id: &str,
        call_type: &str,
        name: &str,
        metadata: serde_json::Value,
    ) {
        let script = self.get_script_name();
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": self.session_id,
                "update": {
                    "sessionUpdate": "call_start",
                    "content": {
                        "call_id": call_id,
                        "call_type": call_type,
                        "name": name,
                        "script": script,
                        "metadata": metadata,
                    },
                },
            }),
        );
    }

    /// Send a `session/update` with `call_end` — signals completion of a call.
    #[allow(dead_code)]
    fn send_call_end(
        &self,
        call_id: &str,
        call_type: &str,
        name: &str,
        duration_ms: u64,
        status: &str,
        metadata: serde_json::Value,
    ) {
        let script = self.get_script_name();
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": self.session_id,
                "update": {
                    "sessionUpdate": "call_end",
                    "content": {
                        "call_id": call_id,
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

    /// Send a JSON-RPC request to the client and await the response.
    async fn call_client(
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

        let timeout = std::time::Duration::from_secs(60);
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

// ---------------------------------------------------------------------------
// Helper functions
// ---------------------------------------------------------------------------

/// Compile harn source code into a bytecode chunk.
fn compile_source(
    source: &str,
    _source_path: Option<&std::path::Path>,
) -> Result<harn_vm::Chunk, String> {
    let mut lexer = harn_lexer::Lexer::new(source);
    let tokens = lexer.tokenize().map_err(|e| e.to_string())?;
    let mut parser = harn_parser::Parser::new(tokens);
    let program = parser.parse().map_err(|e| e.to_string())?;

    // Static type checking.
    let type_diagnostics = harn_parser::TypeChecker::new().check(&program);
    for diag in &type_diagnostics {
        if diag.severity == harn_parser::DiagnosticSeverity::Error {
            return Err(diag.message.clone());
        }
    }

    harn_vm::Compiler::new()
        .compile(&program)
        .map_err(|e| e.to_string())
}

/// Execute a compiled chunk with ACP bridge builtins.
async fn execute_chunk(
    chunk: harn_vm::Chunk,
    bridge: Rc<AcpBridge>,
    host_bridge: Rc<harn_vm::bridge::HostBridge>,
    prompt_text: &str,
    source_path: Option<&std::path::Path>,
    cwd: &std::path::Path,
) -> Result<String, String> {
    let vm_setup_started = Instant::now();
    let mut vm = harn_vm::Vm::new();
    harn_vm::register_vm_stdlib(&mut vm);
    // Use project root (harn.toml) for metadata/store, falling back to cwd.
    let source_parent = source_path.and_then(|p| p.parent()).unwrap_or(cwd);
    let project_root = harn_vm::stdlib::process::find_project_root(source_parent)
        .or_else(|| harn_vm::stdlib::process::find_project_root(cwd));
    let store_base = project_root.as_deref().unwrap_or(cwd);
    harn_vm::register_store_builtins(&mut vm, store_base);
    harn_vm::register_metadata_builtins(&mut vm, store_base);
    let pipeline_name = source_path
        .and_then(|p| p.file_stem())
        .and_then(|s| s.to_str())
        .unwrap_or("acp");
    harn_vm::register_checkpoint_builtins(&mut vm, store_base, pipeline_name);
    bridge.set_script_name(pipeline_name);
    if let Some(ref root) = project_root {
        vm.set_project_root(root);
    }

    if let Some(path) = source_path {
        let path_str = path.to_string_lossy();
        let source = std::fs::read_to_string(path).unwrap_or_default();
        vm.set_source_info(&path_str, &source);
        if let Some(parent) = path.parent() {
            if !parent.as_os_str().is_empty() {
                vm.set_source_dir(parent);
            }
        }
    } else {
        vm.set_source_dir(cwd);
    }

    // Inject the prompt text as a global variable so pipelines can access it.
    vm.set_global("prompt", harn_vm::VmValue::String(Rc::from(prompt_text)));
    vm.set_global(
        "cwd",
        harn_vm::VmValue::String(Rc::from(cwd.to_string_lossy().as_ref())),
    );

    let mcp_globals = load_host_mcp_clients(host_bridge.clone()).await;
    if !mcp_globals.is_empty() {
        vm.set_global("mcp", harn_vm::VmValue::Dict(Rc::new(mcp_globals)));
    }

    // Register ACP-specific builtins that delegate file I/O to the editor.
    register_acp_builtins(&mut vm, bridge.clone()).await;

    // Set up bridge delegation so unknown builtins are forwarded to the ACP
    // client as `builtin_call` JSON-RPC requests. This remains the stable ACP
    // behavior until host-local pseudo-builtins are fully migrated to typed
    // host capabilities and explicit Harn stdlib wrappers.
    host_bridge.set_script_name(pipeline_name);
    vm.set_bridge(host_bridge.clone());

    // Override the native text-only agent_loop with the tool-aware version.
    // This allows agent_loop to execute tools via the ACP bridge (delegated
    // to the editor/CLI which has the full tool infrastructure).
    harn_vm::llm::register_agent_loop_with_bridge(&mut vm, host_bridge.clone());

    // Override llm_call with bridge-aware version for call_start/call_end observability.
    harn_vm::llm::register_llm_call_with_bridge(&mut vm, host_bridge);

    let vm_setup_ms = vm_setup_started.elapsed().as_millis() as u64;
    bridge.send_log(
        "info",
        &format!("ACP_BOOT: vm_setup_ms={vm_setup_ms} pipeline={pipeline_name}"),
        Some(serde_json::json!({
            "pipeline": pipeline_name,
            "vm_setup_ms": vm_setup_ms,
        })),
    );

    let execution = harn_vm::orchestration::RunExecutionRecord {
        cwd: Some(cwd.to_string_lossy().to_string()),
        source_dir: source_path
            .and_then(|p| p.parent())
            .map(|p| p.to_string_lossy().to_string()),
        ..Default::default()
    };
    harn_vm::stdlib::process::set_thread_execution_context(Some(execution));
    let execute_started = Instant::now();
    let result = match vm.execute(&chunk).await {
        Ok(_) => Ok(vm.output().to_string()),
        Err(e) => {
            let formatted = vm.format_runtime_error(&e);
            Err(formatted)
        }
    };
    let execute_ms = execute_started.elapsed().as_millis() as u64;
    bridge.send_log(
        "info",
        &format!("ACP_BOOT: execute_ms={execute_ms} pipeline={pipeline_name}"),
        Some(serde_json::json!({
            "pipeline": pipeline_name,
            "execute_ms": execute_ms,
        })),
    );
    harn_vm::stdlib::process::set_thread_execution_context(None);
    result
}

async fn load_host_mcp_clients(
    host_bridge: Rc<harn_vm::bridge::HostBridge>,
) -> BTreeMap<String, harn_vm::VmValue> {
    let mut mcp_dict = BTreeMap::new();
    let capabilities = host_bridge
        .call("host/capabilities", serde_json::json!({}))
        .await
        .ok()
        .and_then(|value| value.as_object().cloned());
    let has_project_mcp_config = capabilities
        .as_ref()
        .and_then(|root| root.get("project"))
        .and_then(|entry| entry.as_array())
        .is_some_and(|ops| ops.iter().any(|value| value.as_str() == Some("mcp_config")));
    if !has_project_mcp_config {
        return mcp_dict;
    }
    let response = match host_bridge
        .call(
            "host/call",
            serde_json::json!({
                "name": "project.mcp_config",
                "args": {}
            }),
        )
        .await
    {
        Ok(value) => value,
        Err(err) => {
            eprintln!("warning: mcp: failed to load host MCP config: {err}");
            return mcp_dict;
        }
    };

    let Some(servers) = response.as_array() else {
        return mcp_dict;
    };

    for server in servers {
        match harn_vm::connect_mcp_server_from_json(server).await {
            Ok(handle) => {
                eprintln!("[harn] mcp: connected to '{}'", handle.name);
                mcp_dict.insert(handle.name.clone(), harn_vm::VmValue::McpClient(handle));
            }
            Err(err) => {
                let name = server
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or("unknown");
                eprintln!("warning: mcp: failed to connect to '{}': {}", name, err);
            }
        }
    }

    mcp_dict
}

fn normalize_host_capability_manifest(value: harn_vm::VmValue) -> harn_vm::VmValue {
    let Some(root) = value.as_dict() else {
        return harn_vm::VmValue::Dict(Rc::new(BTreeMap::new()));
    };

    let mut normalized = BTreeMap::new();
    for (capability, entry) in root.iter() {
        match entry {
            harn_vm::VmValue::Dict(_) => {
                normalized.insert(capability.clone(), entry.clone());
            }
            harn_vm::VmValue::List(list) => {
                let mut dict = BTreeMap::new();
                dict.insert("ops".to_string(), harn_vm::VmValue::List(list.clone()));
                normalized.insert(capability.clone(), harn_vm::VmValue::Dict(Rc::new(dict)));
            }
            _ => {}
        }
    }

    harn_vm::VmValue::Dict(Rc::new(normalized))
}

/// Register builtins that delegate to the ACP client (editor).
async fn register_acp_builtins(vm: &mut harn_vm::Vm, bridge: Rc<AcpBridge>) {
    let host_capability_manifest = bridge
        .call_client(
            "host/capabilities",
            serde_json::json!({
                "sessionId": bridge.session_id,
            }),
        )
        .await
        .map(|result| {
            normalize_host_capability_manifest(harn_vm::bridge::json_result_to_vm_value(&result))
        })
        .unwrap_or_else(|_| harn_vm::VmValue::Dict(Rc::new(std::collections::BTreeMap::new())));

    // --- Output builtins: route through session/update ---

    let b = bridge.clone();
    vm.register_builtin("log", move |args, _out| {
        let msg = args.first().map(|a| a.display()).unwrap_or_default();
        b.send_update(&format!("[harn] {msg}\n"));
        Ok(harn_vm::VmValue::Nil)
    });

    let b = bridge.clone();
    vm.register_builtin("print", move |args, _out| {
        let msg = args.first().map(|a| a.display()).unwrap_or_default();
        b.send_update(&msg);
        Ok(harn_vm::VmValue::Nil)
    });

    let b = bridge.clone();
    vm.register_builtin("println", move |args, _out| {
        let msg = args.first().map(|a| a.display()).unwrap_or_default();
        b.send_update(&format!("{msg}\n"));
        Ok(harn_vm::VmValue::Nil)
    });

    // --- Host callback — generic escape hatch ---

    let b = bridge.clone();
    vm.register_async_builtin("host_call", move |args| {
        let bridge = b.clone();
        async move {
            let name = args.first().map(|a| a.display()).unwrap_or_default();
            let call_args = args.get(1).cloned().unwrap_or(harn_vm::VmValue::Nil);
            let args_json = harn_vm::llm::vm_value_to_json(&call_args);
            let result = bridge
                .call_client(
                    "host/call",
                    serde_json::json!({
                        "sessionId": bridge.session_id,
                        "name": name,
                        "args": args_json,
                    }),
                )
                .await?;
            Ok(harn_vm::bridge::json_result_to_vm_value(&result))
        }
    });

    // --- Typed host capabilities ---

    let host_capabilities_cache = host_capability_manifest.clone();
    vm.register_builtin("host_capabilities", move |_args, _out| {
        Ok(host_capabilities_cache.clone())
    });

    let host_has_cache = host_capability_manifest.clone();
    vm.register_builtin("host_has", move |args, _out| {
        let capability = args.first().map(|a| a.display()).unwrap_or_default();
        let op = args.get(1).map(|a| a.display());
        let valid = if let Some(manifest) = host_has_cache.as_dict() {
            if let Some(value) = manifest.get(&capability) {
                if let Some(cap) = value.as_dict() {
                    if let Some(op) = op {
                        cap.get("ops")
                            .and_then(|ops| match ops {
                                harn_vm::VmValue::List(list) => {
                                    Some(list.iter().any(|item| item.display() == op))
                                }
                                _ => None,
                            })
                            .unwrap_or(false)
                    } else {
                        true
                    }
                } else {
                    false
                }
            } else {
                false
            }
        } else {
            false
        };
        Ok(harn_vm::VmValue::Bool(valid))
    });

    // --- ask_user — delegate to host (IDE shows modal, CLI reads stdin) ---

    let b = bridge.clone();
    vm.register_async_builtin("ask_user", move |args| {
        let bridge = b.clone();
        async move {
            let question = args.first().map(|a| a.display()).unwrap_or_default();
            let question_type = args.get(1).map(|a| a.display());
            let mut params = serde_json::json!({
                "sessionId": bridge.session_id,
                "name": "ask_user",
                "args": {"question": question},
            });
            if let Some(qt) = question_type {
                params["args"]["type"] = serde_json::json!(qt);
            }
            let result = bridge.call_client("host/call", params).await?;
            Ok(harn_vm::bridge::json_result_to_vm_value(&result))
        }
    });

    // --- run_command — alias to exec/terminal ---

    let b = bridge.clone();
    vm.register_async_builtin("run_command", move |args| {
        let bridge = b.clone();
        async move { acp_terminal_exec(&bridge, &args).await }
    });

    // --- Structured log builtins ---

    for level in ["log_debug", "log_info", "log_warn", "log_error"] {
        let b = bridge.clone();
        let lvl = level.strip_prefix("log_").unwrap_or(level).to_string();
        vm.register_builtin(level, move |args, _out| {
            let msg = args.first().map(|a| a.display()).unwrap_or_default();
            let fields = args.get(1).and_then(|a| {
                if matches!(a, harn_vm::VmValue::Nil) {
                    None
                } else {
                    Some(harn_vm::llm::vm_value_to_json(a))
                }
            });
            b.send_log(&lvl, &msg, fields);
            Ok(harn_vm::VmValue::Nil)
        });
    }

    // --- Live span streaming ---
    //
    // The default `trace_end` builtin writes its `span_end` line to the
    // VM's internal `out` buffer, which only surfaces when the whole
    // pipeline completes. In bridge mode that's too late — pipelines
    // that get stuck in a hot loop never reach the flush point, so
    // timing data is invisible when we need it most. Override the
    // builtin so `span_end` events stream live via `send_log` just
    // like `log_info`.
    let b = bridge.clone();
    vm.register_builtin("trace_end", move |args, _out| {
        let (name, trace_id, span_id, duration_ms) =
            harn_vm::stdlib::tracing::finish_span_from_args(args)?;
        // Stamp the span name + duration into the human-readable message
        // itself so formatters that only surface `message` (not the fields
        // payload) still show useful timing info at the top of log lines.
        let message = format!("span_end {name} duration_ms={duration_ms}");
        let fields = serde_json::json!({
            "trace_id": trace_id,
            "span_id": span_id,
            "name": name,
            "duration_ms": duration_ms,
        });
        b.send_log("info", &message, Some(fields));
        Ok(harn_vm::VmValue::Nil)
    });

    // --- Progress reporting ---

    let b = bridge.clone();
    vm.register_builtin("progress", move |args, _out| {
        let phase = args.first().map(|a| a.display()).unwrap_or_default();
        let message = args.get(1).map(|a| a.display()).unwrap_or_default();
        let progress_val = args.get(2).and_then(|a| a.as_int());
        let total_val = args.get(3).and_then(|a| a.as_int());
        let data = args.get(4).and_then(|a| {
            if matches!(a, harn_vm::VmValue::Nil) {
                None
            } else {
                Some(harn_vm::llm::vm_value_to_json(a))
            }
        });
        b.send_progress(&phase, &message, progress_val, total_val, data);
        Ok(harn_vm::VmValue::Nil)
    });

    let b = bridge.clone();
    vm.register_builtin("emit_response", move |args, _out| {
        let text = args.first().map(|a| a.display()).unwrap_or_default();
        b.send_update(&text);
        Ok(harn_vm::VmValue::Nil)
    });

    // --- Terminal: delegate exec/shell via terminal/create + wait + output + release ---

    for name in ["exec", "shell"] {
        vm.unregister_builtin(name);
    }

    let b = bridge.clone();
    vm.register_async_builtin("exec", move |args| {
        let bridge = b.clone();
        async move { acp_terminal_exec(&bridge, &args).await }
    });

    let b = bridge;
    vm.register_async_builtin("shell", move |args| {
        let bridge = b.clone();
        async move { acp_terminal_exec(&bridge, &args).await }
    });
}

/// Execute a command through ACP terminal/create + wait_for_exit + output + release.
async fn acp_terminal_exec(
    bridge: &AcpBridge,
    args: &[harn_vm::VmValue],
) -> Result<harn_vm::VmValue, harn_vm::VmError> {
    let cmd = args.first().map(|a| a.display()).unwrap_or_default();
    if cmd.is_empty() {
        return Err(harn_vm::VmError::Thrown(harn_vm::VmValue::String(
            Rc::from("exec: command is required"),
        )));
    }

    // 1. Create a terminal with the command.
    let create_result = bridge
        .call_client(
            "terminal/create",
            serde_json::json!({
                "sessionId": bridge.session_id,
                "command": cmd,
            }),
        )
        .await?;

    let terminal_id = create_result
        .get("terminalId")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .to_string();

    if terminal_id.is_empty() {
        // Client doesn't support terminal — fall back to local exec.
        let output = std::process::Command::new("sh")
            .arg("-c")
            .arg(&cmd)
            .output()
            .map_err(|e| {
                harn_vm::VmError::Thrown(harn_vm::VmValue::String(Rc::from(format!(
                    "exec failed: {e}"
                ))))
            })?;
        let stdout = String::from_utf8_lossy(&output.stdout).to_string();
        let stderr = String::from_utf8_lossy(&output.stderr).to_string();
        let exit_code = output.status.code().unwrap_or(-1);
        let mut map = std::collections::BTreeMap::new();
        map.insert(
            "stdout".to_string(),
            harn_vm::VmValue::String(Rc::from(stdout)),
        );
        map.insert(
            "stderr".to_string(),
            harn_vm::VmValue::String(Rc::from(stderr)),
        );
        map.insert(
            "combined".to_string(),
            harn_vm::VmValue::String(Rc::from(format!(
                "{}{}",
                map.get("stdout").map(|v| v.display()).unwrap_or_default(),
                map.get("stderr").map(|v| v.display()).unwrap_or_default()
            ))),
        );
        map.insert(
            "status".to_string(),
            harn_vm::VmValue::Int(exit_code as i64),
        );
        map.insert(
            "success".to_string(),
            harn_vm::VmValue::Bool(output.status.success()),
        );
        return Ok(harn_vm::VmValue::Dict(Rc::new(map)));
    }

    // 2. Wait for the command to finish — the result contains stdout/stderr/combined/exitCode.
    let wait_result = bridge
        .call_client(
            "terminal/wait_for_exit",
            serde_json::json!({
                "sessionId": bridge.session_id,
                "terminalId": terminal_id,
            }),
        )
        .await
        .unwrap_or(serde_json::json!({}));

    // 3. Read any remaining output (usually empty since wait_for_exit reads the pipes).
    let _output_result = bridge
        .call_client(
            "terminal/output",
            serde_json::json!({
                "sessionId": bridge.session_id,
                "terminalId": terminal_id,
            }),
        )
        .await
        .unwrap_or(serde_json::json!({}));

    // Use wait_for_exit result which has the actual stdout/stderr/combined.
    let output_result = wait_result;

    // 4. Release the terminal.
    let _ = bridge
        .call_client(
            "terminal/release",
            serde_json::json!({
                "sessionId": bridge.session_id,
                "terminalId": terminal_id,
            }),
        )
        .await;

    let output = harn_vm::bridge::json_result_to_vm_value(&output_result);
    if let harn_vm::VmValue::Dict(map) = &output {
        let mut normalized = (**map).clone();
        let stdout = normalized
            .get("stdout")
            .map(|v| v.display())
            .unwrap_or_default();
        let stderr = normalized
            .get("stderr")
            .map(|v| v.display())
            .unwrap_or_default();
        if !normalized.contains_key("combined") {
            normalized.insert(
                "combined".to_string(),
                harn_vm::VmValue::String(Rc::from(format!("{stdout}{stderr}"))),
            );
        }
        if !normalized.contains_key("status") {
            let status = normalized
                .get("exit_code")
                .or_else(|| normalized.get("exitCode"))
                .and_then(|v| v.as_int())
                .unwrap_or(-1);
            normalized.insert("status".to_string(), harn_vm::VmValue::Int(status));
        }
        if !normalized.contains_key("success") {
            let success = normalized
                .get("status")
                .and_then(|v| v.as_int())
                .is_some_and(|code| code == 0);
            normalized.insert("success".to_string(), harn_vm::VmValue::Bool(success));
        }
        return Ok(harn_vm::VmValue::Dict(Rc::new(normalized)));
    }
    Ok(output)
}

/// Write a `session/update` notification directly through a stdout lock.
fn send_update_raw(stdout_lock: &Arc<std::sync::Mutex<()>>, session_id: &str, text: &str) {
    let visible_text = sanitize_visible_assistant_text(text, true);
    let notification = serde_json::json!({
        "jsonrpc": "2.0",
        "method": "session/update",
        "params": {
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
        },
    });
    if let Ok(line) = serde_json::to_string(&notification) {
        let _guard = stdout_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(line.as_bytes());
        let _ = stdout.write_all(b"\n");
        let _ = stdout.flush();
    }
}

/// Write a JSON-RPC response directly through a stdout lock.
fn send_json_response(
    stdout_lock: &Arc<std::sync::Mutex<()>>,
    id: &serde_json::Value,
    result: serde_json::Value,
) {
    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "result": result,
    });
    if let Ok(line) = serde_json::to_string(&response) {
        let _guard = stdout_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(line.as_bytes());
        let _ = stdout.write_all(b"\n");
        let _ = stdout.flush();
    }
}

/// Write a JSON-RPC error response directly through a stdout lock.
fn send_json_error(
    stdout_lock: &Arc<std::sync::Mutex<()>>,
    id: &serde_json::Value,
    code: i64,
    message: &str,
) {
    let response = serde_json::json!({
        "jsonrpc": "2.0",
        "id": id,
        "error": {
            "code": code,
            "message": message,
        },
    });
    if let Ok(line) = serde_json::to_string(&response) {
        let _guard = stdout_lock.lock().unwrap_or_else(|e| e.into_inner());
        let mut stdout = std::io::stdout().lock();
        let _ = stdout.write_all(line.as_bytes());
        let _ = stdout.write_all(b"\n");
        let _ = stdout.flush();
    }
}

fn flush_stdio() {
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
}

fn exit_after_fatal_prompt_error(
    stdout_lock: &Arc<std::sync::Mutex<()>>,
    session_id: &str,
    id: &serde_json::Value,
    message: &str,
) -> ! {
    send_update_raw(stdout_lock, session_id, &format!("Error: {message}\n"));
    send_json_error(stdout_lock, id, -32000, message);
    eprintln!("{message}");
    flush_stdio();
    std::process::exit(2);
}

// ---------------------------------------------------------------------------
// Main entry point
// ---------------------------------------------------------------------------

/// Start the ACP server.  Reads JSON-RPC from stdin, writes to stdout.
pub async fn run_acp_server(pipeline: Option<&str>) {
    let local = tokio::task::LocalSet::new();
    let pipeline_owned = pipeline.map(|s| s.to_string());

    local
        .run_until(async move {
            let mut server = AcpServer::new(pipeline_owned);

            // Spawn the stdin reader.  It dispatches:
            //   - responses (has "id" + "result"/"error") -> pending waiters
            //   - requests  (has "id" + "method")          -> request channel
            //   - notifications (no "id" + "method")       -> request channel
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

                    // Is this a response to one of our outgoing requests?
                    if msg.get("method").is_none() && msg.get("id").is_some() {
                        // It's a response.
                        if let Some(id) = msg["id"].as_u64() {
                            let mut pending = pending_clone.lock().await;
                            if let Some(sender) = pending.remove(&id) {
                                let _ = sender.send(msg);
                            }
                        }
                        continue;
                    }

                    // Otherwise it's a request or notification from the client.
                    let _ = request_tx.send(msg);
                }

                // stdin closed — clean up pending.
                let mut pending = pending_clone.lock().await;
                pending.clear();
            });

            // Main request-processing loop.
            while let Some(msg) = request_rx.recv().await {
                let method = match msg.get("method").and_then(|v| v.as_str()) {
                    Some(m) => m.to_string(),
                    None => continue,
                };
                let id = msg.get("id").cloned().unwrap_or(serde_json::Value::Null);
                let params = msg.get("params").cloned().unwrap_or(serde_json::json!({}));

                match method.as_str() {
                    "initialize" => {
                        server.handle_initialize(&id);
                    }
                    "session/new" => {
                        server.handle_session_new(&id, &params);
                    }
                    "session/prompt" => {
                        server.handle_session_prompt(&id, &params).await;
                    }
                    "session/cancel" => {
                        server.handle_session_cancel(&params);
                    }
                    "session/input" | "user_message" | "agent/user_message" => {
                        server.handle_session_input(&params).await;
                    }
                    "agent/resume" => {
                        server.handle_agent_resume(&params);
                    }
                    "session/list" => {
                        server.handle_session_list(&id);
                    }
                    _ => {
                        // Unknown method — send error if it has an id.
                        if !id.is_null() {
                            server.send_error(&id, -32601, &format!("Method not found: {method}"));
                        }
                    }
                }
            }
        })
        .await;
}

#[cfg(test)]
mod tests {
    use super::{normalize_host_capability_manifest, sanitize_visible_assistant_text};
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
