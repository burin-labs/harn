use std::collections::{BTreeMap, BTreeSet, VecDeque};
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
const ACP_DEFAULT_RETAINED_SESSION_SECS: u64 = 5 * 60;
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
    socket_tx: Mutex<Option<mpsc::UnboundedSender<String>>>,
    active_connection_id: Mutex<Option<String>>,
    sessions: Mutex<BTreeSet<String>>,
    replay_buffer: Mutex<VecDeque<AcpReplayEvent>>,
    next_event_id: AtomicU64,
    detached_at: Mutex<Option<Instant>>,
    event_log: Arc<AnyEventLog>,
    hub: Weak<AcpWebSocketHub>,
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
    AlreadyAttached,
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
            socket_tx: Mutex::new(None),
            active_connection_id: Mutex::new(None),
            sessions: Mutex::new(BTreeSet::new()),
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

        let worker_name = worker_id.clone();
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

    fn register_session(&self, session_id: String, worker: &Arc<AcpWorker>) {
        worker
            .sessions
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(session_id.clone());
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .workers_by_session
            .entry(session_id)
            .or_insert_with(|| worker.clone());
    }

    fn attach(
        &self,
        session_id: &str,
        connection_id: &str,
        socket_tx: mpsc::UnboundedSender<String>,
        last_acked_event_id: u64,
    ) -> Result<Arc<AcpWorker>, AcpAttachError> {
        let worker = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .workers_by_session
            .get(session_id)
            .cloned()
            .ok_or(AcpAttachError::NotFound)?;
        worker.attach(connection_id, socket_tx, last_acked_event_id)?;
        Ok(worker)
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
    }
}

impl AcpWorker {
    fn attach(
        &self,
        connection_id: &str,
        socket_tx: mpsc::UnboundedSender<String>,
        last_acked_event_id: u64,
    ) -> Result<(), AcpAttachError> {
        {
            let mut active = self
                .active_connection_id
                .lock()
                .unwrap_or_else(|e| e.into_inner());
            if active
                .as_deref()
                .is_some_and(|active_connection_id| active_connection_id != connection_id)
            {
                return Err(AcpAttachError::AlreadyAttached);
            }
            *active = Some(connection_id.to_string());
        }
        *self.socket_tx.lock().unwrap_or_else(|e| e.into_inner()) = Some(socket_tx.clone());
        *self.detached_at.lock().unwrap_or_else(|e| e.into_inner()) = None;
        self.replay_since(last_acked_event_id, socket_tx);
        Ok(())
    }

    fn detach(&self, connection_id: &str) {
        let mut active = self
            .active_connection_id
            .lock()
            .unwrap_or_else(|e| e.into_inner());
        if active.as_deref() != Some(connection_id) {
            return;
        }
        *active = None;
        *self.socket_tx.lock().unwrap_or_else(|e| e.into_inner()) = None;
        *self.detached_at.lock().unwrap_or_else(|e| e.into_inner()) = Some(Instant::now());
    }

    fn send_request(&self, value: JsonValue) -> Result<(), ()> {
        self.request_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .ok_or(())?
            .send(value)
            .map_err(|_| ())
    }

    fn shutdown(&self) {
        self.request_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
        self.socket_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take();
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
            .iter()
            .cloned()
            .collect()
    }

    async fn handle_output(self: &Arc<Self>, line: String) {
        let session_id = session_id_from_acp_message(&line).or_else(|| {
            let sessions = self.session_ids();
            (sessions.len() == 1).then(|| sessions[0].clone())
        });
        if let Some(session_id) = session_id.clone() {
            if let Some(hub) = self.hub.upgrade() {
                hub.register_session(session_id, self);
            }
        }

        let event_id = self.next_event_id.fetch_add(1, Ordering::SeqCst);
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

        let socket_tx = self
            .socket_tx
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone();
        if let Some(socket_tx) = socket_tx {
            let _ = socket_tx.send(annotated);
        }
    }

    fn replay_since(&self, last_acked_event_id: u64, socket_tx: mpsc::UnboundedSender<String>) {
        let events: Vec<AcpReplayEvent> = self
            .replay_buffer
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .filter(|event| event.id > last_acked_event_id)
            .cloned()
            .collect();
        for event in events {
            let replayed =
                annotate_acp_line(&event.line, event.id, event.session_id.as_deref(), true);
            let _ = socket_tx.send(replayed);
        }
    }
}

pub(super) fn acp_retained_session_duration_from_env() -> Duration {
    let seconds = std::env::var(ACP_RETAINED_SESSION_SECS_ENV)
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .filter(|seconds| *seconds > 0)
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
                                if let Some(load_session_id) = session_load_session_id(&value) {
                                    match state.hub.attach(
                                        &load_session_id,
                                        &connection_id,
                                        socket_tx.clone(),
                                        last_acked_event_id(&value),
                                    ) {
                                        Ok(attached) => {
                                            session_id = Some(load_session_id);
                                            worker = Some(attached);
                                        }
                                        Err(AcpAttachError::AlreadyAttached) => {
                                            send_socket_jsonrpc_error(
                                                &socket_tx,
                                                value.get("id").unwrap_or(&JsonValue::Null),
                                                -32010,
                                                "ACP session is already attached to another WebSocket",
                                            );
                                            continue;
                                        }
                                        Err(AcpAttachError::NotFound) => {
                                            replay_persisted_acp_events(
                                                &state.event_log,
                                                &load_session_id,
                                                last_acked_event_id(&value),
                                                &socket_tx,
                                            )
                                            .await;
                                            if worker.is_none() {
                                                send_socket_jsonrpc_error(
                                                    &socket_tx,
                                                    value.get("id").unwrap_or(&JsonValue::Null),
                                                    -32004,
                                                    &format!("Session not found: {load_session_id}"),
                                                );
                                                continue;
                                            }
                                        }
                                    }
                                }
                                if worker.is_none() {
                                    match state.hub.spawn_worker(state.pipeline.clone()) {
                                        Ok(new_worker) => {
                                            new_worker.attach(
                                                &connection_id,
                                                socket_tx.clone(),
                                                0,
                                            )
                                            .expect("fresh ACP worker is unattached");
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
                                if worker
                                    .as_ref()
                                    .is_none_or(|worker| worker.send_request(value).is_err())
                                {
                                    break;
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
        worker.detach(&connection_id);
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
) {
    let Ok(topic) = Topic::new(format!("{ACP_TOPIC_PREFIX}.{session_id}")) else {
        return;
    };
    let Ok(events) = event_log
        .read_range(&topic, None, ACP_REPLAY_BUFFER_LIMIT)
        .await
    else {
        return;
    };
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
        if acp_event_id <= last_acked_event_id {
            continue;
        }
        let Some(line) = event.payload.get("line").and_then(JsonValue::as_str) else {
            continue;
        };
        let replayed = annotate_acp_line(line, acp_event_id, Some(session_id), true);
        let _ = socket_tx.send(replayed);
    }
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

fn session_id_from_acp_response(line: &str) -> Option<String> {
    serde_json::from_str::<JsonValue>(line)
        .ok()
        .and_then(|value| value.get("result").cloned())
        .and_then(|result| {
            result
                .get("sessionId")
                .or_else(|| result.get("session_id"))
                .and_then(JsonValue::as_str)
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
