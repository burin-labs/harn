use std::collections::HashMap;
use std::path::Path;
use std::sync::{Arc, Mutex};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;
use uuid::Uuid;

/// The supported A2A protocol version.
const SUPPORTED_A2A_VERSION: &str = "1.0.0";

// ---------------------------------------------------------------------------
// Task state tracking
// ---------------------------------------------------------------------------

/// The lifecycle states of an A2A task, aligned with the A2A protocol spec v1.0.
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

/// A part of a message (text, file, data, etc.).
/// For now we store parts as opaque JSON values.
type MessagePart = serde_json::Value;

/// An A2A message in the task history (v1.0: includes unique id).
#[derive(Clone, Debug)]
struct TaskMessage {
    id: String,
    role: String,
    parts: Vec<MessagePart>,
}

/// An artifact produced by the agent during task execution.
#[derive(Clone, Debug)]
struct Artifact {
    id: String,
    title: Option<String>,
    description: Option<String>,
    mime_type: Option<String>,
    parts: Vec<MessagePart>,
}

impl Artifact {
    fn to_json(&self) -> serde_json::Value {
        let mut obj = serde_json::json!({
            "id": self.id,
            "parts": self.parts,
        });
        if let Some(ref t) = self.title {
            obj["title"] = serde_json::Value::String(t.clone());
        }
        if let Some(ref d) = self.description {
            obj["description"] = serde_json::Value::String(d.clone());
        }
        if let Some(ref m) = self.mime_type {
            obj["mimeType"] = serde_json::Value::String(m.clone());
        }
        obj
    }
}

/// Persistent state for a single A2A task.
#[derive(Clone, Debug)]
struct TaskState {
    id: String,
    context_id: Option<String>,
    status: TaskStatus,
    history: Vec<TaskMessage>,
    artifacts: Vec<Artifact>,
}

impl TaskState {
    fn to_json(&self) -> serde_json::Value {
        let history: Vec<serde_json::Value> = self
            .history
            .iter()
            .map(|m| {
                serde_json::json!({
                    "id": m.id,
                    "role": m.role,
                    "parts": m.parts,
                })
            })
            .collect();
        let artifacts: Vec<serde_json::Value> =
            self.artifacts.iter().map(|a| a.to_json()).collect();
        let mut obj = serde_json::json!({
            "id": self.id,
            "status": {"state": self.status.as_str()},
            "history": history,
            "artifacts": artifacts,
        });
        if let Some(ref cid) = self.context_id {
            obj["contextId"] = serde_json::Value::String(cid.clone());
        }
        obj
    }

    /// A summary representation for list operations.
    fn to_summary_json(&self) -> serde_json::Value {
        let mut obj = serde_json::json!({
            "id": self.id,
            "status": {"state": self.status.as_str()},
        });
        if let Some(ref cid) = self.context_id {
            obj["contextId"] = serde_json::Value::String(cid.clone());
        }
        obj
    }
}

/// Thread-safe map of task-id -> TaskState.
type TaskStore = Arc<Mutex<HashMap<String, TaskState>>>;

/// Generate the Agent Card JSON for a pipeline file (v1.0 schema).
fn agent_card(pipeline_name: &str, port: u16) -> serde_json::Value {
    serde_json::json!({
        "id": pipeline_name,
        "name": pipeline_name,
        "description": "Harn pipeline agent",
        "url": format!("http://localhost:{port}"),
        "version": env!("CARGO_PKG_VERSION"),
        "provider": {
            "organization": "Harn",
            "url": "https://harn.dev"
        },
        "interfaces": [
            {"protocol": "jsonrpc", "url": "/"}
        ],
        "securitySchemes": [],
        "capabilities": {
            "streaming": true,
            "pushNotifications": false,
            "extendedAgentCard": false
        },
        "skills": [
            {
                "id": "execute",
                "name": "Execute Pipeline",
                "description": "Run the harn pipeline with a task"
            }
        ]
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

    let chunk = harn_vm::compile_source(&source)?;

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
fn create_task(store: &TaskStore, task_text: &str, context_id: Option<String>) -> String {
    let task_id = Uuid::now_v7().to_string();
    let message_id = Uuid::now_v7().to_string();
    let task = TaskState {
        id: task_id.clone(),
        context_id,
        status: TaskStatus::Submitted,
        history: vec![TaskMessage {
            id: message_id,
            role: "user".to_string(),
            parts: vec![serde_json::json!({"type": "text", "text": task_text})],
        }],
        artifacts: Vec::new(),
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
        let message_id = Uuid::now_v7().to_string();
        task.history.push(TaskMessage {
            id: message_id,
            role: "agent".to_string(),
            parts: vec![serde_json::json!({"type": "text", "text": output.trim_end()})],
        });
    }
}

/// Mark a task as failed with an error message.
fn fail_task(store: &TaskStore, task_id: &str, error: &str) {
    if let Some(task) = store.lock().unwrap().get_mut(task_id) {
        task.status = TaskStatus::Failed;
        let message_id = Uuid::now_v7().to_string();
        task.history.push(TaskMessage {
            id: message_id,
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
    harn_vm::jsonrpc::response(rpc_id.clone(), task_json)
}

// A2A-standard JSON-RPC error codes (aligned with A2A protocol spec v1.0)
const A2A_TASK_NOT_FOUND: i64 = -32001;
const A2A_TASK_NOT_CANCELABLE: i64 = -32002;
const A2A_UNSUPPORTED_OPERATION: i64 = -32003;
#[allow(dead_code)]
const A2A_INVALID_PARAMS: i64 = -32602;
#[allow(dead_code)]
const A2A_INTERNAL_ERROR: i64 = -32603;
const A2A_VERSION_NOT_SUPPORTED: i64 = -32009;

/// Build a JSON-RPC error response.
fn error_response(rpc_id: &serde_json::Value, code: i64, message: &str) -> serde_json::Value {
    harn_vm::jsonrpc::error_response(rpc_id.clone(), code, message)
}

/// Parsed HTTP request including headers.
struct ParsedRequest {
    method: String,
    path: String,
    headers: HashMap<String, String>,
    body: String,
}

/// Parse an HTTP request from raw bytes. Returns a `ParsedRequest`.
fn parse_http_request(raw: &[u8]) -> Option<ParsedRequest> {
    let text = String::from_utf8_lossy(raw);

    // Split headers from body
    let (header_section, body) = if let Some(pos) = text.find("\r\n\r\n") {
        (&text[..pos], text[pos + 4..].to_string())
    } else if let Some(pos) = text.find("\n\n") {
        (&text[..pos], text[pos + 2..].to_string())
    } else {
        return None;
    };

    let mut lines = header_section.lines();
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let path = parts.next()?.to_string();

    let mut headers = HashMap::new();
    for line in lines {
        if let Some((key, value)) = line.split_once(':') {
            headers.insert(key.trim().to_lowercase(), value.trim().to_string());
        }
    }

    Some(ParsedRequest {
        method,
        path,
        headers,
        body,
    })
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
         Access-Control-Allow-Headers: Content-Type, A2A-Version\r\n\
         \r\n",
        body.len()
    );
    stream.write_all(header.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await?;
    Ok(())
}

/// Write an SSE (Server-Sent Events) HTTP response header.
async fn write_sse_header(stream: &mut (impl AsyncWriteExt + Unpin)) -> tokio::io::Result<()> {
    let header = "HTTP/1.1 200 OK\r\n\
         Content-Type: text/event-stream\r\n\
         Cache-Control: no-cache\r\n\
         Connection: keep-alive\r\n\
         Access-Control-Allow-Origin: *\r\n\
         Access-Control-Allow-Methods: GET, POST, OPTIONS\r\n\
         Access-Control-Allow-Headers: Content-Type, A2A-Version\r\n\
         \r\n";
    stream.write_all(header.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

/// Write a single SSE event.
async fn write_sse_event(
    stream: &mut (impl AsyncWriteExt + Unpin),
    event_type: &str,
    data: &serde_json::Value,
) -> tokio::io::Result<()> {
    let json_str = serde_json::to_string(data).unwrap_or_default();
    let event = format!("event: {event_type}\ndata: {json_str}\n\n");
    stream.write_all(event.as_bytes()).await?;
    stream.flush().await?;
    Ok(())
}

/// Check the A2A-Version header. Returns an error response if the version is
/// present but not supported.
fn check_version_header(
    headers: &HashMap<String, String>,
    rpc_id: &serde_json::Value,
) -> Option<serde_json::Value> {
    if let Some(version) = headers.get("a2a-version") {
        if version != SUPPORTED_A2A_VERSION {
            return Some(error_response(
                rpc_id,
                A2A_VERSION_NOT_SUPPORTED,
                &format!(
                    "VersionNotSupportedError: requested version {version}, supported: {SUPPORTED_A2A_VERSION}"
                ),
            ));
        }
    }
    None
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

    let req = match parse_http_request(&buf) {
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

    match (req.method.as_str(), req.path.as_str()) {
        // CORS preflight
        ("OPTIONS", _) => {
            let _ = write_http_response(&mut stream, 204, "No Content", "text/plain", b"").await;
        }
        // Agent Card endpoint (v1.0 path)
        ("GET", "/.well-known/a2a-agent") => {
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
            // Check A2A-Version header before processing
            let rpc_id = serde_json::from_str::<serde_json::Value>(&req.body)
                .ok()
                .and_then(|v| v.get("id").cloned())
                .unwrap_or(serde_json::Value::Null);

            if let Some(version_err) = check_version_header(&req.headers, &rpc_id) {
                let resp_bytes = serde_json::to_string(&version_err).unwrap_or_default();
                let _ = write_http_response(
                    &mut stream,
                    200,
                    "OK",
                    "application/json",
                    resp_bytes.as_bytes(),
                )
                .await;
                return;
            }

            // Check if this is a streaming request
            let parsed: Option<serde_json::Value> = serde_json::from_str(&req.body).ok();
            let method = parsed
                .as_ref()
                .and_then(|v| v.get("method"))
                .and_then(|m| m.as_str())
                .unwrap_or("");

            if method == "a2a.SendStreamingMessage" {
                handle_streaming_request(&mut stream, pipeline_path, &req.body, store).await;
            } else {
                let resp = handle_jsonrpc(pipeline_path, &req.body, store).await;
                let resp_bytes = resp.as_bytes();
                let _ = write_http_response(&mut stream, 200, "OK", "application/json", resp_bytes)
                    .await;
            }
        }
        // REST-style task listing: GET /tasks
        ("GET", "/tasks") => {
            let tasks = list_tasks(store, None, None);
            let body_bytes = serde_json::to_string(&tasks).unwrap_or_default();
            let _ = write_http_response(
                &mut stream,
                200,
                "OK",
                "application/json",
                body_bytes.as_bytes(),
            )
            .await;
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

/// List tasks with optional cursor-based pagination.
/// `cursor` is an optional task id to start after.
/// `limit` is the maximum number of tasks to return (default 50).
fn list_tasks(store: &TaskStore, cursor: Option<&str>, limit: Option<usize>) -> serde_json::Value {
    let map = store.lock().unwrap();
    let limit = limit.unwrap_or(50);

    // Sort tasks by id for stable pagination ordering
    let mut task_ids: Vec<&String> = map.keys().collect();
    task_ids.sort();

    // If cursor is provided, skip to after it
    let start_idx = if let Some(cursor_id) = cursor {
        task_ids
            .iter()
            .position(|id| id.as_str() == cursor_id)
            .map(|pos| pos + 1)
            .unwrap_or(0)
    } else {
        0
    };

    let tasks: Vec<serde_json::Value> = task_ids
        .iter()
        .skip(start_idx)
        .take(limit)
        .filter_map(|id| map.get(id.as_str()).map(|t| t.to_summary_json()))
        .collect();

    let next_cursor = if start_idx + limit < task_ids.len() {
        task_ids.get(start_idx + limit - 1).map(|id| id.as_str())
    } else {
        None
    };

    let mut result = serde_json::json!({
        "tasks": tasks,
    });
    if let Some(nc) = next_cursor {
        result["nextCursor"] = serde_json::Value::String(nc.to_string());
    }
    result
}

/// Extract message text and context_id from a JSON-RPC params object.
fn extract_message_params(parsed: &serde_json::Value) -> (String, Option<String>) {
    let task_text = parsed
        .pointer("/params/message/parts")
        .and_then(|parts| parts.as_array())
        .and_then(|arr| {
            arr.iter().find_map(|p| {
                if p.get("type").and_then(|t| t.as_str()) == Some("text") {
                    p.get("text")
                        .and_then(|t| t.as_str())
                        .map(|s| s.to_string())
                } else {
                    None
                }
            })
        })
        .unwrap_or_default();

    let context_id = parsed
        .pointer("/params/contextId")
        .and_then(|v| v.as_str())
        .map(|s| s.to_string());

    (task_text, context_id)
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
        "a2a.SendMessage" => {
            let (task_text, context_id) = extract_message_params(&parsed);

            if task_text.is_empty() {
                error_response(
                    &rpc_id,
                    -32602,
                    "Invalid params: no text part found in message",
                )
            } else {
                // Create task and track its lifecycle
                let task_id = create_task(store, &task_text, context_id);
                mark_task_working(store, &task_id);

                // Check if cancelled before we even start execution
                if is_task_cancelled(store, &task_id) {
                    let task_json = store.lock().unwrap().get(&task_id).unwrap().to_json();
                    task_rpc_response(&rpc_id, task_json)
                } else {
                    match execute_pipeline(pipeline_path, &task_text).await {
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
        "a2a.GetTask" => {
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
        "a2a.CancelTask" => {
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
        "a2a.ListTasks" => {
            let cursor = parsed.pointer("/params/cursor").and_then(|v| v.as_str());
            let limit = parsed
                .pointer("/params/limit")
                .and_then(|v| v.as_u64())
                .map(|v| v as usize);
            let result = list_tasks(store, cursor, limit);
            task_rpc_response(&rpc_id, result)
        }
        _ => error_response(
            &rpc_id,
            A2A_UNSUPPORTED_OPERATION,
            &format!("UnsupportedOperationError: {method}"),
        ),
    };

    serde_json::to_string(&resp).unwrap_or_default()
}

/// Handle a streaming JSON-RPC request (a2a.SendStreamingMessage).
/// Sends SSE events for task status updates and the final message.
async fn handle_streaming_request(
    stream: &mut (impl AsyncWriteExt + AsyncReadExt + Unpin),
    pipeline_path: &str,
    body: &str,
    store: &TaskStore,
) {
    let parsed: serde_json::Value = match serde_json::from_str(body) {
        Ok(v) => v,
        Err(e) => {
            let resp = error_response(
                &serde_json::Value::Null,
                -32700,
                &format!("Parse error: {e}"),
            );
            let resp_bytes = serde_json::to_string(&resp).unwrap_or_default();
            let _ =
                write_http_response(stream, 200, "OK", "application/json", resp_bytes.as_bytes())
                    .await;
            return;
        }
    };

    let rpc_id = parsed.get("id").cloned().unwrap_or(serde_json::Value::Null);
    let (task_text, context_id) = extract_message_params(&parsed);

    if task_text.is_empty() {
        let resp = error_response(
            &rpc_id,
            -32602,
            "Invalid params: no text part found in message",
        );
        let resp_bytes = serde_json::to_string(&resp).unwrap_or_default();
        let _ =
            write_http_response(stream, 200, "OK", "application/json", resp_bytes.as_bytes()).await;
        return;
    }

    // Create task
    let task_id = create_task(store, &task_text, context_id);

    // Start SSE stream
    if write_sse_header(stream).await.is_err() {
        return;
    }

    // Send submitted status event
    let submitted_event = serde_json::json!({
        "jsonrpc": "2.0",
        "id": rpc_id,
        "result": {
            "type": "status",
            "taskId": task_id,
            "status": {"state": "submitted"}
        }
    });
    if write_sse_event(stream, "message", &submitted_event)
        .await
        .is_err()
    {
        return;
    }

    // Transition to working
    mark_task_working(store, &task_id);
    let working_event = serde_json::json!({
        "jsonrpc": "2.0",
        "id": rpc_id,
        "result": {
            "type": "status",
            "taskId": task_id,
            "status": {"state": "working"}
        }
    });
    if write_sse_event(stream, "message", &working_event)
        .await
        .is_err()
    {
        return;
    }

    // Execute pipeline
    match execute_pipeline(pipeline_path, &task_text).await {
        Ok(output) => {
            if is_task_cancelled(store, &task_id) {
                let cancelled_event = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": rpc_id,
                    "result": {
                        "type": "status",
                        "taskId": task_id,
                        "status": {"state": "cancelled"}
                    }
                });
                let _ = write_sse_event(stream, "message", &cancelled_event).await;
            } else {
                // Send the agent message
                let message_id = Uuid::now_v7().to_string();
                let message_event = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": rpc_id,
                    "result": {
                        "type": "message",
                        "taskId": task_id,
                        "message": {
                            "id": message_id,
                            "role": "agent",
                            "parts": [{"type": "text", "text": output.trim_end()}]
                        }
                    }
                });
                let _ = write_sse_event(stream, "message", &message_event).await;

                complete_task(store, &task_id, &output);

                // Send completed status
                let completed_event = serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": rpc_id,
                    "result": {
                        "type": "status",
                        "taskId": task_id,
                        "status": {"state": "completed"}
                    }
                });
                let _ = write_sse_event(stream, "message", &completed_event).await;
            }
        }
        Err(e) => {
            fail_task(store, &task_id, &e);
            let failed_event = serde_json::json!({
                "jsonrpc": "2.0",
                "id": rpc_id,
                "result": {
                    "type": "status",
                    "taskId": task_id,
                    "status": {"state": "failed"},
                    "error": e
                }
            });
            let _ = write_sse_event(stream, "message", &failed_event).await;
        }
    }
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
    println!("Agent card: http://localhost:{port}/.well-known/a2a-agent");
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_agent_card_v1_fields() {
        let card = agent_card("test-pipeline", 8080);
        assert_eq!(card["id"], "test-pipeline");
        assert_eq!(card["name"], "test-pipeline");
        assert!(card["provider"]["organization"].is_string());
        assert!(card["provider"]["url"].is_string());
        assert!(card["interfaces"].is_array());
        assert_eq!(card["interfaces"][0]["protocol"], "jsonrpc");
        assert!(card["securitySchemes"].is_array());
        assert_eq!(card["securitySchemes"].as_array().unwrap().len(), 0);
        assert_eq!(card["capabilities"]["streaming"], true);
        assert_eq!(card["capabilities"]["pushNotifications"], false);
        assert_eq!(card["capabilities"]["extendedAgentCard"], false);
        // v0.3 fields should not be present
        assert!(card.get("defaultInputModes").is_none());
        assert!(card.get("defaultOutputModes").is_none());
    }

    #[test]
    fn test_agent_card_url() {
        let card = agent_card("my-agent", 3000);
        assert_eq!(card["url"], "http://localhost:3000");
    }

    #[test]
    fn test_task_status_str() {
        assert_eq!(TaskStatus::Submitted.as_str(), "submitted");
        assert_eq!(TaskStatus::Working.as_str(), "working");
        assert_eq!(TaskStatus::Completed.as_str(), "completed");
        assert_eq!(TaskStatus::Failed.as_str(), "failed");
        assert_eq!(TaskStatus::Cancelled.as_str(), "cancelled");
        assert_eq!(TaskStatus::Rejected.as_str(), "rejected");
        assert_eq!(TaskStatus::InputRequired.as_str(), "input-required");
        assert_eq!(TaskStatus::AuthRequired.as_str(), "auth-required");
    }

    #[test]
    fn test_task_status_terminal() {
        assert!(!TaskStatus::Submitted.is_terminal());
        assert!(!TaskStatus::Working.is_terminal());
        assert!(TaskStatus::Completed.is_terminal());
        assert!(TaskStatus::Failed.is_terminal());
        assert!(TaskStatus::Cancelled.is_terminal());
        assert!(TaskStatus::Rejected.is_terminal());
        assert!(!TaskStatus::InputRequired.is_terminal());
    }

    #[test]
    fn test_create_task_generates_uuid() {
        let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
        let id = create_task(&store, "hello", None);
        // UUID v7 format: 8-4-4-4-12 hex chars
        assert_eq!(id.len(), 36);
        assert!(id.contains('-'));
        // Verify it's in the store
        let map = store.lock().unwrap();
        let task = map.get(&id).unwrap();
        assert_eq!(task.status, TaskStatus::Submitted);
        assert_eq!(task.history.len(), 1);
        assert_eq!(task.history[0].role, "user");
        // Message should have an id too
        assert_eq!(task.history[0].id.len(), 36);
        assert!(task.context_id.is_none());
    }

    #[test]
    fn test_create_task_with_context_id() {
        let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
        let id = create_task(&store, "hello", Some("ctx-123".to_string()));
        let map = store.lock().unwrap();
        let task = map.get(&id).unwrap();
        assert_eq!(task.context_id, Some("ctx-123".to_string()));
    }

    #[test]
    fn test_task_to_json_includes_context_id() {
        let task = TaskState {
            id: "task-1".to_string(),
            context_id: Some("ctx-abc".to_string()),
            status: TaskStatus::Submitted,
            history: vec![],
            artifacts: vec![],
        };
        let json = task.to_json();
        assert_eq!(json["contextId"], "ctx-abc");
    }

    #[test]
    fn test_task_to_json_without_context_id() {
        let task = TaskState {
            id: "task-1".to_string(),
            context_id: None,
            status: TaskStatus::Submitted,
            history: vec![],
            artifacts: vec![],
        };
        let json = task.to_json();
        assert!(json.get("contextId").is_none());
    }

    #[test]
    fn test_task_message_includes_id() {
        let task = TaskState {
            id: "task-1".to_string(),
            context_id: None,
            status: TaskStatus::Completed,
            history: vec![TaskMessage {
                id: "msg-abc".to_string(),
                role: "user".to_string(),
                parts: vec![serde_json::json!({"type": "text", "text": "hi"})],
            }],
            artifacts: vec![],
        };
        let json = task.to_json();
        assert_eq!(json["history"][0]["id"], "msg-abc");
        assert_eq!(json["history"][0]["role"], "user");
    }

    #[test]
    fn test_artifact_to_json() {
        let artifact = Artifact {
            id: "art-1".to_string(),
            title: Some("Output".to_string()),
            description: Some("Pipeline output".to_string()),
            mime_type: Some("text/plain".to_string()),
            parts: vec![serde_json::json!({"type": "text", "text": "hello"})],
        };
        let json = artifact.to_json();
        assert_eq!(json["id"], "art-1");
        assert_eq!(json["title"], "Output");
        assert_eq!(json["description"], "Pipeline output");
        assert_eq!(json["mimeType"], "text/plain");
        assert_eq!(json["parts"][0]["type"], "text");
    }

    #[test]
    fn test_artifact_to_json_minimal() {
        let artifact = Artifact {
            id: "art-2".to_string(),
            title: None,
            description: None,
            mime_type: None,
            parts: vec![],
        };
        let json = artifact.to_json();
        assert_eq!(json["id"], "art-2");
        assert!(json.get("title").is_none());
        assert!(json.get("description").is_none());
        assert!(json.get("mimeType").is_none());
    }

    #[test]
    fn test_task_to_json_includes_artifacts() {
        let task = TaskState {
            id: "task-1".to_string(),
            context_id: None,
            status: TaskStatus::Completed,
            history: vec![],
            artifacts: vec![Artifact {
                id: "art-1".to_string(),
                title: Some("Result".to_string()),
                description: None,
                mime_type: None,
                parts: vec![serde_json::json!({"type": "text", "text": "output"})],
            }],
        };
        let json = task.to_json();
        assert_eq!(json["artifacts"][0]["id"], "art-1");
        assert_eq!(json["artifacts"][0]["title"], "Result");
    }

    #[test]
    fn test_cancel_task_success() {
        let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
        let id = create_task(&store, "hello", None);
        mark_task_working(&store, &id);
        let result = cancel_task(&store, &id);
        assert!(result.is_ok());
        let json = result.unwrap();
        assert_eq!(json["status"]["state"], "cancelled");
    }

    #[test]
    fn test_cancel_task_terminal_fails() {
        let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
        let id = create_task(&store, "hello", None);
        complete_task(&store, &id, "done");
        let result = cancel_task(&store, &id);
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("TaskNotCancelableError"));
    }

    #[test]
    fn test_cancel_task_not_found() {
        let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
        let result = cancel_task(&store, "nonexistent");
        assert!(result.is_err());
        assert!(result.unwrap_err().contains("TaskNotFoundError"));
    }

    #[test]
    fn test_list_tasks_empty() {
        let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
        let result = list_tasks(&store, None, None);
        assert_eq!(result["tasks"].as_array().unwrap().len(), 0);
        assert!(result.get("nextCursor").is_none());
    }

    #[test]
    fn test_list_tasks_returns_summaries() {
        let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
        create_task(&store, "task1", Some("ctx-1".to_string()));
        create_task(&store, "task2", None);
        let result = list_tasks(&store, None, None);
        let tasks = result["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 2);
        // Summaries should have id and status but not full history
        for t in tasks {
            assert!(t.get("id").is_some());
            assert!(t.get("status").is_some());
            assert!(t.get("history").is_none());
        }
    }

    #[test]
    fn test_list_tasks_pagination() {
        let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
        let mut ids = Vec::new();
        for i in 0..5 {
            ids.push(create_task(&store, &format!("task{i}"), None));
        }
        // Get first 2
        let result = list_tasks(&store, None, Some(2));
        let tasks = result["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 2);
        // Should have a nextCursor
        assert!(result.get("nextCursor").is_some());
    }

    #[test]
    fn test_task_summary_json() {
        let task = TaskState {
            id: "task-1".to_string(),
            context_id: Some("ctx-abc".to_string()),
            status: TaskStatus::Working,
            history: vec![TaskMessage {
                id: "msg-1".to_string(),
                role: "user".to_string(),
                parts: vec![serde_json::json!({"type": "text", "text": "hello"})],
            }],
            artifacts: vec![],
        };
        let summary = task.to_summary_json();
        assert_eq!(summary["id"], "task-1");
        assert_eq!(summary["status"]["state"], "working");
        assert_eq!(summary["contextId"], "ctx-abc");
        // Summary should not include history
        assert!(summary.get("history").is_none());
    }

    #[test]
    fn test_check_version_header_ok_no_header() {
        let headers = HashMap::new();
        let rpc_id = serde_json::Value::Number(1.into());
        assert!(check_version_header(&headers, &rpc_id).is_none());
    }

    #[test]
    fn test_check_version_header_ok_matching() {
        let mut headers = HashMap::new();
        headers.insert("a2a-version".to_string(), "1.0.0".to_string());
        let rpc_id = serde_json::Value::Number(1.into());
        assert!(check_version_header(&headers, &rpc_id).is_none());
    }

    #[test]
    fn test_check_version_header_unsupported() {
        let mut headers = HashMap::new();
        headers.insert("a2a-version".to_string(), "0.3".to_string());
        let rpc_id = serde_json::Value::Number(1.into());
        let err = check_version_header(&headers, &rpc_id);
        assert!(err.is_some());
        let err = err.unwrap();
        assert_eq!(err["error"]["code"], -32009);
        assert!(err["error"]["message"]
            .as_str()
            .unwrap()
            .contains("VersionNotSupportedError"));
    }

    #[test]
    fn test_parse_http_request_with_headers() {
        let raw = b"POST / HTTP/1.1\r\nContent-Type: application/json\r\nA2A-Version: 1.0.0\r\n\r\n{\"test\":true}";
        let req = parse_http_request(raw).unwrap();
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/");
        assert_eq!(req.headers.get("a2a-version").unwrap(), "1.0.0");
        assert_eq!(req.headers.get("content-type").unwrap(), "application/json");
        assert_eq!(req.body, "{\"test\":true}");
    }

    #[test]
    fn test_error_response_format() {
        let resp = error_response(&serde_json::Value::Number(42.into()), -32009, "test error");
        assert_eq!(resp["jsonrpc"], "2.0");
        assert_eq!(resp["id"], 42);
        assert_eq!(resp["error"]["code"], -32009);
        assert_eq!(resp["error"]["message"], "test error");
    }

    #[test]
    fn test_pipeline_name_from_path() {
        assert_eq!(pipeline_name_from_path("examples/hello.harn"), "hello");
        assert_eq!(pipeline_name_from_path("agent.harn"), "agent");
        assert_eq!(
            pipeline_name_from_path("/path/to/my-pipeline.harn"),
            "my-pipeline"
        );
    }

    #[test]
    fn test_extract_message_params() {
        let parsed = serde_json::json!({
            "params": {
                "message": {
                    "parts": [{"type": "text", "text": "hello world"}]
                },
                "contextId": "ctx-123"
            }
        });
        let (text, ctx) = extract_message_params(&parsed);
        assert_eq!(text, "hello world");
        assert_eq!(ctx, Some("ctx-123".to_string()));
    }

    #[test]
    fn test_extract_message_params_no_context() {
        let parsed = serde_json::json!({
            "params": {
                "message": {
                    "parts": [{"type": "text", "text": "hello"}]
                }
            }
        });
        let (text, ctx) = extract_message_params(&parsed);
        assert_eq!(text, "hello");
        assert!(ctx.is_none());
    }

    #[tokio::test]
    async fn test_handle_jsonrpc_unsupported_method() {
        let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"old/method","params":{}}"#;
        let resp = handle_jsonrpc("/nonexistent.harn", body, &store).await;
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(parsed["error"]["code"], A2A_UNSUPPORTED_OPERATION);
        assert!(parsed["error"]["message"]
            .as_str()
            .unwrap()
            .contains("old/method"));
    }

    #[tokio::test]
    async fn test_handle_jsonrpc_old_method_names_rejected() {
        let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));

        // Old v0.3 method names should be rejected
        for method in &["message/send", "task/get", "task/cancel"] {
            let body = format!(
                r#"{{"jsonrpc":"2.0","id":1,"method":"{}","params":{{}}}}"#,
                method
            );
            let resp = handle_jsonrpc("/nonexistent.harn", &body, &store).await;
            let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
            assert_eq!(
                parsed["error"]["code"], A2A_UNSUPPORTED_OPERATION,
                "Old method {method} should be rejected"
            );
        }
    }

    #[tokio::test]
    async fn test_handle_jsonrpc_parse_error() {
        let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
        let resp = handle_jsonrpc("/nonexistent.harn", "not json", &store).await;
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(parsed["error"]["code"], -32700);
    }

    #[tokio::test]
    async fn test_handle_jsonrpc_get_task_not_found() {
        let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
        let body =
            r#"{"jsonrpc":"2.0","id":1,"method":"a2a.GetTask","params":{"id":"nonexistent"}}"#;
        let resp = handle_jsonrpc("/nonexistent.harn", body, &store).await;
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(parsed["error"]["code"], A2A_TASK_NOT_FOUND);
    }

    #[tokio::test]
    async fn test_handle_jsonrpc_list_tasks() {
        let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
        create_task(&store, "test1", None);
        create_task(&store, "test2", Some("ctx".to_string()));
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"a2a.ListTasks","params":{}}"#;
        let resp = handle_jsonrpc("/nonexistent.harn", body, &store).await;
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert!(parsed.get("error").is_none());
        let tasks = parsed["result"]["tasks"].as_array().unwrap();
        assert_eq!(tasks.len(), 2);
    }

    #[tokio::test]
    async fn test_handle_jsonrpc_send_message_empty() {
        let store: TaskStore = Arc::new(Mutex::new(HashMap::new()));
        let body = r#"{"jsonrpc":"2.0","id":1,"method":"a2a.SendMessage","params":{"message":{"parts":[]}}}"#;
        let resp = handle_jsonrpc("/nonexistent.harn", body, &store).await;
        let parsed: serde_json::Value = serde_json::from_str(&resp).unwrap();
        assert_eq!(parsed["error"]["code"], -32602);
    }
}
