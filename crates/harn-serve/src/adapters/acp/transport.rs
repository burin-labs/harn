//! ACP stdio and in-process channel transport loops.
use super::*;

use std::future::Future;
use std::net::SocketAddr;
use std::sync::atomic::{AtomicBool, Ordering};

use axum::extract::ws::WebSocketUpgrade;
use axum::extract::{OriginalUri, State};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::get;
use axum::Router;

use crate::ws::{WsConfig, WsMessage, WsSession};
use crate::{AuthRequest, HttpTlsConfig};

#[derive(Clone, Debug)]
pub struct AcpWebSocketServeOptions {
    pub bind: SocketAddr,
    pub path: String,
    pub tls: HttpTlsConfig,
}

impl Default for AcpWebSocketServeOptions {
    fn default() -> Self {
        Self {
            bind: SocketAddr::from(([127, 0, 0, 1], 8789)),
            path: "/acp".to_string(),
            tls: HttpTlsConfig::plain(),
        }
    }
}

#[derive(Clone)]
struct AcpWebSocketState {
    config: AcpServerConfig,
}

/// Start an ACP WebSocket endpoint. Each accepted WebSocket gets its own
/// channel-backed ACP server running on a dedicated current-thread runtime.
pub async fn run_acp_websocket_server(
    config: AcpServerConfig,
    options: AcpWebSocketServeOptions,
) -> Result<(), String> {
    if !options.path.starts_with('/') {
        return Err(format!(
            "ACP WebSocket path must start with `/`; got `{}`",
            options.path
        ));
    }

    let router = acp_websocket_router(config, &options.path);
    let router = crate::tls::apply_security_headers(router, &options.tls);
    let listener = crate::tls::bind_listener(options.bind)?;
    let local_addr = listener
        .local_addr()
        .map_err(|error| format!("failed to read local addr: {error}"))?;
    eprintln!(
        "[harn] ACP WebSocket server ready on {}://{local_addr}{}",
        options.tls.listener_scheme(),
        options.path
    );
    crate::tls::serve_router_from_tcp(listener, router, &options.tls)
        .await
        .map_err(|error| format!("ACP WebSocket server failed: {error}"))
}

fn acp_websocket_router(config: AcpServerConfig, path: &str) -> Router {
    let state = AcpWebSocketState { config };
    Router::new()
        .route(path, get(acp_websocket_upgrade))
        .with_state(state)
}

async fn acp_websocket_upgrade(
    State(state): State<AcpWebSocketState>,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let principal =
        match pre_authenticated_principal(&state.config.auth_policy, &headers, uri.path()).await {
            Ok(principal) => principal,
            Err(response) => return response,
        };
    let mut config = state.config.clone();
    if let Some(principal) = principal {
        config = config.with_authenticated_principal(principal);
    }
    crate::ws::ws_accept(WsConfig::default(), headers, upgrade, move |session| {
        acp_websocket_session(config.clone(), session)
    })
    .await
}

async fn pre_authenticated_principal(
    auth_policy: &AuthPolicy,
    headers: &HeaderMap,
    path: &str,
) -> Result<Option<AuthenticatedPrincipal>, Response> {
    if auth_policy.methods.is_empty() {
        return Ok(None);
    }

    let auth = AuthRequest::from_http(&Method::GET, path, Vec::new(), headers);
    if auth.api_key().is_none() {
        return Ok(None);
    }

    match auth_policy.authorize(&auth).await {
        AuthorizationDecision::Authorized(principal) => Ok(Some(principal)),
        AuthorizationDecision::Rejected(message) => Err(acp_ws_unauthorized(message)),
        AuthorizationDecision::MissingScope { required, granted } => Err(acp_ws_unauthorized(
            crate::forbidden_message(&required, &granted),
        )),
        AuthorizationDecision::McpNotAllowlisted { reason, .. } => Err(acp_ws_unauthorized(reason)),
    }
}

fn acp_ws_unauthorized(message: String) -> Response {
    (StatusCode::UNAUTHORIZED, message).into_response()
}

async fn acp_websocket_session(config: AcpServerConfig, session: WsSession) {
    let (request_tx, request_rx) = mpsc::unbounded_channel::<serde_json::Value>();
    let (response_tx, mut response_rx) = mpsc::unbounded_channel::<String>();

    let worker_thread = std::thread::Builder::new()
        .name("harn-acp-ws".to_string())
        .spawn(move || {
            let runtime = match tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
            {
                Ok(runtime) => runtime,
                Err(error) => {
                    eprintln!("[harn] failed to start ACP WebSocket runtime: {error}");
                    return;
                }
            };
            runtime.block_on(run_acp_channel_server(config, request_rx, response_tx));
        });
    let worker_thread = match worker_thread {
        Ok(worker_thread) => worker_thread,
        Err(error) => {
            let _ = session
                .send(
                    serde_json::to_string(&harn_vm::jsonrpc::error_response(
                        serde_json::Value::Null,
                        -32000,
                        &format!("failed to start ACP worker: {error}"),
                    ))
                    .unwrap_or_else(|_| {
                        r#"{"jsonrpc":"2.0","id":null,"error":{"code":-32000,"message":"failed to start ACP worker"}}"#
                            .to_string()
                    }),
                )
                .await;
            let _ = session.close(1011, "failed to start ACP worker").await;
            return;
        }
    };

    loop {
        tokio::select! {
            incoming = session.recv() => {
                match incoming {
                    Ok(Some(WsMessage::Text(text))) => {
                        match serde_json::from_str::<serde_json::Value>(&text) {
                            Ok(message) => {
                                if request_tx.send(message).is_err() {
                                    let _ = session.close(1011, "ACP worker stopped").await;
                                    break;
                                }
                            }
                            Err(error) => {
                                let response = harn_vm::jsonrpc::error_response(
                                    serde_json::Value::Null,
                                    -32700,
                                    &format!("Parse error: {error}"),
                                );
                                if let Ok(line) = serde_json::to_string(&response) {
                                    let _ = session.send(line).await;
                                }
                            }
                        }
                    }
                    Ok(Some(WsMessage::Binary(_))) => {
                        let response = harn_vm::jsonrpc::error_response(
                            serde_json::Value::Null,
                            -32600,
                            "Invalid Request: ACP WebSocket messages must be JSON text frames",
                        );
                        if let Ok(line) = serde_json::to_string(&response) {
                            let _ = session.send(line).await;
                        }
                        let _ = session.close(1003, "ACP requires text frames").await;
                        break;
                    }
                    Ok(None) => break,
                    Err(_) => break,
                }
            }
            outgoing = response_rx.recv() => {
                match outgoing {
                    Some(line) => {
                        if session.send(line).await.is_err() {
                            break;
                        }
                    }
                    None => break,
                }
            }
        }
    }

    drop(request_tx);
    let _ = tokio::task::spawn_blocking(move || worker_thread.join()).await;
}

/// Cross-thread control surface for an in-process ACP channel server.
///
/// The channel-server future is `!Send` (it owns a [`tokio::task::LocalSet`]
/// and `spawn_local`s onto it), so it must be driven on a dedicated
/// current-thread runtime — it cannot be `tokio::spawn`ed onto a multi-thread
/// runtime, and it cannot be moved between threads once polling starts.
/// `AcpChannelHandle`, by contrast, is `Send + Sync + Clone`: it is the piece
/// an embedder keeps on its own thread to observe and steer the server thread.
///
/// It exposes three signals:
///
/// * [`shutdown`](Self::shutdown) — request a graceful stop. The server stops
///   accepting new requests and drains, then the driving future resolves. This
///   is in addition to the existing teardown path of dropping the request
///   sender (closing `request_rx`), which also stops the loop.
/// * [`wait_ready`](Self::wait_ready) — resolves once the server has installed
///   its [`AcpServer`] and entered its message loop, so an embedder can defer
///   the first `session/new` until the runtime is live.
/// * [`wait_terminated`](Self::wait_terminated) — resolves once the server loop
///   has exited (for any reason: shutdown, closed channel, or completion),
///   giving the embedder a join-style rendezvous that does not require holding
///   the `!Send` future's `JoinHandle` across threads.
#[derive(Clone)]
pub struct AcpChannelHandle {
    shutdown_requested: Arc<AtomicBool>,
    shutdown_notify: Arc<Notify>,
    ready: Arc<AtomicBool>,
    ready_notify: Arc<Notify>,
    terminated: Arc<AtomicBool>,
    terminated_notify: Arc<Notify>,
}

impl AcpChannelHandle {
    fn new() -> Self {
        Self {
            shutdown_requested: Arc::new(AtomicBool::new(false)),
            shutdown_notify: Arc::new(Notify::new()),
            ready: Arc::new(AtomicBool::new(false)),
            ready_notify: Arc::new(Notify::new()),
            terminated: Arc::new(AtomicBool::new(false)),
            terminated_notify: Arc::new(Notify::new()),
        }
    }

    /// Request a graceful shutdown of the channel server. Idempotent and safe
    /// to call from any thread. Wakes the server loop, which stops accepting
    /// further requests and resolves its driving future. Calling this when the
    /// server has already terminated is a no-op.
    pub fn shutdown(&self) {
        self.shutdown_requested.store(true, Ordering::SeqCst);
        self.shutdown_notify.notify_waiters();
    }

    /// Whether [`shutdown`](Self::shutdown) has been requested.
    pub fn is_shutdown(&self) -> bool {
        self.shutdown_requested.load(Ordering::SeqCst)
    }

    /// Whether the server has reached its message loop and is ready to accept
    /// requests.
    pub fn is_ready(&self) -> bool {
        self.ready.load(Ordering::SeqCst)
    }

    /// Resolve once the server has entered its message loop. Returns
    /// immediately if it is already ready.
    pub async fn wait_ready(&self) {
        // Register interest before checking the flag so a `notify_waiters()`
        // from `mark_ready` cannot slip between the check and the await.
        let notified = self.ready_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.is_ready() {
            return;
        }
        notified.await;
    }

    /// Whether the server loop has exited.
    pub fn is_terminated(&self) -> bool {
        self.terminated.load(Ordering::SeqCst)
    }

    /// Resolve once the server loop has exited. Returns immediately if it has
    /// already terminated.
    pub async fn wait_terminated(&self) {
        // Register interest before checking the flag so a `notify_waiters()`
        // from `mark_terminated` cannot slip between the check and the await.
        let notified = self.terminated_notify.notified();
        tokio::pin!(notified);
        notified.as_mut().enable();
        if self.is_terminated() {
            return;
        }
        notified.await;
    }

    fn mark_ready(&self) {
        self.ready.store(true, Ordering::SeqCst);
        self.ready_notify.notify_waiters();
    }

    fn mark_terminated(&self) {
        self.terminated.store(true, Ordering::SeqCst);
        self.terminated_notify.notify_waiters();
    }
}

impl Default for AcpChannelHandle {
    fn default() -> Self {
        Self::new()
    }
}

/// In-process ACP channel server with a cross-thread control [`AcpChannelHandle`].
///
/// Returns the `!Send` driving future and a `Send + Sync + Clone` handle. The
/// future must be driven on a dedicated current-thread runtime (see
/// [`AcpChannelHandle`] for why); the handle can be kept on any thread to
/// observe readiness/termination and to trigger a graceful shutdown.
///
/// This is the building block behind both [`run_acp_channel_server`] (which
/// drops the handle and just awaits the future) and the higher-level
/// [`crate::embed::EmbeddedAgent`] wrapper (which owns the dedicated thread for
/// you). Reach for this directly when you already manage your own dedicated
/// thread + runtime but want the shutdown/readiness/termination signals.
///
/// Because the future is `!Send`, an embedder that wants to keep talking to
/// the agent from its own thread must build the future *on* the dedicated
/// worker thread (so it never crosses a boundary), passing the `Send` config +
/// channels + handle across. The handle is created first so a clone can be
/// returned to the caller. [`crate::embed::EmbeddedAgent`] packages exactly
/// this pattern; reach for this function directly only when you need to own the
/// dedicated thread + runtime yourself.
///
/// The simplest correct drive — building the runtime and awaiting the future
/// on the *same* thread — looks like:
///
/// ```no_run
/// use harn_serve::{run_acp_channel_server_with_handle, AcpServerConfig};
/// use tokio::sync::mpsc;
///
/// let (request_tx, request_rx) = mpsc::unbounded_channel();
/// let (response_tx, response_rx) = mpsc::unbounded_channel();
/// let config = AcpServerConfig::new(None);
///
/// let (server_future, handle) =
///     run_acp_channel_server_with_handle(config, request_rx, response_tx);
///
/// // Drive the `!Send` future on a current-thread runtime, on this thread.
/// let runtime = tokio::runtime::Builder::new_current_thread()
///     .enable_all()
///     .build()
///     .expect("start ACP runtime");
/// // `handle` (cloneable + `Send`) can be shared elsewhere before this blocks;
/// // `handle.shutdown()` then resolves the future from another thread.
/// let _control = handle.clone();
/// runtime.block_on(server_future);
/// # let _ = (request_tx, response_rx);
/// ```
pub fn run_acp_channel_server_with_handle(
    config: AcpServerConfig,
    request_rx: mpsc::UnboundedReceiver<serde_json::Value>,
    response_tx: mpsc::UnboundedSender<String>,
) -> (impl Future<Output = ()>, AcpChannelHandle) {
    let handle = AcpChannelHandle::new();
    let future = run_acp_channel_server_with_existing_handle(
        config,
        request_rx,
        response_tx,
        handle.clone(),
    );
    (future, handle)
}

/// Build the channel-server future bound to a caller-supplied
/// [`AcpChannelHandle`]. The handle is created on the embedder's thread (it is
/// `Send`) while the `!Send` future is built and driven on the dedicated
/// worker thread — see [`crate::embed::EmbeddedAgent`], which relies on this
/// split so the `!Send` future never crosses a thread boundary.
pub(crate) fn run_acp_channel_server_with_existing_handle(
    config: AcpServerConfig,
    request_rx: mpsc::UnboundedReceiver<serde_json::Value>,
    response_tx: mpsc::UnboundedSender<String>,
    handle: AcpChannelHandle,
) -> impl Future<Output = ()> {
    run_acp_channel_server_inner(config, request_rx, response_tx, handle)
}

async fn run_acp_channel_server_inner(
    config: AcpServerConfig,
    mut request_rx: mpsc::UnboundedReceiver<serde_json::Value>,
    response_tx: mpsc::UnboundedSender<String>,
    handle: AcpChannelHandle,
) {
    let profile_enabled = config.profile.is_enabled();
    let local = tokio::task::LocalSet::new();
    local
        .run_until(async move {
            let mut server = AcpServer::new_with_output(config, AcpOutput::Channel(response_tx));
            let pending_clone = server.pending.clone();
            let cancellations = server.session_cancellations.clone();
            let (routed_tx, mut routed_rx) =
                tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();

            // Shutdown signalling for the request-routing task. The router
            // task is itself `spawn_local`, so it shares this thread with the
            // loop below; a notify wake lets it stop draining `request_rx`
            // when the embedder calls `handle.shutdown()`.
            let router_shutdown = handle.shutdown_requested.clone();
            let router_notify = handle.shutdown_notify.clone();

            tokio::task::spawn_local(async move {
                loop {
                    // Register interest in the shutdown notify *before* checking
                    // the flag, so a `notify_waiters()` racing with this loop
                    // iteration cannot be missed (an unpolled `Notified` does
                    // not retain a `notify_waiters` wake).
                    let notified = router_notify.notified();
                    tokio::pin!(notified);
                    notified.as_mut().enable();
                    if router_shutdown.load(Ordering::SeqCst) {
                        break;
                    }
                    let msg = tokio::select! {
                        biased;
                        () = notified.as_mut() => {
                            if router_shutdown.load(Ordering::SeqCst) {
                                break;
                            }
                            continue;
                        }
                        msg = request_rx.recv() => match msg {
                            Some(msg) => msg,
                            None => break,
                        },
                    };

                    if msg.get("method").is_none() && msg.get("id").is_some() {
                        if let Some(id) = msg["id"].as_u64() {
                            let mut pending = pending_clone.lock().await;
                            if let Some(sender) = pending.remove(&id) {
                                let _ = sender.send(msg);
                            }
                        }
                        continue;
                    }

                    prepare_session_prompt(&cancellations, &msg);
                    if preempt_session_interruption(&cancellations, &msg) {
                        continue;
                    }
                    if apply_session_budget_rearm(&msg) {
                        continue;
                    }

                    let _ = routed_tx.send(msg);
                }

                let mut pending = pending_clone.lock().await;
                pending.clear();
            });

            handle.mark_ready();

            loop {
                let shutdown_notify = handle.shutdown_notify.notified();
                tokio::pin!(shutdown_notify);
                shutdown_notify.as_mut().enable();
                if handle.shutdown_requested.load(Ordering::SeqCst) {
                    break;
                }
                tokio::select! {
                    biased;
                    () = shutdown_notify.as_mut() => {
                        if handle.shutdown_requested.load(Ordering::SeqCst) {
                            break;
                        }
                    }
                    msg = routed_rx.recv() => match msg {
                        Some(msg) => server.handle_incoming_message(msg).await,
                        None => break,
                    },
                }
            }

            handle.mark_terminated();
        })
        .await;
    if profile_enabled {
        harn_vm::tracing::set_tracing_enabled(false);
    }
}

/// In-process ACP channel server. Reads JSON-RPC requests from `request_rx`
/// and writes responses / notifications to `response_tx`.
///
/// The returned future is `!Send` and must be driven on a dedicated
/// current-thread runtime. Embedders that need a graceful-shutdown trigger,
/// readiness signal, or join-style termination rendezvous should use
/// [`run_acp_channel_server_with_handle`] instead (this function delegates to
/// it and simply drops the handle), or the higher-level
/// [`crate::embed::EmbeddedAgent`] wrapper.
pub async fn run_acp_channel_server(
    config: AcpServerConfig,
    request_rx: mpsc::UnboundedReceiver<serde_json::Value>,
    response_tx: mpsc::UnboundedSender<String>,
) {
    let (future, _handle) = run_acp_channel_server_with_handle(config, request_rx, response_tx);
    future.await;
}

/// Start the ACP server. Reads JSON-RPC from stdin, writes to stdout.
pub async fn run_acp_server(config: AcpServerConfig) {
    let profile_enabled = config.profile.is_enabled();
    let local = tokio::task::LocalSet::new();

    local
        .run_until(async move {
            let mut server = AcpServer::new(config);

            // stdin dispatcher: routes responses to pending waiters, and
            // requests/notifications onto the request channel.
            let pending_clone = server.pending.clone();
            let cancellations = server.session_cancellations.clone();
            let (request_tx, mut request_rx) =
                tokio::sync::mpsc::unbounded_channel::<serde_json::Value>();

            eprintln!("[harn] ACP workflow server ready on stdio");

            tokio::task::spawn_local(async move {
                let stdin = tokio::io::stdin();
                let reader = tokio::io::BufReader::new(stdin);
                let mut lines = reader.lines();

                while let Ok(Some(line)) = lines.next_line().await {
                    let line = line.trim().to_string();
                    if line.is_empty() {
                        continue;
                    }

                    let msg: serde_json::Value = match serde_json::from_str(&line) {
                        Ok(v) => v,
                        Err(_) => continue,
                    };

                    if msg.get("method").is_none() && msg.get("id").is_some() {
                        if let Some(id) = msg["id"].as_u64() {
                            let mut pending = pending_clone.lock().await;
                            if let Some(sender) = pending.remove(&id) {
                                let _ = sender.send(msg);
                            }
                        }
                        continue;
                    }

                    prepare_session_prompt(&cancellations, &msg);
                    if preempt_session_interruption(&cancellations, &msg) {
                        continue;
                    }
                    if apply_session_budget_rearm(&msg) {
                        continue;
                    }

                    let _ = request_tx.send(msg);
                }

                // stdin closed — clean up pending.
                let mut pending = pending_clone.lock().await;
                pending.clear();
            });

            while let Some(msg) = request_rx.recv().await {
                server.handle_incoming_message(msg).await;
            }
        })
        .await;
    if profile_enabled {
        harn_vm::tracing::set_tracing_enabled(false);
    }
}

#[cfg(test)]
mod websocket_tests {
    use super::*;

    use futures::{SinkExt, StreamExt};
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::Message;

    async fn spawn_acp_ws(config: AcpServerConfig) -> SocketAddr {
        let app = acp_websocket_router(config, "/acp");
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        addr
    }

    async fn recv_json(
        socket: &mut tokio_tungstenite::WebSocketStream<
            tokio_tungstenite::MaybeTlsStream<tokio::net::TcpStream>,
        >,
    ) -> serde_json::Value {
        let message = socket
            .next()
            .await
            .expect("ACP WS stream closed")
            .expect("ACP WS frame");
        let text = message.into_text().expect("text frame");
        serde_json::from_str(&text).expect("ACP JSON")
    }

    #[tokio::test]
    async fn acp_websocket_initialize_and_session_new_roundtrip() {
        let addr = spawn_acp_ws(AcpServerConfig::new(None)).await;
        let url = format!("ws://{addr}/acp");
        let (mut socket, _response) = tokio_tungstenite::connect_async(url).await.unwrap();

        socket
            .send(Message::Text(
                serde_json::to_string(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 1,
                    "method": "initialize",
                    "params": {},
                }))
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        let initialize = recv_json(&mut socket).await;
        assert_eq!(initialize["id"], 1);
        assert_eq!(initialize["result"]["agentInfo"]["name"], "harn");

        socket
            .send(Message::Text(
                serde_json::to_string(&serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": 2,
                    "method": "session/new",
                    "params": {"cwd": "."},
                }))
                .unwrap()
                .into(),
            ))
            .await
            .unwrap();
        let created = recv_json(&mut socket).await;
        assert_eq!(created["id"], 2);
        assert!(created["result"]["sessionId"].as_str().is_some());

        socket.send(Message::Close(None)).await.unwrap();
    }
}
