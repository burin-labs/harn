use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use futures::channel::mpsc::UnboundedSender;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use tokio::sync::{broadcast, mpsc, oneshot, Notify};

use harn_vm::event_log::Topic;
use harn_vm::mcp_protocol;

use crate::commands::orchestrator::listener::ListenerAuth;

use super::super::oauth_resource::OAuthResourceServer;
use harn_serve::FilePromptCatalog;

use super::MCP_PROTOCOL_VERSION;

/// Audit and observability topics surfaced to MCP clients via
/// `notifications/message`. Each binding gives the topic a stable
/// `logger` name (per the MCP logging spec) and a fallback severity
/// for events whose kind/headers do not signal a more specific level.
pub(super) struct McpLogStreamBinding {
    pub(super) topic: &'static str,
    pub(super) logger: &'static str,
    pub(super) default_level: mcp_protocol::McpLogLevel,
}

#[derive(Clone)]
pub(crate) struct McpOrchestratorService {
    pub(super) config_path: PathBuf,
    pub(super) state_dir: PathBuf,
    pub(super) manifest_source: Arc<Mutex<String>>,
    pub(super) auth: ListenerAuth,
    pub(super) oauth: Option<OAuthResourceServer>,
    pub(super) prompt_catalog: Arc<Mutex<FilePromptCatalog>>,
    pub(super) list_notify_tx: broadcast::Sender<JsonValue>,
    pub(super) resource_notify_tx: broadcast::Sender<McpResourceNotification>,
    pub(super) task_notify_tx: broadcast::Sender<McpTaskNotification>,
    pub(super) log_notify_tx: broadcast::Sender<McpLogNotification>,
    #[allow(dead_code)]
    pub(super) log_event_log: Option<Arc<harn_vm::event_log::AnyEventLog>>,
    #[allow(dead_code)]
    pub(super) log_watchers_ready: Arc<LogWatcherReadiness>,
    pub(super) tasks: Arc<Mutex<BTreeMap<String, McpTaskRecord>>>,
    pub(super) resource_watchers: Arc<Mutex<BTreeMap<String, tokio::task::JoinHandle<()>>>>,
    pub(super) _list_watcher: Arc<Mutex<Option<notify::RecommendedWatcher>>>,
    pub(super) _log_watchers: Arc<AbortOnDrop>,
}

#[derive(Clone, Debug)]
pub(super) struct McpResourceNotification {
    pub(super) uri: String,
    pub(super) message: JsonValue,
}

#[derive(Clone, Debug)]
pub(super) struct McpTaskNotification {
    pub(super) owner: String,
    pub(super) message: JsonValue,
}

#[derive(Clone, Debug)]
pub(super) struct McpLogNotification {
    pub(super) level: mcp_protocol::McpLogLevel,
    pub(super) message: JsonValue,
}

/// Counts the log topic watchers that have finished registering with
/// the event log so callers (currently tests) can deterministically
/// wait until the broadcast subscription is in place before publishing
/// events. Production code never blocks on this.
#[derive(Default)]
pub(super) struct LogWatcherReadiness {
    pub(super) ready: std::sync::atomic::AtomicUsize,
    pub(super) expected: std::sync::atomic::AtomicUsize,
    pub(super) notify: tokio::sync::Notify,
}

impl LogWatcherReadiness {
    pub(super) fn record_ready(&self) {
        self.ready.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
        self.notify.notify_waiters();
    }
}

/// Owns spawned tokio tasks and aborts them when the wrapper is
/// dropped. The log topic watchers each hold an `Arc<AnyEventLog>` and
/// would otherwise outlive a dropped service, leaking tasks across
/// test cases that build and drop multiple `McpOrchestratorService`
/// instances on the same runtime.
pub(super) struct AbortOnDrop(pub(super) Vec<tokio::task::JoinHandle<()>>);

impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        for handle in self.0.drain(..) {
            handle.abort();
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct McpTaskRecord {
    pub(super) task: McpTaskState,
    pub(super) result: Option<JsonValue>,
    pub(super) notify: Arc<Notify>,
}

#[derive(Clone, Debug)]
pub(super) struct McpTaskState {
    pub(super) task_id: String,
    pub(super) owner: String,
    pub(super) status: mcp_protocol::McpTaskStatus,
    pub(super) status_message: Option<String>,
    pub(super) created_at: String,
    pub(super) last_updated_at: String,
    pub(super) ttl: Option<u64>,
    pub(super) poll_interval: Option<u64>,
}

impl McpTaskState {
    pub(super) fn to_json(&self) -> JsonValue {
        let mut value = json!({
            "taskId": self.task_id,
            "status": self.status.as_str(),
            "createdAt": self.created_at,
            "lastUpdatedAt": self.last_updated_at,
            "ttl": self.ttl,
        });
        if let Some(message) = &self.status_message {
            value["statusMessage"] = json!(message);
        }
        if let Some(poll_interval) = self.poll_interval {
            value["pollInterval"] = json!(poll_interval);
        }
        value
    }

    pub(super) fn notification(&self) -> McpTaskNotification {
        McpTaskNotification {
            owner: self.owner.clone(),
            message: json!({
                "jsonrpc": "2.0",
                "method": mcp_protocol::METHOD_TASK_STATUS_NOTIFICATION,
                "params": self.to_json(),
            }),
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct ConnectionState {
    pub(super) initialized: bool,
    pub(super) authenticated: bool,
    pub(super) client_identity: String,
    pub(super) protocol_version: String,
    pub(super) subscribed_resources: BTreeSet<String>,
    pub(super) log_level: mcp_protocol::McpLogLevel,
    /// RC clients negotiate per request; the orchestrator records the
    /// last observed mode on the connection so list-change notifications
    /// or progress streams know which envelope shape to send. Defaults
    /// to [`McpProtocolMode::Legacy`] until the first RC-tagged request
    /// arrives.
    pub(super) protocol_mode: mcp_protocol::McpProtocolMode,
}

impl Default for ConnectionState {
    fn default() -> Self {
        Self {
            initialized: false,
            authenticated: false,
            client_identity: "unknown".to_string(),
            protocol_version: MCP_PROTOCOL_VERSION.to_string(),
            subscribed_resources: BTreeSet::new(),
            log_level: mcp_protocol::McpLogLevel::Info,
            protocol_mode: mcp_protocol::McpProtocolMode::Legacy,
        }
    }
}

pub(super) struct HttpSession {
    pub(super) state: Mutex<ConnectionState>,
    pub(super) sse_tx: Mutex<Option<UnboundedSender<JsonValue>>>,
}

impl Default for HttpSession {
    fn default() -> Self {
        Self {
            state: Mutex::new(ConnectionState::default()),
            sse_tx: Mutex::new(None),
        }
    }
}

#[derive(Clone)]
pub(super) struct RpcBridge {
    pub(super) tx: mpsc::UnboundedSender<RpcRequest>,
}

pub(super) struct RpcRequest {
    pub(super) session: ConnectionState,
    pub(super) request: JsonValue,
    pub(super) response_tx: oneshot::Sender<(ConnectionState, JsonValue)>,
    /// Optional SSE sender already attached to the calling session.
    /// When present, the worker installs a [`harn_vm::mcp_progress::ProgressBus`]
    /// pointing at it for the duration of `handle_request`, allowing
    /// long-running tools to emit `notifications/progress` updates that
    /// stream out the session's open GET endpoint.
    pub(super) progress_sender: Option<UnboundedSender<JsonValue>>,
}

#[derive(Clone)]
pub(super) struct HttpState {
    pub(super) service: Arc<McpOrchestratorService>,
    pub(super) rpc: RpcBridge,
    pub(super) sessions: Arc<Mutex<HashMap<String, Arc<HttpSession>>>>,
    pub(super) mcp_path: String,
    pub(super) sse_path: String,
    pub(super) messages_path: String,
}

#[derive(Clone, Copy)]
pub(super) enum McpListChangeKind {
    Tools,
    Resources,
    Prompts,
}

impl McpListChangeKind {
    pub(super) fn method(self) -> &'static str {
        match self {
            Self::Tools => "notifications/tools/list_changed",
            Self::Resources => "notifications/resources/list_changed",
            Self::Prompts => "notifications/prompts/list_changed",
        }
    }

    pub(super) fn notification(self) -> JsonValue {
        json!({
            "jsonrpc": "2.0",
            "method": self.method(),
        })
    }
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct TriggerListEntry {
    pub(super) trigger_id: String,
    pub(super) kind: String,
    pub(super) provider: String,
    pub(super) when: Option<String>,
    pub(super) handler: JsonValue,
    pub(super) version: u32,
    pub(super) state: String,
    pub(super) metrics: harn_vm::TriggerMetricsSnapshot,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct QueuePreviewEntry {
    pub(super) event_id: u64,
    pub(super) kind: String,
    pub(super) occurred_at_ms: i64,
    pub(super) headers: BTreeMap<String, String>,
    pub(super) payload: JsonValue,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct QueueSnapshot {
    pub(super) dispatcher: harn_vm::DispatcherStatsSnapshot,
    pub(super) inbox: TopicPreview,
    pub(super) outbox: TopicPreview,
    pub(super) attempts: TopicPreview,
    pub(super) dlq: TopicPreview,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct TopicPreview {
    pub(super) count: usize,
    pub(super) head: Vec<QueuePreviewEntry>,
}

#[derive(Clone, Debug, Serialize)]
pub(super) struct InspectPayload {
    pub(super) dispatcher: harn_vm::DispatcherStatsSnapshot,
    #[serde(flatten)]
    pub(super) inspect: crate::commands::orchestrator::inspect_data::OrchestratorInspectData,
}

#[derive(Clone, Debug, Deserialize, Serialize)]
pub(super) struct RecordedTriggerEvent {
    pub(super) binding_id: String,
    pub(super) binding_version: u32,
    pub(super) replay_of_event_id: Option<String>,
    pub(super) event: harn_vm::TriggerEvent,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct TriggerFireRequest {
    pub(super) trigger_id: String,
    #[serde(default)]
    pub(super) payload: JsonValue,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct TriggerReplayRequest {
    pub(super) event_id: String,
    #[serde(default)]
    pub(super) as_of: Option<String>,
    #[serde(default)]
    pub(super) steer_from: Option<String>,
    #[serde(default)]
    pub(super) to_decision: Option<JsonValue>,
    #[serde(default)]
    pub(super) reason: Option<String>,
    #[serde(default)]
    pub(super) applied_by: Option<String>,
    #[serde(default)]
    pub(super) scope: Option<String>,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct DlqRetryRequest {
    pub(super) entry_id: String,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct SecretScanRequest {
    pub(super) content: String,
}

#[derive(Clone, Debug)]
pub(super) struct ResourceSubscription {
    pub(super) uri: String,
    pub(super) topic: Topic,
}

#[derive(Clone, Debug, Deserialize)]
pub(super) struct TrustQueryRequest {
    #[serde(default)]
    pub(super) agent: Option<String>,
    #[serde(default)]
    pub(super) action: Option<String>,
    #[serde(default)]
    pub(super) since: Option<String>,
    #[serde(default)]
    pub(super) until: Option<String>,
    #[serde(default)]
    pub(super) tier: Option<harn_vm::AutonomyTier>,
    #[serde(default)]
    pub(super) outcome: Option<harn_vm::TrustOutcome>,
    #[serde(default)]
    pub(super) limit: Option<usize>,
    #[serde(default)]
    pub(super) grouped_by_trace: bool,
}
