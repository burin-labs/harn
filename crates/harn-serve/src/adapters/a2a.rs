//! Agent2Agent (A2A) adapter facade.
//!
//! The public server types stay here while transport routing, task lifecycle,
//! schema normalization, auth/push delivery, and worker event conversion live
//! in child modules with narrow ownership.
//!
//! Module map: `transport` owns JSON-RPC/REST/SSE routing, `tasks` owns
//! lifecycle state and push-config persistence, `schema` owns wire shapes,
//! `auth` owns outbound push authentication, and `events` owns worker-event
//! conversion.

use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration as StdDuration;

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
use harn_vm::connectors::{JwtKeySource, JwtVerificationOptions};
use harn_vm::event_log::{AnyEventLog, EventLog, LogEvent, Topic};
use hmac::{Hmac, KeyInit, Mac};
use serde_json::{json, Value as JsonValue};
use sha2::Sha256;
use time::format_description::well_known::Rfc3339;
use time::OffsetDateTime;
use tokio::sync::{mpsc, oneshot};
use tokio::task::LocalSet;
use uuid::Uuid;

use crate::{
    AdapterDescriptor, AuthMethodConfig, AuthPolicy, AuthRequest, AuthorizationDecision,
    CallArguments, CallRequest, CallResponse, DispatchCore, DispatchError, ExportCatalog,
    HttpTlsConfig, TransportAdapter,
};

mod auth;
mod events;
mod schema;
mod tasks;
#[cfg(test)]
mod tests;
mod transport;

use schema::{derived_agent_name, load_push_configs};

pub const A2A_PROTOCOL_VERSION: &str = "0.3.0";

const A2A_VERSION_HEADER: &str = "a2a-version";
const A2A_TRACE_HEADER: &str = "a2a-trace-id";
const A2A_DEPRECATION_HEADER: &str = "deprecation";
const A2A_AGENT_CARD_PATH: &str = "/.well-known/agent-card.json";
/// Base path for the canonical A2A 0.3 HTTP+JSON/REST surface.
const A2A_REST_BASE: &str = "/v1";

const REST_DEPRECATED_MESSAGE_SEND: &str = "Use A2A 0.3.0 REST path `POST /v1/message:send`.";
const REST_DEPRECATED_MESSAGE_STREAM: &str = "Use A2A 0.3.0 REST path `POST /v1/message:stream`.";
const REST_DEPRECATED_CANCEL: &str = "Use A2A 0.3.0 REST path `POST /v1/tasks/{id}:cancel`.";
const REST_DEPRECATED_RESUBSCRIBE: &str =
    "Use A2A 0.3.0 REST path `POST /v1/tasks/{id}:subscribe`.";
const REST_DEPRECATED_SEND: &str =
    "Use A2A 0.3.0 REST path `POST /v1/message:send` (use `configuration.blocking = true` for synchronous waits).";
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
}

#[async_trait::async_trait(?Send)]
impl TransportAdapter for A2aServer {
    fn descriptor(&self) -> AdapterDescriptor {
        self.descriptor.clone()
    }
}
