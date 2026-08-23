use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use harn_vm::mcp_protocol;
use serde::{Deserialize, Serialize};
use serde_json::Value as JsonValue;
use tokio::sync::{mpsc, oneshot};

use crate::commands::orchestrator::listener::ListenerAuth;

use super::super::oauth_resource::OAuthResourceServer;
use super::derived_state::ManifestDerivedState;

#[derive(Clone)]
pub(crate) struct McpOrchestratorService {
    pub(super) config_path: PathBuf,
    pub(super) state_dir: PathBuf,
    pub(super) derived_state: Arc<ManifestDerivedState>,
    pub(super) auth: ListenerAuth,
    pub(super) oauth: Option<OAuthResourceServer>,
    /// The orchestrator's own event log, opened on first use and shared.
    ///
    /// Not the same database as `log_event_log`, which is the listener's auth
    /// log under `<state>/.harn/`; this one is `<state>/events.sqlite`, the log
    /// `OrchestratorRole::build_vm` installs. Handlers that only read or append
    /// events open it directly instead of building an entire orchestrator VM —
    /// stdlib registration, manifest compile, trigger registration — and
    /// discarding all of it.
    pub(super) orchestrator_event_log: std::sync::OnceLock<Arc<harn_vm::event_log::AnyEventLog>>,
    /// Every in-flight `tools/call` this server turned into an MCP task.
    /// The lifecycle is `harn_vm::mcp_tasks`, shared with the script-driven
    /// server so both answer `tasks/get` the same way.
    pub(super) tasks: Arc<harn_vm::mcp_tasks::McpTaskStore>,
    pub(super) _list_watcher: Arc<Mutex<Option<notify::RecommendedWatcher>>>,
}

#[derive(Clone, Debug, Default)]
pub(super) struct ConnectionState {
    pub(super) authenticated: bool,
    pub(super) mcp: mcp_protocol::McpServerSession,
}

#[derive(Clone)]
pub(super) struct RpcBridge {
    pub(super) tx: mpsc::UnboundedSender<RpcRequest>,
}

pub(super) struct RpcRequest {
    pub(super) session: ConnectionState,
    pub(super) request: JsonValue,
    pub(super) response_tx: oneshot::Sender<(ConnectionState, JsonValue)>,
}

#[derive(Clone)]
pub(super) struct HttpState {
    pub(super) service: Arc<McpOrchestratorService>,
    pub(super) rpc: RpcBridge,
    pub(super) mcp_path: String,
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
