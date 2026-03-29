use std::collections::HashMap;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use harn_lexer::Lexer;
use harn_parser::{DiagnosticSeverity, Parser, TypeChecker};

/// Global task ID counter.
static TASK_COUNTER: AtomicU64 = AtomicU64::new(1);

// ---------------------------------------------------------------------------
// Task state tracking
// ---------------------------------------------------------------------------

/// The lifecycle states of an A2A task, aligned with the A2A protocol spec v0.3.
/// See: https://a2a-protocol.org/latest/specification/
#[derive(Clone, Debug, PartialEq, Eq)]
#[allow(dead_code)]
enum TaskStatus {
    Submitted,
    Working,
    Completed,
    Failed,
    Cancelled,
    /// The remote agent rejected the task (e.g., unsupported capability).
    Rejected,
    /// The agent needs additional input from the caller to continue.
    InputRequired,
    /// The agent needs authentication/authorization from the caller.
    AuthRequired,
}

impl TaskStatus {
    fn as_str(&self) -> &'static str {
        match self {
            TaskStatus::Submitted => "submitted",
            TaskStatus::Working => "working",
            TaskStatus::Completed => "completed",
            TaskStatus::Failed => "failed",
            TaskStatus::Cancelled => "cancelled",
            TaskStatus::Rejected => "rejected",
            TaskStatus::InputRequired => "input-required",
            TaskStatus::AuthRequired => "auth-required",
        }
    }

    /// Terminal states: task processing is complete (no further updates).
    fn is_terminal(&self) -> bool {
        matches!(
            self,
            TaskStatus::Completed
                | TaskStatus::Failed
                | TaskStatus::Cancelled
                | TaskStatus::Rejected
        )
    }

    /// Interrupted states: task is paused, waiting for external input.
    #[allow(dead_code)]
    fn is_interrupted(&self) -> bool {
        matches!(self, TaskStatus::InputRequired | TaskStatus::AuthRequired)
    }
}

/// An A2A message in the task history.
#[derive(Clone, Debug)]
struct TaskMessage {
    role: String,
    parts: Vec<serde_json::Value>,
}

/// Persistent state for a single A2A task.
#[derive(Clone, Debug)]
struct TaskState {
    id: String,
    status: TaskStatus,
    history: Vec<TaskMessage>,
}

impl TaskState {
    fn to_json(&self) -> serde_json::Value {
        let history: Vec<serde_json::Value> = self
            .history
            .iter()
            .map(|m| {
                serde_json::json!({
                    "role": m.role,
                    "parts": m.parts,
                })
            })
            .collect();
        serde_json::json!({
            "id": self.id,
            "status": {"state": self.status.as_str()},
            "history": history,
        })
    }
}

/// Thread-safe map of task-id -> TaskState.
type TaskStore = Arc<Mutex<HashMap<String, TaskState>>>;

/// Generate the Agent Card JSON for a pipeline file.
fn agent_card(pipeline_name: &str, port: u16) -> serde_json::Value {
    serde_json::json!({
        "name": pipeline_name,
        "description": "Harn pipeline agent",
        "url": format!("http://localhost:{port}"),
        "version": env!("CARGO_PKG_VERSION"),
        "capabilities": {
            "streaming": false,
            "pushNotifications": false
        },
        "skills": [
            {
                "id": "execute",
                "name": "Execute Pipeline",
                "description": "Run the harn pipeline with a task"
            }
        ],
        "defaultInputModes": ["text/plain"],
        "defaultOutputModes": ["text/plain"]
    })
}

/// Extract the pipeline name from a .harn file path (stem without extension).
fn pipeline_name_from_path(path: &str) -> String {
    Path::new(path)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or("default")
        .to_string()
}

/// Compile and execute a pipeline with the given task text, returning the
/// pipeline's printed output.
async fn execute_pipeline(path: &str, task_text: &str) -> Result<String, String> {
    let source = std::fs::read_to_string(path).map_err(|e| format!("read error: {e}"))?;

    let mut lexer = Lexer::new(&source);
    let tokens = lexer.tokenize().map_err(|e| e.to_string())?;
    let mut parser = Parser::new(tokens);
    let program = parser.parse().map_err(|e| e.to_string())?;

    let type_diagnostics = TypeChecker::new().check(&program);
    for diag in &type_diagnostics {
        if diag.severity == DiagnosticSeverity::Error {
            return Err(diag.message.clone());
        }
    }

    let chunk = harn_vm::Compiler::new()
        .compile(&program)
        .map_err(|e| e.to_string())?;

    let local = tokio::task::LocalSet::new();
    let source_owned = source;
    let path_owned = path.to_string();
    let task_owned = task_text.to_string();

    local
        .run_until(async move {
            let mut vm = harn_vm::Vm::new();
            harn_vm::register_vm_stdlib(&mut vm);
            let source_parent = Path::new(&path_owned).parent().unwrap_or(Path::new("."));
            let project_root = harn_vm::stdlib::process::find_project_root(source_parent);
            let store_base = project_root.as_deref().unwrap_or(source_parent);
            harn_vm::register_store_builtins(&mut vm, store_base);
            harn_vm::register_metadata_builtins(&mut vm, store_base);
            if let Some(ref root) = project_root {
                vm.set_project_root(root);
            }
            vm.set_source_info(&path_owned, &source_owned);

            if let Some(p) = Path::new(&path_owned).parent() {
                if !p.as_os_str().is_empty() {
                    vm.set_source_dir(p);
                }
            }

            // Inject the task text as the pipeline parameter
            vm.set_global(
                "task",
                harn_vm::VmValue::String(std::rc::Rc::from(task_owned.as_str())),
            );

            vm.execute(&chunk).await.map_err(|e| e.to_string())?;
            Ok(vm.output().to_string())
        })
        .await
}

/// Create a new task in the store with `submitted` status, returning its id.
fn create_task(store: &TaskStore, task_text: &str) -> String {
    let task_id = format!("task-{}", TASK_COUNTER.fetch_add(1, Ordering::Relaxed));
    let task = TaskState {
        id: task_id.clone(),
        status: TaskStatus::Submitted,
        history: vec![TaskMessage {
            role: "user".to_string(),
            parts: vec![serde_json::json!({"type": "text", "text": task_text})],
        }],
    };
    store.lock().unwrap().insert(task_id.clone(), task);
    task_id
}

/// Transition a task to `working`.
fn mark_task_working(store: &TaskStore, task_id: &str) {
    if let Some(task) = store.lock().unwrap().get_mut(task_id) {
        task.status = TaskStatus::Working;
    }
}

/// Check whether a task has been cancelled (used to skip execution).
fn is_task_cancelled(store: &TaskStore, task_id: &str) -> bool {
    store
        .lock()
        .unwrap()
        .get(task_id)
        .is_some_and(|t| t.status == TaskStatus::Cancelled)
}

/// Complete a task with the agent's output.
fn complete_task(store: &TaskStore, task_id: &str, output: &str) {
    if let Some(task) = store.lock().unwrap().get_mut(task_id) {
        task.status = TaskStatus::Completed;
        task.history.push(TaskMessage {
            role: "agent".to_string(),
            parts: vec![serde_json::json!({"type": "text", "text": output.trim_end()})],
        });
    }
}

/// Mark a task as failed with an error message.
fn fail_task(store: &TaskStore, task_id: &str, error: &str) {
    if let Some(task) = store.lock().unwrap().get_mut(task_id) {
        task.status = TaskStatus::Failed;
        task.history.push(TaskMessage {
            role: "agent".to_string(),
            parts: vec![serde_json::json!({"type": "text", "text": error})],
        });
    }
}

/// Build a JSON-RPC success response wrapping a task's JSON representation.
fn task_rpc_response(
    rpc_id: &serde_json::Value,
    task_json: serde_json::Value,
) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": rpc_id,
        "result": task_json,
    })
}

// A2A-standard JSON-RPC error codes (aligned with A2A protocol spec v0.3)
const A2A_TASK_NOT_FOUND: i64 = -32001;
const A2A_TASK_NOT_CANCELABLE: i64 = -32002;
const A2A_UNSUPPORTED_OPERATION: i64 = -32003;
#[allow(dead_code)]
const A2A_INVALID_PARAMS: i64 = -32602;
#[allow(dead_code)]
const A2A_INTERNAL_ERROR: i64 = -32603;

/// Build a JSON-RPC error response.
fn error_response(rpc_id: &serde_json::Value, code: i64, message: &str) -> serde_json::Value {
    serde_json::json!({
        "jsonrpc": "2.0",
        "id": rpc_id,
        "error": {
            "code": code,
            "message": message
        }
    })
}

/// Parse an HTTP request from raw bytes. Returns (method, path, body).
fn parse_http_request(raw: &[u8]) -> Option<(String, String, String)> {
    let text = String::from_utf8_lossy(raw);

    // Split headers from body
    let (header_section, body) = if let Some(pos) = text.find("\r\n\r\n") {
        (&text[..pos], text[pos + 4..].to_string())
    } else if let Some(pos) = text.find("\n\n") {
        (&text[..pos], text[pos + 2..].to_string())
    } else {
        return None;
    };

    let request_line = header_section.lines().next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    Some((method, path, body))
}

/// Write an HTTP response with the given status, content-type, and body.
async fn write_http_response(
    stream: &mut (impl AsyncWriteExt + Unpin),
    status: u16,
    status_text: &str,
    content_type: &str,
    body: &[u8],
) -> tokio::io::Result<()> {
    let header = format!(
        "HTTP/1.1 {status} {status_text}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
         Access-Control-Allow-Headers: Content-Type\r\n\
         \r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await?;
    Ok(())
}

/// Handle a single HTTP connection.
async fn handle_connection(
    mut stream: tokio::net::TcpStream,
    pipeline_path: &str,
    card_json: &str,
    store: &TaskStore,
) {
    let mut buf = vec![0u8; 65536];
    let n = match stream.read(&mut buf).await {
        Ok(0) => return,
        Ok(n) => n,
        Err(_) => return,
    };
    buf.truncate(n);

    // If headers indicate a Content-Length larger than what we read, read more.
    let header_text = String::from_utf8_lossy(&buf);
    let content_length = header_text
        .lines()
        .find(|l| l.to_lowercase().starts_with("content-length:"))
        .and_then(|l| l.split(':').nth(1))
        .and_then(|v| v.trim().parse::<usize>().ok())
        .unwrap_or(0);

    // Find where the body starts
    let header_end = header_text
        .find("\r\n\r\n")
        .map(|p| p + 4)
        .or_else(|| header_text.find("\n\n").map(|p| p + 2))
        .unwrap_or(n);

    let body_so_far = n.saturating_sub(header_end);
    if body_so_far < content_length {
        let remaining = content_length - body_so_far;
        let mut extra = vec![0u8; remaining];
        let mut read_total = 0;
        while read_total < remaining {
            match stream.read(&mut extra[read_total..]).await {
                Ok(0) => break,
                Ok(nr) => read_total += nr,
                Err(_) => break,
            }
        }
        buf.extend_from_slice(&extra[..read_total]);
    }

    let (method, path, body) = match parse_http_request(&buf) {
        Some(parsed) => parsed,
        None => {
            let _ = write_http_response(
                &mut stream,
                400,
                "Bad Request",
                "text/plain",
                b"Bad Request",
            )
            .await;
            return;
        }
    };

    match (method.as_str(), path.as_str()) {
        // CORS preflight
        ("OPTIONS", _) => {
            let _ = write_http_response(&mut stream, 204, "No Content", "text/plain", b"").await;
        }
        // Agent Card endpoint
        ("GET", "/.well-known/agent.json") => {
            let _ = write_http_response(
                &mut stream,
                200,
                "OK",
                "application/json",
                card_json.as_bytes(),
            )
            .await;
        }
        // A2A JSON-RPC endpoint
        ("POST", "/") => {
            let resp = handle_jsonrpc(pipeline_path, &body, store).await;
            let resp_bytes = resp.as_bytes();
            let _ =
                write_http_response(&mut stream, 200, "OK", "application/json", resp_bytes).await;
        }
        // REST-style task retrieval: GET /tasks/:id
        ("GET", p) if p.starts_with("/tasks/") => {
            let task_id = &p["/tasks/".len()..];
            let task_json = store.lock().unwrap().get(task_id).map(|t| t.to_json());
            match task_json {
                Some(json) => {
                    let body_bytes = serde_json::to_string(&json).unwrap_or_default();
                    let _ = write_http_response(
                        &mut stream,
                        200,
                        "OK",
                        "application/json",
                        body_bytes.as_bytes(),
                    )
                    .await;
                }
                None => {
                    let _ = write_http_response(
                        &mut stream,
                        404,
                        "Not Found",
                        "application/json",
                        b"{\"error\":\"task not found\"}",
                    )
                    .await;
                }
            }
        }
        // REST-style task cancellation: POST /tasks/:id/cancel
        ("POST", p) if p.starts_with("/tasks/") && p.ends_with("/cancel") => {
            let task_id = &p["/tasks/".len()..p.len() - "/cancel".len()];
            let result = cancel_task(store, task_id);
            match result {
                Ok(json) => {
                    let body_bytes = serde_json::to_string(&json).unwrap_or_default();
                    let _ = write_http_response(
                        &mut stream,
                        200,
                        "OK",
                        "application/json",
                        body_bytes.as_bytes(),
                    )
                    .await;
                }
                Err(msg) => {
                    let status = if msg.contains("not found") { 404 } else { 409 };
                    let status_text = if status == 404 {
                        "Not Found"
                    } else {
                        "Conflict"
                    };
                    let err_body = serde_json::json!({"error": msg}).to_string();
                    let _ = write_http_response(
                        &mut stream,
                        status,
                        status_text,
                        "application/json",
                        err_body.as_bytes(),
                    )
                    .await;
                }
            }
        }
        _ => {
            let _ = write_http_response(&mut stream, 404, "Not Found", "text/plain", b"Not Found")
                .await;
        }
    }
}

/// Cancel a task by id. Returns the updated task JSON on success, or an error
/// message on failure.
fn cancel_task(store: &TaskStore, task_id: &str) -> Result<serde_json::Value, String> {
    let mut map = store.lock().unwrap();
    let task = map
        .get_mut(task_id)
        .ok_or_else(|| format!("TaskNotFoundError: {task_id}"))?;
    if task.status.is_terminal() {
        return Err(format!(
            "TaskNotCancelableError: task {} is in terminal state '{}'",
            task_id,
            task.status.as_str()
        ));
    }
    task.status = TaskStatus::Cancelled;
    Ok(task.to_json())
}

/// Handle a JSON-RPC request body, returning the JSON response string.
async fn handle_jsonrpc(pipeline_path: &str, body: &str, store: &TaskStore) -> String {
    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            let resp = error_response(
                &serde_json::Value::Null,
                -32700,
                &format!("Parse error: {e}"),
            );
            return serde_json::to_string(&resp).unwrap_or_default();
        }
    };

    let rpc_id = parsed.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let method = parsed.get("method").and_then(|m| m.as_str()).unwrap_or("");

    let resp = match method {
        "message/send" => {
            // Extract the task text from the message parts
            let task_text = parsed
                .pointer("/params/message/parts")
                .and_then(|parts| parts.as_array())
                .and_then(|arr| {
                    arr.iter().find_map(|p| {
                        if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                            p.get("text").and_then(|t| t.as_str())
                        } else {
                            None
                        }
                    })
                })
                .unwrap_or("");

            if task_text.is_empty() {
                error_response(
                    &rpc_id,
                    -32602,
                    "Invalid params: no text part found in message",
                )
            } else {
                // Create task and track its lifecycle
                let task_id = create_task(store, task_text);
                mark_task_working(store, &task_id);

                // Check if cancelled before we even start execution
                if is_task_cancelled(store, &task_id) {
                    let task_json = store.lock().unwrap().get(&task_id).unwrap().to_json();
                    task_rpc_response(&rpc_id, task_json)
                } else {
                    match execute_pipeline(pipeline_path, task_text).await {
                        Ok(output) => {
                            // Check cancellation after execution too
                            if is_task_cancelled(store, &task_id) {
                                let task_json =
                                    store.lock().unwrap().get(&task_id).unwrap().to_json();
                                task_rpc_response(&rpc_id, task_json)
                            } else {
                                complete_task(store, &task_id, &output);
                                let task_json =
                                    store.lock().unwrap().get(&task_id).unwrap().to_json();
                                task_rpc_response(&rpc_id, task_json)
                            }
                        }
                        Err(e) => {
                            fail_task(store, &task_id, &e);
                            error_response(&rpc_id, -32000, &format!("Pipeline error: {e}"))
                        }
                    }
                }
            }
        }
        "task/get" => {
            let task_id = parsed
                .pointer("/params/id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if task_id.is_empty() {
                error_response(&rpc_id, -32602, "Invalid params: missing task id")
            } else {
                let task_json = store.lock().unwrap().get(task_id).map(|t| t.to_json());
                match task_json {
                    Some(json) => task_rpc_response(&rpc_id, json),
                    None => error_response(
                        &rpc_id,
                        A2A_TASK_NOT_FOUND,
                        &format!("TaskNotFoundError: {task_id}"),
                    ),
                }
            }
        }
        "task/cancel" => {
            let task_id = parsed
                .pointer("/params/id")
                .and_then(|v| v.as_str())
                .unwrap_or("");
            if task_id.is_empty() {
                error_response(&rpc_id, -32602, "Invalid params: missing task id")
            } else {
                match cancel_task(store, task_id) {
                    Ok(json) => task_rpc_response(&rpc_id, json),
                    Err(msg) => error_response(&rpc_id, A2A_TASK_NOT_CANCELABLE, &msg),
                }
            }
        }
        _ => error_response(
            &rpc_id,
            A2A_UNSUPPORTED_OPERATION,
            &format!("UnsupportedOperationError: {method}"),
        ),
    };

    serde_json::to_string(&resp).unwrap_or_default()
}

/// Start the A2A server for a pipeline file.
pub async fn run_a2a_server(pipeline_path: &str, port: u16) {
    // Verify the pipeline file exists and is parseable before starting
    let path = Path::new(pipeline_path);
    if !path.exists() {
        eprintln!("Error: file not found: {pipeline_path}");
        std::process::exit(1);
    }

    let name = pipeline_name_from_path(pipeline_path);
    let card = agent_card(&name, port);
    let card_json = serde_json::to_string_pretty(&card).unwrap_or_default();

    let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));

    let addr = format!("0.0.0.0:{port}");
    let listener = match TcpListener::bind(&addr).await {
        Ok(l) => l,
        Err(e) => {
            eprintln!("Error: could not bind to {addr}: {e}");
            std::process::exit(1);
        }
    };

    println!("Harn A2A server listening on http://localhost:{port}");
    println!("Agent card: http://localhost:{port}/.well-known/agent.json");
    println!("Pipeline: {pipeline_path}");

    loop {
        match listener.accept().await {
            Ok((stream, _addr)) => {
                let pipeline = pipeline_path.to_string();
                let card = card_json.clone();
                // Each connection is handled inline (not spawned) because the
                // VM uses LocalSet and is !Send. For a simple A2A server this
                // is fine -- requests are handled sequentially.
                handle_connection(stream, &pipeline, &card, &store).await;
            }
            Err(e) => {
                eprintln!("Accept error: {e}");
            }
        }
    }
}
