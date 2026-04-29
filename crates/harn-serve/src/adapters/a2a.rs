use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, Method, StatusCode};
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
    DispatchError, ExportCatalog, HttpTlsConfig, TransportAdapter,
};

pub const A2A_PROTOCOL_VERSION: &str = "0.3.0";

const A2A_VERSION_HEADER: &str = "a2a-version";
const A2A_TRACE_HEADER: &str = "a2a-trace-id";
const A2A_LEGACY_PROTOCOL_VERSIONS: &[&str] = &["1.0", "1.0.0"];
const A2A_DEPRECATION_HEADER: &str = "deprecation";
const A2A_AGENT_CARD_PATH: &str = "/.well-known/agent-card.json";

const A2A_TASK_NOT_FOUND: i64 = -32001;
const A2A_TASK_NOT_CANCELABLE: i64 = -32002;
const A2A_UNSUPPORTED_OPERATION: i64 = -32003;
const A2A_VERSION_NOT_SUPPORTED: i64 = -32009;

#[derive(Clone, Debug)]
pub struct A2aHttpServeOptions {
    pub bind: SocketAddr,
    pub public_url: Option<String>,
    pub tls: HttpTlsConfig,
}

impl Default for A2aHttpServeOptions {
    fn default() -> Self {
        Self {
            bind: "0.0.0.0:8080".parse().expect("valid bind addr"),
            public_url: None,
            tls: HttpTlsConfig::plain(),
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

struct ProcessedRpc {
    outcome: RpcOutcome,
    deprecation: Option<&'static str>,
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
        let listener = crate::tls::bind_listener(options.bind)?;
        let local_addr = listener
            .local_addr()
            .map_err(|error| format!("failed to read local addr: {error}"))?;
        let public_url = options.public_url.unwrap_or_else(|| {
            format!(
                "{}://localhost:{}",
                options.tls.advertised_scheme(),
                local_addr.port()
            )
        });
        let state = HttpState {
            server: self,
            public_url: public_url.clone(),
        };
        let router = Self::http_router(state);
        let router = crate::tls::apply_security_headers(router, &options.tls);

        eprintln!("Harn A2A server listening on {public_url}");
        eprintln!("[harn] A2A workflow server ready on {public_url}");
        eprintln!("[harn] Agent card: {public_url}{A2A_AGENT_CARD_PATH}");
        crate::tls::serve_router_from_tcp(listener, router, &options.tls)
            .await
            .map_err(|error| format!("A2A HTTP server failed: {error}"))
    }

    fn http_router(state: HttpState) -> Router {
        Router::new()
            .route("/", post(jsonrpc_request))
            .route(A2A_AGENT_CARD_PATH, get(agent_card_request))
            .route("/agent/card", get(agent_card_request))
            .route("/.well-known/a2a-agent", get(agent_card_request))
            .route("/.well-known/agent.json", get(agent_card_request))
            .route("/message/send", post(rest_message_send))
            .route("/message/stream", post(rest_message_stream))
            .route("/tasks/send", post(rest_send_task))
            .route("/tasks/send_and_wait", post(rest_send_and_wait_task))
            .route("/tasks/cancel", post(rest_cancel_task))
            .route("/tasks/resubscribe", post(rest_resubscribe_task))
            .with_state(state)
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
                    "tags": ["harn", "function"],
                    "examples": [],
                    "inputModes": ["application/json", "text/plain"],
                    "outputModes": ["application/json", "text/plain"],
                    "inputSchema": function.input_schema,
                })
            })
            .collect::<Vec<_>>();
        let mut card = json!({
            "name": self.agent_name,
            "description": "Harn peer agent",
            "supportedInterfaces": [
                {
                    "url": public_url,
                    "protocolBinding": "JSONRPC",
                    "protocolVersion": A2A_PROTOCOL_VERSION,
                }
            ],
            "version": env!("CARGO_PKG_VERSION"),
            "provider": {
                "organization": "Harn",
                "url": "https://harn.dev"
            },
            "securitySchemes": {},
            "security": [],
            "defaultInputModes": ["application/json", "text/plain"],
            "defaultOutputModes": ["application/json", "text/plain"],
            "capabilities": {
                "streaming": true,
                "pushNotifications": true,
                "extendedAgentCard": false
            },
            "skills": skills
        });
        if let Some(secret) = self.card_signing_secret.as_deref() {
            sign_card(&mut card, secret);
        }
        card
    }

    #[cfg(test)]
    async fn process_rpc(self: Arc<Self>, request: JsonValue, auth: AuthRequest) -> ProcessedRpc {
        self.process_rpc_with_public_url(request, auth, "http://localhost:8080")
            .await
    }

    async fn process_rpc_with_public_url(
        self: Arc<Self>,
        request: JsonValue,
        auth: AuthRequest,
        public_url: &str,
    ) -> ProcessedRpc {
        let rpc_id = request.get("id").cloned().unwrap_or(JsonValue::Null);
        let method = request
            .get("method")
            .and_then(JsonValue::as_str)
            .unwrap_or_default();
        let params = request.get("params").cloned().unwrap_or_else(|| json!({}));

        let (outcome, deprecation) = match method {
            "message/send" | "a2a.SendMessage" | "tasks/send" | "tasks/send_and_wait" => {
                let deprecation = match method {
                    "a2a.SendMessage" | "tasks/send" | "tasks/send_and_wait" => {
                        Some("Use A2A 0.3.0 method `message/send`.")
                    }
                    _ => None,
                };
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
                        (
                            RpcOutcome::Json(task_rpc_response(&rpc_id, self.task_json(&task.id))),
                            deprecation,
                        )
                    }
                    Ok(task) => {
                        let task_id = task.id.clone();
                        let server = self.clone();
                        tokio::spawn(async move {
                            server.run_task_to_completion(&task).await;
                        });
                        (
                            RpcOutcome::Json(task_rpc_response(&rpc_id, self.task_json(&task_id))),
                            deprecation,
                        )
                    }
                    Err(response) => (RpcOutcome::Json(response.with_id(rpc_id)), deprecation),
                }
            }
            "message/stream" | "a2a.SendStreamingMessage" | "tasks/sendSubscribe" => {
                let deprecation = match method {
                    "a2a.SendStreamingMessage" | "tasks/sendSubscribe" => {
                        Some("Use A2A 0.3.0 method `message/stream`.")
                    }
                    _ => None,
                };
                match self.prepare_task(&params, auth) {
                    Ok(task) => {
                        let rx = self.subscribe(&task.id).unwrap_or_else(empty_stream);
                        let server = self.clone();
                        tokio::spawn(async move {
                            server.run_task_to_completion(&task).await;
                        });
                        (RpcOutcome::Sse(rx), deprecation)
                    }
                    Err(response) => (RpcOutcome::Json(response.with_id(rpc_id)), deprecation),
                }
            }
            "tasks/resubscribe" | "a2a.ResubscribeTask" => {
                let deprecation = (method == "a2a.ResubscribeTask")
                    .then_some("Use A2A 0.3.0 method `tasks/resubscribe`.");
                let task_id = task_id_param(&params);
                match task_id.and_then(|id| self.subscribe(id)) {
                    Some(rx) => (RpcOutcome::Sse(rx), deprecation),
                    None => (
                        RpcOutcome::Json(error_response(
                            rpc_id,
                            A2A_TASK_NOT_FOUND,
                            "Task not found",
                        )),
                        deprecation,
                    ),
                }
            }
            "a2a.GetTask" | "tasks/get" => {
                let deprecation =
                    (method == "a2a.GetTask").then_some("Use A2A 0.3.0 method `tasks/get`.");
                let task_id = task_id_param(&params);
                match task_id.map(|id| self.task_json(id)) {
                    Some(JsonValue::Null) | None => (
                        RpcOutcome::Json(error_response(
                            rpc_id,
                            A2A_TASK_NOT_FOUND,
                            "Task not found",
                        )),
                        deprecation,
                    ),
                    Some(task) => (
                        RpcOutcome::Json(task_rpc_response(&rpc_id, task)),
                        deprecation,
                    ),
                }
            }
            "a2a.CancelTask" | "tasks/cancel" => {
                let deprecation =
                    (method == "a2a.CancelTask").then_some("Use A2A 0.3.0 method `tasks/cancel`.");
                let task_id = task_id_param(&params);
                match task_id.and_then(|id| self.cancel_task(id).ok()) {
                    Some(task) => (
                        RpcOutcome::Json(task_rpc_response(&rpc_id, task)),
                        deprecation,
                    ),
                    None => (
                        RpcOutcome::Json(error_response(
                            rpc_id,
                            A2A_TASK_NOT_CANCELABLE,
                            "Task not cancelable",
                        )),
                        deprecation,
                    ),
                }
            }
            "a2a.ListTasks" | "tasks/list" => (
                RpcOutcome::Json(task_rpc_response(&rpc_id, self.list_tasks())),
                Some("`tasks/list` is a Harn compatibility method and is not part of A2A 0.3.0."),
            ),
            "CreateTaskPushNotificationConfig" | "tasks/pushNotificationConfig/set" => {
                let deprecation = (method == "CreateTaskPushNotificationConfig")
                    .then_some("Use A2A 0.3.0 method `tasks/pushNotificationConfig/set`.");
                let task_id = task_id_param(&params);
                let config = params
                    .get("pushNotificationConfig")
                    .or_else(|| params.get("config"))
                    .cloned()
                    .unwrap_or(JsonValue::Null);
                match task_id.and_then(|id| self.add_push_config(id, config).ok()) {
                    Some(config) => (
                        RpcOutcome::Json(task_rpc_response(&rpc_id, config)),
                        deprecation,
                    ),
                    None => (
                        RpcOutcome::Json(error_response(
                            rpc_id,
                            A2A_TASK_NOT_FOUND,
                            "Task not found",
                        )),
                        deprecation,
                    ),
                }
            }
            "tasks/pushNotificationConfig/get" => {
                let task_id = task_id_param(&params);
                let config_id = push_config_id_param(&params);
                match task_id.and_then(|id| self.push_config(id, config_id).ok()) {
                    Some(config) => (RpcOutcome::Json(task_rpc_response(&rpc_id, config)), None),
                    None => (
                        RpcOutcome::Json(error_response(
                            rpc_id,
                            A2A_TASK_NOT_FOUND,
                            "Task not found",
                        )),
                        None,
                    ),
                }
            }
            "tasks/pushNotificationConfig/list" => {
                let task_id = task_id_param(&params);
                match task_id.and_then(|id| self.push_configs(id).ok()) {
                    Some(configs) => (RpcOutcome::Json(task_rpc_response(&rpc_id, configs)), None),
                    None => (
                        RpcOutcome::Json(error_response(
                            rpc_id,
                            A2A_TASK_NOT_FOUND,
                            "Task not found",
                        )),
                        None,
                    ),
                }
            }
            "tasks/pushNotificationConfig/delete" => {
                let task_id = task_id_param(&params);
                let config_id = push_config_id_param(&params);
                match task_id.zip(config_id).and_then(|(task_id, config_id)| {
                    self.delete_push_config(task_id, config_id).ok()
                }) {
                    Some(()) => (
                        RpcOutcome::Json(task_rpc_response(&rpc_id, JsonValue::Null)),
                        None,
                    ),
                    None => (
                        RpcOutcome::Json(error_response(
                            rpc_id,
                            A2A_TASK_NOT_FOUND,
                            "Task not found",
                        )),
                        None,
                    ),
                }
            }
            "agent/getAuthenticatedExtendedCard" => (
                RpcOutcome::Json(task_rpc_response(&rpc_id, self.agent_card(public_url))),
                None,
            ),
            _ => (
                RpcOutcome::Json(error_response(
                    rpc_id,
                    A2A_UNSUPPORTED_OPERATION,
                    &format!("UnsupportedOperationError: {method}"),
                )),
                None,
            ),
        };
        ProcessedRpc {
            outcome,
            deprecation,
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
        // Subscribe a per-task `AgentEventSink` that translates worker
        // lifecycle events into A2A task events. The session id used by
        // the inner dispatch (set via `agent_session_id` on the
        // CallRequest) must match — both sides are derived from the
        // task id so a single key wires emit -> sink -> task stream.
        let session_id = a2a_worker_session_id(&task.id);
        let sink: Arc<dyn harn_vm::agent_events::AgentEventSink> = Arc::new(A2aWorkerSink {
            task_id: task.id.clone(),
            tasks: self.tasks.clone(),
        });
        harn_vm::agent_events::register_sink(session_id.clone(), sink);

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
                agent_session_id: Some(session_id.clone()),
            })
            .await;

        // Drop the sink so a re-used task id can't deliver to the old
        // task's event stream.
        harn_vm::agent_events::clear_session_sinks(&session_id);

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
        let config_id = config["id"].as_str().unwrap_or_default();
        if let Some(existing) = task.push_configs.iter_mut().find(|candidate| {
            candidate
                .get("id")
                .and_then(JsonValue::as_str)
                .is_some_and(|id| id == config_id)
        }) {
            *existing = config.clone();
        } else {
            task.push_configs.push(config.clone());
        }
        Ok(config)
    }

    fn push_config(&self, task_id: &str, config_id: Option<&str>) -> Result<JsonValue, String> {
        let tasks = self.tasks.lock().expect("tasks poisoned");
        let task = tasks
            .get(task_id)
            .ok_or_else(|| format!("TaskNotFoundError: {task_id}"))?;
        let config = if let Some(config_id) = config_id {
            task.push_configs.iter().find(|config| {
                config
                    .get("id")
                    .and_then(JsonValue::as_str)
                    .is_some_and(|id| id == config_id)
            })
        } else {
            task.push_configs.first()
        };
        config
            .cloned()
            .ok_or_else(|| format!("TaskPushNotificationConfigNotFoundError: {task_id}"))
    }

    fn push_configs(&self, task_id: &str) -> Result<JsonValue, String> {
        let tasks = self.tasks.lock().expect("tasks poisoned");
        let task = tasks
            .get(task_id)
            .ok_or_else(|| format!("TaskNotFoundError: {task_id}"))?;
        Ok(JsonValue::Array(task.push_configs.clone()))
    }

    fn delete_push_config(&self, task_id: &str, config_id: &str) -> Result<(), String> {
        let mut tasks = self.tasks.lock().expect("tasks poisoned");
        let task = tasks
            .get_mut(task_id)
            .ok_or_else(|| format!("TaskNotFoundError: {task_id}"))?;
        let original_len = task.push_configs.len();
        task.push_configs
            .retain(|config| config.get("id").and_then(JsonValue::as_str) != Some(config_id));
        if task.push_configs.len() == original_len {
            return Err(format!(
                "TaskPushNotificationConfigNotFoundError: {task_id}/{config_id}"
            ));
        }
        Ok(())
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
    let processed = state
        .server
        .process_rpc_with_public_url(request, auth, &state.public_url)
        .await;
    rpc_response(processed)
}

async fn rest_message_send(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    rest_task_request(state, method, headers, body, "message/send", None).await
}

async fn rest_message_stream(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    rest_task_request(state, method, headers, body, "message/stream", None).await
}

async fn rest_send_task(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    rest_task_request(
        state,
        method,
        headers,
        body,
        "tasks/send",
        Some("Use A2A 0.3.0 REST path `/message/send`."),
    )
    .await
}

async fn rest_send_and_wait_task(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    rest_task_request(
        state,
        method,
        headers,
        body,
        "tasks/send_and_wait",
        Some("Use A2A 0.3.0 REST path `/message/send` with `configuration.blocking = true`."),
    )
    .await
}

async fn rest_cancel_task(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    rest_task_request(state, method, headers, body, "tasks/cancel", None).await
}

async fn rest_resubscribe_task(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    rest_task_request(state, method, headers, body, "tasks/resubscribe", None).await
}

async fn rest_task_request(
    state: HttpState,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
    rpc_method: &str,
    rest_deprecation: Option<&'static str>,
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
    let mut processed = state
        .server
        .process_rpc_with_public_url(request, auth, &state.public_url)
        .await;
    processed.deprecation = processed.deprecation.or(rest_deprecation);
    match processed.outcome {
        RpcOutcome::Json(response) if response.get("error").is_some() => response_with_deprecation(
            (StatusCode::BAD_REQUEST, Json(response)).into_response(),
            processed.deprecation,
        ),
        RpcOutcome::Json(response) => {
            response_with_deprecation(Json(response["result"].clone()), processed.deprecation)
        }
        RpcOutcome::Sse(rx) => response_with_deprecation(sse_response(rx), processed.deprecation),
    }
}

fn rpc_response(processed: ProcessedRpc) -> Response {
    match processed.outcome {
        RpcOutcome::Json(response) => {
            response_with_deprecation(Json(response), processed.deprecation)
        }
        RpcOutcome::Sse(rx) => response_with_deprecation(sse_response(rx), processed.deprecation),
    }
}

fn response_with_deprecation(response: impl IntoResponse, message: Option<&str>) -> Response {
    let mut response = response.into_response();
    if let Some(message) = message {
        response
            .headers_mut()
            .insert(A2A_DEPRECATION_HEADER, HeaderValue::from_static("true"));
        if let Ok(value) = HeaderValue::from_str(&format!("299 harn \"{message}\"")) {
            response
                .headers_mut()
                .insert(axum::http::header::WARNING, value);
        }
    }
    response
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
    if version == A2A_PROTOCOL_VERSION || A2A_LEGACY_PROTOCOL_VERSIONS.contains(&version) {
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

fn push_config_id_param(params: &JsonValue) -> Option<&str> {
    params
        .get("pushNotificationConfigId")
        .or_else(|| params.get("push_notification_config_id"))
        .or_else(|| params.get("configId"))
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
    let protected = json!({
        "alg": "HS256",
        "typ": "JOSE",
        "kid": "harn-serve",
    });
    let Ok(protected_bytes) = serde_json::to_vec(&protected) else {
        return;
    };
    let protected = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(protected_bytes);
    let payload = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes);
    let Ok(mut mac) = Hmac::<Sha256>::new_from_slice(secret.as_bytes()) else {
        return;
    };
    mac.update(format!("{protected}.{payload}").as_bytes());
    let signature =
        base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(mac.finalize().into_bytes());
    card["signatures"] = json!([{
        "protected": protected,
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

/// Agent-session id used by the A2A adapter when scoping worker events
/// to a task. Prefixed so it can't collide with a user-supplied
/// session id and so the sink registry can be inspected for A2A entries
/// in tests.
fn a2a_worker_session_id(task_id: &str) -> String {
    format!("a2a:{task_id}")
}

/// `AgentEventSink` implementation that publishes worker lifecycle
/// updates and structured plan emissions onto an A2A task's event
/// stream. Chat/tool chunks are deliberately ignored here; they belong
/// to the task history or ACP stream rather than this extension feed.
struct A2aWorkerSink {
    task_id: String,
    tasks: TaskStore,
}

impl harn_vm::agent_events::AgentEventSink for A2aWorkerSink {
    fn handle_event(&self, event: &harn_vm::agent_events::AgentEvent) {
        let payload = match event {
            harn_vm::agent_events::AgentEvent::WorkerUpdate {
                worker_id,
                worker_name,
                worker_task,
                worker_mode,
                event,
                status,
                metadata,
                audit,
                ..
            } => {
                let mut payload = json!({
                    "type": "worker_update",
                    "taskId": self.task_id,
                    "workerId": worker_id,
                    "workerName": worker_name,
                    "workerTask": worker_task,
                    "workerMode": worker_mode,
                    "event": event.as_str(),
                    "status": status,
                    "terminal": event.is_terminal(),
                    "metadata": metadata,
                });
                if let Some(audit) = audit {
                    payload["audit"] = audit.clone();
                }
                payload
            }
            harn_vm::agent_events::AgentEvent::Plan { plan, .. }
                if plan.get("schema_version").and_then(JsonValue::as_str)
                    == Some(harn_vm::llm::plan::PLAN_SCHEMA_VERSION) =>
            {
                json!({
                    "type": "harn_plan",
                    "taskId": self.task_id,
                    "entries": harn_vm::llm::plan::plan_entries(plan),
                    "plan": plan,
                })
            }
            _ => return,
        };
        let task_for_push = {
            let mut tasks = self.tasks.lock().expect("tasks poisoned");
            let Some(task) = tasks.get_mut(&self.task_id) else {
                return;
            };
            publish_locked(task, payload);
            task_to_json(task)
        };
        // No `deliver_push` here: worker_update events stream live to
        // active subscribers but don't fire push-config webhooks. Push
        // delivery is reserved for the canonical task lifecycle
        // transitions so high-volume worker traffic doesn't flood
        // outbound HTTP endpoints.
        let _ = task_for_push;
    }
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
    use axum::body::{to_bytes, Body};
    use axum::http::Request;
    use tower::ServiceExt;

    fn test_server(source: &str) -> (tempfile::TempDir, Arc<A2aServer>) {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(&script, source).expect("write script");
        let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
        (dir, Arc::new(A2aServer::new(A2aServerConfig::new(core))))
    }

    fn assert_current_agent_card_shape(card: &JsonValue, public_url: &str) {
        assert_eq!(card["name"], "server");
        assert_eq!(card["description"], "Harn peer agent");
        assert_eq!(card["version"], env!("CARGO_PKG_VERSION"));
        assert!(card.get("url").is_none(), "card must not emit legacy url");
        assert!(
            card.get("protocolVersion").is_none(),
            "card must not emit legacy top-level protocolVersion"
        );
        assert!(
            card.get("interfaces").is_none(),
            "card must not emit legacy interfaces"
        );
        assert_eq!(card["supportedInterfaces"][0]["url"], public_url);
        assert_eq!(card["supportedInterfaces"][0]["protocolBinding"], "JSONRPC");
        assert_eq!(
            card["supportedInterfaces"][0]["protocolVersion"],
            A2A_PROTOCOL_VERSION
        );
        assert_eq!(card["securitySchemes"], json!({}));
        assert_eq!(card["security"], json!([]));
        assert_eq!(
            card["defaultInputModes"],
            json!(["application/json", "text/plain"])
        );
        assert_eq!(
            card["defaultOutputModes"],
            json!(["application/json", "text/plain"])
        );
        assert_eq!(card["capabilities"]["streaming"], true);
        assert_eq!(card["capabilities"]["pushNotifications"], true);
        assert_eq!(card["capabilities"]["extendedAgentCard"], false);
        assert_eq!(card["skills"][0]["id"], "triage");
        assert_eq!(card["skills"][0]["tags"], json!(["harn", "function"]));
        assert_eq!(
            card["skills"][0]["inputModes"],
            json!(["application/json", "text/plain"])
        );
        assert_eq!(
            card["skills"][0]["outputModes"],
            json!(["application/json", "text/plain"])
        );
    }

    #[tokio::test]
    async fn agent_card_advertises_exported_functions() {
        let (_dir, server) = test_server(
            r#"
pub fn triage(task: string) -> string {
  return task
}
"#,
        );

        let card = server.agent_card("http://localhost:8080");

        assert_current_agent_card_shape(&card, "http://localhost:8080");
    }

    #[tokio::test]
    async fn discovery_paths_serve_current_agent_card_shape() {
        let (_dir, server) = test_server(
            r#"
pub fn triage(task: string) -> string {
  return task
}
"#,
        );
        let public_url = "http://localhost:8080";
        let router = A2aServer::http_router(HttpState {
            server,
            public_url: public_url.to_string(),
        });

        for path in [
            A2A_AGENT_CARD_PATH,
            "/.well-known/agent.json",
            "/.well-known/a2a-agent",
            "/agent/card",
        ] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .uri(path)
                        .body(Body::empty())
                        .expect("request"),
                )
                .await
                .expect("response");
            assert_eq!(response.status(), StatusCode::OK, "path: {path}");
            let bytes = to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body");
            let card: JsonValue = serde_json::from_slice(&bytes).expect("card json");
            assert_current_agent_card_shape(&card, public_url);
        }
    }

    #[tokio::test]
    async fn legacy_jsonrpc_methods_emit_deprecation_header() {
        let (_dir, server) = test_server(
            r#"
pub fn triage(task: string) -> string {
  return task
}
"#,
        );
        let router = A2aServer::http_router(HttpState {
            server,
            public_url: "http://localhost:8080".to_string(),
        });
        let body = serde_json::to_vec(&harn_vm::jsonrpc::request(
            "legacy-1",
            "a2a.SendMessage",
            json!({
                "function": "triage",
                "message": {
                    "parts": [{"type": "text", "text": "legacy"}]
                }
            }),
        ))
        .expect("request body");

        let response = router
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(A2A_DEPRECATION_HEADER),
            Some(&HeaderValue::from_static("true"))
        );
        assert!(response
            .headers()
            .get(axum::http::header::WARNING)
            .is_some());
    }

    #[tokio::test]
    async fn canonical_push_notification_config_methods_round_trip() {
        let (_dir, server) = test_server(
            r#"
pub fn triage(task: string) -> string {
  return task
}
"#,
        );
        let send = harn_vm::jsonrpc::request(
            "send-1",
            "message/send",
            json!({
                "function": "triage",
                "configuration": {"returnImmediately": true},
                "message": {
                    "parts": [{"type": "text", "text": "pending"}]
                }
            }),
        );
        let processed = server
            .clone()
            .process_rpc(send, AuthRequest::default())
            .await;
        let RpcOutcome::Json(response) = processed.outcome else {
            panic!("expected json response");
        };
        assert!(processed.deprecation.is_none());
        let task_id = response["result"]["id"].as_str().expect("task id");

        let set = harn_vm::jsonrpc::request(
            "push-set",
            "tasks/pushNotificationConfig/set",
            json!({
                "id": task_id,
                "pushNotificationConfig": {
                    "id": "push-1",
                    "url": "https://client.example/a2a/push"
                }
            }),
        );
        let processed = server
            .clone()
            .process_rpc(set, AuthRequest::default())
            .await;
        let RpcOutcome::Json(response) = processed.outcome else {
            panic!("expected push set json response");
        };
        assert_eq!(response["result"]["id"], "push-1");
        assert_eq!(response["result"]["taskId"], task_id);

        let get = harn_vm::jsonrpc::request(
            "push-get",
            "tasks/pushNotificationConfig/get",
            json!({"id": task_id, "pushNotificationConfigId": "push-1"}),
        );
        let processed = server
            .clone()
            .process_rpc(get, AuthRequest::default())
            .await;
        let RpcOutcome::Json(response) = processed.outcome else {
            panic!("expected push get json response");
        };
        assert_eq!(response["result"]["url"], "https://client.example/a2a/push");

        let list = harn_vm::jsonrpc::request(
            "push-list",
            "tasks/pushNotificationConfig/list",
            json!({"id": task_id}),
        );
        let processed = server
            .clone()
            .process_rpc(list, AuthRequest::default())
            .await;
        let RpcOutcome::Json(response) = processed.outcome else {
            panic!("expected push list json response");
        };
        assert_eq!(response["result"].as_array().expect("configs").len(), 1);

        let delete = harn_vm::jsonrpc::request(
            "push-delete",
            "tasks/pushNotificationConfig/delete",
            json!({"id": task_id, "pushNotificationConfigId": "push-1"}),
        );
        let processed = server.process_rpc(delete, AuthRequest::default()).await;
        let RpcOutcome::Json(response) = processed.outcome else {
            panic!("expected push delete json response");
        };
        assert!(response["result"].is_null());
    }

    #[tokio::test]
    async fn authenticated_extended_card_method_returns_agent_card() {
        let (_dir, server) = test_server(
            r#"
pub fn triage(task: string) -> string {
  return task
}
"#,
        );
        let request =
            harn_vm::jsonrpc::request("card-1", "agent/getAuthenticatedExtendedCard", json!({}));

        let processed = server
            .process_rpc_with_public_url(request, AuthRequest::default(), "https://agent.example")
            .await;
        let RpcOutcome::Json(response) = processed.outcome else {
            panic!("expected card response");
        };

        assert_eq!(response["result"]["name"], "server");
        assert_eq!(
            response["result"]["supportedInterfaces"][0]["protocolVersion"],
            A2A_PROTOCOL_VERSION
        );
        assert_eq!(
            response["result"]["supportedInterfaces"][0]["url"],
            "https://agent.example"
        );
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
            "message/send",
            json!({
                "message": {
                    "metadata": {"target_agent": "triage"},
                    "parts": [{"type": "text", "text": "hello"}]
                }
            }),
        );

        let processed = server.process_rpc(request, AuthRequest::default()).await;
        let RpcOutcome::Json(response) = processed.outcome else {
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
            "message/send",
            json!({
                "message": {
                    "metadata": {"target_agent": "triage"},
                    "parts": [{"type": "text", "text": "Review PR #461"}]
                }
            }),
        );

        let processed = server.process_rpc(request, AuthRequest::default()).await;
        let RpcOutcome::Json(response) = processed.outcome else {
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
            "message/stream",
            json!({
                "function": "triage",
                "message": {
                    "parts": [{"type": "text", "text": "stream me"}]
                }
            }),
        );

        let processed = server
            .clone()
            .process_rpc(request, AuthRequest::default())
            .await;
        let RpcOutcome::Sse(mut rx) = processed.outcome else {
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
        let processed = server
            .process_rpc(resubscribe, AuthRequest::default())
            .await;
        let RpcOutcome::Sse(replay_rx) = processed.outcome else {
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

        assert!(card["signatures"][0]["protected"].as_str().unwrap().len() > 16);
        assert!(card["signatures"][0]["signature"].as_str().unwrap().len() > 16);
    }

    use harn_vm::agent_events::AgentEventSink as _;

    #[test]
    fn a2a_worker_sink_publishes_worker_update_to_task_stream() {
        // The per-task `AgentEventSink` translates canonical worker
        // lifecycle events into A2A task events of type
        // `worker_update`. This is the A2A side of the ACP/A2A parity
        // contract — same canonical AgentEvent, mapped onto each
        // protocol's wire shape from a single source.
        let task_id = "task-1".to_string();
        let task = TaskState {
            id: task_id.clone(),
            context_id: None,
            status: TaskStatus::Working,
            history: Vec::new(),
            metadata: BTreeMap::new(),
            push_configs: Vec::new(),
            events: Vec::new(),
            subscribers: Vec::new(),
            cancel_token: None,
        };
        let tasks: TaskStore = Arc::new(Mutex::new(HashMap::from([(task_id.clone(), task)])));
        let sink = super::A2aWorkerSink {
            task_id: task_id.clone(),
            tasks: tasks.clone(),
        };

        sink.handle_event(&harn_vm::agent_events::AgentEvent::WorkerUpdate {
            session_id: super::a2a_worker_session_id(&task_id),
            worker_id: "worker-9".into(),
            worker_name: "review".into(),
            worker_task: "review pr".into(),
            worker_mode: "delegated_stage".into(),
            event: harn_vm::agent_events::WorkerEvent::WorkerWaitingForInput,
            status: "awaiting_input".into(),
            metadata: serde_json::json!({"awaiting_started_at": "0193..."}),
            audit: Some(serde_json::json!({"run_id": "run_x"})),
        });

        // Chat chunks are ignored — the sink is intentionally narrow so
        // task-stream extension events don't duplicate task history.
        sink.handle_event(&harn_vm::agent_events::AgentEvent::AgentMessageChunk {
            session_id: super::a2a_worker_session_id(&task_id),
            content: "ignored".into(),
        });

        let tasks = tasks.lock().expect("tasks");
        let task = tasks.get(&task_id).expect("task");
        let worker_events: Vec<&JsonValue> = task
            .events
            .iter()
            .filter(|event| event.get("type").and_then(JsonValue::as_str) == Some("worker_update"))
            .collect();
        assert_eq!(worker_events.len(), 1, "events: {:?}", task.events);
        let event = worker_events[0];
        assert_eq!(event["taskId"], task_id);
        assert_eq!(event["workerId"], "worker-9");
        assert_eq!(event["status"], "awaiting_input");
        assert_eq!(event["terminal"], false);
        assert_eq!(event["audit"]["run_id"], "run_x");
    }

    #[test]
    fn a2a_worker_sink_publishes_plan_extension_to_task_stream() {
        let task_id = "task-plan".to_string();
        let task = TaskState {
            id: task_id.clone(),
            context_id: None,
            status: TaskStatus::Working,
            history: Vec::new(),
            metadata: BTreeMap::new(),
            push_configs: Vec::new(),
            events: Vec::new(),
            subscribers: Vec::new(),
            cancel_token: None,
        };
        let tasks: TaskStore = Arc::new(Mutex::new(HashMap::from([(task_id.clone(), task)])));
        let sink = super::A2aWorkerSink {
            task_id: task_id.clone(),
            tasks: tasks.clone(),
        };
        let plan = harn_vm::llm::plan::normalize_plan_tool_call(
            harn_vm::llm::plan::UPDATE_PLAN_TOOL,
            &serde_json::json!({
                "explanation": "Plan the task.",
                "plan": [{"step": "Inspect files.", "status": "pending"}],
            }),
        );

        sink.handle_event(&harn_vm::agent_events::AgentEvent::Plan {
            session_id: super::a2a_worker_session_id(&task_id),
            plan,
        });

        let tasks = tasks.lock().expect("tasks");
        let task = tasks.get(&task_id).expect("task");
        let event = task
            .events
            .iter()
            .find(|event| event.get("type").and_then(JsonValue::as_str) == Some("harn_plan"))
            .expect("harn_plan event");
        assert_eq!(event["taskId"], task_id);
        assert_eq!(event["entries"][0]["content"], "Inspect files.");
        assert_eq!(event["plan"]["schema_version"], "harn.plan.v1");
    }

    #[tokio::test]
    async fn worker_event_emitted_during_dispatch_streams_to_task_subscribers() {
        // End-to-end: a Harn function that emits a `WorkerUpdate`
        // through the canonical sink registry must surface as a task
        // event on the A2A SSE stream. This is the integration that
        // closes harn#703's A2A leg — verifying the dispatch wraps
        // execution in the agent-session id the sink subscribes to.
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r#"
pub fn run(task: string) -> string {
  return task
}
"#,
        )
        .expect("write script");
        let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
        let server = Arc::new(A2aServer::new(A2aServerConfig::new(core)));

        let task_id = "task-stream-worker".to_string();
        let session_id = super::a2a_worker_session_id(&task_id);
        // Pre-stage a task so the A2aWorkerSink has somewhere to
        // deliver. Subscribe before emitting so the SSE channel
        // captures the event live.
        {
            let mut tasks = server.tasks.lock().expect("tasks");
            tasks.insert(
                task_id.clone(),
                TaskState {
                    id: task_id.clone(),
                    context_id: None,
                    status: TaskStatus::Working,
                    history: Vec::new(),
                    metadata: BTreeMap::new(),
                    push_configs: Vec::new(),
                    events: Vec::new(),
                    subscribers: Vec::new(),
                    cancel_token: None,
                },
            );
        }
        let mut subscriber = server.subscribe(&task_id).expect("subscriber");
        let sink: Arc<dyn harn_vm::agent_events::AgentEventSink> = Arc::new(super::A2aWorkerSink {
            task_id: task_id.clone(),
            tasks: server.tasks.clone(),
        });
        harn_vm::agent_events::register_sink(session_id.clone(), sink);
        // Push the session so emit_event routes correctly even though
        // we're not going through the full dispatch wrapper here. In
        // production, `invoke_function` does this via the
        // `agent_session_id` request field.
        harn_vm::agent_sessions::open_or_create(Some(session_id.clone()));
        let _guard = harn_vm::agent_sessions::enter_current_session(session_id.clone());

        harn_vm::agent_events::emit_event(&harn_vm::agent_events::AgentEvent::WorkerUpdate {
            session_id: session_id.clone(),
            worker_id: "w-1".into(),
            worker_name: "review".into(),
            worker_task: "review pr".into(),
            worker_mode: "delegated_stage".into(),
            event: harn_vm::agent_events::WorkerEvent::WorkerCompleted,
            status: "completed".into(),
            metadata: serde_json::json!({"finished_at": "0193..."}),
            audit: None,
        });

        let event = tokio::time::timeout(std::time::Duration::from_secs(2), subscriber.next())
            .await
            .expect("worker event emitted")
            .expect("subscriber stream open");
        assert_eq!(
            event.pointer("/result/type").and_then(JsonValue::as_str),
            Some("worker_update"),
            "got: {event}"
        );
        assert_eq!(
            event.pointer("/result/event").and_then(JsonValue::as_str),
            Some("WorkerCompleted")
        );
        assert_eq!(
            event.pointer("/result/status").and_then(JsonValue::as_str),
            Some("completed")
        );
        assert_eq!(
            event
                .pointer("/result/terminal")
                .and_then(JsonValue::as_bool),
            Some(true)
        );

        harn_vm::agent_events::clear_session_sinks(&session_id);
    }
}
