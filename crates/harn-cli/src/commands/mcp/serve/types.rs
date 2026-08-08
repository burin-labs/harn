use std::collections::BTreeMap;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use harn_vm::mcp_protocol;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value as JsonValue};
use tokio::sync::{mpsc, oneshot, Notify};

use crate::commands::orchestrator::listener::ListenerAuth;

use super::super::oauth_resource::OAuthResourceServer;
use harn_serve::FilePromptCatalog;

#[derive(Clone)]
pub(crate) struct McpOrchestratorService {
    pub(super) config_path: PathBuf,
    pub(super) state_dir: PathBuf,
    pub(super) manifest_source: Arc<Mutex<String>>,
    pub(super) auth: ListenerAuth,
    pub(super) oauth: Option<OAuthResourceServer>,
    pub(super) prompt_catalog: Arc<Mutex<FilePromptCatalog>>,
    /// The orchestrator's own event log, opened on first use and shared.
    ///
    /// Not the same database as `log_event_log`, which is the listener's auth
    /// log under `<state>/.harn/`; this one is `<state>/events.sqlite`, the log
    /// `OrchestratorRole::build_vm` installs. Handlers that only read or append
    /// events open it directly instead of building an entire orchestrator VM —
    /// stdlib registration, manifest compile, trigger registration — and
    /// discarding all of it.
    pub(super) orchestrator_event_log: std::sync::OnceLock<Arc<harn_vm::event_log::AnyEventLog>>,
    pub(super) tasks: Arc<Mutex<BTreeMap<String, McpTaskRecord>>>,
    pub(super) _list_watcher: Arc<Mutex<Option<notify::RecommendedWatcher>>>,
}

#[derive(Clone, Debug)]
pub(super) struct McpTaskRecord {
    pub(super) task: McpTaskState,
    pub(super) result: Option<JsonValue>,
    pub(super) notify: Arc<Notify>,
}

impl McpTaskRecord {
    pub(super) fn to_detailed_json(&self) -> JsonValue {
        let mut value = self.task.to_json();
        value["resultType"] = json!(mcp_protocol::RESULT_TYPE_COMPLETE);
        match self.task.status {
            mcp_protocol::McpTaskStatus::Completed => {
                value["result"] = self.result.clone().unwrap_or_else(|| json!({}));
            }
            mcp_protocol::McpTaskStatus::Failed => {
                value["error"] = json!({
                    "code": -32603,
                    "message": self.task.status_message.as_deref().unwrap_or("Task failed"),
                });
            }
            mcp_protocol::McpTaskStatus::Working
            | mcp_protocol::McpTaskStatus::InputRequired
            | mcp_protocol::McpTaskStatus::Cancelled => {}
            _ => unreachable!("Harn only creates MCP task statuses it handles"),
        }
        value
    }
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
            "status": mcp_protocol::mcp_task_status_wire_name(self.status),
            "createdAt": self.created_at,
            "lastUpdatedAt": self.last_updated_at,
            "ttlMs": self.ttl,
        });
        if let Some(message) = &self.status_message {
            value["statusMessage"] = json!(message);
        }
        if let Some(poll_interval) = self.poll_interval {
            value["pollIntervalMs"] = json!(poll_interval);
        }
        value
    }
}

#[derive(Clone, Debug)]
pub(super) struct ConnectionState {
    pub(super) authenticated: bool,
    pub(super) client_identity: String,
}

impl Default for ConnectionState {
    fn default() -> Self {
        Self {
            authenticated: false,
            client_identity: "unknown".to_string(),
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
