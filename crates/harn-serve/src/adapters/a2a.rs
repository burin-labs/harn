use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use base64::Engine;
use futures::channel::mpsc::{unbounded, UnboundedReceiver, UnboundedSender};
use futures::StreamExt;
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{json, Value as JsonValue};
use sha2::Sha256;
use tokio::sync::{mpsc, oneshot};
use tokio::task::LocalSet;
use uuid::Uuid;

use crate::{
    AdapterDescriptor, AuthRequest, CallArguments, CallRequest, CallResponse, DispatchCore,
    DispatchError, ExportCatalog, TransportAdapter,
};

pub const A2A_PROTOCOL_VERSION: &str = "1.0.0";

const A2A_VERSION_HEADER: &str = "a2a-version";
const A2A_TRACE_HEADER: &str = "a2a-trace-id";

const A2A_TASK_NOT_FOUND: i64 = -32001;
const A2A_TASK_NOT_CANCELABLE: i64 = -32002;
const A2A_UNSUPPORTED_OPERATION: i64 = -32003;
const A2A_VERSION_NOT_SUPPORTED: i64 = -32009;

#[derive(Clone, Debug)]
pub struct A2aHttpServeOptions {
    pub bind: SocketAddr,
    pub public_url: Option<String>,
}

impl Default for A2aHttpServeOptions {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:8080".parse().expect("valid bind addr"),
            public_url: None,
        }
    }
}

pub struct A2aServerConfig {
    pub core: DispatchCore,
    pub agent_name: Option<String>,
    pub card_signing_secret: Option<String>,
}

impl A2aServerConfig {
    pub fn new(core: DispatchCore) -> Self {
        Self {
            agent_name: Some(derived_agent_name(core.catalog())),
            core,
            card_signing_secret: None,
        }
    }
}

pub struct A2aServer {
    descriptor: AdapterDescriptor,
    agent_name: String,
    card_signing_secret: Option<String>,
    catalog: ExportCatalog,
    executor: ExecutionRuntime,
    tasks: TaskStore,
}

#[derive(Clone)]
struct HttpState {
    server: Arc<A2aServer>,
    public_url: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
enum TaskStatus {
    Submitted,
    Working,
    Completed,
    Failed,
    Cancelled,
}

impl TaskStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Submitted => "submitted",
            Self::Working => "working",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(self, Self::Completed | Self::Failed | Self::Cancelled)
    }
}

#[derive(Clone, Debug)]
struct TaskMessage {
    id: String,
    role: String,
    parts: Vec<JsonValue>,
}

#[derive(Debug)]
struct TaskState {
    id: String,
    context_id: Option<String>,
    status: TaskStatus,
    history: Vec<TaskMessage>,
    metadata: BTreeMap<String, JsonValue>,
    push_configs: Vec<JsonValue>,
    events: Vec<JsonValue>,
    subscribers: Vec<UnboundedSender<JsonValue>>,
    cancel_token: Option<Arc<AtomicBool>>,
}

type TaskStore = Arc<Mutex<HashMap<String, TaskState>>>;

struct ExecutionRuntime {
    tx: mpsc::UnboundedSender<ExecutionJob>,
}

struct ExecutionJob {
    request: CallRequest,
    response_tx: oneshot::Sender<Result<CallResponse, DispatchError>>,
}

struct PreparedTask {
    id: String,
    function: String,
    arguments: CallArguments,
    auth: AuthRequest,
    caller: String,
    trace_id: Option<harn_vm::TraceId>,
    cancel_token: Arc<AtomicBool>,
}

enum RpcOutcome {
    Json(JsonValue),
    Sse(UnboundedReceiver<JsonValue>),
}

impl A2aServer {
    pub fn new(config: A2aServerConfig) -> Self {
        let agent_name = config
            .agent_name
            .unwrap_or_else(|| derived_agent_name(config.core.catalog()));
        let core = Arc::new(config.core);
        let catalog = core.catalog().clone();
        Self {
            descriptor: AdapterDescriptor {
                id: "a2a".to_string(),
                caller_shape: "peer-agent-task".to_string(),
                supports_streaming: true,
                supports_cancel: true,
            },
            agent_name,
            card_signing_secret: config.card_signing_secret,
            catalog,
            executor: ExecutionRuntime::start(core),
            tasks: Arc::new(Mutex::new(HashMap::new())),
        }
    }

    pub async fn run_http(self: Arc<Self>, options: A2aHttpServeOptions) -> Result<(), String> {
        let listener = tokio::net::TcpListener::bind(options.bind)
            .await
            .map_err(|error| format!("failed to bind {}: {error}", options.bind))?;
        let local_addr = listener
            .local_addr()
            .map_err(|error| format!("failed to read local addr: {error}"))?;
        let public_url = options
            .public_url
            .unwrap_or_else(|| format!("http://localhost:{}", local_addr.port()));
        let state = HttpState {
            server: self,
            public_url: public_url.clone(),
        };
        let router = Router::new()
            .route("/", post(jsonrpc_request))
            .route("/agent/card", get(agent_card_request))
            .route("/.well-known/a2a-agent", get(agent_card_request))
            .route("/.well-known/agent.json", get(agent_card_request))
            .route("/tasks/send", post(rest_send_task))
            .route("/tasks/send_and_wait", post(rest_send_and_wait_task))
            .route("/tasks/cancel", post(rest_cancel_task))
            .route("/tasks/resubscribe", post(rest_resubscribe_task))
            .with_state(state);

        eprintln!("Harn A2A server listening on {public_url}");
        eprintln!("[harn] A2A workflow server ready on {public_url}");
        eprintln!("[harn] Agent card: {public_url}/.well-known/a2a-agent");
        axum::serve(listener, router)
            .await
            .map_err(|error| format!("A2A HTTP server failed: {error}"))
    }

    fn agent_card(&self, public_url: &str) -> JsonValue {
        let skills = self
            .catalog
            .functions
            .values()
            .map(|function| {
                json!({
                    "id": function.name,
                    "name": function.name,
                    "description": format!("Invoke exported Harn function '{}'.", function.name),
                    "inputSchema": function.input_schema,
                })
            })
            .collect::<Vec<_>>();
        let mut card = json!({
            "id": self.agent_name,
            "name": self.agent_name,
            "description": "Harn peer agent",
            "url": public_url,
            "version": env!("CARGO_PKG_VERSION"),
            "protocolVersion": A2A_PROTOCOL_VERSION,
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
                "pushNotifications": true,
                "resubscribe": true,
                "cancel": true,
                "extendedAgentCard": false
            },
            "skills": skills
        });
        if let Some(secret) = self.card_signing_secret.as_deref() {
            sign_card(&mut card, secret);
        }
        card
    }

    async fn process_rpc(self: Arc<Self>, request: JsonValue, auth: AuthRequest) -> RpcOutcome {
        let rpc_id = request.get("id").cloned().unwrap_or(JsonValue::Null);
        let method = request
            .get("method")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));

        match method {
            "a2a.SendMessage" | "tasks/send" | "tasks/send_and_wait" => {
                let wait = if method == "tasks/send" {
                    false
                } else if method == "tasks/send_and_wait" {
                    true
                } else {
                    !return_immediately(&params)
                };
                match self.prepare_task(&params, auth) {
                    Ok(task) if wait => {
                        self.run_task_to_completion(&task).await;
                        RpcOutcome::Json(task_rpc_response(&rpc_id, self.task_json(&task.id)))
                    }
                    Ok(task) => {
                        let task_id = task.id.clone();
                        let server = self.clone();
                        tokio::spawn(async move {
                            server.run_task_to_completion(&task).await;
                        });
                        RpcOutcome::Json(task_rpc_response(&rpc_id, self.task_json(&task_id)))
                    }
                    Err(response) => RpcOutcome::Json(response.with_id(rpc_id)),
                }
            }
            "a2a.SendStreamingMessage" | "tasks/sendSubscribe" => {
                match self.prepare_task(&params, auth) {
                    Ok(task) => {
                        let rx = self.subscribe(&task.id).unwrap_or_else(empty_stream);
                        let server = self.clone();
                        tokio::spawn(async move {
                            server.run_task_to_completion(&task).await;
                        });
                        RpcOutcome::Sse(rx)
                    }
                    Err(response) => RpcOutcome::Json(response.with_id(rpc_id)),
                }
            }
            "tasks/resubscribe" | "a2a.ResubscribeTask" => {
                let task_id = task_id_param(&params);
                match task_id.and_then(|id| self.subscribe(id)) {
                    Some(rx) => RpcOutcome::Sse(rx),
                    None => RpcOutcome::Json(error_response(
                        rpc_id,
                        A2A_TASK_NOT_FOUND,
                        "Task not found",
                    )),
                }
            }
            "a2a.GetTask" | "tasks/get" => {
                let task_id = task_id_param(&params);
                match task_id.map(|id| self.task_json(id)) {
                    Some(JsonValue::Null) | None => RpcOutcome::Json(error_response(
                        rpc_id,
                        A2A_TASK_NOT_FOUND,
                        "Task not found",
                    )),
                    Some(task) => RpcOutcome::Json(task_rpc_response(&rpc_id, task)),
                }
            }
            "a2a.CancelTask" | "tasks/cancel" => {
                let task_id = task_id_param(&params);
                match task_id.and_then(|id| self.cancel_task(id).ok()) {
                    Some(task) => RpcOutcome::Json(task_rpc_response(&rpc_id, task)),
                    None => RpcOutcome::Json(error_response(
                        rpc_id,
                        A2A_TASK_NOT_CANCELABLE,
                        "Task not cancelable",
                    )),
                }
            }
            "a2a.ListTasks" | "tasks/list" => {
                RpcOutcome::Json(task_rpc_response(&rpc_id, self.list_tasks()))
            }
            "CreateTaskPushNotificationConfig" | "tasks/pushNotificationConfig/set" => {
                let task_id = task_id_param(&params);
                let config = params
                    .get("pushNotificationConfig")
                    .or_else(|| params.get("config"))
                    .cloned()
                    .unwrap_or(JsonValue::Null);
                match task_id.and_then(|id| self.add_push_config(id, config).ok()) {
                    Some(config) => RpcOutcome::Json(task_rpc_response(&rpc_id, config)),
                    None => RpcOutcome::Json(error_response(
                        rpc_id,
                        A2A_TASK_NOT_FOUND,
                        "Task not found",
                    )),
                }
            }
            _ => RpcOutcome::Json(error_response(
                rpc_id,
                A2A_UNSUPPORTED_OPERATION,
                &format!("UnsupportedOperationError: {method}"),
            )),
        }
    }

    fn prepare_task(
        &self,
        params: &JsonValue,
        auth: AuthRequest,
    ) -> Result<PreparedTask, A2aPrepareError> {
        let text = message_text(params);
        let function = select_function(&self.catalog, params)?;
        let arguments = message_arguments(
            self.catalog
                .function(&function)
                .expect("selected function exists"),
            params,
            &text,
        )?;
        let task_id = Uuid::now_v7().to_string();
        let cancel_token = Arc::new(AtomicBool::new(false));
        let context_id = params
            .get("contextId")
            .and_then(JsonValue::as_str)
            .map(str::to_string);
        let trace_id = auth
            .headers
            .get(A2A_TRACE_HEADER)
            .cloned()
            .or_else(|| context_id.clone())
            .map(harn_vm::TraceId);
        let push_config = params
            .pointer("/configuration/pushNotificationConfig")
            .cloned();

        let mut task = TaskState {
            id: task_id.clone(),
            context_id,
            status: TaskStatus::Submitted,
            history: vec![TaskMessage {
                id: Uuid::now_v7().to_string(),
                role: "user".to_string(),
                parts: vec![json!({"type": "text", "text": text})],
            }],
            metadata: BTreeMap::new(),
            push_configs: push_config.into_iter().collect(),
            events: Vec::new(),
            subscribers: Vec::new(),
            cancel_token: Some(cancel_token.clone()),
        };
        task.events
            .push(status_event(&task_id, TaskStatus::Submitted));
        self.tasks
            .lock()
            .expect("tasks poisoned")
            .insert(task_id.clone(), task);

        Ok(PreparedTask {
            id: task_id,
            function,
            arguments,
            auth,
            caller: caller_label(params),
            trace_id,
            cancel_token,
        })
    }

    async fn run_task_to_completion(self: &Arc<Self>, task: &PreparedTask) {
        self.transition(&task.id, TaskStatus::Working);
        let result = self
            .executor
            .call(CallRequest {
                adapter: self.descriptor.id.clone(),
                function: task.function.clone(),
                arguments: task.arguments.clone(),
                auth: task.auth.clone(),
                caller: task.caller.clone(),
                replay_key: Some(task.id.clone()),
                trace_id: task.trace_id.clone(),
                parent_span_id: None,
                metadata: BTreeMap::new(),
                cancel_token: Some(task.cancel_token.clone()),
            })
            .await;

        if self.is_cancelled(&task.id) {
            return;
        }

        match result {
            Ok(response) => self.complete_task(&task.id, response),
            Err(error) => self.fail_task(&task.id, &error.to_string()),
        }
    }

    fn transition(&self, task_id: &str, status: TaskStatus) {
        let event = status_event(task_id, status.clone());
        let task_for_push = {
            let mut tasks = self.tasks.lock().expect("tasks poisoned");
            let Some(task) = tasks.get_mut(task_id) else {
                return;
            };
            task.status = status;
            publish_locked(task, event);
            task_to_json(task)
        };
        self.deliver_push(task_for_push);
    }

    fn complete_task(&self, task_id: &str, response: CallResponse) {
        let text = response_text(&response.value);
        let handoff_metadata = handoff_task_metadata(&response);
        let message = json!({
            "type": "message",
            "taskId": task_id,
            "message": {
                "id": Uuid::now_v7().to_string(),
                "role": "agent",
                "parts": [{"type": "text", "text": text}]
            }
        });
        let task_for_push = {
            let mut tasks = self.tasks.lock().expect("tasks poisoned");
            let Some(task) = tasks.get_mut(task_id) else {
                return;
            };
            task.history.push(TaskMessage {
                id: Uuid::now_v7().to_string(),
                role: "agent".to_string(),
                parts: vec![json!({"type": "text", "text": text})],
            });
            if let Some(metadata) = handoff_metadata {
                task.metadata.extend(metadata);
            }
            publish_locked(task, message);
            task.status = TaskStatus::Completed;
            publish_locked(task, status_event(task_id, TaskStatus::Completed));
            task.cancel_token = None;
            task_to_json(task)
        };
        self.deliver_push(task_for_push);
    }

    fn fail_task(&self, task_id: &str, message: &str) {
        let event = json!({
            "type": "status",
            "taskId": task_id,
            "status": {"state": "failed"},
            "error": message,
        });
        let task_for_push = {
            let mut tasks = self.tasks.lock().expect("tasks poisoned");
            let Some(task) = tasks.get_mut(task_id) else {
                return;
            };
            task.status = TaskStatus::Failed;
            task.history.push(TaskMessage {
                id: Uuid::now_v7().to_string(),
                role: "agent".to_string(),
                parts: vec![json!({"type": "text", "text": message})],
            });
            publish_locked(task, event);
            task.cancel_token = None;
            task_to_json(task)
        };
        self.deliver_push(task_for_push);
    }

    fn cancel_task(&self, task_id: &str) -> Result<JsonValue, String> {
        let task_for_push = {
            let mut tasks = self.tasks.lock().expect("tasks poisoned");
            let task = tasks
                .get_mut(task_id)
                .ok_or_else(|| format!("TaskNotFoundError: {task_id}"))?;
            if task.status.is_terminal() {
                return Err(format!(
                    "TaskNotCancelableError: task {} is in terminal state '{}'",
                    task_id,
                    task.status.as_str()
                ));
            }
            if let Some(cancel_token) = task.cancel_token.as_ref() {
                cancel_token.store(true, Ordering::SeqCst);
            }
            task.status = TaskStatus::Cancelled;
            publish_locked(task, status_event(task_id, TaskStatus::Cancelled));
            task.cancel_token = None;
            task_to_json(task)
        };
        self.deliver_push(task_for_push.clone());
        Ok(task_for_push)
    }

    fn is_cancelled(&self, task_id: &str) -> bool {
        self.tasks
            .lock()
            .expect("tasks poisoned")
            .get(task_id)
            .is_some_and(|task| task.status == TaskStatus::Cancelled)
    }

    fn subscribe(&self, task_id: &str) -> Option<UnboundedReceiver<JsonValue>> {
        let (tx, rx) = unbounded();
        let mut tasks = self.tasks.lock().expect("tasks poisoned");
        let task = tasks.get_mut(task_id)?;
        for event in &task.events {
            let _ = tx.unbounded_send(wrap_event(JsonValue::Null, event.clone()));
        }
        if !task.status.is_terminal() {
            task.subscribers.push(tx);
        }
        Some(rx)
    }

    fn task_json(&self, task_id: &str) -> JsonValue {
        self.tasks
            .lock()
            .expect("tasks poisoned")
            .get(task_id)
            .map(task_to_json)
            .unwrap_or(JsonValue::Null)
    }

    fn list_tasks(&self) -> JsonValue {
        let tasks = self
            .tasks
            .lock()
            .expect("tasks poisoned")
            .values()
            .map(|task| {
                json!({
                    "id": task.id,
                    "status": {"state": task.status.as_str()},
                    "contextId": task.context_id,
                })
            })
            .collect::<Vec<_>>();
        json!({ "tasks": tasks })
    }

    fn add_push_config(&self, task_id: &str, mut config: JsonValue) -> Result<JsonValue, String> {
        let mut tasks = self.tasks.lock().expect("tasks poisoned");
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| format!("TaskNotFoundError: {task_id}"))?;
        if config.get("id").and_then(JsonValue::as_str).is_none() {
            config["id"] = JsonValue::String(Uuid::now_v7().to_string());
        }
        config["taskId"] = JsonValue::String(task_id.to_string());
        task.push_configs.push(config.clone());
        Ok(config)
    }

    fn deliver_push(&self, task: JsonValue) {
        let configs = self
            .tasks
            .lock()
            .expect("tasks poisoned")
            .get(task["id"].as_str().unwrap_or_default())
            .map(|task| task.push_configs.clone())
            .unwrap_or_default();
        if configs.is_empty() {
            return;
        }
        tokio::spawn(async move {
            let client = reqwest::Client::new();
            let payload = json!({ "statusUpdate": task });
            for config in configs {
                let Some(url) = config.get("url").and_then(JsonValue::as_str) else {
                    continue;
                };
                let mut request = client
                    .post(url)
                    .header(reqwest::header::CONTENT_TYPE, "application/a2a+json")
                    .json(&payload);
                if let Some(token) = config.get("token").and_then(JsonValue::as_str) {
                    request = request.bearer_auth(token);
                } else if let Some(auth) = config.get("authentication") {
                    if let Some(scheme) = auth.get("scheme").and_then(JsonValue::as_str) {
                        let credentials = auth
                            .get("credentials")
                            .and_then(JsonValue::as_str)
                            .unwrap_or_default();
                        if !credentials.is_empty() {
                            request = request.header(
                                reqwest::header::AUTHORIZATION,
                                format!("{scheme} {credentials}"),
                            );
                        }
                    }
                }
                let _ = request.send().await;
            }
        });
    }
}

#[async_trait::async_trait(?Send)]
impl TransportAdapter for A2aServer {
    fn descriptor(&self) -> AdapterDescriptor {
        self.descriptor.clone()
    }
}

impl ExecutionRuntime {
    fn start(core: Arc<DispatchCore>) -> Self {
        let (tx, mut rx) = mpsc::unbounded_channel::<ExecutionJob>();
        std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("build A2A runtime");
            let local = LocalSet::new();
            local.block_on(&runtime, async move {
                while let Some(job) = rx.recv().await {
                    let core = core.clone();
                    tokio::task::spawn_local(async move {
                        let result = core.dispatch(job.request).await;
                        let _ = job.response_tx.send(result);
                    });
                }
            });
        });
        Self { tx }
    }

    async fn call(&self, request: CallRequest) -> Result<CallResponse, DispatchError> {
        let (response_tx, response_rx) = oneshot::channel();
        self.tx
            .send(ExecutionJob {
                request,
                response_tx,
            })
            .map_err(|_| DispatchError::Execution("A2A executor is not running".to_string()))?;
        response_rx
            .await
            .map_err(|_| DispatchError::Execution("A2A executor dropped response".to_string()))?
    }
}

async fn agent_card_request(State(state): State<HttpState>) -> Response {
    Json(state.server.agent_card(&state.public_url)).into_response()
}

async fn jsonrpc_request(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Some(response) = check_version_header(&headers, &body) {
        return Json(response).into_response();
    }
    let request = match serde_json::from_slice::<JsonValue>(body.as_ref()) {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(error_response(
                    JsonValue::Null,
                    -32700,
                    &format!("Parse error: {error}"),
                )),
            )
                .into_response()
        }
    };
    let auth = http_auth_request(method, "/", body.to_vec(), &headers);
    match state.server.process_rpc(request, auth).await {
        RpcOutcome::Json(response) => Json(response).into_response(),
        RpcOutcome::Sse(rx) => sse_response(rx).into_response(),
    }
}

async fn rest_send_task(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    rest_task_request(state, method, headers, body, "tasks/send").await
}

async fn rest_send_and_wait_task(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    rest_task_request(state, method, headers, body, "tasks/send_and_wait").await
}

async fn rest_cancel_task(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    rest_task_request(state, method, headers, body, "tasks/cancel").await
}

async fn rest_resubscribe_task(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    rest_task_request(state, method, headers, body, "tasks/resubscribe").await
}

async fn rest_task_request(
    state: HttpState,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
    rpc_method: &str,
) -> Response {
    let params = match serde_json::from_slice::<JsonValue>(body.as_ref()) {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(error_response(
                    JsonValue::Null,
                    -32700,
                    &format!("Parse error: {error}"),
                )),
            )
                .into_response()
        }
    };
    let auth_path = format!("/{rpc_method}");
    let auth = http_auth_request(method, &auth_path, body.to_vec(), &headers);
    let request = harn_vm::jsonrpc::request(Uuid::now_v7().to_string(), rpc_method, params);
    match state.server.process_rpc(request, auth).await {
        RpcOutcome::Json(response) if response.get("error").is_some() => {
            (StatusCode::BAD_REQUEST, Json(response)).into_response()
        }
        RpcOutcome::Json(response) => Json(response["result"].clone()).into_response(),
        RpcOutcome::Sse(rx) => sse_response(rx).into_response(),
    }
}

fn sse_response(
    rx: UnboundedReceiver<JsonValue>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    Sse::new(sse_events(rx)).keep_alive(KeepAlive::default())
}

fn sse_events(
    rx: UnboundedReceiver<JsonValue>,
) -> impl futures::Stream<Item = Result<Event, Infallible>> {
    rx.map(|message| {
        Ok(Event::default()
            .id(Uuid::now_v7().to_string())
            .event("message")
            .data(serde_json::to_string(&message).unwrap_or_else(|_| "{}".to_string())))
    })
}

fn empty_stream() -> UnboundedReceiver<JsonValue> {
    let (_tx, rx) = unbounded();
    rx
}

fn publish_locked(task: &mut TaskState, event: JsonValue) {
    task.events.push(event.clone());
    task.subscribers.retain(|tx| {
        tx.unbounded_send(wrap_event(JsonValue::Null, event.clone()))
            .is_ok()
    });
    if task.status.is_terminal() {
        task.subscribers.clear();
    }
}

fn wrap_event(rpc_id: JsonValue, event: JsonValue) -> JsonValue {
    harn_vm::jsonrpc::response(rpc_id, event)
}

fn task_to_json(task: &TaskState) -> JsonValue {
    let history = task
        .history
        .iter()
        .map(|message| {
            json!({
                "id": message.id,
                "role": message.role,
                "parts": message.parts,
            })
        })
        .collect::<Vec<_>>();
    let mut value = json!({
        "id": task.id,
        "status": {"state": task.status.as_str()},
        "history": history,
        "artifacts": [],
    });
    if let Some(context_id) = task.context_id.as_ref() {
        value["contextId"] = JsonValue::String(context_id.clone());
    }
    if !task.metadata.is_empty() {
        value["metadata"] = serde_json::to_value(&task.metadata)
            .unwrap_or_else(|_| JsonValue::Object(Default::default()));
    }
    value
}

fn handoff_task_metadata(response: &CallResponse) -> Option<BTreeMap<String, JsonValue>> {
    let handoffs = harn_vm::orchestration::extract_handoffs_from_json_value(&response.value);
    if handoffs.is_empty() {
        return None;
    }
    Some(BTreeMap::from([
        (
            "handoff_ids".to_string(),
            JsonValue::Array(
                handoffs
                    .iter()
                    .map(|handoff| JsonValue::String(handoff.id.clone()))
                    .collect(),
            ),
        ),
        (
            "handoffs".to_string(),
            serde_json::to_value(&handoffs).unwrap_or_else(|_| JsonValue::Array(Vec::new())),
        ),
    ]))
}

fn status_event(task_id: &str, status: TaskStatus) -> JsonValue {
    json!({
        "type": "status",
        "taskId": task_id,
        "status": {"state": status.as_str()},
    })
}

fn task_rpc_response(rpc_id: &JsonValue, task_json: JsonValue) -> JsonValue {
    harn_vm::jsonrpc::response(rpc_id.clone(), task_json)
}

fn error_response(rpc_id: JsonValue, code: i64, message: &str) -> JsonValue {
    harn_vm::jsonrpc::error_response(rpc_id, code, message)
}

fn check_version_header(headers: &HeaderMap, body: &[u8]) -> Option<JsonValue> {
    let version = headers
        .get(A2A_VERSION_HEADER)
        .and_then(|value| value.to_str().ok())?;
    if version == A2A_PROTOCOL_VERSION {
        return None;
    }
    let rpc_id = serde_json::from_slice::<JsonValue>(body)
        .ok()
        .and_then(|value| value.get("id").cloned())
        .unwrap_or(JsonValue::Null);
    Some(error_response(
        rpc_id,
        A2A_VERSION_NOT_SUPPORTED,
        &format!(
            "VersionNotSupportedError: requested version {version}, supported: {A2A_PROTOCOL_VERSION}"
        ),
    ))
}

fn return_immediately(params: &JsonValue) -> bool {
    params
        .pointer("/configuration/returnImmediately")
        .and_then(JsonValue::as_bool)
        .or_else(|| {
            params
                .pointer("/configuration/blocking")
                .and_then(JsonValue::as_bool)
                .map(|blocking| !blocking)
        })
        .unwrap_or(false)
}

fn task_id_param(params: &JsonValue) -> Option<&str> {
    params
        .get("taskId")
        .or_else(|| params.get("task_id"))
        .or_else(|| params.get("id"))
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
}

fn message_text(params: &JsonValue) -> String {
    params
        .pointer("/message/parts")
        .and_then(JsonValue::as_array)
        .and_then(|parts| {
            parts.iter().find_map(|part| {
                if part.get("type").and_then(JsonValue::as_str) == Some("text") {
                    part.get("text").and_then(JsonValue::as_str)
                } else {
                    None
                }
            })
        })
        .or_else(|| params.get("text").and_then(JsonValue::as_str))
        .unwrap_or_default()
        .to_string()
}

fn caller_label(params: &JsonValue) -> String {
    params
        .pointer("/message/metadata/caller")
        .or_else(|| params.pointer("/metadata/caller"))
        .and_then(JsonValue::as_str)
        .unwrap_or("a2a-peer")
        .to_string()
}

fn select_function(catalog: &ExportCatalog, params: &JsonValue) -> Result<String, A2aPrepareError> {
    for pointer in [
        "/function",
        "/skillId",
        "/message/metadata/function",
        "/message/metadata/skillId",
        "/message/metadata/target_agent",
        "/metadata/target_agent",
    ] {
        let Some(name) = params
            .pointer(pointer)
            .and_then(JsonValue::as_str)
            .map(str::trim)
            .filter(|value| !value.is_empty())
        else {
            continue;
        };
        let name = name.rsplit('/').next().unwrap_or(name);
        if catalog.function(name).is_some() {
            return Ok(name.to_string());
        }
    }

    for candidate in ["execute", "default", "main", "handle", "run"] {
        if catalog.function(candidate).is_some() {
            return Ok(candidate.to_string());
        }
    }
    if catalog.functions.len() == 1 {
        return Ok(catalog
            .functions
            .keys()
            .next()
            .expect("one function")
            .clone());
    }
    Err(A2aPrepareError::new(
        -32602,
        "A2A task must identify an exported function when multiple functions are exported",
    ))
}

fn message_arguments(
    function: &crate::ExportedFunction,
    params: &JsonValue,
    text: &str,
) -> Result<CallArguments, A2aPrepareError> {
    if let Some(arguments) = params
        .get("arguments")
        .or_else(|| params.pointer("/message/metadata/arguments"))
    {
        return json_arguments(arguments.clone());
    }

    if function.params.is_empty() {
        return Ok(CallArguments::Positional(Vec::new()));
    }

    let target_param = ["task", "message", "input"]
        .iter()
        .find_map(|name| function.params.iter().find(|param| param.name == *name))
        .or_else(|| (function.params.len() == 1).then(|| &function.params[0]));
    let Some(param) = target_param else {
        return Err(A2aPrepareError::new(
            -32602,
            "A2A task text can only be inferred for a single-argument export or a task/message/input parameter",
        ));
    };
    Ok(CallArguments::Named(BTreeMap::from([(
        param.name.clone(),
        JsonValue::String(text.to_string()),
    )])))
}

fn json_arguments(value: JsonValue) -> Result<CallArguments, A2aPrepareError> {
    match value {
        JsonValue::Null => Ok(CallArguments::Named(BTreeMap::new())),
        JsonValue::Object(values) => Ok(CallArguments::Named(values.into_iter().collect())),
        JsonValue::Array(values) => Ok(CallArguments::Positional(values)),
        _ => Err(A2aPrepareError::new(
            -32602,
            "A2A arguments must be an object, array, or null",
        )),
    }
}

fn response_text(value: &JsonValue) -> String {
    match value {
        JsonValue::String(text) => text.clone(),
        _ => serde_json::to_string(value).unwrap_or_else(|_| value.to_string()),
    }
}

fn http_auth_request(
    method: Method,
    path: &str,
    body: Vec<u8>,
    headers: &HeaderMap,
) -> AuthRequest {
    AuthRequest {
        method: method.as_str().to_string(),
        path: path.to_string(),
        body,
        headers: normalized_headers(headers),
        validated_oauth: None,
    }
}

fn normalized_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_ascii_lowercase(), value.to_string()))
        })
        .collect()
}

fn sign_card(card: &mut JsonValue, secret: &str) {
    let Ok(bytes) = serde_json::to_vec(card) else {
        return;
    };
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return;
    };
    mac.update(&bytes);
    let signature = base64::engine::general_purpose::STANDARD.encode(mac.finalize().into_bytes());
    card["signatures"] = json!([{
        "alg": "HS256",
        "kid": "harn-serve",
        "signature": signature,
    }]);
}

fn derived_agent_name(catalog: &ExportCatalog) -> String {
    catalog
        .script_path
        .file_stem()
        .and_then(|value| value.to_str())
        .unwrap_or("harn-serve")
        .to_string()
}

struct A2aPrepareError {
    code: i64,
    message: String,
}

impl A2aPrepareError {
    fn new(code: i64, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
        }
    }

    fn with_id(self, rpc_id: JsonValue) -> JsonValue {
        error_response(rpc_id, self.code, &self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{DispatchCore, DispatchCoreConfig};

    #[tokio::test]
    async fn agent_card_advertises_exported_functions() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r#"
pub fn triage(task: string) -> string {
  return task
}
"#,
        )
        .expect("write script");
        let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
        let server = A2aServer::new(A2aServerConfig::new(core));

        let card = server.agent_card("http://localhost:8080");

        assert_eq!(card["capabilities"]["streaming"], true);
        assert_eq!(card["capabilities"]["pushNotifications"], true);
        assert_eq!(card["skills"][0]["id"], "triage");
    }

    #[tokio::test]
    async fn send_message_dispatches_to_shared_core_export() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r#"
pub fn triage(task: string) -> string {
  return task
}
"#,
        )
        .expect("write script");
        let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
        let server = Arc::new(A2aServer::new(A2aServerConfig::new(core)));
        let request = harn_vm::jsonrpc::request(
            "1",
            "a2a.SendMessage",
            json!({
                "message": {
                    "metadata": {"target_agent": "triage"},
                    "parts": [{"type": "text", "text": "hello"}]
                }
            }),
        );

        let RpcOutcome::Json(response) = server.process_rpc(request, AuthRequest::default()).await
        else {
            panic!("expected json response");
        };

        assert_eq!(response["result"]["status"]["state"], "completed");
        assert_eq!(
            response["result"]["history"][1]["parts"][0]["text"],
            "hello"
        );
    }

    #[tokio::test]
    async fn send_message_surfaces_handoff_metadata() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r#"
import "std/agents"

pub fn triage(task: string) -> dict {
  let review = handoff({
    source_persona: "merge_captain",
    target_persona_or_human: {
      kind: "persona",
      id: "review_captain",
      label: "review_captain"
    },
    task: task,
    reason: "Need explicit code review before merge",
    evidence_refs: [{artifact_id: "artifact_diff", label: "Patch summary"}],
    files_or_entities_touched: ["crates/harn-vm/src/orchestration/handoffs.rs"],
    open_questions: ["Is the side-effect budget acceptable?"],
    blocked_on: ["review_captain approval"],
    requested_capabilities: ["review", "comment"],
    allowed_side_effects: ["comment_on_pr"],
    budget_remaining: {tokens: 900, tool_calls: 2},
    deadline_checkback: {checkback_at: "2026-04-24T10:00:00Z"},
    confidence: 0.74
  })
  return workflow_result_run(
    task,
    "triage",
    {visible_text: "handoff ready"},
    [handoff_artifact(review)],
    {}
  )
}
"#,
        )
        .expect("write script");
        let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
        let server = Arc::new(A2aServer::new(A2aServerConfig::new(core)));
        let request = harn_vm::jsonrpc::request(
            "handoff-1",
            "a2a.SendMessage",
            json!({
                "message": {
                    "metadata": {"target_agent": "triage"},
                    "parts": [{"type": "text", "text": "Review PR #461"}]
                }
            }),
        );

        let RpcOutcome::Json(response) = server.process_rpc(request, AuthRequest::default()).await
        else {
            panic!("expected json response");
        };

        assert_eq!(response["result"]["status"]["state"], "completed");
        assert!(response["result"]["metadata"]["handoff_ids"][0]
            .as_str()
            .is_some_and(|value| !value.is_empty()));
        assert_eq!(
            response["result"]["metadata"]["handoffs"][0]["source_persona"],
            "merge_captain"
        );
        assert_eq!(
            response["result"]["metadata"]["handoffs"][0]["target_persona_or_human"]["label"],
            "review_captain"
        );
    }

    #[tokio::test]
    async fn streaming_send_and_resubscribe_replay_task_events() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r#"
pub fn triage(task: string) -> string {
  return task
}
"#,
        )
        .expect("write script");
        let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
        let server = Arc::new(A2aServer::new(A2aServerConfig::new(core)));
        let request = harn_vm::jsonrpc::request(
            "stream-1",
            "a2a.SendStreamingMessage",
            json!({
                "function": "triage",
                "message": {
                    "parts": [{"type": "text", "text": "stream me"}]
                }
            }),
        );

        let RpcOutcome::Sse(mut rx) = server
            .clone()
            .process_rpc(request, AuthRequest::default())
            .await
        else {
            panic!("expected sse response");
        };
        let mut events = Vec::new();
        while let Some(event) = tokio::time::timeout(std::time::Duration::from_secs(2), rx.next())
            .await
            .expect("stream event")
        {
            let done = event
                .pointer("/result/status/state")
                .and_then(JsonValue::as_str)
                == Some("completed");
            events.push(event);
            if done {
                break;
            }
        }

        let task_id = events[0]["result"]["taskId"].as_str().expect("task id");
        assert!(events.iter().any(|event| {
            event
                .pointer("/result/status/state")
                .and_then(JsonValue::as_str)
                == Some("working")
        }));
        assert!(events.iter().any(|event| {
            event
                .pointer("/result/message/parts/0/text")
                .and_then(JsonValue::as_str)
                == Some("stream me")
        }));

        let resubscribe =
            harn_vm::jsonrpc::request("resub-1", "tasks/resubscribe", json!({"id": task_id}));
        let RpcOutcome::Sse(replay_rx) = server
            .process_rpc(resubscribe, AuthRequest::default())
            .await
        else {
            panic!("expected replay stream");
        };
        let replayed = replay_rx.collect::<Vec<_>>().await;
        assert!(replayed.iter().any(|event| {
            event
                .pointer("/result/status/state")
                .and_then(JsonValue::as_str)
                == Some("completed")
        }));
    }

    #[test]
    fn signed_card_adds_signature_envelope() {
        let mut card = json!({"id": "agent", "skills": []});
        sign_card(&mut card, "secret");

        assert_eq!(card["signatures"][0]["alg"], "HS256");
        assert!(card["signatures"][0]["signature"].as_str().unwrap().len() > 16);
    }
}
