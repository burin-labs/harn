//! Agent Client Protocol (ACP) server implementation.
//!
//! Implements the ACP specification (<https://agentclientprotocol.com>) so that
//! harn can act as a coding agent usable from any editor (JetBrains, VS Code,
//! etc.).  Communication is JSON-RPC 2.0 over stdin/stdout, following the same
//! structural pattern as the existing `--bridge` mode.

use std::collections::HashMap;
use std::io::Write;
use std::path::PathBuf;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;

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
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": session_id,
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {
                        "type": "text",
                        "text": text,
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
                Err(e) => {
                    self.send_error(
                        id,
                        -32000,
                        &format!("Failed to read pipeline {}: {e}", full_path.display()),
                    );
                    return;
                }
            }
        } else {
            // Treat the prompt text as inline harn source code.
            // Wrap in a pipeline so the compiler has an entry point.
            let wrapped = format!("pipeline main() {{\n{prompt_text}\n}}");
            (wrapped, None)
        };

        // Compile the source.
        let chunk = match compile_source(&source, source_path.as_deref()) {
            Ok(c) => c,
            Err(e) => {
                self.send_error(id, -32000, &format!("Compilation error: {e}"));
                return;
            }
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
        });
        let host_bridge = Rc::new(harn_vm::bridge::HostBridge::from_parts(
            bridge.pending.clone(),
            Arc::new(AtomicBool::new(false)),
            bridge.stdout_lock.clone(),
            bridge.next_id_counter.fetch_add(10_000, Ordering::SeqCst),
        ));
        host_bridge.set_session_id(&bridge.session_id);
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
                    // Send output as an update notification.
                    send_update_raw(&send_lock, &sid, &output);
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
                    // Send error as update, then complete with error reason.
                    send_update_raw(&send_lock, &sid, &format!("Error: {e}\n"));
                    send_json_response(
                        &send_lock,
                        &id_owned,
                        serde_json::json!({"stopReason": "error"}),
                    );
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
        self.send_notification(
            "session/update",
            serde_json::json!({
                "sessionId": self.session_id,
                "update": {
                    "sessionUpdate": "agent_message_chunk",
                    "content": {
                        "type": "text",
                        "text": text,
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

    // Register ACP-specific builtins that delegate file I/O to the editor.
    register_acp_builtins(&mut vm, bridge.clone());

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

    match vm.execute(&chunk).await {
        Ok(_) => Ok(vm.output().to_string()),
        Err(e) => {
            let formatted = vm.format_runtime_error(&e);
            Err(formatted)
        }
    }
}

/// Register builtins that delegate to the ACP client (editor).
fn register_acp_builtins(vm: &mut harn_vm::Vm, bridge: Rc<AcpBridge>) {
    // Override sync file builtins so async versions take precedence.
    for name in ["read_file", "write_file"] {
        vm.unregister_builtin(name);
    }

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

    // --- File I/O: delegate to editor via ACP fs/ methods ---

    let b = bridge.clone();
    vm.register_async_builtin("read_file", move |args| {
        let bridge = b.clone();
        async move {
            let path = args.first().map(|a| a.display()).unwrap_or_default();
            let result = bridge
                .call_client(
                    "fs/read_text_file",
                    serde_json::json!({
                        "sessionId": bridge.session_id,
                        "path": path,
                    }),
                )
                .await?;
            if let Some(content) = result.get("content").and_then(|v| v.as_str()) {
                Ok(harn_vm::VmValue::String(Rc::from(content)))
            } else {
                Ok(harn_vm::bridge::json_result_to_vm_value(&result))
            }
        }
    });

    let b = bridge.clone();
    vm.register_async_builtin("write_file", move |args| {
        let bridge = b.clone();
        async move {
            let path = args.first().map(|a| a.display()).unwrap_or_default();
            let content = args.get(1).map(|a| a.display()).unwrap_or_default();
            bridge
                .call_client(
                    "fs/write_text_file",
                    serde_json::json!({
                        "sessionId": bridge.session_id,
                        "path": path,
                        "content": content,
                    }),
                )
                .await?;
            Ok(harn_vm::VmValue::Nil)
        }
    });

    // --- Additional file I/O builtins (previously bridge-only) ---

    let b = bridge.clone();
    vm.register_async_builtin("apply_edit", move |args| {
        let bridge = b.clone();
        async move {
            let file = args.first().map(|a| a.display()).unwrap_or_default();
            let old_str = args.get(1).map(|a| a.display()).unwrap_or_default();
            let new_str = args.get(2).map(|a| a.display()).unwrap_or_default();
            bridge
                .call_client(
                    "fs/apply_edit",
                    serde_json::json!({
                        "sessionId": bridge.session_id,
                        "path": file,
                        "old_string": old_str,
                        "new_string": new_str,
                    }),
                )
                .await?;
            Ok(harn_vm::VmValue::Nil)
        }
    });

    vm.unregister_builtin("delete_file");

    let b = bridge.clone();
    vm.register_async_builtin("delete_file", move |args| {
        let bridge = b.clone();
        async move {
            let path = args.first().map(|a| a.display()).unwrap_or_default();
            bridge
                .call_client(
                    "fs/delete_file",
                    serde_json::json!({
                        "sessionId": bridge.session_id,
                        "path": path,
                    }),
                )
                .await?;
            Ok(harn_vm::VmValue::Nil)
        }
    });

    // file_exists is sync — delegates to local filesystem (same as bridge mode)
    vm.register_builtin("file_exists", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(harn_vm::VmValue::Bool(std::path::Path::new(&path).exists()))
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

    vm.register_builtin("host_capabilities", |_args, _out| {
        Ok(harn_vm::VmValue::Dict(Rc::new(
            std::collections::BTreeMap::from([
                (
                    "workspace".to_string(),
                    harn_vm::VmValue::Dict(Rc::new(std::collections::BTreeMap::from([
                        (
                            "description".to_string(),
                            harn_vm::VmValue::String(Rc::from(
                                "Workspace file and directory operations.",
                            )),
                        ),
                        (
                            "ops".to_string(),
                            harn_vm::VmValue::List(Rc::new(vec![
                                harn_vm::VmValue::String(Rc::from("read_text")),
                                harn_vm::VmValue::String(Rc::from("write_text")),
                                harn_vm::VmValue::String(Rc::from("apply_edit")),
                                harn_vm::VmValue::String(Rc::from("delete")),
                                harn_vm::VmValue::String(Rc::from("exists")),
                                harn_vm::VmValue::String(Rc::from("file_exists")),
                                harn_vm::VmValue::String(Rc::from("list")),
                                harn_vm::VmValue::String(Rc::from("project_root")),
                            ])),
                        ),
                    ]))),
                ),
                (
                    "process".to_string(),
                    harn_vm::VmValue::Dict(Rc::new(std::collections::BTreeMap::from([
                        (
                            "description".to_string(),
                            harn_vm::VmValue::String(Rc::from("Process execution.")),
                        ),
                        (
                            "ops".to_string(),
                            harn_vm::VmValue::List(Rc::new(vec![harn_vm::VmValue::String(
                                Rc::from("exec"),
                            )])),
                        ),
                    ]))),
                ),
                (
                    "template".to_string(),
                    harn_vm::VmValue::Dict(Rc::new(std::collections::BTreeMap::from([
                        (
                            "description".to_string(),
                            harn_vm::VmValue::String(Rc::from("Template rendering.")),
                        ),
                        (
                            "ops".to_string(),
                            harn_vm::VmValue::List(Rc::new(vec![harn_vm::VmValue::String(
                                Rc::from("render"),
                            )])),
                        ),
                    ]))),
                ),
                (
                    "interaction".to_string(),
                    harn_vm::VmValue::Dict(Rc::new(std::collections::BTreeMap::from([
                        (
                            "description".to_string(),
                            harn_vm::VmValue::String(Rc::from("User interaction.")),
                        ),
                        (
                            "ops".to_string(),
                            harn_vm::VmValue::List(Rc::new(vec![harn_vm::VmValue::String(
                                Rc::from("ask"),
                            )])),
                        ),
                    ]))),
                ),
            ]),
        )))
    });

    vm.register_builtin("host_has", |args, _out| {
        let capability = args.first().map(|a| a.display()).unwrap_or_default();
        let op = args.get(1).map(|a| a.display());
        let valid = matches!(
            (capability.as_str(), op.as_deref()),
            ("workspace", None)
                | (
                    "workspace",
                    Some(
                        "read_text"
                            | "write_text"
                            | "apply_edit"
                            | "delete"
                            | "exists"
                            | "file_exists"
                            | "list"
                            | "project_root"
                    ),
                )
                | ("process", None)
                | ("process", Some("exec"))
                | ("template", None)
                | ("template", Some("render"))
                | ("interaction", None)
                | ("interaction", Some("ask"))
        );
        Ok(harn_vm::VmValue::Bool(valid))
    });

    let b = bridge.clone();
    vm.register_async_builtin("host_invoke", move |args| {
        let bridge = b.clone();
        async move {
            let capability = args.first().map(|a| a.display()).unwrap_or_default();
            let operation = args.get(1).map(|a| a.display()).unwrap_or_default();
            let params = args.get(2).cloned().unwrap_or(harn_vm::VmValue::Nil);
            let params_json = harn_vm::llm::vm_value_to_json(&params);

            match (capability.as_str(), operation.as_str()) {
                ("workspace", "read_text") => {
                    let path = params_json
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    let result = bridge
                        .call_client(
                            "fs/read_text_file",
                            serde_json::json!({
                                "sessionId": bridge.session_id,
                                "path": path,
                            }),
                        )
                        .await?;
                    if let Some(content) = result.get("content").and_then(|v| v.as_str()) {
                        Ok(harn_vm::VmValue::String(Rc::from(content)))
                    } else {
                        Ok(harn_vm::bridge::json_result_to_vm_value(&result))
                    }
                }
                ("workspace", "write_text") => {
                    let path = params_json
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    let content = params_json
                        .get("content")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    bridge
                        .call_client(
                            "fs/write_text_file",
                            serde_json::json!({
                                "sessionId": bridge.session_id,
                                "path": path,
                                "content": content,
                            }),
                        )
                        .await?;
                    Ok(harn_vm::VmValue::Nil)
                }
                ("workspace", "apply_edit") => {
                    let path = params_json
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    let old_string = params_json
                        .get("old_string")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    let new_string = params_json
                        .get("new_string")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    bridge
                        .call_client(
                            "fs/apply_edit",
                            serde_json::json!({
                                "sessionId": bridge.session_id,
                                "path": path,
                                "old_string": old_string,
                                "new_string": new_string,
                            }),
                        )
                        .await?;
                    Ok(harn_vm::VmValue::Nil)
                }
                ("workspace", "delete") => {
                    let path = params_json
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    bridge
                        .call_client(
                            "fs/delete_file",
                            serde_json::json!({
                                "sessionId": bridge.session_id,
                                "path": path,
                            }),
                        )
                        .await?;
                    Ok(harn_vm::VmValue::Nil)
                }
                ("workspace", "exists") => {
                    let path = params_json
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    let result = bridge
                        .call_client(
                            "fs/read_text_file",
                            serde_json::json!({
                                "sessionId": bridge.session_id,
                                "path": path,
                            }),
                        )
                        .await;
                    Ok(harn_vm::VmValue::Bool(result.is_ok()))
                }
                ("workspace", "list") => {
                    let path = params_json
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or(".");
                    let result = bridge
                        .call_client(
                            "host/call",
                            serde_json::json!({
                                "sessionId": bridge.session_id,
                                "name": "list_directory",
                                "args": {"path": path},
                            }),
                        )
                        .await?;
                    Ok(harn_vm::bridge::json_result_to_vm_value(&result))
                }
                ("process", "exec") => {
                    acp_terminal_exec(
                        &bridge,
                        &[harn_vm::VmValue::String(Rc::from(
                            params_json
                                .get("command")
                                .and_then(|v| v.as_str())
                                .unwrap_or_default(),
                        ))],
                    )
                    .await
                }
                ("template", "render") => {
                    let template = params_json
                        .get("path")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    let bindings = params_json
                        .get("bindings")
                        .cloned()
                        .unwrap_or(serde_json::Value::Null);
                    let result = bridge
                        .call_client(
                            "host/call",
                            serde_json::json!({
                                "sessionId": bridge.session_id,
                                "name": "render",
                                "args": {"template": template, "bindings": bindings},
                            }),
                        )
                        .await?;
                    Ok(harn_vm::bridge::json_result_to_vm_value(&result))
                }
                ("interaction", "ask") => {
                    let question = params_json
                        .get("question")
                        .and_then(|v| v.as_str())
                        .unwrap_or_default();
                    let result = bridge
                        .call_client(
                            "host/call",
                            serde_json::json!({
                                "sessionId": bridge.session_id,
                                "name": "ask_user",
                                "args": {"question": question},
                            }),
                        )
                        .await?;
                    Ok(harn_vm::bridge::json_result_to_vm_value(&result))
                }
                _ => {
                    let result = bridge
                        .call_client(
                            "host/call",
                            serde_json::json!({
                                "sessionId": bridge.session_id,
                                "name": "host_invoke",
                                "args": {
                                    "capability": capability,
                                    "operation": operation,
                                    "params": params_json,
                                },
                            }),
                        )
                        .await?;
                    Ok(harn_vm::bridge::json_result_to_vm_value(&result))
                }
            }
        }
    });

    // --- Render — template rendering delegated to host ---

    let b = bridge.clone();
    vm.register_async_builtin("render", move |args| {
        let bridge = b.clone();
        async move {
            let template = args.first().map(|a| a.display()).unwrap_or_default();
            let bindings = args.get(1).cloned().unwrap_or(harn_vm::VmValue::Nil);
            let bindings_json = harn_vm::llm::vm_value_to_json(&bindings);
            let result = bridge
                .call_client(
                    "host/call",
                    serde_json::json!({
                        "sessionId": bridge.session_id,
                        "name": "render",
                        "args": {"template": template, "bindings": bindings_json},
                    }),
                )
                .await?;
            Ok(harn_vm::bridge::json_result_to_vm_value(&result))
        }
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

    // 2. Wait for the command to finish.
    let _ = bridge
        .call_client(
            "terminal/wait_for_exit",
            serde_json::json!({
                "sessionId": bridge.session_id,
                "terminalId": terminal_id,
            }),
        )
        .await;

    // 3. Read the output.
    let output_result = bridge
        .call_client(
            "terminal/output",
            serde_json::json!({
                "sessionId": bridge.session_id,
                "terminalId": terminal_id,
            }),
        )
        .await
        .unwrap_or(serde_json::json!({}));

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
