//! Embedding helpers for running the in-process ACP agent loop.
//!
//! The ACP channel server future is `!Send` — it owns a
//! [`tokio::task::LocalSet`] and `spawn_local`s onto it — so it cannot be
//! `tokio::spawn`ed onto a multi-thread runtime. The canonical way to embed it
//! is therefore: spawn a dedicated OS thread, build a current-thread tokio
//! runtime on that thread, and `block_on` the server future there, talking to
//! it over a pair of unbounded channels.
//!
//! Every in-process embedder (the orchestrator's ACP WebSocket hub, the API
//! adapter, and Burin Code's Rust TUI) re-implements that exact dance.
//! [`EmbeddedAgent`] packages it once: call [`EmbeddedAgent::spawn`], get back
//! the request sender, the response receiver, and an [`AcpChannelHandle`] for
//! graceful shutdown / readiness / termination, and let `Drop` join the worker
//! thread for you.

use std::collections::VecDeque;
use std::fmt;
use std::path::Path;
use std::thread::{self, JoinHandle};

use serde::de::DeserializeOwned;
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::adapters::acp::{
    run_acp_channel_server_with_existing_handle, AcpChannelHandle, AcpJsonRpcError,
    AcpJsonRpcErrorResponse, AcpJsonRpcId, AcpJsonRpcRequest, AcpJsonRpcResponse, AcpServerConfig,
    AcpSessionIdParams, AcpSessionInjectParams, AcpSessionNewParams, AcpSessionPromptParams,
    AcpSessionPromptResult, AcpSessionRestoreResult, ACP_METHOD_INITIALIZE,
    ACP_METHOD_SESSION_CANCEL, ACP_METHOD_SESSION_CLOSE, ACP_METHOD_SESSION_INJECT,
    ACP_METHOD_SESSION_LOAD, ACP_METHOD_SESSION_NEW, ACP_METHOD_SESSION_PROMPT,
    ACP_METHOD_SESSION_RESUME, HARN_AGENT_EVENT_METHOD,
};

/// Default name for the dedicated ACP worker thread.
const DEFAULT_THREAD_NAME: &str = "harn-acp-embed";

/// A running in-process ACP agent: a dedicated worker thread driving the
/// `!Send` channel-server future on its own current-thread tokio runtime.
///
/// Construct one with [`EmbeddedAgent::spawn`]. The embedder communicates over
/// the [request sender](Self::requests) (host -> agent JSON-RPC) and
/// [response receiver](Self::take_responses) (agent -> host JSON-RPC lines),
/// and steers/observes the worker through [`handle`](Self::handle).
///
/// Dropping the `EmbeddedAgent` requests a graceful shutdown and joins the
/// worker thread, so an embedder does not have to manage the `!Send` /
/// dedicated-thread lifecycle by hand. Call [`shutdown`](Self::shutdown)
/// (and optionally [`join`](Self::join)) for explicit, error-observable
/// teardown.
pub struct EmbeddedAgent {
    // `Option` so `into_parts` can hand over the *owning* sender (the worker
    // only ever sees the matching receiver). Dropping the last sender closes
    // the request channel and is the legacy EOF-style teardown.
    request_tx: Option<mpsc::UnboundedSender<serde_json::Value>>,
    response_rx: Option<mpsc::UnboundedReceiver<String>>,
    handle: AcpChannelHandle,
    thread: Option<JoinHandle<()>>,
}

impl EmbeddedAgent {
    /// Spawn an in-process ACP agent on a dedicated worker thread.
    ///
    /// The worker thread builds a current-thread tokio runtime (`enable_all`)
    /// and `block_on`s the channel server. The agent runs until the request
    /// sender is dropped, [`shutdown`](Self::shutdown) is called, or the
    /// `EmbeddedAgent` is dropped.
    ///
    /// # Panics
    ///
    /// Panics if the OS refuses to spawn the worker thread or the worker
    /// thread cannot build its tokio runtime — both are unrecoverable
    /// process-level failures at embed time.
    pub fn spawn(config: AcpServerConfig) -> Self {
        Self::spawn_named(config, DEFAULT_THREAD_NAME)
    }

    /// [`spawn`](Self::spawn) with a caller-chosen worker thread name (useful
    /// when an embedder runs several agents and wants them distinguishable in
    /// stack traces and profilers).
    ///
    /// # Panics
    ///
    /// See [`spawn`](Self::spawn).
    pub fn spawn_named(config: AcpServerConfig, thread_name: impl Into<String>) -> Self {
        let (request_tx, request_rx) = mpsc::unbounded_channel::<serde_json::Value>();
        let (response_tx, response_rx) = mpsc::unbounded_channel::<String>();
        // The handle is `Send`, so we build it here and hand a clone to the
        // worker. The channel-server future is `!Send`, so it must be built and
        // driven entirely on the worker thread — never moved across the
        // boundary. `config`, the channels, and the handle clone are all
        // `Send`, so only those cross.
        let handle = AcpChannelHandle::default();
        let worker_handle = handle.clone();

        let thread = thread::Builder::new()
            .name(thread_name.into())
            .spawn(move || {
                let runtime = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .expect("EmbeddedAgent: build current-thread tokio runtime");
                let server_future = run_acp_channel_server_with_existing_handle(
                    config,
                    request_rx,
                    response_tx,
                    worker_handle,
                );
                runtime.block_on(server_future);
            })
            .expect("EmbeddedAgent: spawn ACP worker thread");

        Self {
            request_tx: Some(request_tx),
            response_rx: Some(response_rx),
            handle,
            thread: Some(thread),
        }
    }

    /// A clonable sender for host -> agent JSON-RPC requests
    /// (`session/new`, `session/prompt`, `session/cancel`, …). Dropping every
    /// clone of this sender (and the [`EmbeddedAgent`], which holds the owning
    /// sender) stops the agent, mirroring stdin EOF.
    ///
    /// # Panics
    ///
    /// Panics if called after [`into_parts`](Self::into_parts) took the
    /// owning sender. (`into_parts` consumes `self`, so this only matters for
    /// internal use.)
    pub fn requests(&self) -> mpsc::UnboundedSender<serde_json::Value> {
        self.request_tx
            .as_ref()
            .expect("EmbeddedAgent request sender was taken by into_parts")
            .clone()
    }

    /// Take the agent -> host response receiver (JSON-RPC response and
    /// `session/update` notification lines). Returns `None` if already taken;
    /// the receiver is single-consumer.
    pub fn take_responses(&mut self) -> Option<mpsc::UnboundedReceiver<String>> {
        self.response_rx.take()
    }

    /// The cross-thread control handle for readiness, shutdown, and
    /// termination signalling.
    pub fn handle(&self) -> &AcpChannelHandle {
        &self.handle
    }

    /// Consume the agent into its raw parts: the request sender, the response
    /// receiver, and the control handle.
    ///
    /// The worker [`JoinHandle`] is detached when you take the parts this way
    /// — the agent stops when the returned sender is dropped or
    /// [`AcpChannelHandle::shutdown`] is called, and the thread exits on its
    /// own. Use this when you want the bare channels and manage lifetime
    /// through the handle; keep the [`EmbeddedAgent`] if you want `Drop` to
    /// join the thread.
    ///
    /// Returns the response receiver as `None` if it was already taken with
    /// [`take_responses`](Self::take_responses).
    #[allow(clippy::type_complexity)]
    pub fn into_parts(
        mut self,
    ) -> (
        mpsc::UnboundedSender<serde_json::Value>,
        Option<mpsc::UnboundedReceiver<String>>,
        AcpChannelHandle,
    ) {
        // Move out the *owning* sender (not a clone) so the worker's
        // `request_rx` closes when the caller drops it — preserving the
        // EOF teardown path.
        let request_tx = self
            .request_tx
            .take()
            .expect("EmbeddedAgent request sender was already taken");
        let response_rx = self.response_rx.take();
        let handle = self.handle.clone();
        // Detach the worker thread; its lifetime is now owned by the handle
        // and the moved-out sender. Clearing `thread` makes the subsequent
        // `Drop` a no-op (it only shuts down / joins when a thread is owned),
        // so no `mem::forget`/leak is needed.
        self.thread.take();
        (request_tx, response_rx, handle)
    }

    /// Request a graceful shutdown of the agent. Idempotent. Does not block;
    /// pair with [`join`](Self::join) to wait for the worker thread to exit.
    pub fn shutdown(&self) {
        self.handle.shutdown();
    }

    /// Request shutdown and join the worker thread, returning the thread's
    /// join result. Subsequent calls (and `Drop`) become no-ops.
    pub fn join(&mut self) -> thread::Result<()> {
        self.handle.shutdown();
        match self.thread.take() {
            Some(thread) => thread.join(),
            None => Ok(()),
        }
    }
}

/// Result type used by the high-level embedded ACP client facade.
pub type EmbeddedAgentResult<T> = Result<T, EmbeddedAgentError>;

/// Errors surfaced by [`EmbeddedAgentClient`].
#[derive(Debug)]
#[non_exhaustive]
pub enum EmbeddedAgentError {
    /// Failed to serialize a request or parse a response/event line.
    Json(serde_json::Error),
    /// The host -> agent request channel has closed.
    RequestChannelClosed,
    /// The agent -> host response receiver was already taken.
    ResponseChannelUnavailable,
    /// The agent -> host response channel closed while a caller was waiting.
    ResponseChannelClosed,
    /// ACP returned a JSON-RPC error response for a request.
    JsonRpc(AcpJsonRpcError),
    /// A stable run/session view could not be loaded or projected.
    View(String),
    /// The agent emitted a JSON line that is neither a JSON-RPC request,
    /// response, nor notification.
    UnexpectedMessage(Value),
    /// The worker thread panicked during explicit shutdown.
    WorkerPanicked,
}

impl fmt::Display for EmbeddedAgentError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Json(error) => write!(f, "embedded ACP JSON error: {error}"),
            Self::RequestChannelClosed => write!(f, "embedded ACP request channel closed"),
            Self::ResponseChannelUnavailable => {
                write!(f, "embedded ACP response receiver is unavailable")
            }
            Self::ResponseChannelClosed => write!(f, "embedded ACP response channel closed"),
            Self::JsonRpc(error) => {
                write!(
                    f,
                    "embedded ACP JSON-RPC error {}: {}",
                    error.code, error.message
                )
            }
            Self::View(error) => write!(f, "embedded ACP view error: {error}"),
            Self::UnexpectedMessage(value) => {
                write!(f, "embedded ACP emitted an unexpected message: {value}")
            }
            Self::WorkerPanicked => write!(f, "embedded ACP worker thread panicked"),
        }
    }
}

impl std::error::Error for EmbeddedAgentError {}

impl From<serde_json::Error> for EmbeddedAgentError {
    fn from(error: serde_json::Error) -> Self {
        Self::Json(error)
    }
}

/// Demultiplexed messages emitted by an [`EmbeddedAgentClient`].
#[derive(Clone, Debug, PartialEq)]
#[non_exhaustive]
pub enum EmbeddedAgentEvent {
    /// A canonical ACP `session/update` notification.
    SessionUpdate {
        session_id: Option<String>,
        update: Value,
        raw: Value,
    },
    /// A `harn.session_timeline.update` notification from a timeline
    /// subscription.
    TimelineUpdate {
        subscription_id: String,
        update: Value,
        raw: Value,
    },
    /// Harn extension agent event emitted outside canonical ACP
    /// `session/update`.
    AgentEvent {
        kind: Option<String>,
        event: Value,
        raw: Value,
    },
    /// A JSON-RPC request from the agent to the host. Hosts answer approvals,
    /// auth prompts, and host capability probes with
    /// [`EmbeddedAgentClient::respond_to_host_request`] or
    /// [`EmbeddedAgentClient::fail_host_request`].
    HostRequest {
        id: AcpJsonRpcId,
        method: String,
        params: Value,
        raw: Value,
    },
    /// A successful JSON-RPC response for a request that was started with
    /// [`EmbeddedAgentClient::begin_request`].
    RequestCompleted {
        id: AcpJsonRpcId,
        result: Value,
        raw: Value,
    },
    /// A failed JSON-RPC response for a request that was started with
    /// [`EmbeddedAgentClient::begin_request`].
    RequestFailed {
        id: AcpJsonRpcId,
        error: AcpJsonRpcError,
        raw: Value,
    },
    /// Any other JSON-RPC notification.
    Notification {
        method: String,
        params: Value,
        raw: Value,
    },
}

/// Stable embedded-agent facade over the in-process ACP channel server.
///
/// This wraps the low-level [`EmbeddedAgent`] worker-thread lifecycle with
/// typed ACP lifecycle calls, stable run/session view helpers, and a single
/// event stream that separates request results, session updates, timeline
/// updates, Harn extension events, and host callback requests.
pub struct EmbeddedAgentClient {
    agent: EmbeddedAgent,
    request_tx: mpsc::UnboundedSender<Value>,
    response_rx: mpsc::UnboundedReceiver<String>,
    pending: VecDeque<EmbeddedAgentEvent>,
    next_id: u64,
}

impl EmbeddedAgentClient {
    /// Spawn an embedded ACP server and wait until the worker loop is ready.
    pub async fn spawn(config: AcpServerConfig) -> EmbeddedAgentResult<Self> {
        Self::spawn_named(config, DEFAULT_THREAD_NAME).await
    }

    /// [`spawn`](Self::spawn) with a caller-chosen worker thread name.
    pub async fn spawn_named(
        config: AcpServerConfig,
        thread_name: impl Into<String>,
    ) -> EmbeddedAgentResult<Self> {
        let mut agent = EmbeddedAgent::spawn_named(config, thread_name);
        let request_tx = agent.requests();
        let response_rx = agent
            .take_responses()
            .ok_or(EmbeddedAgentError::ResponseChannelUnavailable)?;
        agent.handle().wait_ready().await;
        Ok(Self {
            agent,
            request_tx,
            response_rx,
            pending: VecDeque::new(),
            next_id: 1,
        })
    }

    /// Access the underlying worker control handle.
    pub fn handle(&self) -> &AcpChannelHandle {
        self.agent.handle()
    }

    /// Request graceful shutdown of the embedded ACP worker.
    pub fn shutdown(&self) {
        self.agent.shutdown();
    }

    /// Request shutdown and join the embedded worker thread.
    pub fn join(&mut self) -> EmbeddedAgentResult<()> {
        self.agent
            .join()
            .map_err(|_| EmbeddedAgentError::WorkerPanicked)
    }

    /// Start a JSON-RPC request without waiting for the result.
    ///
    /// Use this when the request may trigger host callbacks such as approval
    /// or auth prompts. Drive [`next_event`](Self::next_event) until the
    /// matching [`EmbeddedAgentEvent::RequestCompleted`] or
    /// [`EmbeddedAgentEvent::RequestFailed`] arrives, answering
    /// [`EmbeddedAgentEvent::HostRequest`] values along the way.
    pub fn begin_request<P: Serialize>(
        &mut self,
        method: impl Into<String>,
        params: P,
    ) -> EmbeddedAgentResult<u64> {
        let id = self.next_request_id();
        let request = AcpJsonRpcRequest::new(id, method, params).into_json_value()?;
        self.request_tx
            .send(request)
            .map_err(|_| EmbeddedAgentError::RequestChannelClosed)?;
        Ok(id)
    }

    /// Read the next demultiplexed event from the embedded agent.
    pub async fn next_event(&mut self) -> EmbeddedAgentResult<EmbeddedAgentEvent> {
        if let Some(event) = self.pending.pop_front() {
            return Ok(event);
        }
        self.recv_wire_event().await
    }

    async fn recv_wire_event(&mut self) -> EmbeddedAgentResult<EmbeddedAgentEvent> {
        let line = self
            .response_rx
            .recv()
            .await
            .ok_or(EmbeddedAgentError::ResponseChannelClosed)?;
        let value: Value = serde_json::from_str(&line)?;
        classify_embedded_agent_message(value)
    }

    /// Wait for a request started with [`begin_request`](Self::begin_request)
    /// to complete, buffering unrelated events for later
    /// [`next_event`](Self::next_event) calls.
    pub async fn recv_request_result(&mut self, request_id: u64) -> EmbeddedAgentResult<Value> {
        let mut buffered = std::mem::take(&mut self.pending);
        loop {
            let event = match self.recv_wire_event().await {
                Ok(event) => event,
                Err(error) => {
                    self.pending = buffered;
                    return Err(error);
                }
            };
            match event {
                EmbeddedAgentEvent::RequestCompleted { id, result, .. }
                    if acp_id_matches(&id, request_id) =>
                {
                    self.pending = buffered;
                    return Ok(result);
                }
                EmbeddedAgentEvent::RequestFailed { id, error, .. }
                    if acp_id_matches(&id, request_id) =>
                {
                    self.pending = buffered;
                    return Err(EmbeddedAgentError::JsonRpc(error));
                }
                other => buffered.push_back(other),
            }
        }
    }

    /// Send a JSON-RPC request and wait for its `result`.
    pub async fn request_value<P: Serialize>(
        &mut self,
        method: impl Into<String>,
        params: P,
    ) -> EmbeddedAgentResult<Value> {
        let request_id = self.begin_request(method, params)?;
        self.recv_request_result(request_id).await
    }

    /// Answer a host callback request with a successful JSON-RPC result.
    pub fn respond_to_host_request(
        &self,
        id: AcpJsonRpcId,
        result: Value,
    ) -> EmbeddedAgentResult<()> {
        self.request_tx
            .send(json!({
                "jsonrpc": "2.0",
                "id": id,
                "result": result,
            }))
            .map_err(|_| EmbeddedAgentError::RequestChannelClosed)
    }

    /// Answer a host callback request with a JSON-RPC error.
    pub fn fail_host_request(
        &self,
        id: AcpJsonRpcId,
        code: i64,
        message: impl Into<String>,
        data: Option<Value>,
    ) -> EmbeddedAgentResult<()> {
        let mut error = json!({
            "code": code,
            "message": message.into(),
        });
        if let Some(data) = data {
            error["data"] = data;
        }
        self.request_tx
            .send(json!({
                "jsonrpc": "2.0",
                "id": id,
                "error": error,
            }))
            .map_err(|_| EmbeddedAgentError::RequestChannelClosed)
    }

    /// Convenience alias for approval host callbacks.
    pub fn answer_approval(&self, id: AcpJsonRpcId, result: Value) -> EmbeddedAgentResult<()> {
        self.respond_to_host_request(id, result)
    }

    /// Convenience alias for auth host callbacks.
    pub fn answer_auth_prompt(&self, id: AcpJsonRpcId, result: Value) -> EmbeddedAgentResult<()> {
        self.respond_to_host_request(id, result)
    }

    /// ACP `initialize`.
    pub async fn initialize(&mut self) -> EmbeddedAgentResult<Value> {
        self.request_value(ACP_METHOD_INITIALIZE, json!({})).await
    }

    /// ACP `session/new`.
    pub async fn start_run(
        &mut self,
        params: AcpSessionNewParams,
    ) -> EmbeddedAgentResult<AcpSessionRestoreResult> {
        let result = self.request_value(ACP_METHOD_SESSION_NEW, params).await?;
        restore_result_from_value(result)
    }

    /// ACP `session/load`, replaying persisted session events into a live
    /// in-process session when available.
    pub async fn load_run(
        &mut self,
        session_id: impl Into<String>,
    ) -> EmbeddedAgentResult<AcpSessionRestoreResult> {
        let result = self
            .request_value(
                ACP_METHOD_SESSION_LOAD,
                AcpSessionIdParams::new(session_id.into()),
            )
            .await?;
        restore_result_from_value(result)
    }

    /// ACP `session/load` with an explicit working directory.
    pub async fn load_run_with_cwd(
        &mut self,
        session_id: impl Into<String>,
        cwd: impl Into<String>,
    ) -> EmbeddedAgentResult<AcpSessionRestoreResult> {
        let result = self
            .request_value(
                ACP_METHOD_SESSION_LOAD,
                json!({"sessionId": session_id.into(), "cwd": cwd.into()}),
            )
            .await?;
        restore_result_from_value(result)
    }

    /// ACP `session/resume`.
    pub async fn resume_run(
        &mut self,
        session_id: impl Into<String>,
    ) -> EmbeddedAgentResult<AcpSessionRestoreResult> {
        let result = self
            .request_value(
                ACP_METHOD_SESSION_RESUME,
                AcpSessionIdParams::new(session_id.into()),
            )
            .await?;
        restore_result_from_value(result)
    }

    /// ACP `session/prompt`.
    pub async fn send_user_input(
        &mut self,
        params: AcpSessionPromptParams,
    ) -> EmbeddedAgentResult<AcpSessionPromptResult> {
        self.request_typed(ACP_METHOD_SESSION_PROMPT, params).await
    }

    /// Start ACP `session/prompt` without waiting for the result.
    pub fn begin_user_input(&mut self, params: AcpSessionPromptParams) -> EmbeddedAgentResult<u64> {
        self.begin_request(ACP_METHOD_SESSION_PROMPT, params)
    }

    /// ACP `session/inject`.
    pub async fn inject_user_input(
        &mut self,
        params: AcpSessionInjectParams,
    ) -> EmbeddedAgentResult<Value> {
        self.request_value(ACP_METHOD_SESSION_INJECT, params).await
    }

    /// ACP `session/cancel`.
    pub async fn cancel_session(
        &mut self,
        session_id: impl Into<String>,
    ) -> EmbeddedAgentResult<Value> {
        self.request_value(
            ACP_METHOD_SESSION_CANCEL,
            AcpSessionIdParams::new(session_id.into()),
        )
        .await
    }

    /// ACP `session/close`.
    pub async fn close_session(
        &mut self,
        session_id: impl Into<String>,
    ) -> EmbeddedAgentResult<Value> {
        self.request_value(
            ACP_METHOD_SESSION_CLOSE,
            AcpSessionIdParams::new(session_id.into()),
        )
        .await
    }

    /// Query the stable `harn.session_view.v1` projection for a session.
    pub async fn session_view(
        &mut self,
        session_id: impl Into<String>,
    ) -> EmbeddedAgentResult<harn_vm::orchestration::SessionView> {
        self.request_typed(
            harn_vm::orchestration::SESSION_VIEW_QUERY_METHOD,
            json!({"sessionId": session_id.into()}),
        )
        .await
    }

    /// Query a stable session view rooted at a persisted run record.
    pub async fn session_view_for_run_path(
        &mut self,
        run_path: impl Into<String>,
        session_id: Option<String>,
    ) -> EmbeddedAgentResult<harn_vm::orchestration::SessionView> {
        let mut params = json!({"runPath": run_path.into()});
        if let Some(session_id) = session_id {
            params["sessionId"] = Value::String(session_id);
        }
        self.request_typed(harn_vm::orchestration::SESSION_VIEW_QUERY_METHOD, params)
            .await
    }

    /// Load a persisted run record as the stable `harn.run_view.v1`
    /// projection without exposing private run-record internals.
    pub fn run_view_from_path(
        path: impl AsRef<Path>,
    ) -> EmbeddedAgentResult<harn_vm::orchestration::RunView> {
        let path = path.as_ref();
        let run = harn_vm::orchestration::load_run_record(path)
            .map_err(|error| EmbeddedAgentError::View(error.to_string()))?;
        Ok(harn_vm::orchestration::build_run_view_with_path(
            &run,
            Some(path.to_string_lossy().to_string()),
        ))
    }

    /// Subscribe to stable session timeline updates for a session.
    pub async fn subscribe_session_events(
        &mut self,
        session_id: impl Into<String>,
    ) -> EmbeddedAgentResult<String> {
        let result = self
            .request_value(
                harn_vm::session_timeline::SESSION_TIMELINE_SUBSCRIBE_METHOD,
                json!({"sessionId": session_id.into()}),
            )
            .await?;
        result
            .get("subscriptionId")
            .and_then(Value::as_str)
            .map(str::to_string)
            .ok_or(EmbeddedAgentError::UnexpectedMessage(result))
    }

    /// Unsubscribe from a session timeline subscription.
    pub async fn unsubscribe_session_events(
        &mut self,
        subscription_id: impl Into<String>,
    ) -> EmbeddedAgentResult<Value> {
        self.request_value(
            harn_vm::session_timeline::SESSION_TIMELINE_UNSUBSCRIBE_METHOD,
            json!({"subscriptionId": subscription_id.into()}),
        )
        .await
    }

    /// Pause a workflow by id, resolving its base directory from a session.
    pub async fn pause_workflow(
        &mut self,
        session_id: impl Into<String>,
        workflow_id: impl Into<String>,
    ) -> EmbeddedAgentResult<Value> {
        self.request_value(
            "workflow/pause",
            json!({"sessionId": session_id.into(), "workflowId": workflow_id.into()}),
        )
        .await
    }

    /// Resume a workflow by id, resolving its base directory from a session.
    pub async fn resume_workflow(
        &mut self,
        session_id: impl Into<String>,
        workflow_id: impl Into<String>,
    ) -> EmbeddedAgentResult<Value> {
        self.request_value(
            "workflow/resume",
            json!({"sessionId": session_id.into(), "workflowId": workflow_id.into()}),
        )
        .await
    }

    fn next_request_id(&mut self) -> u64 {
        let id = self.next_id;
        self.next_id = if id == u64::MAX { 1 } else { id + 1 };
        id
    }

    async fn request_typed<P, R>(
        &mut self,
        method: impl Into<String>,
        params: P,
    ) -> EmbeddedAgentResult<R>
    where
        P: Serialize,
        R: DeserializeOwned,
    {
        let value = self.request_value(method, params).await?;
        Ok(serde_json::from_value(value)?)
    }
}

fn acp_id_matches(id: &AcpJsonRpcId, request_id: u64) -> bool {
    match id {
        AcpJsonRpcId::Number(value) => *value == request_id,
        AcpJsonRpcId::String(value) => value == &request_id.to_string(),
        AcpJsonRpcId::Null => false,
    }
}

fn restore_result_from_value(mut value: Value) -> EmbeddedAgentResult<AcpSessionRestoreResult> {
    if value.get("sessionId").is_none() {
        if let Some(session_id) = value
            .get("session")
            .and_then(|session| session.get("sessionId"))
            .cloned()
        {
            value["sessionId"] = session_id;
        }
    }
    Ok(serde_json::from_value(value)?)
}

fn classify_embedded_agent_message(value: Value) -> EmbeddedAgentResult<EmbeddedAgentEvent> {
    let raw = value.clone();
    if value.get("id").is_some() {
        if let Some(method) = value.get("method").and_then(Value::as_str) {
            let id: AcpJsonRpcId = serde_json::from_value(value["id"].clone())?;
            return Ok(EmbeddedAgentEvent::HostRequest {
                id,
                method: method.to_string(),
                params: value.get("params").cloned().unwrap_or(Value::Null),
                raw,
            });
        }
        if value.get("error").is_some() {
            let response: AcpJsonRpcErrorResponse = serde_json::from_value(value)?;
            return Ok(EmbeddedAgentEvent::RequestFailed {
                id: response.id,
                error: response.error,
                raw,
            });
        }
        if value.get("result").is_some() {
            let response: AcpJsonRpcResponse<Value> = serde_json::from_value(value)?;
            return Ok(EmbeddedAgentEvent::RequestCompleted {
                id: response.id,
                result: response.result,
                raw,
            });
        }
    }

    let Some(method) = value.get("method").and_then(Value::as_str) else {
        return Err(EmbeddedAgentError::UnexpectedMessage(raw));
    };
    let params = value.get("params").cloned().unwrap_or(Value::Null);
    match method {
        "session/update" => Ok(EmbeddedAgentEvent::SessionUpdate {
            session_id: params
                .get("sessionId")
                .and_then(Value::as_str)
                .map(str::to_string),
            update: params
                .get("update")
                .cloned()
                .unwrap_or_else(|| params.clone()),
            raw,
        }),
        harn_vm::session_timeline::SESSION_TIMELINE_UPDATE_METHOD => {
            Ok(EmbeddedAgentEvent::TimelineUpdate {
                subscription_id: params
                    .get("subscriptionId")
                    .and_then(Value::as_str)
                    .unwrap_or_default()
                    .to_string(),
                update: params
                    .get("update")
                    .cloned()
                    .unwrap_or_else(|| params.clone()),
                raw,
            })
        }
        HARN_AGENT_EVENT_METHOD => {
            let event = params
                .get("event")
                .cloned()
                .unwrap_or_else(|| params.clone());
            Ok(EmbeddedAgentEvent::AgentEvent {
                kind: params
                    .get("kind")
                    .or_else(|| event.get("kind"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
                event,
                raw,
            })
        }
        _ => Ok(EmbeddedAgentEvent::Notification {
            method: method.to_string(),
            params,
            raw,
        }),
    }
}

impl Drop for EmbeddedAgent {
    fn drop(&mut self) {
        // Only own teardown when we still hold the worker thread. `into_parts`
        // detaches it (sets `thread` to `None`), handing lifetime control to
        // the returned sender + handle, so dropping the husk must be inert.
        if let Some(thread) = self.thread.take() {
            self.handle.shutdown();
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn block_on<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime")
            .block_on(future)
    }

    async fn recv_json(rx: &mut mpsc::UnboundedReceiver<String>) -> serde_json::Value {
        let line = rx.recv().await.expect("ACP response channel closed");
        serde_json::from_str(&line).expect("valid JSON-RPC line")
    }

    #[test]
    fn embedded_agent_round_trips_session_new_and_shuts_down() {
        let mut agent = EmbeddedAgent::spawn(AcpServerConfig::new(None));
        let requests = agent.requests();
        let mut responses = agent.take_responses().expect("responses receiver");

        block_on(agent.handle().wait_ready());

        requests
            .send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 1,
                "method": "session/new",
                "params": {"cwd": "."},
            }))
            .expect("send session/new");

        let created = block_on(recv_json(&mut responses));
        assert!(
            created["result"]["sessionId"].as_str().is_some(),
            "session/new should return a sessionId, got: {created}"
        );

        // Graceful shutdown stops the worker thread.
        agent.shutdown();
        assert!(agent.join().is_ok(), "worker thread should join cleanly");
        assert!(agent.handle().is_shutdown());
        assert!(agent.handle().is_terminated());
    }

    #[test]
    fn shutdown_handle_terminates_idle_agent() {
        let agent = EmbeddedAgent::spawn(AcpServerConfig::new(None));
        let handle = agent.handle().clone();

        block_on(handle.wait_ready());
        assert!(!handle.is_terminated());

        // A shutdown trigger from a cloned handle (a different owner than the
        // EmbeddedAgent) must stop an otherwise-idle server loop.
        handle.shutdown();
        block_on(handle.wait_terminated());

        drop(agent); // Drop joins the worker; must not hang.
        assert!(handle.is_shutdown());
    }

    #[test]
    fn dropping_request_sender_terminates_agent() {
        // `into_parts` detaches the worker thread and hands over the *only*
        // request sender, so dropping it closes `request_rx`. That must stop
        // the router and the loop even without an explicit shutdown() — the
        // legacy EOF-style teardown that existing callers rely on.
        let agent = EmbeddedAgent::spawn(AcpServerConfig::new(None));
        let (requests, _responses, handle) = agent.into_parts();

        block_on(handle.wait_ready());
        assert!(!handle.is_terminated());

        drop(requests);
        block_on(handle.wait_terminated());
        assert!(
            !handle.is_shutdown(),
            "EOF teardown must not set the shutdown flag"
        );
    }

    #[test]
    fn into_parts_detaches_thread_and_keeps_channels_live() {
        let agent = EmbeddedAgent::spawn(AcpServerConfig::new(None));
        let (requests, responses, handle) = agent.into_parts();
        let mut responses = responses.expect("responses receiver");

        block_on(handle.wait_ready());

        requests
            .send(serde_json::json!({
                "jsonrpc": "2.0",
                "id": 7,
                "method": "session/new",
                "params": {"cwd": "."},
            }))
            .expect("send session/new");
        let created = block_on(recv_json(&mut responses));
        assert!(created["result"]["sessionId"].as_str().is_some());

        handle.shutdown();
        block_on(handle.wait_terminated());
    }

    #[test]
    fn embedded_agent_client_wraps_session_lifecycle_and_views() {
        let mut client = block_on(EmbeddedAgentClient::spawn(AcpServerConfig::new(None)))
            .expect("spawn embedded client");

        let created = block_on(client.start_run(AcpSessionNewParams::cwd(".")))
            .expect("session/new through client");
        assert!(!created.session_id.is_empty());

        let resumed = block_on(client.resume_run(created.session_id.clone()))
            .expect("session/resume through client");
        assert_eq!(resumed.session_id, created.session_id);

        let view =
            block_on(client.session_view(created.session_id.clone())).expect("session view query");
        assert_eq!(view.schema, harn_vm::orchestration::SESSION_VIEW_SCHEMA);
        assert_eq!(view.session.session_id, Some(created.session_id));

        client.shutdown();
        client.join().expect("client worker joins");
    }

    #[test]
    fn embedded_agent_client_can_drive_request_results_as_events() {
        let mut client = block_on(EmbeddedAgentClient::spawn(AcpServerConfig::new(None)))
            .expect("spawn embedded client");

        let request_id = client
            .begin_request(ACP_METHOD_SESSION_NEW, AcpSessionNewParams::cwd("."))
            .expect("begin session/new");
        let event = block_on(client.next_event()).expect("next event");
        match event {
            EmbeddedAgentEvent::RequestCompleted { id, result, .. } => {
                assert!(acp_id_matches(&id, request_id));
                assert!(result["sessionId"].as_str().is_some());
            }
            other => panic!("expected request completion, got {other:?}"),
        }

        client.shutdown();
        client.join().expect("client worker joins");
    }

    #[test]
    fn embedded_agent_client_preserves_buffered_events_while_waiting_for_result() {
        let mut client = block_on(EmbeddedAgentClient::spawn(AcpServerConfig::new(None)))
            .expect("spawn embedded client");
        client.pending.push_back(EmbeddedAgentEvent::Notification {
            method: "test/notification".to_string(),
            params: serde_json::json!({"ok": true}),
            raw: serde_json::json!({"method": "test/notification"}),
        });

        let created = block_on(client.start_run(AcpSessionNewParams::cwd(".")))
            .expect("session/new through client");
        assert!(!created.session_id.is_empty());

        let buffered = block_on(client.next_event()).expect("buffered event");
        match buffered {
            EmbeddedAgentEvent::Notification { method, params, .. } => {
                assert_eq!(method, "test/notification");
                assert_eq!(params["ok"], true);
            }
            other => panic!("expected buffered notification, got {other:?}"),
        }

        client.shutdown();
        client.join().expect("client worker joins");
    }

    #[test]
    fn embedded_agent_client_classifies_host_requests_and_notifications() {
        let host = classify_embedded_agent_message(serde_json::json!({
            "jsonrpc": "2.0",
            "id": "approval_1",
            "method": "session/request_permission",
            "params": {"toolCallId": "tool_1"},
        }))
        .expect("host request");
        match host {
            EmbeddedAgentEvent::HostRequest {
                id, method, params, ..
            } => {
                assert_eq!(id, AcpJsonRpcId::String("approval_1".to_string()));
                assert_eq!(method, "session/request_permission");
                assert_eq!(params["toolCallId"], "tool_1");
            }
            other => panic!("expected host request, got {other:?}"),
        }

        let update = classify_embedded_agent_message(serde_json::json!({
            "jsonrpc": "2.0",
            "method": "session/update",
            "params": {
                "sessionId": "s1",
                "update": {"sessionUpdate": "agent_message_chunk", "content": {"type": "text", "text": "hi"}}
            },
        }))
        .expect("session update");
        match update {
            EmbeddedAgentEvent::SessionUpdate {
                session_id, update, ..
            } => {
                assert_eq!(session_id.as_deref(), Some("s1"));
                assert_eq!(update["sessionUpdate"], "agent_message_chunk");
            }
            other => panic!("expected session update, got {other:?}"),
        }

        let timeline = classify_embedded_agent_message(serde_json::json!({
            "jsonrpc": "2.0",
            "method": harn_vm::session_timeline::SESSION_TIMELINE_UPDATE_METHOD,
            "params": {"subscriptionId": "sub_1", "update": {"schemaVersion": 1}},
        }))
        .expect("timeline update");
        match timeline {
            EmbeddedAgentEvent::TimelineUpdate {
                subscription_id,
                update,
                ..
            } => {
                assert_eq!(subscription_id, "sub_1");
                assert_eq!(update["schemaVersion"], 1);
            }
            other => panic!("expected timeline update, got {other:?}"),
        }
    }
}
