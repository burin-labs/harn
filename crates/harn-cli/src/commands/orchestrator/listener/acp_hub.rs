use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::{Duration, Instant};

use axum::extract::ws::{Message as WsMessage, WebSocket, WebSocketUpgrade};
use axum::extract::{Extension, OriginalUri};
use axum::http::{HeaderMap, Method};
use axum::response::{IntoResponse, Response};
use futures::{SinkExt, StreamExt};
use serde_json::{json, Value as JsonValue};
use tokio::sync::mpsc;

use harn_vm::event_log::{AnyEventLog, EventLog, LogEvent, Topic};

use super::routes::{normalize_headers, HttpError, ListenerAuth};
use crate::commands::orchestrator::errors::OrchestratorError;

pub(super) const ACP_PATH: &str = "/acp";
const ACP_TOPIC_PREFIX: &str = "acp.session";
const ACP_PING_INTERVAL: Duration = Duration::from_secs(30);
const ACP_PONG_TIMEOUT: Duration = Duration::from_secs(10);
pub(super) const ACP_RETAINED_SESSION_SECS_ENV: &str = "HARN_ACP_WS_RETAIN_SECS";
pub(super) const ACP_DEFAULT_RETAINED_SESSION_SECS: u64 = 5 * 60;
const ACP_REPLAY_BUFFER_LIMIT: usize = 4096;

#[derive(Clone)]
pub(super) struct AcpWebSocketState {
    pub(super) event_log: Arc<AnyEventLog>,
    pub(super) auth: Arc<ListenerAuth>,
    pub(super) pipeline: Option<String>,
    pub(super) hub: Arc<AcpWebSocketHub>,
}

pub(super) struct AcpWebSocketHub {
    state: Mutex<AcpWebSocketHubState>,
    event_log: Arc<AnyEventLog>,
    retention: Duration,
}

#[derive(Default)]
struct AcpWebSocketHubState {
    workers_by_id: BTreeMap<String, Arc<AcpWorker>>,
    workers_by_session: BTreeMap<String, Arc<AcpWorker>>,
}

struct AcpWorker {
    id: String,
    request_tx: Mutex<Option<mpsc::UnboundedSender<JsonValue>>>,
    clients: Mutex<BTreeMap<String, AcpClient>>,
    host_owner_client_id: Mutex<Option<String>>,
    pending_client_requests: Mutex<BTreeMap<String, String>>,
    pending_host_requests: Mutex<BTreeMap<String, AcpHostRequest>>,
    decided_host_requests: Mutex<BTreeMap<String, AcpHostDecision>>,
    sessions: Mutex<BTreeMap<String, AcpSessionSummary>>,
    replay_buffer: Mutex<VecDeque<AcpReplayEvent>>,
    next_event_id: AtomicU64,
    detached_at: Mutex<Option<Instant>>,
    event_log: Arc<AnyEventLog>,
    hub: Weak<AcpWebSocketHub>,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
enum AcpAttachRole {
    HostOwner,
    Controller,
    Observer,
}

#[derive(Clone)]
struct AcpClient {
    connection_id: String,
    role: AcpAttachRole,
    socket_tx: mpsc::UnboundedSender<String>,
}

struct AcpAttachOptions {
    client_id: String,
    connection_id: String,
    role: AcpAttachRole,
    socket_tx: mpsc::UnboundedSender<String>,
    last_acked_event_id: u64,
}

struct AcpAttachResult {
    session_id: String,
    role: AcpAttachRole,
    session: Option<AcpSessionSummary>,
    live_state: &'static str,
    attachable_roles: Vec<&'static str>,
    presence_state: &'static str,
}

#[derive(Clone, Debug)]
struct AcpSessionSummary {
    session_id: String,
    cwd: Option<String>,
    title: Option<String>,
    meta: serde_json::Map<String, JsonValue>,
    workspace_anchor: Option<JsonValue>,
    last_event_id: Option<u64>,
}

#[derive(Clone)]
struct AcpReplayEvent {
    id: u64,
    line: String,
    session_id: Option<String>,
}

#[derive(Debug)]
enum AcpAttachError {
    NotFound,
    HostOwnerClaimed,
}

#[derive(Debug)]
enum AcpClientRequestError {
    Disconnected,
    DuplicateRequestId,
    UnknownHostRequest {
        request_id: String,
    },
    IdempotentHostDecision {
        decision: AcpHostDecision,
    },
    AlreadyDecided {
        decision: AcpHostDecision,
        attempted_actor: AcpControlActor,
        attempted_payload: JsonValue,
    },
    Forbidden {
        method: String,
        role: AcpAttachRole,
    },
}

enum AcpOutboundRoute {
    HostRequest { id_key: String, method: String },
    Response(Option<String>),
    Broadcast,
}

#[derive(Clone, Debug)]
struct AcpHostRequest {
    method: String,
    session_id: Option<String>,
}

#[derive(Clone, Debug)]
struct AcpControlActor {
    client_id: String,
    connection_id: Option<String>,
    role: AcpAttachRole,
    source: String,
}

#[derive(Clone, Debug)]
struct AcpHostDecision {
    request_id: String,
    method: String,
    session_id: Option<String>,
    actor: AcpControlActor,
    payload: JsonValue,
    decided_at_ms: u128,
}

struct PersistedAcpReplay {
    summary: Option<AcpSessionSummary>,
    replayed: Vec<JsonValue>,
}

impl AcpWebSocketHub {
    pub(super) fn new(event_log: Arc<AnyEventLog>, retention: Duration) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(AcpWebSocketHubState::default()),
            event_log,
            retention,
        })
    }

    fn spawn_worker(
        self: &Arc<Self>,
        pipeline: Option<String>,
    ) -> Result<Arc<AcpWorker>, OrchestratorError> {
        let worker_id = uuid::Uuid::new_v4().to_string();
        let (to_acp_tx, to_acp_rx) = mpsc::unbounded_channel::<JsonValue>();
        let (from_acp_tx, mut from_acp_rx) = mpsc::unbounded_channel::<String>();
        let worker = Arc::new(AcpWorker {
            id: worker_id.clone(),
            request_tx: Mutex::new(Some(to_acp_tx)),
            clients: Mutex::new(BTreeMap::new()),
            host_owner_client_id: Mutex::new(None),
            pending_client_requests: Mutex::new(BTreeMap::new()),
            pending_host_requests: Mutex::new(BTreeMap::new()),
            decided_host_requests: Mutex::new(BTreeMap::new()),
            sessions: Mutex::new(BTreeMap::new()),
            replay_buffer: Mutex::new(VecDeque::new()),
            next_event_id: AtomicU64::new(1),
            detached_at: Mutex::new(None),
            event_log: self.event_log.clone(),
            hub: Arc::downgrade(self),
        });
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .workers_by_id
            .insert(worker_id.clone(), worker.clone());

        let worker_for_output = worker.clone();
        tokio::spawn(async move {
            while let Some(line) = from_acp_rx.recv().await {
                worker_for_output.handle_output(line).await;
            }
        });

        let worker_name = worker_id;
        std::thread::Builder::new()
            .name(format!("harn-acp-ws-{worker_name}"))
            .spawn(move || {
                let runtime = match tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                {
                    Ok(runtime) => runtime,
                    Err(error) => {
                        eprintln!("[harn] failed to start ACP WebSocket worker: {error}");
                        return;
                    }
                };
                runtime.block_on(crate::acp::run_acp_channel_server(
                    pipeline,
                    to_acp_rx,
                    from_acp_tx,
                ));
            })
            .map_err(|error| format!("worker spawn failed: {error}"))?;

        Ok(worker)
    }

    fn register_session(
        &self,
        session_id: String,
        worker: &Arc<AcpWorker>,
        summary: Option<AcpSessionSummary>,
    ) {
        worker.merge_session_summary(session_id.clone(), summary);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .workers_by_session
            .entry(session_id)
            .or_insert_with(|| worker.clone());
    }

    fn attach(
        &self,
        session_id: &str,
        options: AcpAttachOptions,
    ) -> Result<(Arc<AcpWorker>, AcpAttachResult), AcpAttachError> {
        let worker = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .workers_by_session
            .get(session_id)
            .cloned()
            .ok_or(AcpAttachError::NotFound)?;
        let attached = worker.attach(session_id, options)?;
        Ok((worker, attached))
    }

    fn remove_worker(&self, worker: &Arc<AcpWorker>) {
        let sessions = worker.session_ids();
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.workers_by_id.remove(&worker.id);
        for session_id in sessions {
            if state
                .workers_by_session
                .get(&session_id)
                .is_some_and(|mapped| Arc::ptr_eq(mapped, worker))
            {
                state.workers_by_session.remove(&session_id);
            }
        }
        worker.shutdown();
    }

    pub(super) async fn run_expiry_sweeper(self: Arc<Self>) {
        let sweep_interval = self
            .retention
            .min(Duration::from_secs(15))
            .max(Duration::from_secs(1));
        let mut interval = tokio::time::interval(sweep_interval);
        interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
        loop {
            interval.tick().await;
            self.sweep_expired_once().await;
        }
    }

    async fn sweep_expired_once(&self) {
        let expired: Vec<Arc<AcpWorker>> = {
            let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state
                .workers_by_id
                .values()
                .filter(|worker| worker.is_expired(self.retention))
                .cloned()
                .collect()
        };
        for worker in expired {
            let sessions = worker.session_ids();
            self.remove_worker(&worker);
            append_acp_event(
                &self.event_log,
                &worker.id,
                "session_worker_expired",
                json!({
                    "worker_id": worker.id,
                    "session_ids": sessions,
                    "retention_ms": self.retention.as_millis(),
                }),
            )
            .await;
        }
    }

    #[cfg(test)]
    pub(super) async fn sweep_expired_once_for_test(&self) {
        self.sweep_expired_once().await;
    }

    #[cfg(test)]
    pub(super) fn session_is_detached_for_test(&self, session_id: &str) -> bool {
        let worker = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .workers_by_session
            .get(session_id)
            .cloned();
        worker.is_some_and(|worker| {
            worker
                .detached_at
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .is_some()
        })
    }

    async fn discover_sessions(&self, params: &JsonValue) -> Vec<JsonValue> {
        let mut summaries: BTreeMap<String, JsonValue> = BTreeMap::new();
        let workers: Vec<Arc<AcpWorker>> = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .workers_by_session
            .values()
            .cloned()
            .collect();
        for worker in workers {
            let live_state = worker.live_state();
            let attachable_roles = worker.attachable_roles();
            for summary in worker.session_summaries() {
                if acp_session_summary_matches(&summary, live_state, params) {
                    summaries.insert(
                        summary.session_id.clone(),
                        summary.to_json(live_state, &attachable_roles),
                    );
                }
            }
        }

        for summary in persisted_acp_session_summaries(&self.event_log).await {
            if summaries.contains_key(&summary.session_id) {
                continue;
            }
            if acp_session_summary_matches(&summary, "expired_replay_only", params) {
                summaries.insert(
                    summary.session_id.clone(),
                    summary.to_json("expired_replay_only", &[]),
                );
            }
        }

        summaries.into_values().collect()
    }
}

impl AcpSessionSummary {
    fn new(session_id: String) -> Self {
        Self {
            session_id,
            cwd: None,
            title: None,
            meta: serde_json::Map::new(),
            workspace_anchor: None,
            last_event_id: None,
        }
    }

    fn merge(&mut self, other: AcpSessionSummary) {
        if other.cwd.is_some() {
            self.cwd = other.cwd;
        }
        if other.title.is_some() {
            self.title = other.title;
        }
        if other.workspace_anchor.is_some() {
            self.workspace_anchor = other.workspace_anchor;
        }
        if other.last_event_id.is_some() {
            self.last_event_id = other.last_event_id;
        }
        for (key, value) in other.meta {
            self.meta.insert(key, value);
        }
    }

    fn to_json(&self, live_state: &str, attachable_roles: &[&str]) -> JsonValue {
        let mut item = json!({
            "sessionId": self.session_id,
            "liveState": live_state,
            "attachableRoles": attachable_roles,
        });
        if let Some(cwd) = self.cwd.as_ref() {
            item["cwd"] = json!(cwd);
        }
        if let Some(title) = self.title.as_ref() {
            item["title"] = json!(title);
        }
        if let Some(workspace_anchor) = self.workspace_anchor.as_ref() {
            item["workspaceAnchor"] = workspace_anchor.clone();
        }
        if let Some(last_event_id) = self.last_event_id {
            item["lastEventId"] = json!(last_event_id);
        }

        let mut meta = self.meta.clone();
        let mut harn_meta = match meta.remove("harn") {
            Some(JsonValue::Object(map)) => map,
            _ => serde_json::Map::new(),
        };
        harn_meta.insert("liveState".to_string(), json!(live_state));
        harn_meta.insert("attachableRoles".to_string(), json!(attachable_roles));
        if let Some(workspace_anchor) = self.workspace_anchor.as_ref() {
            harn_meta.insert("workspaceAnchor".to_string(), workspace_anchor.clone());
        }
        if let Some(last_event_id) = self.last_event_id {
            harn_meta.insert("lastEventId".to_string(), json!(last_event_id));
        }
        meta.insert("harn".to_string(), JsonValue::Object(harn_meta));
        item["_meta"] = JsonValue::Object(meta);
        item
    }
}

impl AcpAttachRole {
    fn as_str(self) -> &'static str {
        match self {
            Self::HostOwner => "host_owner",
            Self::Controller => "controller",
            Self::Observer => "observer",
        }
    }

    fn parse(value: &str) -> Option<Self> {
        match value {
            "host_owner" | "hostOwner" | "owner" => Some(Self::HostOwner),
            "controller" => Some(Self::Controller),
            "observer" | "read_only" | "readOnly" => Some(Self::Observer),
            _ => None,
        }
    }

    fn capabilities_json(self) -> JsonValue {
        match self {
            Self::HostOwner => json!({
                "hostOwner": true,
                "control": true,
                "observe": true,
                "receiveReplay": true,
            }),
            Self::Controller => json!({
                "hostOwner": false,
                "control": true,
                "observe": true,
                "receiveReplay": true,
            }),
            Self::Observer => json!({
                "hostOwner": false,
                "control": false,
                "observe": true,
                "receiveReplay": true,
            }),
        }
    }
}

impl AcpControlActor {
    fn to_json(&self) -> JsonValue {
        let mut value = json!({
            "clientId": self.client_id,
            "role": self.role.as_str(),
            "source": self.source,
        });
        if let Some(connection_id) = self.connection_id.as_ref() {
            value["connectionId"] = json!(connection_id);
        }
        value
    }
}

impl AcpHostDecision {
    fn error_data(&self, reason: &str, attempted_actor: Option<&AcpControlActor>) -> JsonValue {
        let mut data = json!({
            "reason": reason,
            "requestId": self.request_id,
            "method": self.method,
            "decidedBy": self.actor.to_json(),
            "decidedAtMs": self.decided_at_ms,
            "decision": self.payload,
        });
        if let Some(session_id) = self.session_id.as_ref() {
            data["sessionId"] = json!(session_id);
        }
        if let Some(actor) = attempted_actor {
            data["attemptedBy"] = actor.to_json();
        }
        data
    }
}

impl AcpWorker {
    fn attach(
        &self,
        session_id: &str,
        options: AcpAttachOptions,
    ) -> Result<AcpAttachResult, AcpAttachError> {
        if options.role == AcpAttachRole::HostOwner {
            let mut host_owner = self
                .host_owner_client_id
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if host_owner
                .as_deref()
                .is_some_and(|client_id| client_id != options.client_id)
            {
                return Err(AcpAttachError::HostOwnerClaimed);
            }
            *host_owner = Some(options.client_id.clone());
            *self.detached_at.lock().unwrap_or_else(|e| e.into_inner()) = None;
        }

        let presence_state = {
            let mut clients = self.clients.lock().unwrap_or_else(|e| e.into_inner());
            let presence_state = if clients.contains_key(&options.client_id) {
                "reconnected"
            } else {
                "attached"
            };
            clients.insert(
                options.client_id.clone(),
                AcpClient {
                    connection_id: options.connection_id,
                    role: options.role,
                    socket_tx: options.socket_tx.clone(),
                },
            );
            presence_state
        };

        self.replay_since(options.last_acked_event_id, options.role, options.socket_tx);
        Ok(AcpAttachResult {
            session_id: session_id.to_string(),
            role: options.role,
            session: self.session_summary(session_id),
            live_state: self.live_state(),
            attachable_roles: self.attachable_roles(),
            presence_state,
        })
    }

    fn detach(&self, connection_id: &str) -> Option<(String, AcpAttachRole)> {
        let removed = {
            let mut clients = self.clients.lock().unwrap_or_else(|e| e.into_inner());
            let client_id = clients.iter().find_map(|(client_id, client)| {
                (client.connection_id == connection_id).then(|| client_id.clone())
            });
            client_id.and_then(|client_id| {
                let removed = clients.remove(&client_id)?;
                Some((client_id, removed.role))
            })
        };
        let (client_id, role) = removed?;
        if role == AcpAttachRole::HostOwner {
            let mut host_owner = self
                .host_owner_client_id
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if host_owner.as_deref() == Some(client_id.as_str()) {
                *host_owner = None;
                *self.detached_at.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
            }
        }
        Some((client_id, role))
    }

    fn client_actor(&self, client_id: &str, role: AcpAttachRole) -> AcpControlActor {
        let connection_id = self
            .clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(client_id)
            .map(|client| client.connection_id.clone());
        AcpControlActor {
            client_id: client_id.to_string(),
            connection_id,
            role,
            source: "websocket".to_string(),
        }
    }

    fn pending_host_request(&self, id_key: &str) -> Option<AcpHostRequest> {
        self.pending_host_requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(id_key)
            .cloned()
    }

    fn annotate_control_actor(&self, value: &mut JsonValue, actor: &AcpControlActor) {
        let Some(params) = value.get_mut("params") else {
            return;
        };
        let Some(params) = params.as_object_mut() else {
            return;
        };
        let harn = params
            .entry("_harn".to_string())
            .or_insert_with(|| json!({}));
        if let Some(harn) = harn.as_object_mut() {
            harn.insert("actor".to_string(), actor.to_json());
            harn.entry("clientId".to_string())
                .or_insert_with(|| json!(actor.client_id));
            harn.entry("role".to_string())
                .or_insert_with(|| json!(actor.role.as_str()));
            harn.entry("source".to_string())
                .or_insert_with(|| json!(actor.source));
            if let Some(connection_id) = actor.connection_id.as_ref() {
                harn.entry("connectionId".to_string())
                    .or_insert_with(|| json!(connection_id));
            }
        }
    }

    fn decision_payload(value: &JsonValue) -> JsonValue {
        if let Some(error) = value.get("error") {
            json!({"error": error})
        } else {
            json!({"result": value.get("result").cloned().unwrap_or(JsonValue::Null)})
        }
    }

    async fn append_control_outcome(&self, decision: &AcpHostDecision, status: &str, reason: &str) {
        let topic_id = decision.session_id.as_deref().unwrap_or(&self.id);
        append_acp_event(
            &self.event_log,
            topic_id,
            "control_outcome",
            json!({
                "request_id": decision.request_id,
                "method": decision.method,
                "status": status,
                "reason": reason,
                "actor": decision.actor.to_json(),
                "decision": decision.payload,
                "decided_at_ms": decision.decided_at_ms,
            }),
        )
        .await;
    }

    async fn send_client_request(
        &self,
        client_id: &str,
        role: AcpAttachRole,
        mut value: JsonValue,
    ) -> Result<(), AcpClientRequestError> {
        let actor = self.client_actor(client_id, role);
        if let Some(method) = value.get("method").and_then(JsonValue::as_str) {
            if !role_may_send_method(role, method) {
                return Err(AcpClientRequestError::Forbidden {
                    method: method.to_string(),
                    role,
                });
            }
            self.annotate_control_actor(&mut value, &actor);
        } else if let Some(id_key) = jsonrpc_id_key(&value) {
            if let Some(decision) = self
                .decided_host_requests
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .get(&id_key)
                .cloned()
            {
                let attempted_payload = Self::decision_payload(&value);
                if decision.actor.client_id == actor.client_id
                    && decision.payload == attempted_payload
                {
                    return Err(AcpClientRequestError::IdempotentHostDecision { decision });
                }
                return Err(AcpClientRequestError::AlreadyDecided {
                    decision,
                    attempted_actor: actor,
                    attempted_payload,
                });
            }

            let Some(host_request) = self.pending_host_request(&id_key) else {
                return Err(AcpClientRequestError::UnknownHostRequest { request_id: id_key });
            };
            if role != AcpAttachRole::HostOwner
                && !(role == AcpAttachRole::Controller
                    && host_request.method == "session/request_permission")
            {
                return Err(AcpClientRequestError::Forbidden {
                    method: host_request.method,
                    role,
                });
            }

            self.request_tx
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .as_ref()
                .ok_or(AcpClientRequestError::Disconnected)?
                .send(value.clone())
                .map_err(|_| AcpClientRequestError::Disconnected)?;

            self.pending_host_requests
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .remove(&id_key);
            let decision = AcpHostDecision {
                request_id: id_key.clone(),
                method: host_request.method,
                session_id: host_request.session_id,
                actor,
                payload: Self::decision_payload(&value),
                decided_at_ms: unix_epoch_ms(),
            };
            {
                let mut decided = self
                    .decided_host_requests
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                decided.insert(id_key, decision.clone());
                while decided.len() > ACP_REPLAY_BUFFER_LIMIT {
                    let Some(first_key) = decided.keys().next().cloned() else {
                        break;
                    };
                    decided.remove(&first_key);
                }
            }
            self.append_control_outcome(&decision, "accepted", "first_valid_response")
                .await;
            return Ok(());
        } else {
            return Err(AcpClientRequestError::Forbidden {
                method: "<response>".to_string(),
                role,
            });
        }

        if let Some(id_key) = jsonrpc_id_key(&value) {
            if value.get("method").is_some() {
                let mut pending = self
                    .pending_client_requests
                    .lock()
                    .unwrap_or_else(|e| e.into_inner());
                if pending
                    .get(&id_key)
                    .is_some_and(|pending_client_id| pending_client_id != client_id)
                {
                    return Err(AcpClientRequestError::DuplicateRequestId);
                }
                pending.insert(id_key, client_id.to_string());
            }
        }

        self.request_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .ok_or(AcpClientRequestError::Disconnected)?
            .send(value)
            .map_err(|_| AcpClientRequestError::Disconnected)
    }

    fn shutdown(&self) {
        self.request_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        self.clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.host_owner_client_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        self.pending_host_requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
        self.decided_host_requests
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clear();
    }

    fn is_expired(&self, retention: Duration) -> bool {
        self.detached_at
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some_and(|detached_at| detached_at.elapsed() >= retention)
    }

    fn session_ids(&self) -> Vec<String> {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect()
    }

    fn session_summaries(&self) -> Vec<AcpSessionSummary> {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .cloned()
            .collect()
    }

    fn session_summary(&self, session_id: &str) -> Option<AcpSessionSummary> {
        self.sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(session_id)
            .cloned()
    }

    fn merge_session_summary(&self, session_id: String, summary: Option<AcpSessionSummary>) {
        let mut sessions = self.sessions.lock().unwrap_or_else(|e| e.into_inner());
        let entry = sessions
            .entry(session_id.clone())
            .or_insert_with(|| AcpSessionSummary::new(session_id));
        if let Some(summary) = summary {
            entry.merge(summary);
        }
    }

    fn live_state(&self) -> &'static str {
        if self.has_host_owner() {
            "live"
        } else {
            "detached_retained"
        }
    }

    fn attachable_roles(&self) -> Vec<&'static str> {
        if self.has_host_owner() {
            vec!["observer", "controller"]
        } else {
            vec!["host_owner", "observer"]
        }
    }

    fn has_host_owner(&self) -> bool {
        self.host_owner_client_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .is_some()
    }

    async fn handle_output(self: &Arc<Self>, line: String) {
        let mut summaries = acp_session_summaries_from_message(&line);
        let session_id = summaries
            .first()
            .map(|summary| summary.session_id.clone())
            .or_else(|| session_id_from_acp_message(&line))
            .or_else(|| {
                let sessions = self.session_ids();
                (sessions.len() == 1).then(|| sessions[0].clone())
            });

        let event_id = self.next_event_id.fetch_add(1, Ordering::SeqCst);
        if summaries.is_empty() {
            if let Some(session_id) = session_id.clone() {
                summaries.push(AcpSessionSummary::new(session_id));
            }
        }
        if let Some(hub) = self.hub.upgrade() {
            for mut summary in summaries {
                summary.last_event_id = Some(event_id);
                hub.register_session(summary.session_id.clone(), self, Some(summary));
            }
        }
        let annotated = annotate_acp_line(&line, event_id, session_id.as_deref(), false);
        {
            let mut replay_buffer = self.replay_buffer.lock().unwrap_or_else(|e| e.into_inner());
            replay_buffer.push_back(AcpReplayEvent {
                id: event_id,
                line: annotated.clone(),
                session_id: session_id.clone(),
            });
            while replay_buffer.len() > ACP_REPLAY_BUFFER_LIMIT {
                replay_buffer.pop_front();
            }
        }

        let topic_id = session_id.as_deref().unwrap_or(&self.id);
        append_acp_event(
            &self.event_log,
            topic_id,
            "message_sent",
            acp_replay_log_payload(&annotated, event_id, session_id.as_deref()),
        )
        .await;

        if let AcpOutboundRoute::HostRequest { id_key, method } = acp_outbound_route(&line) {
            self.pending_host_requests
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .insert(
                    id_key,
                    AcpHostRequest {
                        method,
                        session_id: session_id.clone(),
                    },
                );
        }

        self.deliver_output(&line, annotated);
    }

    async fn emit_presence(
        self: &Arc<Self>,
        session_id: &str,
        client_id: &str,
        connection_id: &str,
        role: AcpAttachRole,
        state: &str,
    ) {
        let event_id = self.next_event_id.fetch_add(1, Ordering::SeqCst);
        let line = harn_vm::jsonrpc::notification(
            "_harn/presence",
            json!({
                "sessionId": session_id,
                "clientId": client_id,
                "connectionId": connection_id,
                "role": role.as_str(),
                "state": state,
            }),
        );
        let Ok(line) = serde_json::to_string(&line) else {
            return;
        };
        let annotated = annotate_acp_line(&line, event_id, Some(session_id), false);
        {
            let mut replay_buffer = self.replay_buffer.lock().unwrap_or_else(|e| e.into_inner());
            replay_buffer.push_back(AcpReplayEvent {
                id: event_id,
                line: annotated.clone(),
                session_id: Some(session_id.to_string()),
            });
            while replay_buffer.len() > ACP_REPLAY_BUFFER_LIMIT {
                replay_buffer.pop_front();
            }
        }
        append_acp_event(
            &self.event_log,
            session_id,
            "message_sent",
            acp_replay_log_payload(&annotated, event_id, Some(session_id)),
        )
        .await;
        self.broadcast(annotated);
    }

    fn replay_since(
        &self,
        last_acked_event_id: u64,
        role: AcpAttachRole,
        socket_tx: mpsc::UnboundedSender<String>,
    ) {
        let events: Vec<AcpReplayEvent> = self
            .replay_buffer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|event| event.id > last_acked_event_id)
            .filter(|event| {
                role == AcpAttachRole::HostOwner || replay_visible_to_non_owner(&event.line)
            })
            .cloned()
            .collect();
        for event in events {
            let replayed =
                annotate_acp_line(&event.line, event.id, event.session_id.as_deref(), true);
            let _ = socket_tx.send(replayed);
        }
    }

    fn deliver_output(&self, original_line: &str, annotated: String) {
        match acp_outbound_route(original_line) {
            AcpOutboundRoute::HostRequest { method, .. } => {
                if method == "session/request_permission" {
                    self.send_to_control_actors(annotated);
                } else {
                    self.send_to_host_owner(annotated);
                }
            }
            AcpOutboundRoute::Response(id_key) => {
                let target = id_key.and_then(|id_key| {
                    self.pending_client_requests
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .remove(&id_key)
                });
                if let Some(client_id) = target {
                    if !self.send_to_client(&client_id, annotated.clone()) {
                        self.send_to_host_owner(annotated);
                    }
                } else {
                    self.send_to_host_owner(annotated);
                }
            }
            AcpOutboundRoute::Broadcast => self.broadcast(annotated),
        }
    }

    fn send_to_host_owner(&self, line: String) {
        let host_owner = self
            .host_owner_client_id
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(client_id) = host_owner {
            let _ = self.send_to_client(&client_id, line);
        }
    }

    fn send_to_control_actors(&self, line: String) {
        let clients: Vec<mpsc::UnboundedSender<String>> = self
            .clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .filter(|client| {
                matches!(
                    client.role,
                    AcpAttachRole::HostOwner | AcpAttachRole::Controller
                )
            })
            .map(|client| client.socket_tx.clone())
            .collect();
        for socket_tx in clients {
            let _ = socket_tx.send(line.clone());
        }
    }

    fn broadcast(&self, line: String) {
        let clients: Vec<mpsc::UnboundedSender<String>> = self
            .clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .values()
            .map(|client| client.socket_tx.clone())
            .collect();
        for socket_tx in clients {
            let _ = socket_tx.send(line.clone());
        }
    }

    fn send_to_client(&self, client_id: &str, line: String) -> bool {
        let socket_tx = self
            .clients
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(client_id)
            .map(|client| client.socket_tx.clone());
        socket_tx
            .map(|socket_tx| socket_tx.send(line).is_ok())
            .unwrap_or(false)
    }
}

pub(super) fn acp_retained_session_duration_from_env() -> Duration {
    let seconds = std::env::var(ACP_RETAINED_SESSION_SECS_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(ACP_DEFAULT_RETAINED_SESSION_SECS);
    Duration::from_secs(seconds)
}

pub(super) async fn acp_websocket_endpoint(
    Extension(state): Extension<Arc<AcpWebSocketState>>,
    ws: WebSocketUpgrade,
    method: Method,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
) -> Response {
    let normalized_headers = normalize_headers(&headers);
    if state.auth.has_credentials()
        && state
            .auth
            .authorize(
                state.event_log.as_ref(),
                method.as_str(),
                uri.path(),
                &normalized_headers,
                &[],
            )
            .await
            .is_err()
    {
        return HttpError::unauthorized("auth failed").into_response();
    }

    ws.on_upgrade(move |socket| run_acp_websocket(socket, state))
        .into_response()
}

async fn run_acp_websocket(socket: WebSocket, state: Arc<AcpWebSocketState>) {
    let connection_id = uuid::Uuid::new_v4().to_string();
    append_acp_event(
        &state.event_log,
        &connection_id,
        "connection_opened",
        json!({
            "transport": "websocket",
            "path": ACP_PATH,
        }),
    )
    .await;

    let (mut sender, mut receiver) = socket.split();
    let (socket_tx, mut socket_rx) = mpsc::unbounded_channel::<String>();
    let mut ping_interval = tokio::time::interval_at(
        tokio::time::Instant::now() + ACP_PING_INTERVAL,
        ACP_PING_INTERVAL,
    );
    ping_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut liveness_interval = tokio::time::interval(Duration::from_secs(1));
    liveness_interval.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
    let mut ping_sent_at: Option<Instant> = None;
    let mut session_id: Option<String> = None;
    let mut worker: Option<Arc<AcpWorker>> = None;
    let mut client_id = connection_id.clone();
    let mut client_role = AcpAttachRole::HostOwner;

    loop {
        tokio::select! {
            Some(line) = socket_rx.recv() => {
                if let Some(id) = session_id_from_acp_response(&line) {
                    session_id = Some(id.clone());
                    append_acp_event(
                        &state.event_log,
                        &connection_id,
                        "session_opened",
                        json!({"session_id": id}),
                    )
                    .await;
                }
                append_acp_event(
                    &state.event_log,
                    &connection_id,
                    "message_sent",
                    acp_message_log_payload(&line, session_id.as_deref()),
                )
                .await;
                if sender.send(WsMessage::Text(line.into())).await.is_err() {
                    break;
                }
            }
            frame = receiver.next() => {
                let Some(frame) = frame else {
                    break;
                };
                let Ok(frame) = frame else {
                    break;
                };
                match frame {
                    WsMessage::Text(text) => {
                        let line = text.to_string();
                        append_acp_event(
                            &state.event_log,
                            &connection_id,
                            "message_received",
                            acp_message_log_payload(&line, session_id.as_deref()),
                        )
                        .await;
                        match serde_json::from_str::<JsonValue>(&line) {
                            Ok(value) => {
                                if is_session_list_request(&value) {
                                    let sessions = state
                                        .hub
                                        .discover_sessions(
                                            value.get("params").unwrap_or(&JsonValue::Null),
                                        )
                                        .await;
                                    send_socket_jsonrpc_result(
                                        &socket_tx,
                                        value.get("id").unwrap_or(&JsonValue::Null),
                                        json!({"sessions": sessions}),
                                    );
                                    continue;
                                }
                                if let Some(load_session_id) = session_load_session_id(&value) {
                                    let requested_role = attach_role_from_request(&value);
                                    let requested_client_id =
                                        client_id_from_request(&value, &connection_id);
                                    match state.hub.attach(
                                        &load_session_id,
                                        AcpAttachOptions {
                                            client_id: requested_client_id.clone(),
                                            connection_id: connection_id.clone(),
                                            role: requested_role,
                                            socket_tx: socket_tx.clone(),
                                            last_acked_event_id: last_acked_event_id(&value),
                                        },
                                    ) {
                                        Ok((attached, attach_result)) => {
                                            session_id = Some(load_session_id.clone());
                                            client_id = requested_client_id;
                                            client_role = attach_result.role;
                                            attached
                                                .emit_presence(
                                                    &load_session_id,
                                                    &client_id,
                                                    &connection_id,
                                                    client_role,
                                                    attach_result.presence_state,
                                                )
                                                .await;
                                            worker = Some(attached);
                                            if attach_result.role != AcpAttachRole::HostOwner {
                                                send_socket_jsonrpc_result(
                                                    &socket_tx,
                                                    value.get("id").unwrap_or(&JsonValue::Null),
                                                    session_load_attach_result(attach_result),
                                                );
                                                continue;
                                            }
                                        }
                                        Err(AcpAttachError::HostOwnerClaimed) => {
                                            send_socket_jsonrpc_error(
                                                &socket_tx,
                                                value.get("id").unwrap_or(&JsonValue::Null),
                                                -32010,
                                                "ACP session host_owner role is already attached to another WebSocket",
                                            );
                                            continue;
                                        }
                                        Err(AcpAttachError::NotFound) => {
                                            let replay = replay_persisted_acp_events(
                                                &state.event_log,
                                                &load_session_id,
                                                last_acked_event_id(&value),
                                                &socket_tx,
                                            )
                                            .await;
                                            if worker.is_none() {
                                                if let Some(summary) = replay.summary {
                                                    send_socket_jsonrpc_result(
                                                        &socket_tx,
                                                        value
                                                            .get("id")
                                                            .unwrap_or(&JsonValue::Null),
                                                        json!({
                                                            "session": summary
                                                                .to_json("expired_replay_only", &[]),
                                                            "replayed": replay.replayed,
                                                        }),
                                                    );
                                                } else {
                                                    send_socket_jsonrpc_error(
                                                        &socket_tx,
                                                        value
                                                            .get("id")
                                                            .unwrap_or(&JsonValue::Null),
                                                        -32004,
                                                        &format!("Session not found: {load_session_id}"),
                                                    );
                                                }
                                                continue;
                                            }
                                        }
                                    }
                                }
                                if worker.is_none() {
                                    match state.hub.spawn_worker(state.pipeline.clone()) {
                                        Ok(new_worker) => {
                                            client_id = client_id_from_request(&value, &connection_id);
                                            client_role = AcpAttachRole::HostOwner;
                                            if let Err(attach_err) = new_worker.attach(
                                                session_id.as_deref().unwrap_or(&new_worker.id),
                                                AcpAttachOptions {
                                                    client_id: client_id.clone(),
                                                    connection_id: connection_id.clone(),
                                                    role: client_role,
                                                    socket_tx: socket_tx.clone(),
                                                    last_acked_event_id: 0,
                                                },
                                            ) {
                                                append_acp_event(
                                                    &state.event_log,
                                                    &connection_id,
                                                    "connection_failed",
                                                    json!({"reason": format!("{attach_err:?}")}),
                                                )
                                                .await;
                                                break;
                                            }
                                            worker = Some(new_worker);
                                        }
                                        Err(error) => {
                                            append_acp_event(
                                                &state.event_log,
                                                &connection_id,
                                                "connection_failed",
                                                json!({"reason": error.to_string()}),
                                            )
                                            .await;
                                            break;
                                        }
                                    }
                                }
                                let Some(worker) = worker.as_ref() else {
                                    break;
                                };
                                let request_id =
                                    value.get("id").cloned().unwrap_or(JsonValue::Null);
                                match worker
                                    .send_client_request(&client_id, client_role, value)
                                    .await
                                {
                                    Ok(()) => {}
                                    Err(AcpClientRequestError::Forbidden { method, role }) => {
                                        send_socket_jsonrpc_error_with_data(
                                            &socket_tx,
                                            &request_id,
                                            -32011,
                                            "ACP client role is not authorized for this method",
                                            json!({
                                                "method": method,
                                                "role": role.as_str(),
                                                "reason": "role_not_authorized",
                                            }),
                                        );
                                    }
                                    Err(AcpClientRequestError::UnknownHostRequest {
                                        request_id: unknown_request_id,
                                    }) => {
                                        send_socket_jsonrpc_error_with_data(
                                            &socket_tx,
                                            &request_id,
                                            -32014,
                                            "ACP host request is not pending",
                                            json!({
                                                "requestId": unknown_request_id,
                                                "reason": "unknown_request_id",
                                            }),
                                        );
                                    }
                                    Err(AcpClientRequestError::IdempotentHostDecision {
                                        decision,
                                    }) => {
                                        worker
                                            .append_control_outcome(
                                                &decision,
                                                "idempotent",
                                                "same_actor_same_decision",
                                            )
                                            .await;
                                        send_socket_jsonrpc_result(
                                            &socket_tx,
                                            &request_id,
                                            json!({
                                                "status": "already_applied",
                                                "_meta": {
                                                    "harn": decision.error_data(
                                                        "same_actor_same_decision",
                                                        None,
                                                    ),
                                                },
                                            }),
                                        );
                                    }
                                    Err(AcpClientRequestError::AlreadyDecided {
                                        decision,
                                        attempted_actor,
                                        attempted_payload,
                                    }) => {
                                        worker
                                            .append_control_outcome(
                                                &decision,
                                                "rejected",
                                                "already_decided",
                                            )
                                            .await;
                                        let mut data = decision
                                            .error_data("already_decided", Some(&attempted_actor));
                                        data["attemptedDecision"] = attempted_payload;
                                        send_socket_jsonrpc_error_with_data(
                                            &socket_tx,
                                            &request_id,
                                            -32013,
                                            "ACP host request already has a decision",
                                            data,
                                        );
                                    }
                                    Err(AcpClientRequestError::DuplicateRequestId) => {
                                        send_socket_jsonrpc_error(
                                            &socket_tx,
                                            &request_id,
                                            -32012,
                                            "JSON-RPC request id is already in flight for another ACP client",
                                        );
                                    }
                                    Err(AcpClientRequestError::Disconnected) => break,
                                }
                            }
                            Err(error) => {
                                let response = harn_vm::jsonrpc::error_response(
                                    JsonValue::Null,
                                    -32700,
                                    &format!("Parse error: {error}"),
                                );
                                if let Ok(line) = serde_json::to_string(&response) {
                                    let _ = sender.send(WsMessage::Text(line.into())).await;
                                }
                            }
                        }
                    }
                    WsMessage::Binary(_) => {
                        let response = harn_vm::jsonrpc::error_response(
                            JsonValue::Null,
                            -32600,
                            "ACP WebSocket transport only accepts JSON-RPC text frames",
                        );
                        if let Ok(line) = serde_json::to_string(&response) {
                            let _ = sender.send(WsMessage::Text(line.into())).await;
                        }
                    }
                    WsMessage::Ping(payload) => {
                        if sender.send(WsMessage::Pong(payload)).await.is_err() {
                            break;
                        }
                    }
                    WsMessage::Pong(_) => {
                        ping_sent_at = None;
                    }
                    WsMessage::Close(_) => {
                        break;
                    }
                }
            }
            _ = ping_interval.tick() => {
                if ping_sent_at.is_none() {
                    ping_sent_at = Some(Instant::now());
                    if sender.send(WsMessage::Ping(Vec::new().into())).await.is_err() {
                        break;
                    }
                }
            }
            _ = liveness_interval.tick() => {
                if ping_sent_at.is_some_and(|sent| sent.elapsed() > ACP_PONG_TIMEOUT) {
                    let _ = sender.send(WsMessage::Close(None)).await;
                    append_acp_event(
                        &state.event_log,
                        &connection_id,
                        "connection_liveness_timeout",
                        json!({"timeout_ms": ACP_PONG_TIMEOUT.as_millis()}),
                    )
                    .await;
                    break;
                }
            }
        }
    }

    if let Some(worker) = worker.as_ref() {
        if let Some((client_id, role)) = worker.detach(&connection_id) {
            if let Some(session_id) = session_id.as_deref() {
                worker
                    .emit_presence(session_id, &client_id, &connection_id, role, "detached")
                    .await;
            }
        }
    }
    append_acp_event(
        &state.event_log,
        &connection_id,
        "connection_closed",
        json!({
            "session_id": session_id,
            "retention_ms": state.hub.retention.as_millis(),
        }),
    )
    .await;
}

async fn append_acp_event(
    event_log: &Arc<AnyEventLog>,
    connection_id: &str,
    kind: &str,
    payload: JsonValue,
) {
    let Ok(topic) = Topic::new(format!("{ACP_TOPIC_PREFIX}.{connection_id}")) else {
        return;
    };
    let _ = event_log.append(&topic, LogEvent::new(kind, payload)).await;
}

async fn replay_persisted_acp_events(
    event_log: &Arc<AnyEventLog>,
    session_id: &str,
    last_acked_event_id: u64,
    socket_tx: &mpsc::UnboundedSender<String>,
) -> PersistedAcpReplay {
    let Ok(topic) = Topic::new(format!("{ACP_TOPIC_PREFIX}.{session_id}")) else {
        return PersistedAcpReplay {
            summary: None,
            replayed: Vec::new(),
        };
    };
    let Ok(events) = event_log
        .read_range(&topic, None, ACP_REPLAY_BUFFER_LIMIT)
        .await
    else {
        return PersistedAcpReplay {
            summary: None,
            replayed: Vec::new(),
        };
    };
    let mut summary: Option<AcpSessionSummary> = None;
    let mut replayed = Vec::new();
    for (_, event) in events {
        if event.kind != "message_sent" {
            continue;
        }
        let Some(acp_event_id) = event
            .payload
            .get("acp_event_id")
            .and_then(JsonValue::as_u64)
        else {
            continue;
        };
        if let Some(event_summary) =
            acp_session_summary_from_log_payload(session_id, &event.payload)
        {
            match summary.as_mut() {
                Some(summary) => summary.merge(event_summary),
                None => summary = Some(event_summary),
            }
        }
        if acp_event_id <= last_acked_event_id {
            continue;
        }
        let Some(line) = event.payload.get("line").and_then(JsonValue::as_str) else {
            continue;
        };
        let replayed_line = annotate_acp_line(line, acp_event_id, Some(session_id), true);
        let _ = socket_tx.send(replayed_line);
        replayed.push(json!({"eventId": acp_event_id}));
    }
    PersistedAcpReplay { summary, replayed }
}

async fn persisted_acp_session_summaries(event_log: &Arc<AnyEventLog>) -> Vec<AcpSessionSummary> {
    let Ok(topics) = event_log.topics().await else {
        return Vec::new();
    };
    let mut summaries = Vec::new();
    for topic in topics {
        let Some(session_id) = topic.as_str().strip_prefix(&format!("{ACP_TOPIC_PREFIX}.")) else {
            continue;
        };
        let Ok(events) = event_log
            .read_range(&topic, None, ACP_REPLAY_BUFFER_LIMIT)
            .await
        else {
            continue;
        };
        let mut summary: Option<AcpSessionSummary> = None;
        for (_, event) in events {
            if event.kind != "message_sent"
                || event
                    .payload
                    .get("acp_event_id")
                    .and_then(JsonValue::as_u64)
                    .is_none()
            {
                continue;
            }
            if let Some(event_summary) =
                acp_session_summary_from_log_payload(session_id, &event.payload)
            {
                match summary.as_mut() {
                    Some(summary) => summary.merge(event_summary),
                    None => summary = Some(event_summary),
                }
            }
        }
        if let Some(summary) = summary {
            summaries.push(summary);
        }
    }
    summaries
}

fn acp_replay_log_payload(line: &str, acp_event_id: u64, session_id: Option<&str>) -> JsonValue {
    let mut payload = acp_message_log_payload(line, session_id);
    payload["acp_event_id"] = json!(acp_event_id);
    payload["line"] = json!(line);
    payload
}

fn acp_message_log_payload(line: &str, session_id: Option<&str>) -> JsonValue {
    match serde_json::from_str::<JsonValue>(line) {
        Ok(value) => {
            let mut payload = json!({
                "method": value.get("method").and_then(JsonValue::as_str),
                "id": value.get("id").cloned(),
                "session_id": session_id,
            });
            if let Some(params_session_id) = value
                .get("params")
                .and_then(|params| params.get("sessionId").or_else(|| params.get("session_id")))
                .and_then(JsonValue::as_str)
            {
                payload["session_id"] = json!(params_session_id);
            }
            if let Some(result_session_id) = value
                .get("result")
                .and_then(|result| result.get("sessionId").or_else(|| result.get("session_id")))
                .and_then(JsonValue::as_str)
            {
                payload["session_id"] = json!(result_session_id);
            }
            payload
        }
        Err(_) => json!({
            "malformed": true,
            "session_id": session_id,
        }),
    }
}

fn acp_session_summary_from_log_payload(
    topic_session_id: &str,
    payload: &JsonValue,
) -> Option<AcpSessionSummary> {
    let last_event_id = payload.get("acp_event_id").and_then(JsonValue::as_u64);
    let mut summary = payload
        .get("line")
        .and_then(JsonValue::as_str)
        .and_then(|line| {
            let mut summaries = acp_session_summaries_from_message(line);
            summaries
                .iter()
                .position(|summary| summary.session_id == topic_session_id)
                .map(|index| summaries.remove(index))
                .or_else(|| summaries.into_iter().next())
        })
        .unwrap_or_else(|| AcpSessionSummary::new(topic_session_id.to_string()));
    summary.last_event_id = last_event_id;
    Some(summary)
}

fn acp_session_summaries_from_message(line: &str) -> Vec<AcpSessionSummary> {
    let Ok(value) = serde_json::from_str::<JsonValue>(line) else {
        return Vec::new();
    };
    let mut summaries = BTreeMap::<String, AcpSessionSummary>::new();
    if let Some(session) = value.get("result").and_then(|result| result.get("session")) {
        if let Some(summary) = acp_session_summary_from_session_value(session) {
            summaries.insert(summary.session_id.clone(), summary);
        }
    }
    if let Some(items) = value
        .get("result")
        .and_then(|result| result.get("sessions"))
        .and_then(JsonValue::as_array)
    {
        for item in items {
            if let Some(summary) = acp_session_summary_from_session_value(item) {
                summaries
                    .entry(summary.session_id.clone())
                    .and_modify(|existing| existing.merge(summary.clone()))
                    .or_insert(summary);
            }
        }
    }
    if let Some(summary) = acp_session_summary_from_result_value(&value) {
        summaries
            .entry(summary.session_id.clone())
            .and_modify(|existing| existing.merge(summary.clone()))
            .or_insert(summary);
    }
    if let Some(session_id) = session_id_from_acp_message(line) {
        summaries
            .entry(session_id.clone())
            .or_insert_with(|| AcpSessionSummary::new(session_id));
    }
    summaries.into_values().collect()
}

fn acp_session_summary_from_result_value(value: &JsonValue) -> Option<AcpSessionSummary> {
    let result = value.get("result")?;
    let session_id = result
        .get("sessionId")
        .or_else(|| result.get("session_id"))
        .and_then(JsonValue::as_str)?;
    let mut summary = AcpSessionSummary::new(session_id.to_string());
    summary.cwd = result
        .get("cwd")
        .and_then(JsonValue::as_str)
        .map(str::to_string);
    summary.workspace_anchor = workspace_anchor_json(result);
    summary.last_event_id = result
        .get("lastEventId")
        .or_else(|| result.get("last_event_id"))
        .and_then(JsonValue::as_u64);
    Some(summary)
}

fn acp_session_summary_from_session_value(session: &JsonValue) -> Option<AcpSessionSummary> {
    let session_id = session
        .get("sessionId")
        .or_else(|| session.get("session_id"))
        .and_then(JsonValue::as_str)?;
    let mut summary = AcpSessionSummary::new(session_id.to_string());
    summary.cwd = session
        .get("cwd")
        .and_then(JsonValue::as_str)
        .map(str::to_string);
    summary.title = session
        .get("title")
        .and_then(JsonValue::as_str)
        .map(str::to_string);
    summary.meta = session
        .get("_meta")
        .and_then(JsonValue::as_object)
        .cloned()
        .unwrap_or_default();
    summary.workspace_anchor = workspace_anchor_json(session);
    summary.last_event_id = session
        .get("lastEventId")
        .or_else(|| session.get("last_event_id"))
        .and_then(JsonValue::as_u64)
        .or_else(|| {
            session
                .get("_meta")
                .and_then(|meta| meta.get("harn"))
                .and_then(|harn| {
                    harn.get("lastEventId")
                        .or_else(|| harn.get("last_event_id"))
                        .and_then(JsonValue::as_u64)
                })
        });
    Some(summary)
}

fn workspace_anchor_json(value: &JsonValue) -> Option<JsonValue> {
    value
        .get("workspaceAnchor")
        .or_else(|| value.get("workspace_anchor"))
        .cloned()
        .or_else(|| {
            value
                .get("_meta")
                .and_then(|meta| meta.get("harn"))
                .and_then(|harn| {
                    harn.get("workspaceAnchor")
                        .or_else(|| harn.get("workspace_anchor"))
                })
                .cloned()
        })
}

fn is_session_list_request(value: &JsonValue) -> bool {
    value.get("method").and_then(JsonValue::as_str) == Some("session/list")
}

fn acp_session_summary_matches(
    summary: &AcpSessionSummary,
    live_state: &str,
    params: &JsonValue,
) -> bool {
    if let Some(cwd) = acp_session_filter_value(params, "cwd", "cwd").and_then(JsonValue::as_str) {
        if summary.cwd.as_deref() != Some(cwd) {
            return false;
        }
    }
    if let Some(states) = acp_session_state_filter(params) {
        if !states.iter().any(|state| state == live_state) {
            return false;
        }
    }
    if let Some(filter) = acp_session_filter_value(params, "workspaceAnchor", "workspace_anchor") {
        let Some(anchor) = summary.workspace_anchor.as_ref() else {
            return false;
        };
        if let Some(primary) = filter.as_str() {
            return anchor
                .get("primary")
                .and_then(JsonValue::as_str)
                .is_some_and(|value| value == primary);
        }
        if let Some(primary) = filter.get("primary").and_then(JsonValue::as_str) {
            return anchor
                .get("primary")
                .and_then(JsonValue::as_str)
                .is_some_and(|value| value == primary);
        }
        return anchor == filter;
    }
    true
}

fn acp_session_state_filter(params: &JsonValue) -> Option<Vec<String>> {
    let value = acp_session_filter_value(params, "liveState", "live_state")
        .or_else(|| acp_session_filter_value(params, "state", "state"))?;
    if let Some(raw) = value.as_str() {
        return Some(vec![raw.to_string()]);
    }
    value.as_array().map(|items| {
        items
            .iter()
            .filter_map(|item| item.as_str().map(str::to_string))
            .collect()
    })
}

fn acp_session_filter_value<'a>(
    params: &'a JsonValue,
    camel: &str,
    snake: &str,
) -> Option<&'a JsonValue> {
    params
        .get(camel)
        .or_else(|| params.get(snake))
        .or_else(|| {
            params
                .get("filter")
                .and_then(|filter| filter.get(camel).or_else(|| filter.get(snake)))
        })
        .or_else(|| {
            params
                .get("_meta")
                .and_then(|meta| meta.get("harn"))
                .and_then(|harn| harn.get(camel).or_else(|| harn.get(snake)))
        })
}

fn session_load_attach_result(attach: AcpAttachResult) -> JsonValue {
    let session = attach
        .session
        .unwrap_or_else(|| AcpSessionSummary::new(attach.session_id))
        .to_json(attach.live_state, &attach.attachable_roles);
    json!({
        "session": session,
        "role": attach.role.as_str(),
        "capabilities": attach.role.capabilities_json(),
    })
}

fn attach_role_from_request(value: &JsonValue) -> AcpAttachRole {
    request_harn_value(value, "role")
        .and_then(JsonValue::as_str)
        .and_then(AcpAttachRole::parse)
        .or_else(|| {
            value
                .get("params")
                .and_then(|params| params.get("role").or_else(|| params.get("attachRole")))
                .and_then(JsonValue::as_str)
                .and_then(AcpAttachRole::parse)
        })
        .unwrap_or(AcpAttachRole::HostOwner)
}

fn client_id_from_request(value: &JsonValue, connection_id: &str) -> String {
    request_harn_value(value, "clientId")
        .or_else(|| request_harn_value(value, "client_id"))
        .and_then(JsonValue::as_str)
        .or_else(|| {
            value
                .get("params")
                .and_then(|params| params.get("clientId").or_else(|| params.get("client_id")))
                .and_then(JsonValue::as_str)
        })
        .unwrap_or(connection_id)
        .to_string()
}

fn request_harn_value<'a>(value: &'a JsonValue, key: &str) -> Option<&'a JsonValue> {
    value
        .get("_harn")
        .and_then(|harn| harn.get(key))
        .or_else(|| {
            value
                .get("params")
                .and_then(|params| params.get("_harn"))
                .and_then(|harn| harn.get(key))
        })
}

fn role_may_send_method(role: AcpAttachRole, method: &str) -> bool {
    match role {
        AcpAttachRole::HostOwner => true,
        AcpAttachRole::Controller => matches!(
            method,
            "session/cancel"
                | "session/inject"
                | "session/revoke_inject"
                | "session/replace_inject"
                | "session/truncate"
                | "session/remind"
                | "session/pending_injections"
                | "session/revoke_reminder"
                | "session/set_mode"
                | "session/set_config_option"
        ),
        AcpAttachRole::Observer => false,
    }
}

fn acp_outbound_route(line: &str) -> AcpOutboundRoute {
    let Ok(value) = serde_json::from_str::<JsonValue>(line) else {
        return AcpOutboundRoute::Broadcast;
    };
    let method = value.get("method").and_then(JsonValue::as_str);
    let id_key = jsonrpc_id_key(&value);
    match (method, id_key) {
        (Some(method), Some(id_key)) => AcpOutboundRoute::HostRequest {
            id_key,
            method: method.to_string(),
        },
        (None, id_key) => AcpOutboundRoute::Response(id_key),
        (Some(_), None) => AcpOutboundRoute::Broadcast,
    }
}

fn replay_visible_to_non_owner(line: &str) -> bool {
    let Ok(value) = serde_json::from_str::<JsonValue>(line) else {
        return true;
    };
    value.get("id").is_none()
}

fn jsonrpc_id_key(value: &JsonValue) -> Option<String> {
    value
        .get("id")
        .and_then(|id| serde_json::to_string(id).ok())
}

fn unix_epoch_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or(0)
}

fn annotate_acp_line(
    line: &str,
    event_id: u64,
    session_id: Option<&str>,
    replayed: bool,
) -> String {
    let Ok(mut value) = serde_json::from_str::<JsonValue>(line) else {
        return line.to_string();
    };
    let Some(object) = value.as_object_mut() else {
        return line.to_string();
    };
    let harn_meta = object
        .entry("_harn")
        .or_insert_with(|| json!({}))
        .as_object_mut();
    if let Some(harn_meta) = harn_meta {
        harn_meta.insert("eventId".to_string(), json!(event_id));
        harn_meta.insert("replayed".to_string(), json!(replayed));
        if let Some(session_id) = session_id {
            harn_meta.insert("sessionId".to_string(), json!(session_id));
        }
    }
    serde_json::to_string(&value).unwrap_or_else(|_| line.to_string())
}

fn session_load_session_id(value: &JsonValue) -> Option<String> {
    if value.get("method").and_then(JsonValue::as_str) != Some("session/load") {
        return None;
    }
    value
        .get("params")
        .and_then(|params| params.get("sessionId").or_else(|| params.get("session_id")))
        .and_then(JsonValue::as_str)
        .map(ToString::to_string)
}

fn last_acked_event_id(value: &JsonValue) -> u64 {
    value
        .get("params")
        .and_then(|params| {
            params
                .get("lastAckedEventId")
                .or_else(|| params.get("last_acked_event_id"))
                .or_else(|| params.get("lastEventId"))
        })
        .and_then(JsonValue::as_u64)
        .unwrap_or(0)
}

fn send_socket_jsonrpc_error(
    socket_tx: &mpsc::UnboundedSender<String>,
    id: &JsonValue,
    code: i64,
    message: &str,
) {
    let response = harn_vm::jsonrpc::error_response(id.clone(), code, message);
    if let Ok(line) = serde_json::to_string(&response) {
        let _ = socket_tx.send(line);
    }
}

fn send_socket_jsonrpc_error_with_data(
    socket_tx: &mpsc::UnboundedSender<String>,
    id: &JsonValue,
    code: i64,
    message: &str,
    data: JsonValue,
) {
    let response = harn_vm::jsonrpc::error_response_with_data(id.clone(), code, message, data);
    if let Ok(line) = serde_json::to_string(&response) {
        let _ = socket_tx.send(line);
    }
}

fn send_socket_jsonrpc_result(
    socket_tx: &mpsc::UnboundedSender<String>,
    id: &JsonValue,
    result: JsonValue,
) {
    let response = harn_vm::jsonrpc::response(id.clone(), result);
    if let Ok(line) = serde_json::to_string(&response) {
        let _ = socket_tx.send(line);
    }
}

fn session_id_from_acp_response(line: &str) -> Option<String> {
    serde_json::from_str::<JsonValue>(line)
        .ok()
        .and_then(|value| value.get("result").cloned())
        .and_then(|result| {
            result
                .get("sessionId")
                .or_else(|| result.get("session_id"))
                .and_then(JsonValue::as_str)
                .or_else(|| {
                    result
                        .get("session")
                        .and_then(|session| {
                            session
                                .get("sessionId")
                                .or_else(|| session.get("session_id"))
                        })
                        .and_then(JsonValue::as_str)
                })
                .map(ToString::to_string)
        })
}

fn session_id_from_acp_message(line: &str) -> Option<String> {
    serde_json::from_str::<JsonValue>(line)
        .ok()
        .and_then(|value| {
            value
                .get("params")
                .and_then(|params| params.get("sessionId").or_else(|| params.get("session_id")))
                .and_then(JsonValue::as_str)
                .or_else(|| {
                    value
                        .get("result")
                        .and_then(|result| {
                            result.get("sessionId").or_else(|| result.get("session_id"))
                        })
                        .and_then(JsonValue::as_str)
                })
                .or_else(|| {
                    value
                        .get("result")
                        .and_then(|result| result.get("session"))
                        .and_then(|session| {
                            session
                                .get("sessionId")
                                .or_else(|| session.get("session_id"))
                        })
                        .and_then(JsonValue::as_str)
                })
                .map(ToString::to_string)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use harn_vm::event_log::{AnyEventLog, MemoryEventLog};
    use std::collections::{BTreeMap, VecDeque};
    use std::sync::atomic::AtomicU64;
    use std::sync::{Arc, Mutex, Weak};
    use tokio::sync::mpsc;

    type TestWorkerChannels = (
        Arc<AcpWorker>,
        mpsc::UnboundedReceiver<JsonValue>,
        mpsc::UnboundedReceiver<String>,
        mpsc::UnboundedReceiver<String>,
        mpsc::UnboundedReceiver<String>,
    );

    fn test_worker() -> TestWorkerChannels {
        let (request_tx, request_rx) = mpsc::unbounded_channel();
        let (owner_tx, owner_rx) = mpsc::unbounded_channel();
        let (controller_tx, controller_rx) = mpsc::unbounded_channel();
        let (observer_tx, observer_rx) = mpsc::unbounded_channel();
        let worker = Arc::new(AcpWorker {
            id: "worker-test".to_string(),
            request_tx: Mutex::new(Some(request_tx)),
            clients: Mutex::new(BTreeMap::from([
                (
                    "owner".to_string(),
                    AcpClient {
                        connection_id: "owner-conn".to_string(),
                        role: AcpAttachRole::HostOwner,
                        socket_tx: owner_tx,
                    },
                ),
                (
                    "controller".to_string(),
                    AcpClient {
                        connection_id: "controller-conn".to_string(),
                        role: AcpAttachRole::Controller,
                        socket_tx: controller_tx,
                    },
                ),
                (
                    "observer".to_string(),
                    AcpClient {
                        connection_id: "observer-conn".to_string(),
                        role: AcpAttachRole::Observer,
                        socket_tx: observer_tx,
                    },
                ),
            ])),
            host_owner_client_id: Mutex::new(Some("owner".to_string())),
            pending_client_requests: Mutex::new(BTreeMap::new()),
            pending_host_requests: Mutex::new(BTreeMap::new()),
            decided_host_requests: Mutex::new(BTreeMap::new()),
            sessions: Mutex::new(BTreeMap::new()),
            replay_buffer: Mutex::new(VecDeque::new()),
            next_event_id: AtomicU64::new(1),
            detached_at: Mutex::new(None),
            event_log: Arc::new(AnyEventLog::Memory(MemoryEventLog::new(64))),
            hub: Weak::new(),
        });
        (worker, request_rx, owner_rx, controller_rx, observer_rx)
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acp_hub_arbitrates_permission_responses() {
        let (worker, mut request_rx, mut owner_rx, mut controller_rx, mut observer_rx) =
            test_worker();
        worker
            .handle_output(
                json!({
                    "jsonrpc": "2.0",
                    "id": 77,
                    "method": "session/request_permission",
                    "params": {
                        "sessionId": "session-1",
                        "toolCall": {
                            "sessionUpdate": "tool_call_update",
                            "toolCallId": "tool-1",
                            "title": "edit",
                            "kind": "other"
                        },
                        "options": [
                            {"optionId": "allow", "name": "Allow", "kind": "allow_once"},
                            {"optionId": "reject", "name": "Reject", "kind": "reject_once"}
                        ]
                    },
                })
                .to_string(),
            )
            .await;

        let owner_request: JsonValue =
            serde_json::from_str(&owner_rx.recv().await.expect("owner request")).unwrap();
        let controller_request: JsonValue =
            serde_json::from_str(&controller_rx.recv().await.expect("controller request")).unwrap();
        assert_eq!(owner_request["method"], "session/request_permission");
        assert_eq!(controller_request["method"], "session/request_permission");
        assert!(observer_rx.try_recv().is_err());

        worker
            .send_client_request(
                "controller",
                AcpAttachRole::Controller,
                json!({
                    "jsonrpc": "2.0",
                    "id": 77,
                    "result": {"outcome": {"outcome": "selected", "optionId": "allow"}},
                }),
            )
            .await
            .expect("first controller decision wins");
        let forwarded = request_rx.recv().await.expect("forwarded response");
        assert_eq!(forwarded["id"], 77);
        assert_eq!(forwarded["result"]["outcome"]["outcome"], "selected");
        assert_eq!(forwarded["result"]["outcome"]["optionId"], "allow");

        let duplicate = worker
            .send_client_request(
                "controller",
                AcpAttachRole::Controller,
                json!({
                    "jsonrpc": "2.0",
                    "id": 77,
                    "result": {"outcome": {"outcome": "selected", "optionId": "allow"}},
                }),
            )
            .await
            .expect_err("same actor duplicate should be idempotent");
        assert!(matches!(
            duplicate,
            AcpClientRequestError::IdempotentHostDecision { .. }
        ));

        let conflict = worker
            .send_client_request(
                "owner",
                AcpAttachRole::HostOwner,
                json!({
                    "jsonrpc": "2.0",
                    "id": 77,
                    "result": {"outcome": {"outcome": "selected", "optionId": "reject"}},
                }),
            )
            .await
            .expect_err("late conflicting decision should be rejected");
        match conflict {
            AcpClientRequestError::AlreadyDecided {
                decision,
                attempted_actor,
                attempted_payload,
            } => {
                assert_eq!(decision.actor.client_id, "controller");
                assert_eq!(attempted_actor.client_id, "owner");
                assert_eq!(attempted_payload["result"]["outcome"]["optionId"], "reject");
            }
            other => panic!("expected AlreadyDecided, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn acp_hub_adds_actor_metadata_to_controller_controls() {
        let (worker, mut request_rx, _owner_rx, _controller_rx, _observer_rx) = test_worker();
        worker
            .send_client_request(
                "controller",
                AcpAttachRole::Controller,
                json!({
                    "jsonrpc": "2.0",
                    "id": 9,
                    "method": "session/cancel",
                    "params": {"sessionId": "session-1"},
                }),
            )
            .await
            .expect("controller cancel forwards");
        let forwarded = request_rx.recv().await.expect("forwarded cancel");
        assert_eq!(
            forwarded["params"]["_harn"]["actor"],
            json!({
                "clientId": "controller",
                "connectionId": "controller-conn",
                "role": "controller",
                "source": "websocket",
            })
        );
    }
}
