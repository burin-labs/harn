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
use harn_vm::event_log::{AnyEventLog, EventLog, LogEvent, Topic};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{json, Value as JsonValue};
use sha2::Sha256;
use tokio::sync::{mpsc, oneshot};
use tokio::task::LocalSet;
use uuid::Uuid;

use crate::{
    AdapterDescriptor, AuthMethodConfig, AuthPolicy, AuthRequest, AuthorizationDecision,
    CallArguments, CallRequest, CallResponse, DispatchCore, DispatchError, ExportCatalog,
    HttpTlsConfig, TransportAdapter,
};

pub const A2A_PROTOCOL_VERSION: &str = "0.3.0";

const A2A_VERSION_HEADER: &str = "a2a-version";
const A2A_TRACE_HEADER: &str = "a2a-trace-id";
const A2A_DEPRECATION_HEADER: &str = "deprecation";
const A2A_AGENT_CARD_PATH: &str = "/.well-known/agent-card.json";
const A2A_AUTH_REALM: &str = "harn-a2a";

const A2A_TASK_NOT_FOUND: i64 = -32001;
const A2A_TASK_NOT_CANCELABLE: i64 = -32002;
const A2A_UNSUPPORTED_OPERATION: i64 = -32003;
const A2A_EXTENDED_AGENT_CARD_NOT_CONFIGURED: i64 = -32007;
const A2A_PUSH_CONFIG_TOPIC: &str = "a2a.push_notification_configs";
const A2A_PUSH_CONFIG_SET_KIND: &str = "a2a.push_notification_config.set";
const A2A_PUSH_CONFIG_DELETE_KIND: &str = "a2a.push_notification_config.delete";

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
    core: Arc<DispatchCore>,
    executor: ExecutionRuntime,
    tasks: TaskStore,
    push_configs: PushConfigStore,
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
    /// The agent has paused execution and is waiting on the client to
    /// supply input that the script asked for via a HITL primitive
    /// (`ask_user`, `request_approval`, `dual_control`, `escalate`).
    /// Non-terminal: the task transitions back to `Working` once the
    /// HITL response arrives and the waitpoint resumes.
    InputRequired,
    /// The agent's execution requires fresh credentials before it can
    /// continue. Surfaced when a downstream call fails with an auth
    /// classification mid-task. Non-terminal per A2A 0.3.0: the client
    /// is expected to re-authenticate and resubscribe.
    AuthRequired,
    Completed,
    Failed,
    Cancelled,
    /// The server declined to accept the task. Surfaced synchronously
    /// when the dispatch core's `AuthPolicy` denies the caller before
    /// any work runs. Terminal — the client must adjust its request
    /// (or its credentials at the policy layer) and submit a new task.
    Rejected,
}

impl TaskStatus {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Submitted => "submitted",
            Self::Working => "working",
            Self::InputRequired => "input-required",
            Self::AuthRequired => "auth-required",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Rejected => "rejected",
        }
    }

    fn is_terminal(&self) -> bool {
        matches!(
            self,
            Self::Completed | Self::Failed | Self::Cancelled | Self::Rejected
        )
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
    artifacts: Vec<JsonValue>,
    metadata: BTreeMap<String, JsonValue>,
    events: Vec<JsonValue>,
    subscribers: Vec<UnboundedSender<JsonValue>>,
    cancel_token: Option<Arc<AtomicBool>>,
}

type TaskStore = Arc<Mutex<HashMap<String, TaskState>>>;
type PushConfigStore = Arc<Mutex<HashMap<String, BTreeMap<String, JsonValue>>>>;

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
    /// HTTP status override. When `None`, the transport applies its
    /// default (200 for ok, 400 for json-rpc errors). Set this for
    /// transport-level outcomes that aren't expressed by the JSON-RPC
    /// body — e.g. 401 for unauthenticated extended-card requests.
    status: Option<StatusCode>,
    /// `WWW-Authenticate` header value, paired with a 401 status to
    /// advertise the auth scheme(s) the caller can satisfy.
    auth_challenge: Option<HeaderValue>,
}

impl A2aServer {
    pub fn new(config: A2aServerConfig) -> Self {
        let agent_name = config
            .agent_name
            .unwrap_or_else(|| derived_agent_name(config.core.catalog()));
        let core = Arc::new(config.core);
        let catalog = core.catalog().clone();
        let push_configs = Arc::new(Mutex::new(load_push_configs(&core.event_log())));
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
            executor: ExecutionRuntime::start(core.clone()),
            core,
            tasks: Arc::new(Mutex::new(HashMap::new())),
            push_configs,
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
            .map(public_skill_card)
            .collect::<Vec<_>>();
        let extended_supported = self.extended_card_available();
        let (security_schemes, security) = if extended_supported {
            policy_security_schemes(self.core.auth_policy())
        } else {
            (json!({}), json!([]))
        };
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
            "securitySchemes": security_schemes,
            "security": security,
            "supportsAuthenticatedExtendedCard": extended_supported,
            "defaultInputModes": ["application/json", "text/plain", "application/octet-stream"],
            "defaultOutputModes": ["application/json", "text/plain", "application/octet-stream"],
            "capabilities": {
                "streaming": true,
                "pushNotifications": true,
                "extendedAgentCard": extended_supported
            },
            "skills": skills
        });
        if let Some(secret) = self.card_signing_secret.as_deref() {
            sign_card(&mut card, secret);
        }
        card
    }

    /// Authenticated extended card. The public card advertises the
    /// available skills with a generic description and the set of
    /// declared security schemes; the extended card adds per-skill
    /// `outputModes` detail (currently identical to the public card),
    /// includes the authenticated principal's subject so callers can
    /// verify the auth round-trip, and tags itself with
    /// `metadata.extendedAgentCard: true` so it cannot be confused with
    /// the public card.
    fn extended_agent_card(&self, public_url: &str, principal_subject: &str) -> JsonValue {
        let mut card = self.agent_card(public_url);
        if let Some(object) = card.as_object_mut() {
            object.insert(
                "metadata".to_string(),
                json!({
                    "extendedAgentCard": true,
                    "principal": principal_subject,
                }),
            );
            // Mirror declared schemes/requirements onto the extended
            // card. They are also on the public card when the feature
            // is enabled, but a future change might choose to omit
            // them publicly while keeping the extended card intact.
            let (security_schemes, security) = policy_security_schemes(self.core.auth_policy());
            object.insert("securitySchemes".to_string(), security_schemes);
            object.insert("security".to_string(), security);
            object.insert(
                "skills".to_string(),
                JsonValue::Array(
                    self.catalog
                        .functions
                        .values()
                        .map(extended_skill_card)
                        .collect(),
                ),
            );
        }
        card
    }

    fn extended_card_available(&self) -> bool {
        !self.core.auth_policy().methods.is_empty()
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

        let mut status: Option<StatusCode> = None;
        let mut auth_challenge: Option<HeaderValue> = None;
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
                match self.prepare_task(&params, auth).await {
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
                match self.prepare_task(&params, auth).await {
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
                match task_id {
                    Some(id) => match self.add_push_config(id, config).await {
                        Ok(config) => (
                            RpcOutcome::Json(task_rpc_response(&rpc_id, config)),
                            deprecation,
                        ),
                        Err(error) => (
                            RpcOutcome::Json(push_config_error_response(rpc_id, &error)),
                            deprecation,
                        ),
                    },
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
                match self.push_configs(task_id) {
                    Ok(configs) => (RpcOutcome::Json(task_rpc_response(&rpc_id, configs)), None),
                    Err(error) => (
                        RpcOutcome::Json(push_config_error_response(rpc_id, &error)),
                        None,
                    ),
                }
            }
            "tasks/pushNotificationConfig/delete" => {
                let task_id = task_id_param(&params);
                let config_id = push_config_id_param(&params);
                match task_id.zip(config_id) {
                    Some((task_id, config_id)) => {
                        match self.delete_push_config(task_id, config_id).await {
                            Ok(()) => (
                                RpcOutcome::Json(task_rpc_response(&rpc_id, JsonValue::Null)),
                                None,
                            ),
                            Err(error) => (
                                RpcOutcome::Json(push_config_error_response(rpc_id, &error)),
                                None,
                            ),
                        }
                    }
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
            "agent/getAuthenticatedExtendedCard" => {
                let policy = self.core.auth_policy();
                if policy.methods.is_empty() {
                    (
                        RpcOutcome::Json(error_response(
                            rpc_id,
                            A2A_EXTENDED_AGENT_CARD_NOT_CONFIGURED,
                            "ExtendedAgentCardNotConfiguredError: agent has no authentication methods configured",
                        )),
                        None,
                    )
                } else {
                    match policy.authorize(&auth).await {
                        AuthorizationDecision::Authorized(principal) => (
                            RpcOutcome::Json(task_rpc_response(
                                &rpc_id,
                                self.extended_agent_card(public_url, &principal.subject),
                            )),
                            None,
                        ),
                        AuthorizationDecision::Rejected(message) => {
                            status = Some(StatusCode::UNAUTHORIZED);
                            auth_challenge = Some(www_authenticate_header(policy));
                            (
                                RpcOutcome::Json(error_response(
                                    rpc_id,
                                    -32000,
                                    &format!("Unauthorized: {message}"),
                                )),
                                None,
                            )
                        }
                    }
                }
            }
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
            status,
            auth_challenge,
        }
    }

    async fn prepare_task(
        &self,
        params: &JsonValue,
        auth: AuthRequest,
    ) -> Result<PreparedTask, A2aPrepareError> {
        let parts = message_parts(params)?;
        let text = message_text(params, &parts);
        let function = select_function(&self.catalog, params)?;
        let arguments = message_arguments(
            self.catalog
                .function(&function)
                .expect("selected function exists"),
            params,
            &parts,
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
                parts: parts.clone(),
            }],
            artifacts: a2a_artifacts_from_parts(&parts),
            metadata: BTreeMap::new(),
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

        if let Some(push_config) = push_config {
            if let Err(error) = self.add_push_config(&task_id, push_config).await {
                self.tasks.lock().expect("tasks poisoned").remove(&task_id);
                return Err(A2aPrepareError::new(-32603, error));
            }
        }

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
            // The dispatch core's `AuthPolicy.authorize` is what produces
            // `DispatchError::Unauthorized`. It runs synchronously at the
            // start of `core.dispatch` before any script work, so the
            // policy denial is "the server declined this task" — A2A
            // 0.3.0's `rejected` terminal state. Any post-policy auth
            // failure (e.g. an LLM/HTTP 401 raised by the script itself)
            // surfaces through `Execution(...)` with an `auth`-classified
            // message and maps to non-terminal `auth-required` so the
            // client can resupply credentials and resubscribe.
            Err(DispatchError::Unauthorized(message)) => self.reject_task(&task.id, &message),
            Err(DispatchError::Execution(message))
                if matches!(
                    harn_vm::value::classify_error_message(&message),
                    harn_vm::value::ErrorCategory::Auth
                ) =>
            {
                self.auth_required_task(&task.id, &message);
            }
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
        let parts = response_parts(&response.value);
        let artifacts = response_artifacts(&response.value, &parts);
        let handoff_metadata = handoff_task_metadata(&response);
        let message = json!({
            "type": "message",
            "taskId": task_id,
            "message": {
                "id": Uuid::now_v7().to_string(),
                "role": "agent",
                "parts": parts
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
                parts,
            });
            task.artifacts.extend(artifacts);
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
        self.terminate_task(task_id, TaskStatus::Failed, message);
    }

    /// Terminal — the dispatch core's `AuthPolicy` synchronously denied
    /// the caller before any script work could run. The A2A spec calls
    /// this `rejected`: the client cannot resume by re-authing or
    /// retrying, it has to adjust its request (or the server-side
    /// policy) and send a new task.
    fn reject_task(&self, task_id: &str, message: &str) {
        self.terminate_task(task_id, TaskStatus::Rejected, message);
    }

    fn terminate_task(&self, task_id: &str, status: TaskStatus, message: &str) {
        debug_assert!(
            status.is_terminal(),
            "terminate_task expects a terminal status"
        );
        let event = json!({
            "type": "status",
            "taskId": task_id,
            "status": {"state": status.as_str()},
            "error": message,
        });
        let task_for_push = {
            let mut tasks = self.tasks.lock().expect("tasks poisoned");
            let Some(task) = tasks.get_mut(task_id) else {
                return;
            };
            task.status = status;
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

    /// Non-terminal — a downstream auth check failed mid-task (the
    /// script itself raised an `auth`-classified error). The client is
    /// expected to refresh its credentials and resubscribe; the task
    /// remains in the store so a follow-up `tasks/resubscribe` finds it.
    /// Subscribers are kept attached because the state is non-terminal.
    fn auth_required_task(&self, task_id: &str, message: &str) {
        let event = json!({
            "type": "status",
            "taskId": task_id,
            "status": {"state": TaskStatus::AuthRequired.as_str()},
            "error": message,
        });
        let task_for_push = {
            let mut tasks = self.tasks.lock().expect("tasks poisoned");
            let Some(task) = tasks.get_mut(task_id) else {
                return;
            };
            task.status = TaskStatus::AuthRequired;
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

    async fn add_push_config(
        &self,
        task_id: &str,
        mut config: JsonValue,
    ) -> Result<JsonValue, String> {
        if !self.push_config_task_known(task_id) {
            return Err(format!("TaskNotFoundError: {task_id}"));
        }
        if config.get("id").and_then(JsonValue::as_str).is_none() {
            config["id"] = JsonValue::String(Uuid::now_v7().to_string());
        }
        config["taskId"] = JsonValue::String(task_id.to_string());
        let config_id = config["id"].as_str().unwrap_or_default().to_string();

        self.append_push_config_event(
            A2A_PUSH_CONFIG_SET_KIND,
            json!({
                "taskId": task_id,
                "configId": config_id,
                "config": config,
            }),
        )
        .await?;
        self.apply_push_config_set(task_id, &config_id, config.clone());
        Ok(config)
    }

    fn push_config(&self, task_id: &str, config_id: Option<&str>) -> Result<JsonValue, String> {
        let configs = self.push_configs_for_task(task_id)?;
        let config = if let Some(config_id) = config_id {
            configs.into_iter().find(|config| {
                config
                    .get("id")
                    .and_then(JsonValue::as_str)
                    .is_some_and(|id| id == config_id)
            })
        } else {
            configs.into_iter().next()
        };
        config.ok_or_else(|| format!("TaskPushNotificationConfigNotFoundError: {task_id}"))
    }

    fn push_configs(&self, task_id: Option<&str>) -> Result<JsonValue, String> {
        let configs = if let Some(task_id) = task_id {
            self.push_configs_for_task(task_id)?
        } else {
            self.push_configs
                .lock()
                .expect("push configs poisoned")
                .values()
                .flat_map(|configs| configs.values().cloned())
                .collect::<Vec<_>>()
        };
        Ok(JsonValue::Array(configs))
    }

    async fn delete_push_config(&self, task_id: &str, config_id: &str) -> Result<(), String> {
        if !self
            .push_configs
            .lock()
            .expect("push configs poisoned")
            .get(task_id)
            .is_some_and(|configs| configs.contains_key(config_id))
        {
            return Err(format!(
                "TaskPushNotificationConfigNotFoundError: {task_id}/{config_id}"
            ));
        }
        self.append_push_config_event(
            A2A_PUSH_CONFIG_DELETE_KIND,
            json!({
                "taskId": task_id,
                "configId": config_id,
            }),
        )
        .await?;
        self.apply_push_config_delete(task_id, config_id);
        Ok(())
    }

    fn push_config_task_known(&self, task_id: &str) -> bool {
        self.tasks
            .lock()
            .expect("tasks poisoned")
            .contains_key(task_id)
            || self
                .push_configs
                .lock()
                .expect("push configs poisoned")
                .contains_key(task_id)
    }

    fn push_configs_for_task(&self, task_id: &str) -> Result<Vec<JsonValue>, String> {
        if let Some(configs) = self
            .push_configs
            .lock()
            .expect("push configs poisoned")
            .get(task_id)
        {
            return Ok(configs.values().cloned().collect());
        }
        if self
            .tasks
            .lock()
            .expect("tasks poisoned")
            .contains_key(task_id)
        {
            return Ok(Vec::new());
        }
        Err(format!("TaskNotFoundError: {task_id}"))
    }

    async fn append_push_config_event(
        &self,
        kind: &'static str,
        payload: JsonValue,
    ) -> Result<(), String> {
        let topic = push_config_topic();
        let log = self.core.event_log();
        log.append(&topic, LogEvent::new(kind, payload))
            .await
            .map_err(|error| format!("EventLogError: {error}"))?;
        log.flush()
            .await
            .map_err(|error| format!("EventLogError: {error}"))
    }

    fn apply_push_config_set(&self, task_id: &str, config_id: &str, config: JsonValue) {
        self.push_configs
            .lock()
            .expect("push configs poisoned")
            .entry(task_id.to_string())
            .or_default()
            .insert(config_id.to_string(), config);
    }

    fn apply_push_config_delete(&self, task_id: &str, config_id: &str) {
        if let Some(configs) = self
            .push_configs
            .lock()
            .expect("push configs poisoned")
            .get_mut(task_id)
        {
            configs.remove(config_id);
        }
    }

    fn deliver_push(&self, task: JsonValue) {
        let task_id = task["id"].as_str().unwrap_or_default();
        let configs = self
            .push_configs
            .lock()
            .expect("push configs poisoned")
            .get(task_id)
            .map(|configs| configs.values().cloned().collect::<Vec<_>>())
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
    log_legacy_version_header(&headers);
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
    log_legacy_version_header(&headers);
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
    let auth_challenge = processed.auth_challenge.clone();
    let response = match processed.outcome {
        RpcOutcome::Json(response) if response.get("error").is_some() => {
            let status = processed.status.unwrap_or(StatusCode::BAD_REQUEST);
            response_with_deprecation(
                (status, Json(response)).into_response(),
                processed.deprecation,
            )
        }
        RpcOutcome::Json(response) => {
            response_with_deprecation(Json(response["result"].clone()), processed.deprecation)
        }
        RpcOutcome::Sse(rx) => response_with_deprecation(sse_response(rx), processed.deprecation),
    };
    apply_auth_challenge(response, auth_challenge)
}

fn rpc_response(processed: ProcessedRpc) -> Response {
    let auth_challenge = processed.auth_challenge.clone();
    let response = match processed.outcome {
        RpcOutcome::Json(response) => {
            let mut http = response_with_deprecation(Json(response), processed.deprecation);
            if let Some(status) = processed.status {
                *http.status_mut() = status;
            }
            http
        }
        RpcOutcome::Sse(rx) => response_with_deprecation(sse_response(rx), processed.deprecation),
    };
    apply_auth_challenge(response, auth_challenge)
}

fn apply_auth_challenge(mut response: Response, challenge: Option<HeaderValue>) -> Response {
    if let Some(value) = challenge {
        response
            .headers_mut()
            .insert(axum::http::header::WWW_AUTHENTICATE, value);
    }
    response
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
        "artifacts": task.artifacts.clone(),
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

fn push_config_error_response(rpc_id: JsonValue, message: &str) -> JsonValue {
    if message.starts_with("EventLogError:") {
        return error_response(rpc_id, -32603, message);
    }
    error_response(rpc_id, A2A_TASK_NOT_FOUND, message)
}

fn push_config_topic() -> Topic {
    Topic::new(A2A_PUSH_CONFIG_TOPIC).expect("valid A2A push config topic")
}

fn load_push_configs(log: &Arc<AnyEventLog>) -> HashMap<String, BTreeMap<String, JsonValue>> {
    futures::executor::block_on(async {
        let topic = push_config_topic();
        let mut store = HashMap::<String, BTreeMap<String, JsonValue>>::new();
        let mut cursor = None;
        loop {
            let events = match log.read_range(&topic, cursor, 512).await {
                Ok(events) => events,
                Err(error) => {
                    tracing::warn!(
                        target: "harn_serve::a2a",
                        %error,
                        "failed to replay A2A push notification configs"
                    );
                    break;
                }
            };
            if events.is_empty() {
                break;
            }
            for (event_id, event) in events {
                apply_persisted_push_config_event(&mut store, event);
                cursor = Some(event_id);
            }
        }
        store
    })
}

fn apply_persisted_push_config_event(
    store: &mut HashMap<String, BTreeMap<String, JsonValue>>,
    event: LogEvent,
) {
    let Some(task_id) = event.payload.get("taskId").and_then(JsonValue::as_str) else {
        return;
    };
    let Some(config_id) = event.payload.get("configId").and_then(JsonValue::as_str) else {
        return;
    };
    match event.kind.as_str() {
        A2A_PUSH_CONFIG_SET_KIND => {
            let Some(config) = event.payload.get("config").cloned() else {
                return;
            };
            store
                .entry(task_id.to_string())
                .or_default()
                .insert(config_id.to_string(), config);
        }
        A2A_PUSH_CONFIG_DELETE_KIND => {
            if let Some(configs) = store.get_mut(task_id) {
                configs.remove(config_id);
            }
        }
        _ => {}
    }
}

/// Soft-deprecation observer for the legacy `a2a-version` request header.
///
/// A2A 0.3.0 negotiates protocol version through AgentCard discovery, not via
/// request headers. We no longer reject requests carrying `a2a-version`; we
/// just log a warning so we can spot residual client usage during the
/// deprecation window. Slated for full removal one minor cycle after v0.7.x.
fn log_legacy_version_header(headers: &HeaderMap) {
    if let Some(version) = headers
        .get(A2A_VERSION_HEADER)
        .and_then(|value| value.to_str().ok())
    {
        tracing::warn!(
            target: "harn_serve::a2a",
            requested_version = %version,
            supported_version = %A2A_PROTOCOL_VERSION,
            "a2a-version request header is deprecated; clients should negotiate via AgentCard discovery"
        );
    }
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

fn message_parts(params: &JsonValue) -> Result<Vec<JsonValue>, A2aPrepareError> {
    let parts = params
        .pointer("/message/parts")
        .and_then(JsonValue::as_array)
        .map(|parts| {
            parts
                .iter()
                .enumerate()
                .map(|(index, part)| normalize_part(part, index))
                .collect::<Result<Vec<_>, _>>()
        })
        .transpose()?;
    if let Some(parts) = parts {
        if !parts.is_empty() {
            return Ok(parts);
        }
    }
    Ok(vec![json!({
        "type": "text",
        "text": params
            .get("text")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
    })])
}

fn message_text(params: &JsonValue, parts: &[JsonValue]) -> String {
    let text = parts
        .iter()
        .filter(|part| part_kind(part) == Some("text"))
        .filter_map(|part| part.get("text").and_then(JsonValue::as_str))
        .collect::<Vec<_>>()
        .join("\n\n");
    if text.is_empty() {
        params
            .get("text")
            .and_then(JsonValue::as_str)
            .unwrap_or_default()
            .to_string()
    } else {
        text
    }
}

fn normalize_part(part: &JsonValue, index: usize) -> Result<JsonValue, A2aPrepareError> {
    let Some(object) = part.as_object() else {
        return Err(A2aPrepareError::new(
            -32602,
            format!("A2A message part {index} must be an object"),
        ));
    };
    let kind = part_kind(part);
    match kind {
        Some("text") | None if object.contains_key("text") => {
            let text = part
                .get("text")
                .and_then(JsonValue::as_str)
                .ok_or_else(|| {
                    A2aPrepareError::new(-32602, format!("A2A text part {index} requires text"))
                })?;
            let mut normalized = json!({"type": "text", "text": text});
            copy_optional_part_fields(part, &mut normalized);
            Ok(normalized)
        }
        Some("file") | None if object.contains_key("file") || has_flat_file_fields(part) => {
            let file = normalize_file_part(part, index)?;
            let mut normalized = json!({"type": "file", "file": file});
            copy_optional_part_fields(part, &mut normalized);
            Ok(normalized)
        }
        Some("data") | None if object.contains_key("data") => {
            let data = part.get("data").cloned().ok_or_else(|| {
                A2aPrepareError::new(-32602, format!("A2A data part {index} requires data"))
            })?;
            let mut normalized = json!({"type": "data", "data": data});
            copy_optional_part_fields(part, &mut normalized);
            Ok(normalized)
        }
        Some(kind) => Err(A2aPrepareError::new(
            -32602,
            format!("unsupported A2A message part type '{kind}' at index {index}"),
        )),
        None => Err(A2aPrepareError::new(
            -32602,
            format!("A2A message part {index} requires text, file, or data content"),
        )),
    }
}

fn part_kind(part: &JsonValue) -> Option<&str> {
    part.get("type")
        .or_else(|| part.get("kind"))
        .and_then(JsonValue::as_str)
}

fn has_flat_file_fields(part: &JsonValue) -> bool {
    part.get("bytes").is_some() || part.get("uri").is_some()
}

fn copy_optional_part_fields(source: &JsonValue, target: &mut JsonValue) {
    for field in ["metadata", "mediaType"] {
        if let Some(value) = source.get(field) {
            target[field] = value.clone();
        }
    }
}

fn normalize_file_part(part: &JsonValue, index: usize) -> Result<JsonValue, A2aPrepareError> {
    let source = part.get("file").unwrap_or(part);
    let bytes = source
        .get("bytes")
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty());
    let uri = source
        .get("uri")
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty());
    match (bytes, uri) {
        (Some(_), Some(_)) => {
            return Err(A2aPrepareError::new(
                -32602,
                format!("A2A file part {index} must contain exactly one of bytes or uri"),
            ));
        }
        (None, None) => {
            return Err(A2aPrepareError::new(
                -32602,
                format!("A2A file part {index} requires bytes or uri"),
            ));
        }
        (Some(bytes), None) => {
            base64::engine::general_purpose::STANDARD
                .decode(bytes.as_bytes())
                .map_err(|error| {
                    A2aPrepareError::new(
                        -32602,
                        format!("A2A file part {index} bytes must be base64: {error}"),
                    )
                })?;
        }
        (None, Some(_)) => {}
    }

    let mut file = json!({});
    if let Some(bytes) = bytes {
        file["bytes"] = JsonValue::String(bytes.to_string());
    }
    if let Some(uri) = uri {
        file["uri"] = JsonValue::String(uri.to_string());
    }
    if let Some(mime_type) = source
        .get("mimeType")
        .or_else(|| source.get("mime_type"))
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
    {
        file["mimeType"] = JsonValue::String(mime_type.to_string());
    } else {
        file["mimeType"] = JsonValue::String("application/octet-stream".to_string());
    }
    if let Some(name) = source
        .get("name")
        .or_else(|| source.get("filename"))
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
    {
        file["name"] = JsonValue::String(name.to_string());
    }
    Ok(file)
}

fn artifacts_from_parts(parts: &[JsonValue]) -> Vec<JsonValue> {
    parts
        .iter()
        .enumerate()
        .filter_map(|(index, part)| artifact_from_part(index, part))
        .collect()
}

fn a2a_artifacts_from_parts(parts: &[JsonValue]) -> Vec<JsonValue> {
    artifacts_from_parts(parts)
        .iter()
        .map(a2a_artifact_from_harn_artifact)
        .collect()
}

fn artifact_from_part(index: usize, part: &JsonValue) -> Option<JsonValue> {
    match part_kind(part)? {
        "file" => {
            let file = part.get("file")?;
            let id = part
                .pointer("/metadata/artifact_id")
                .or_else(|| part.pointer("/metadata/id"))
                .and_then(JsonValue::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("a2a-file-{index}"));
            Some(json!({
                "_type": "artifact",
                "id": id,
                "kind": "file",
                "title": file.get("name").and_then(JsonValue::as_str).unwrap_or("file"),
                "data": file,
                "metadata": {
                    "a2a_part_index": index,
                    "mimeType": file.get("mimeType").cloned().unwrap_or(JsonValue::Null)
                }
            }))
        }
        "data" => {
            let id = part
                .pointer("/metadata/artifact_id")
                .or_else(|| part.pointer("/metadata/id"))
                .and_then(JsonValue::as_str)
                .map(str::to_string)
                .unwrap_or_else(|| format!("a2a-data-{index}"));
            Some(json!({
                "_type": "artifact",
                "id": id,
                "kind": "data",
                "title": "structured data",
                "data": part.get("data").cloned().unwrap_or(JsonValue::Null),
                "metadata": {
                    "a2a_part_index": index
                }
            }))
        }
        _ => None,
    }
}

fn message_argument_payload(params: &JsonValue, parts: &[JsonValue], text: &str) -> JsonValue {
    let mut message = params
        .get("message")
        .cloned()
        .unwrap_or_else(|| json!({"role": "user"}));
    message["parts"] = JsonValue::Array(parts.to_vec());
    if message.get("role").and_then(JsonValue::as_str).is_none() {
        message["role"] = JsonValue::String("user".to_string());
    }

    let mut payload = json!({
        "message": message,
        "parts": parts,
        "text": text,
        "artifacts": artifacts_from_parts(parts),
    });
    if let Some(context_id) = params.get("contextId").cloned() {
        payload["contextId"] = context_id;
    }
    payload
}

fn param_accepts_structured_message(param: &crate::ExportedParam, parts: &[JsonValue]) -> bool {
    let has_non_text = parts.iter().any(|part| part_kind(part) != Some("text"));
    if !has_non_text {
        return false;
    }
    param
        .type_expr
        .as_ref()
        .is_some_and(type_expr_accepts_json_object)
        || param.input_schema.get("type").and_then(JsonValue::as_str) == Some("object")
}

fn type_expr_accepts_json_object(type_expr: &harn_parser::TypeExpr) -> bool {
    match type_expr {
        harn_parser::TypeExpr::Named(name) => name == "dict",
        harn_parser::TypeExpr::Shape(_) | harn_parser::TypeExpr::DictType(_, _) => true,
        harn_parser::TypeExpr::Union(types) | harn_parser::TypeExpr::Intersection(types) => {
            types.iter().any(type_expr_accepts_json_object)
        }
        _ => false,
    }
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
    parts: &[JsonValue],
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
    let value = if param_accepts_structured_message(param, parts) {
        message_argument_payload(params, parts, text)
    } else {
        JsonValue::String(text.to_string())
    };
    Ok(CallArguments::Named(BTreeMap::from([(
        param.name.clone(),
        value,
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

fn response_parts(value: &JsonValue) -> Vec<JsonValue> {
    for pointer in ["/parts", "/message/parts", "/result/parts"] {
        if let Some(parts) = value.pointer(pointer).and_then(JsonValue::as_array) {
            let normalized = parts
                .iter()
                .enumerate()
                .filter_map(|(index, part)| normalize_part(part, index).ok())
                .collect::<Vec<_>>();
            if !normalized.is_empty() {
                return normalized;
            }
        }
    }

    let mut parts = Vec::new();
    if let Some(text) = value
        .get("visible_text")
        .or_else(|| value.get("text"))
        .and_then(JsonValue::as_str)
        .filter(|text| !text.is_empty())
    {
        parts.push(json!({"type": "text", "text": text}));
    }

    for artifact in artifacts_in_value(value) {
        if let Some(part) = part_from_artifact(artifact) {
            parts.push(part);
        }
    }

    if parts.is_empty() {
        parts.push(json!({"type": "text", "text": response_text(value)}));
    }
    parts
}

fn response_artifacts(value: &JsonValue, parts: &[JsonValue]) -> Vec<JsonValue> {
    let artifacts = artifacts_in_value(value)
        .into_iter()
        .map(a2a_artifact_from_harn_artifact)
        .collect::<Vec<_>>();
    if artifacts.is_empty() {
        a2a_artifacts_from_parts(parts)
    } else {
        artifacts
    }
}

fn artifacts_in_value(value: &JsonValue) -> Vec<&JsonValue> {
    let mut artifacts = Vec::new();
    if is_harn_artifact(value) {
        artifacts.push(value);
    }
    for pointer in ["/artifacts", "/run/artifacts", "/result/artifacts"] {
        if let Some(items) = value.pointer(pointer).and_then(JsonValue::as_array) {
            artifacts.extend(items.iter().filter(|item| is_harn_artifact(item)));
        }
    }
    artifacts
}

fn is_harn_artifact(value: &JsonValue) -> bool {
    value.get("_type").and_then(JsonValue::as_str) == Some("artifact")
        || value.get("kind").and_then(JsonValue::as_str).is_some()
            && (value.get("data").is_some()
                || value.get("text").is_some()
                || value.get("metadata").is_some())
}

fn part_from_artifact(artifact: &JsonValue) -> Option<JsonValue> {
    let kind = artifact
        .get("kind")
        .and_then(JsonValue::as_str)
        .unwrap_or("");
    if let Some(file) = file_part_from_artifact(artifact, kind) {
        return Some(file);
    }
    if kind == "data" || kind == "handoff" {
        return Some(json!({
            "type": "data",
            "data": artifact.get("data").cloned().unwrap_or_else(|| artifact.clone()),
            "metadata": {
                "artifact_id": artifact.get("id").cloned().unwrap_or(JsonValue::Null),
                "artifact_kind": kind,
            }
        }));
    }
    None
}

fn file_part_from_artifact(artifact: &JsonValue, kind: &str) -> Option<JsonValue> {
    let data = artifact.get("data");
    let metadata = artifact.get("metadata");
    let bytes = data
        .and_then(|data| data.get("bytes").or_else(|| data.get("bytes_base64")))
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string)
        .or_else(|| {
            if matches!(kind, "workspace_file" | "file") {
                data.and_then(|data| data.get("content"))
                    .or_else(|| artifact.get("text"))
                    .and_then(JsonValue::as_str)
                    .map(|content| {
                        base64::engine::general_purpose::STANDARD.encode(content.as_bytes())
                    })
            } else {
                None
            }
        });
    let uri = data
        .and_then(|data| data.get("uri").or_else(|| data.get("url")))
        .or_else(|| {
            metadata.and_then(|metadata| metadata.get("uri").or_else(|| metadata.get("url")))
        })
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
        .map(str::to_string);
    if bytes.is_none() && uri.is_none() && !matches!(kind, "file" | "workspace_file") {
        return None;
    }
    let mut file = json!({});
    if let Some(bytes) = bytes {
        file["bytes"] = JsonValue::String(bytes);
    } else if let Some(uri) = uri {
        file["uri"] = JsonValue::String(uri);
    } else {
        return None;
    }
    file["mimeType"] = JsonValue::String(
        data.and_then(|data| data.get("mimeType").or_else(|| data.get("mime_type")))
            .or_else(|| {
                metadata.and_then(|metadata| {
                    metadata
                        .get("mimeType")
                        .or_else(|| metadata.get("mime_type"))
                })
            })
            .and_then(JsonValue::as_str)
            .filter(|value| !value.is_empty())
            .unwrap_or(if kind == "workspace_file" {
                "text/plain"
            } else {
                "application/octet-stream"
            })
            .to_string(),
    );
    if let Some(name) = data
        .and_then(|data| data.get("name").or_else(|| data.get("filename")))
        .or_else(|| metadata.and_then(|metadata| metadata.get("path")))
        .or_else(|| artifact.get("title"))
        .and_then(JsonValue::as_str)
        .filter(|value| !value.is_empty())
    {
        file["name"] = JsonValue::String(name.to_string());
    }
    Some(json!({
        "type": "file",
        "file": file,
        "metadata": {
            "artifact_id": artifact.get("id").cloned().unwrap_or(JsonValue::Null),
            "artifact_kind": kind,
        }
    }))
}

fn a2a_artifact_from_harn_artifact(artifact: &JsonValue) -> JsonValue {
    let part = part_from_artifact(artifact).unwrap_or_else(|| {
        json!({
            "type": "data",
            "data": artifact,
        })
    });
    let mut value = json!({
        "artifactId": artifact
            .get("id")
            .and_then(JsonValue::as_str)
            .unwrap_or("artifact"),
        "name": artifact
            .get("title")
            .or_else(|| artifact.get("kind"))
            .and_then(JsonValue::as_str)
            .unwrap_or("artifact"),
        "parts": [part],
    });
    if let Some(metadata) = artifact.get("metadata") {
        value["metadata"] = metadata.clone();
    }
    value
}

fn public_skill_card(function: &crate::ExportedFunction) -> JsonValue {
    json!({
        "id": function.name,
        "name": function.name,
        "description": format!("Invoke exported Harn function '{}'.", function.name),
        "tags": ["harn", "function"],
        "examples": [],
        "inputModes": ["application/json", "text/plain", "application/octet-stream"],
        "outputModes": ["application/json", "text/plain", "application/octet-stream"],
        "inputSchema": function.input_schema,
    })
}

fn extended_skill_card(function: &crate::ExportedFunction) -> JsonValue {
    let mut card = public_skill_card(function);
    if let Some(object) = card.as_object_mut() {
        object.insert(
            "description".to_string(),
            JsonValue::String(format!(
                "Invoke exported Harn function '{}'. Includes detailed schemas for authenticated callers.",
                function.name
            )),
        );
        // The output schema is not currently introspected from the
        // typed return value of an exported Harn function. Emit an
        // empty object so authenticated tooling can rely on the field
        // being present even when the schema is unknown.
        object.insert("outputSchema".to_string(), json!({}));
    }
    card
}

fn policy_security_schemes(policy: &AuthPolicy) -> (JsonValue, JsonValue) {
    let mut schemes = serde_json::Map::new();
    let mut requirements: Vec<JsonValue> = Vec::new();
    for method in &policy.methods {
        match method {
            AuthMethodConfig::ApiKey(_) => {
                schemes.insert(
                    "apiKey".to_string(),
                    json!({
                        "type": "apiKey",
                        "in": "header",
                        "name": "Authorization",
                        "description": "API key supplied as `Authorization: Bearer <key>` or `X-API-Key`.",
                    }),
                );
                requirements.push(json!({"apiKey": []}));
            }
            AuthMethodConfig::Hmac(config) => {
                schemes.insert(
                    "hmac".to_string(),
                    json!({
                        "type": "http",
                        "scheme": "HMAC-SHA256",
                        "description": format!(
                            "HMAC-SHA256 canonical request signature (provider '{}').",
                            config.provider
                        ),
                    }),
                );
                requirements.push(json!({"hmac": []}));
            }
            AuthMethodConfig::OAuth21(config) => {
                let mut scheme = json!({
                    "type": "oauth2",
                    "description": "OAuth 2.1 access token validated by the transport.",
                });
                if let Some(object) = scheme.as_object_mut() {
                    object.insert(
                        "issuer".to_string(),
                        JsonValue::String(config.issuer.clone()),
                    );
                    if let Some(audience) = config.audience.as_ref() {
                        object.insert("audience".to_string(), JsonValue::String(audience.clone()));
                    }
                }
                schemes.insert("oauth2".to_string(), scheme);
                let scopes = config
                    .required_scopes
                    .iter()
                    .cloned()
                    .map(JsonValue::String)
                    .collect::<Vec<_>>();
                requirements.push(json!({"oauth2": scopes}));
            }
        }
    }
    (JsonValue::Object(schemes), JsonValue::Array(requirements))
}

fn www_authenticate_header(policy: &AuthPolicy) -> HeaderValue {
    let mut schemes = Vec::new();
    for method in &policy.methods {
        match method {
            AuthMethodConfig::ApiKey(_) | AuthMethodConfig::OAuth21(_) => {
                if !schemes.contains(&"Bearer") {
                    schemes.push("Bearer");
                }
            }
            AuthMethodConfig::Hmac(_) => {
                if !schemes.contains(&"HMAC-SHA256") {
                    schemes.push("HMAC-SHA256");
                }
            }
        }
    }
    if schemes.is_empty() {
        schemes.push("Bearer");
    }
    let value = schemes
        .into_iter()
        .map(|scheme| format!("{scheme} realm=\"{A2A_AUTH_REALM}\""))
        .collect::<Vec<_>>()
        .join(", ");
    HeaderValue::from_str(&value)
        .unwrap_or_else(|_| HeaderValue::from_static("Bearer realm=\"harn-a2a\""))
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
            harn_vm::agent_events::AgentEvent::HitlRequested {
                request_id,
                kind,
                payload,
                ..
            } => {
                self.transition_input_required(request_id, kind, payload);
                return;
            }
            harn_vm::agent_events::AgentEvent::HitlResolved {
                request_id,
                kind,
                outcome,
                ..
            } => {
                self.resolve_input_required(request_id, kind, outcome);
                return;
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

impl A2aWorkerSink {
    /// Flip the task into `input-required` while a HITL primitive is
    /// blocked waiting for a response. The script remains suspended on
    /// a waitpoint; subscribers see two events — a structured `hitl`
    /// extension event carrying the request payload, then the canonical
    /// `status` transition. Idempotent for repeat HITL requests inside
    /// the same task: only the first transitions the status.
    ///
    /// No push-config webhook delivery here, mirroring the
    /// `worker_update` policy: HITL transitions stream live to active
    /// SSE subscribers and surface on `tasks/get`, but high-frequency
    /// status flips don't fan out to outbound webhook endpoints.
    fn transition_input_required(&self, request_id: &str, kind: &str, payload: &JsonValue) {
        let mut tasks = self.tasks.lock().expect("tasks poisoned");
        let Some(task) = tasks.get_mut(&self.task_id) else {
            return;
        };
        // Don't override a terminal/cancelled task: the waitpoint
        // emit can race the cancel path. Once the task is dead it
        // must stay dead.
        if task.status.is_terminal() {
            return;
        }
        let hitl_event = json!({
            "type": "hitl",
            "taskId": self.task_id,
            "phase": "requested",
            "requestId": request_id,
            "kind": kind,
            "payload": payload,
        });
        publish_locked(task, hitl_event);
        if task.status != TaskStatus::InputRequired {
            task.status = TaskStatus::InputRequired;
            publish_locked(task, status_event(&self.task_id, TaskStatus::InputRequired));
        }
    }

    /// Companion to `transition_input_required`. Flip back to `working`
    /// once the waitpoint resolves so subscribers see the task resume
    /// (or terminate naturally on the next tick if the script returned
    /// from the HITL call). Only flips out of `input-required`; if a
    /// later `auth-required` / cancellation snuck in, leave it.
    fn resolve_input_required(&self, request_id: &str, kind: &str, outcome: &str) {
        let mut tasks = self.tasks.lock().expect("tasks poisoned");
        let Some(task) = tasks.get_mut(&self.task_id) else {
            return;
        };
        let hitl_event = json!({
            "type": "hitl",
            "taskId": self.task_id,
            "phase": "resolved",
            "requestId": request_id,
            "kind": kind,
            "outcome": outcome,
        });
        publish_locked(task, hitl_event);
        if task.status == TaskStatus::InputRequired {
            task.status = TaskStatus::Working;
            publish_locked(task, status_event(&self.task_id, TaskStatus::Working));
        }
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
            json!(["application/json", "text/plain", "application/octet-stream"])
        );
        assert_eq!(
            card["defaultOutputModes"],
            json!(["application/json", "text/plain", "application/octet-stream"])
        );
        assert_eq!(card["capabilities"]["streaming"], true);
        assert_eq!(card["capabilities"]["pushNotifications"], true);
        // The default test_server configures no auth methods, so the
        // extended-card capability is advertised as unsupported.
        assert_eq!(card["capabilities"]["extendedAgentCard"], false);
        assert_eq!(card["supportsAuthenticatedExtendedCard"], false);
        assert_eq!(card["skills"][0]["id"], "triage");
        assert_eq!(card["skills"][0]["tags"], json!(["harn", "function"]));
        assert_eq!(
            card["skills"][0]["inputModes"],
            json!(["application/json", "text/plain", "application/octet-stream"])
        );
        assert_eq!(
            card["skills"][0]["outputModes"],
            json!(["application/json", "text/plain", "application/octet-stream"])
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
    async fn unknown_a2a_version_header_no_longer_rejects_request() {
        // Per A2A 0.3.0, version negotiation happens through AgentCard
        // discovery; the request header is non-canonical. A request that
        // carries an unknown `a2a-version` value must still dispatch — we
        // only log a soft-deprecation warning. The previous behavior
        // returned JSON-RPC `-32009 VersionNotSupportedError`.
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
            "version-1",
            "message/send",
            json!({
                "message": {
                    "metadata": {"target_agent": "triage"},
                    "parts": [{"type": "text", "text": "hello"}]
                }
            }),
        ))
        .expect("request body");

        for header_value in ["1.0", "0.3.0", "9.9.9", "garbage"] {
            let response = router
                .clone()
                .oneshot(
                    Request::builder()
                        .method(Method::POST)
                        .uri("/")
                        .header(axum::http::header::CONTENT_TYPE, "application/json")
                        .header(A2A_VERSION_HEADER, header_value)
                        .body(Body::from(body.clone()))
                        .expect("request"),
                )
                .await
                .expect("response");

            assert_eq!(
                response.status(),
                StatusCode::OK,
                "header {header_value} unexpectedly rejected"
            );
            let bytes = to_bytes(response.into_body(), usize::MAX)
                .await
                .expect("body");
            let value: JsonValue = serde_json::from_slice(&bytes).expect("json body");
            assert!(
                value.get("error").is_none(),
                "header {header_value} produced JSON-RPC error: {value}"
            );
            assert_eq!(
                value["result"]["status"]["state"], "completed",
                "header {header_value} did not dispatch: {value}"
            );
        }
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
        let task_id = response["result"]["id"]
            .as_str()
            .expect("task id")
            .to_string();

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
    async fn push_notification_configs_survive_server_restart() {
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
        let task_id = response["result"]["id"].as_str().expect("task id");

        let set = harn_vm::jsonrpc::request(
            "push-set",
            "tasks/pushNotificationConfig/set",
            json!({
                "id": task_id,
                "pushNotificationConfig": {
                    "id": "push-persisted",
                    "url": "https://client.example/a2a/persisted"
                }
            }),
        );
        let processed = server.process_rpc(set, AuthRequest::default()).await;
        let RpcOutcome::Json(response) = processed.outcome else {
            panic!("expected push set json response");
        };
        assert_eq!(response["result"]["id"], "push-persisted");

        let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
        let restarted = Arc::new(A2aServer::new(A2aServerConfig::new(core)));
        let get = harn_vm::jsonrpc::request(
            "push-get",
            "tasks/pushNotificationConfig/get",
            json!({"id": task_id, "pushNotificationConfigId": "push-persisted"}),
        );
        let processed = restarted
            .clone()
            .process_rpc(get, AuthRequest::default())
            .await;
        let RpcOutcome::Json(response) = processed.outcome else {
            panic!("expected push get json response");
        };
        assert_eq!(
            response["result"]["url"],
            "https://client.example/a2a/persisted"
        );

        let delete = harn_vm::jsonrpc::request(
            "push-delete",
            "tasks/pushNotificationConfig/delete",
            json!({"id": task_id, "pushNotificationConfigId": "push-persisted"}),
        );
        let processed = restarted.process_rpc(delete, AuthRequest::default()).await;
        let RpcOutcome::Json(response) = processed.outcome else {
            panic!("expected push delete json response");
        };
        assert!(response["result"].is_null());

        let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
        let restarted = Arc::new(A2aServer::new(A2aServerConfig::new(core)));
        let list = harn_vm::jsonrpc::request(
            "push-list",
            "tasks/pushNotificationConfig/list",
            json!({"id": task_id}),
        );
        let processed = restarted.process_rpc(list, AuthRequest::default()).await;
        let RpcOutcome::Json(response) = processed.outcome else {
            panic!("expected push list json response");
        };
        assert!(response["result"].as_array().expect("configs").is_empty());
    }

    fn server_with_api_key_policy(
        source: &str,
        api_key: &str,
    ) -> (tempfile::TempDir, Arc<A2aServer>) {
        use crate::ApiKeyAuthConfig;
        use std::collections::BTreeSet;
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(&script, source).expect("write script");
        let mut config = DispatchCoreConfig::for_script(&script);
        config.auth_policy = AuthPolicy {
            methods: vec![AuthMethodConfig::ApiKey(ApiKeyAuthConfig {
                keys: BTreeSet::from([api_key.to_string()]),
            })],
        };
        let core = DispatchCore::new(config).expect("core");
        (dir, Arc::new(A2aServer::new(A2aServerConfig::new(core))))
    }

    fn auth_request_with_bearer(token: &str) -> AuthRequest {
        AuthRequest {
            method: "POST".to_string(),
            path: "/".to_string(),
            body: Vec::new(),
            headers: std::collections::BTreeMap::from([(
                "authorization".to_string(),
                format!("Bearer {token}"),
            )]),
            validated_oauth: None,
        }
    }

    #[tokio::test]
    async fn extended_card_unauthenticated_when_no_auth_configured_returns_not_configured() {
        // Per A2A 0.3.0: if the agent does not have an extended card
        // configured (i.e., no auth scheme is wired in), the server
        // MUST return ExtendedAgentCardNotConfiguredError (-32007).
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
            panic!("expected json response");
        };
        assert!(processed.status.is_none());
        assert!(processed.auth_challenge.is_none());
        assert_eq!(
            response["error"]["code"],
            A2A_EXTENDED_AGENT_CARD_NOT_CONFIGURED
        );
    }

    #[tokio::test]
    async fn extended_card_without_token_returns_401_with_challenge() {
        let (_dir, server) = server_with_api_key_policy(
            r#"
pub fn triage(task: string) -> string {
  return task
}
"#,
            "secret",
        );
        let request =
            harn_vm::jsonrpc::request("card-2", "agent/getAuthenticatedExtendedCard", json!({}));

        let processed = server
            .clone()
            .process_rpc_with_public_url(request, AuthRequest::default(), "https://agent.example")
            .await;
        let RpcOutcome::Json(response) = processed.outcome else {
            panic!("expected json response");
        };
        assert_eq!(processed.status, Some(StatusCode::UNAUTHORIZED));
        let challenge = processed
            .auth_challenge
            .as_ref()
            .expect("auth challenge")
            .to_str()
            .expect("ascii challenge");
        assert!(
            challenge.starts_with("Bearer realm="),
            "challenge missing scheme: {challenge}"
        );
        assert_eq!(response["error"]["code"], -32000);
    }

    #[tokio::test]
    async fn extended_card_with_valid_bearer_returns_extended_payload() {
        let (_dir, server) = server_with_api_key_policy(
            r#"
pub fn triage(task: string) -> string {
  return task
}
"#,
            "secret",
        );
        let request =
            harn_vm::jsonrpc::request("card-3", "agent/getAuthenticatedExtendedCard", json!({}));

        let processed = server
            .clone()
            .process_rpc_with_public_url(
                request,
                auth_request_with_bearer("secret"),
                "https://agent.example",
            )
            .await;
        let RpcOutcome::Json(response) = processed.outcome else {
            panic!("expected json response");
        };
        assert!(processed.status.is_none());
        assert!(processed.auth_challenge.is_none());

        let card = &response["result"];
        assert_eq!(card["name"], "server");
        assert_eq!(
            card["supportedInterfaces"][0]["protocolVersion"],
            A2A_PROTOCOL_VERSION
        );
        assert_eq!(
            card["supportedInterfaces"][0]["url"],
            "https://agent.example"
        );
        assert_eq!(card["metadata"]["extendedAgentCard"], true);
        assert_eq!(card["metadata"]["principal"], "api-key");
        assert_eq!(card["securitySchemes"]["apiKey"]["type"], "apiKey");
        assert_eq!(card["security"][0]["apiKey"], json!([]));
        assert_eq!(card["skills"][0]["id"], "triage");
        assert_eq!(card["skills"][0]["outputSchema"], json!({}));
    }

    #[tokio::test]
    async fn public_card_advertises_extended_support_when_auth_configured() {
        let (_dir, server) = server_with_api_key_policy(
            r#"
pub fn triage(task: string) -> string {
  return task
}
"#,
            "secret",
        );

        let card = server.agent_card("https://agent.example");
        assert_eq!(card["capabilities"]["extendedAgentCard"], true);
        assert_eq!(card["supportsAuthenticatedExtendedCard"], true);
        assert_eq!(card["securitySchemes"]["apiKey"]["type"], "apiKey");
        assert_eq!(card["security"][0]["apiKey"], json!([]));
    }

    #[tokio::test]
    async fn http_extended_card_unauthenticated_returns_401_with_www_authenticate() {
        // End-to-end: drive the request through the HTTP router and
        // confirm an unauthenticated JSON-RPC call to
        // agent/getAuthenticatedExtendedCard yields HTTP 401 plus a
        // WWW-Authenticate header.
        let (_dir, server) = server_with_api_key_policy(
            r#"
pub fn triage(task: string) -> string {
  return task
}
"#,
            "secret",
        );
        let public_url = "https://agent.example";
        let router = A2aServer::http_router(HttpState {
            server,
            public_url: public_url.to_string(),
        });
        let body = serde_json::to_vec(&harn_vm::jsonrpc::request(
            "card-http-1",
            "agent/getAuthenticatedExtendedCard",
            json!({}),
        ))
        .expect("request body");

        let response = router
            .clone()
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

        assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
        let challenge = response
            .headers()
            .get(axum::http::header::WWW_AUTHENTICATE)
            .expect("WWW-Authenticate header")
            .to_str()
            .expect("ascii challenge");
        assert!(
            challenge.starts_with("Bearer realm="),
            "challenge missing scheme: {challenge}"
        );
    }

    #[tokio::test]
    async fn http_extended_card_authenticated_returns_extended_payload() {
        let (_dir, server) = server_with_api_key_policy(
            r#"
pub fn triage(task: string) -> string {
  return task
}
"#,
            "secret",
        );
        let public_url = "https://agent.example";
        let router = A2aServer::http_router(HttpState {
            server,
            public_url: public_url.to_string(),
        });
        let body = serde_json::to_vec(&harn_vm::jsonrpc::request(
            "card-http-2",
            "agent/getAuthenticatedExtendedCard",
            json!({}),
        ))
        .expect("request body");

        let response = router
            .clone()
            .oneshot(
                Request::builder()
                    .method(Method::POST)
                    .uri("/")
                    .header(axum::http::header::CONTENT_TYPE, "application/json")
                    .header(axum::http::header::AUTHORIZATION, "Bearer secret")
                    .body(Body::from(body))
                    .expect("request"),
            )
            .await
            .expect("response");

        assert_eq!(response.status(), StatusCode::OK);
        assert!(response
            .headers()
            .get(axum::http::header::WWW_AUTHENTICATE)
            .is_none());
        let bytes = to_bytes(response.into_body(), usize::MAX)
            .await
            .expect("body");
        let envelope: JsonValue = serde_json::from_slice(&bytes).expect("envelope");
        assert_eq!(envelope["result"]["metadata"]["extendedAgentCard"], true);
        assert_eq!(envelope["result"]["metadata"]["principal"], "api-key");
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
    async fn send_message_round_trips_file_and_data_parts() {
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r#"
pub fn triage(message: dict) -> dict {
  return message
}
"#,
        )
        .expect("write script");
        let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
        let server = Arc::new(A2aServer::new(A2aServerConfig::new(core)));
        let request = harn_vm::jsonrpc::request(
            "parts-1",
            "message/send",
            json!({
                "message": {
                    "metadata": {"target_agent": "triage"},
                    "parts": [
                        {"type": "text", "text": "inspect attachments"},
                        {
                            "type": "file",
                            "file": {
                                "bytes": "AAEC/w==",
                                "mimeType": "application/octet-stream",
                                "name": "payload.bin"
                            }
                        },
                        {
                            "kind": "file",
                            "file": {
                                "uri": "https://example.test/report.pdf",
                                "mimeType": "application/pdf",
                                "name": "report.pdf"
                            }
                        },
                        {
                            "type": "data",
                            "data": {"ticket": "HARN-891", "priority": 2}
                        }
                    ]
                }
            }),
        );

        let processed = server.process_rpc(request, AuthRequest::default()).await;
        let RpcOutcome::Json(response) = processed.outcome else {
            panic!("expected json response");
        };

        assert_eq!(response["result"]["status"]["state"], "completed");
        let user_parts = response["result"]["history"][0]["parts"]
            .as_array()
            .expect("user parts");
        assert_eq!(user_parts[1]["type"], "file");
        assert_eq!(user_parts[1]["file"]["bytes"], "AAEC/w==");
        assert_eq!(
            user_parts[2]["file"]["uri"],
            "https://example.test/report.pdf"
        );
        assert_eq!(user_parts[3]["type"], "data");
        assert_eq!(user_parts[3]["data"]["ticket"], "HARN-891");

        let agent_parts = response["result"]["history"][1]["parts"]
            .as_array()
            .expect("agent parts");
        assert_eq!(agent_parts, user_parts);
        assert!(response["result"]["artifacts"]
            .as_array()
            .expect("artifacts")
            .iter()
            .any(|artifact| artifact["parts"][0]["type"] == "file"));
    }

    #[test]
    fn response_artifacts_emit_file_and_data_parts() {
        let response = json!({
            "visible_text": "done",
            "artifacts": [
                {
                    "_type": "artifact",
                    "id": "artifact_file",
                    "kind": "file",
                    "title": "payload.bin",
                    "data": {
                        "bytes": "AAEC/w==",
                        "mimeType": "application/octet-stream",
                        "name": "payload.bin"
                    }
                },
                {
                    "_type": "artifact",
                    "id": "artifact_data",
                    "kind": "data",
                    "data": {"answer": 42}
                }
            ]
        });

        let parts = super::response_parts(&response);
        assert_eq!(parts[0], json!({"type": "text", "text": "done"}));
        assert_eq!(parts[1]["type"], "file");
        assert_eq!(parts[1]["file"]["bytes"], "AAEC/w==");
        assert_eq!(parts[1]["file"]["mimeType"], "application/octet-stream");
        assert_eq!(parts[2]["type"], "data");
        assert_eq!(parts[2]["data"]["answer"], 42);

        let artifacts = super::response_artifacts(&response, &parts);
        assert_eq!(artifacts[0]["artifactId"], "artifact_file");
        assert_eq!(artifacts[0]["parts"][0]["type"], "file");
        assert_eq!(artifacts[1]["parts"][0]["type"], "data");
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
            artifacts: Vec::new(),
            metadata: BTreeMap::new(),
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
            artifacts: Vec::new(),
            metadata: BTreeMap::new(),
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
                    artifacts: Vec::new(),
                    metadata: BTreeMap::new(),
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

    #[test]
    fn task_status_renders_a2a_0_3_0_state_strings() {
        // The wire-level state names follow A2A 0.3.0's hyphenated
        // schema. Pin them so a typo can't silently regress the public
        // surface of the SSE / push-config payloads.
        assert_eq!(TaskStatus::Submitted.as_str(), "submitted");
        assert_eq!(TaskStatus::Working.as_str(), "working");
        assert_eq!(TaskStatus::InputRequired.as_str(), "input-required");
        assert_eq!(TaskStatus::AuthRequired.as_str(), "auth-required");
        assert_eq!(TaskStatus::Completed.as_str(), "completed");
        assert_eq!(TaskStatus::Failed.as_str(), "failed");
        assert_eq!(TaskStatus::Cancelled.as_str(), "cancelled");
        assert_eq!(TaskStatus::Rejected.as_str(), "rejected");

        // Terminal states cannot be cancelled or transitioned out of.
        // `input-required` and `auth-required` are pause states — the
        // task is alive and the client is expected to act on it.
        assert!(TaskStatus::Completed.is_terminal());
        assert!(TaskStatus::Failed.is_terminal());
        assert!(TaskStatus::Cancelled.is_terminal());
        assert!(TaskStatus::Rejected.is_terminal());
        assert!(!TaskStatus::Submitted.is_terminal());
        assert!(!TaskStatus::Working.is_terminal());
        assert!(!TaskStatus::InputRequired.is_terminal());
        assert!(!TaskStatus::AuthRequired.is_terminal());
    }

    #[test]
    fn hitl_requested_event_transitions_task_into_input_required() {
        // A2A 0.3.0 `input-required` is the wire signal a client uses
        // to know the task is paused on a HITL waitpoint. Our sink
        // listens for the canonical `AgentEvent::HitlRequested` emitted
        // by the HITL primitives in `harn-vm` and flips task status
        // accordingly. `HitlResolved` flips it back to `working` so
        // subscribers can observe the resume before the task ultimately
        // completes / fails.
        let task_id = "task-hitl".to_string();
        let task = TaskState {
            id: task_id.clone(),
            context_id: None,
            status: TaskStatus::Working,
            history: Vec::new(),
            artifacts: Vec::new(),
            metadata: BTreeMap::new(),
            events: Vec::new(),
            subscribers: Vec::new(),
            cancel_token: None,
        };
        let tasks: TaskStore = Arc::new(Mutex::new(HashMap::from([(task_id.clone(), task)])));
        let sink = super::A2aWorkerSink {
            task_id: task_id.clone(),
            tasks: tasks.clone(),
        };

        sink.handle_event(&harn_vm::agent_events::AgentEvent::HitlRequested {
            session_id: super::a2a_worker_session_id(&task_id),
            request_id: "hitl_question_t1_1".into(),
            kind: "question".into(),
            payload: serde_json::json!({"prompt": "Approve?"}),
        });

        {
            let tasks = tasks.lock().expect("tasks");
            let task = tasks.get(&task_id).expect("task");
            assert_eq!(task.status, TaskStatus::InputRequired);
            let hitl_event = task
                .events
                .iter()
                .find(|event| event.get("type").and_then(JsonValue::as_str) == Some("hitl"))
                .expect("hitl event");
            assert_eq!(hitl_event["phase"], "requested");
            assert_eq!(hitl_event["kind"], "question");
            assert_eq!(hitl_event["requestId"], "hitl_question_t1_1");
            assert_eq!(hitl_event["payload"]["prompt"], "Approve?");
            let status_event = task
                .events
                .iter()
                .filter_map(|event| {
                    if event.get("type").and_then(JsonValue::as_str) == Some("status") {
                        event.pointer("/status/state").and_then(JsonValue::as_str)
                    } else {
                        None
                    }
                })
                .next_back()
                .expect("status event");
            assert_eq!(status_event, "input-required");
        }

        sink.handle_event(&harn_vm::agent_events::AgentEvent::HitlResolved {
            session_id: super::a2a_worker_session_id(&task_id),
            request_id: "hitl_question_t1_1".into(),
            kind: "question".into(),
            outcome: "answered".into(),
        });

        let tasks = tasks.lock().expect("tasks");
        let task = tasks.get(&task_id).expect("task");
        assert_eq!(task.status, TaskStatus::Working);
        let resolved_event = task
            .events
            .iter()
            .rfind(|event| event.get("type").and_then(JsonValue::as_str) == Some("hitl"))
            .expect("resolved hitl event");
        assert_eq!(resolved_event["phase"], "resolved");
        assert_eq!(resolved_event["outcome"], "answered");
    }

    #[test]
    fn hitl_requested_event_does_not_override_terminal_task() {
        // The waitpoint emit can race with cancellation/completion.
        // Once a task is terminal, a stray `HitlRequested` must not
        // reanimate it into `input-required`.
        let task_id = "task-terminal".to_string();
        let task = TaskState {
            id: task_id.clone(),
            context_id: None,
            status: TaskStatus::Cancelled,
            history: Vec::new(),
            artifacts: Vec::new(),
            metadata: BTreeMap::new(),
            events: Vec::new(),
            subscribers: Vec::new(),
            cancel_token: None,
        };
        let tasks: TaskStore = Arc::new(Mutex::new(HashMap::from([(task_id.clone(), task)])));
        let sink = super::A2aWorkerSink {
            task_id: task_id.clone(),
            tasks: tasks.clone(),
        };

        sink.handle_event(&harn_vm::agent_events::AgentEvent::HitlRequested {
            session_id: super::a2a_worker_session_id(&task_id),
            request_id: "late".into(),
            kind: "question".into(),
            payload: serde_json::json!({}),
        });

        let tasks = tasks.lock().expect("tasks");
        let task = tasks.get(&task_id).expect("task");
        assert_eq!(task.status, TaskStatus::Cancelled);
        // No HITL event is published either — the late emission is
        // dropped wholesale rather than partially recorded.
        assert!(
            task.events
                .iter()
                .all(|event| event.get("type").and_then(JsonValue::as_str) != Some("hitl")),
            "events: {:?}",
            task.events
        );
    }

    #[tokio::test]
    async fn rejected_state_surfaces_when_auth_policy_denies_dispatch() {
        // Synchronous policy denial: `AuthPolicy.authorize` returns
        // `Rejected` before any script work runs, so the task lands in
        // the terminal `rejected` state per A2A 0.3.0. The client sees
        // the `rejected` status alongside the policy's reason in the
        // task history; subsequent `tasks/cancel` is rejected because
        // the task is already terminal.
        let (_dir, server) = server_with_api_key_policy(
            r#"
pub fn triage(task: string) -> string {
  return task
}
"#,
            "secret-key",
        );
        let request = harn_vm::jsonrpc::request(
            "rej-1",
            "message/send",
            json!({
                "message": {
                    "metadata": {"target_agent": "triage"},
                    "parts": [{"type": "text", "text": "hello"}]
                },
                "configuration": {"blocking": true}
            }),
        );

        // No bearer token — the API-key policy will deny the dispatch.
        let processed = server.process_rpc(request, AuthRequest::default()).await;
        let RpcOutcome::Json(response) = processed.outcome else {
            panic!(
                "expected json response, got: {processed:?}",
                processed = match processed.outcome {
                    RpcOutcome::Json(_) => "json",
                    RpcOutcome::Sse(_) => "sse",
                }
            );
        };

        assert_eq!(
            response["result"]["status"]["state"], "rejected",
            "got: {response}"
        );
        // The denial reason lands in the task history as the agent
        // turn so callers can render it without cracking error fields.
        let history = response["result"]["history"]
            .as_array()
            .expect("history array");
        let agent_message = history
            .iter()
            .find(|message| message["role"] == "agent")
            .expect("agent reply");
        let text = agent_message["parts"][0]["text"]
            .as_str()
            .expect("text part")
            .to_lowercase();
        assert!(
            text.contains("auth") || text.contains("missing") || text.contains("invalid"),
            "expected denial reason in history, got: {agent_message}"
        );
    }

    #[tokio::test]
    async fn auth_required_state_surfaces_when_script_raises_auth_error() {
        // Mid-task downstream auth failure: the script raises an
        // auth-classified error (e.g. an LLM/HTTP 401 surfaces through
        // `error_to_category`). The dispatch returns `Execution(...)`
        // wrapping the message; the adapter classifies it via
        // `harn_vm::value::classify_error_message` and flips the task
        // into the non-terminal `auth-required` state so the client
        // can refresh credentials and resubscribe.
        let dir = tempfile::tempdir().expect("tempdir");
        let script = dir.path().join("server.harn");
        std::fs::write(
            &script,
            r#"
pub fn triage(task: string) -> string {
  // The auth classifier matches "401" (HTTP status code) and well-
  // known error identifier substrings. This message hits both so the
  // path is exercised regardless of which heuristic fires first.
  throw "downstream HTTP 401: invalid_api_key"
  return task
}
"#,
        )
        .expect("write script");
        let core = DispatchCore::new(DispatchCoreConfig::for_script(&script)).expect("core");
        let server = Arc::new(A2aServer::new(A2aServerConfig::new(core)));
        let request = harn_vm::jsonrpc::request(
            "auth-1",
            "message/send",
            json!({
                "message": {
                    "metadata": {"target_agent": "triage"},
                    "parts": [{"type": "text", "text": "hello"}]
                },
                "configuration": {"blocking": true}
            }),
        );

        let processed = server.process_rpc(request, AuthRequest::default()).await;
        let RpcOutcome::Json(response) = processed.outcome else {
            panic!("expected json response");
        };

        assert_eq!(
            response["result"]["status"]["state"], "auth-required",
            "got: {response}"
        );
    }
}
