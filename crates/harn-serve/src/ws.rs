//! WebSocket upgrade primitive for axum routers built by `harn-serve`
//! adapters.
//!
//! Adapters mount [`ws_route`] on a path, supply a handler closure
//! `Fn(WsSession) -> Future`, and the layer takes care of:
//!
//! * Subprotocol negotiation — the server picks the first
//!   `Sec-WebSocket-Protocol` offered by the client that matches one
//!   of the route's configured subprotocols.
//! * Idle keepalive — periodic ping frames keep the connection open
//!   through intermediaries that drop silent sockets.
//! * Heartbeat reply — outbound pings get a matching pong; inbound
//!   pings get an automatic pong from axum's stream.
//!
//! `.harn` handlers reach this primitive through the
//! `http_upgrade_ws(req, options)` envelope tag: the codec recognises
//! the upgrade marker and the hosting adapter routes it into a
//! [`WsSession`] over channels (`#1870`). The channel bridge belongs
//! in the future `.harn` HTTP host; this module provides the
//! transport-side mechanics independent of that hosting story.

use std::sync::Arc;
use std::time::Duration;

use axum::extract::ws::{Message, WebSocket, WebSocketUpgrade};
use axum::http::HeaderMap;
use axum::response::Response;
use axum::routing::{any, MethodRouter};
use tokio::sync::Mutex;

/// Default idle ping cadence — every 30 s.
pub const DEFAULT_IDLE_PING_MS: u64 = 30_000;

/// Default per-message size cap (1 MiB). Matches what most browsers
/// will accept without explicit configuration.
pub const DEFAULT_MAX_MESSAGE_BYTES: usize = 1024 * 1024;

/// Per-route configuration for [`ws_route`]. All defaults are sane for
/// the cloud-gateway use cases (event streams, CLI proxy).
#[derive(Clone, Debug)]
pub struct WsConfig {
    /// Subprotocols the server can serve, in preference order. If the
    /// client offers any of these, the first match wins. An empty list
    /// disables subprotocol negotiation — the server accepts the
    /// upgrade with no `Sec-WebSocket-Protocol` echo.
    pub subprotocols: Vec<String>,
    /// Idle ping interval. `None` disables keepalive.
    pub idle_ping: Option<Duration>,
    /// Per-message size cap. Frames larger than this terminate the
    /// connection with a 1009 (`Message Too Big`).
    pub max_message_bytes: usize,
}

impl Default for WsConfig {
    fn default() -> Self {
        Self {
            subprotocols: Vec::new(),
            idle_ping: Some(Duration::from_millis(DEFAULT_IDLE_PING_MS)),
            max_message_bytes: DEFAULT_MAX_MESSAGE_BYTES,
        }
    }
}

/// A bidirectional WebSocket session handed to the route handler. The
/// handler reads via [`WsSession::recv`] and writes via
/// [`WsSession::send`] / [`WsSession::send_binary`]; closing happens
/// implicitly when the handler returns, or explicitly via
/// [`WsSession::close`].
pub struct WsSession {
    inner: Arc<Mutex<WebSocket>>,
    pub negotiated_subprotocol: Option<String>,
    pub max_message_bytes: usize,
}

impl WsSession {
    /// Receive the next message. Returns `Ok(None)` on a clean close
    /// or `Err(WsError::Closed)` if the peer has already gone away.
    pub async fn recv(&self) -> Result<Option<WsMessage>, WsError> {
        loop {
            let next = {
                let mut guard = self.inner.lock().await;
                use futures::StreamExt;
                guard.next().await
            };
            match next {
                Some(Ok(message)) => match message {
                    Message::Text(text) => {
                        if text.len() > self.max_message_bytes {
                            self.send_close(1009, "message too big").await?;
                            return Err(WsError::MessageTooBig);
                        }
                        return Ok(Some(WsMessage::Text(text.to_string())));
                    }
                    Message::Binary(bytes) => {
                        if bytes.len() > self.max_message_bytes {
                            self.send_close(1009, "message too big").await?;
                            return Err(WsError::MessageTooBig);
                        }
                        return Ok(Some(WsMessage::Binary(bytes.to_vec())));
                    }
                    Message::Ping(_) | Message::Pong(_) => continue,
                    Message::Close(_) => return Ok(None),
                },
                Some(Err(error)) => return Err(WsError::Transport(error.to_string())),
                None => return Ok(None),
            }
        }
    }

    /// Send a text message.
    pub async fn send(&self, text: impl Into<String>) -> Result<(), WsError> {
        let text = text.into();
        if text.len() > self.max_message_bytes {
            return Err(WsError::MessageTooBig);
        }
        let mut guard = self.inner.lock().await;
        guard
            .send(Message::Text(text.into()))
            .await
            .map_err(|error| WsError::Transport(error.to_string()))
    }

    /// Send a binary message.
    pub async fn send_binary(&self, bytes: Vec<u8>) -> Result<(), WsError> {
        if bytes.len() > self.max_message_bytes {
            return Err(WsError::MessageTooBig);
        }
        let mut guard = self.inner.lock().await;
        guard
            .send(Message::Binary(bytes.into()))
            .await
            .map_err(|error| WsError::Transport(error.to_string()))
    }

    /// Send a ping frame. Heartbeats sent during the idle interval
    /// happen automatically; callers usually do not need this.
    pub async fn ping(&self) -> Result<(), WsError> {
        let mut guard = self.inner.lock().await;
        guard
            .send(Message::Ping(Default::default()))
            .await
            .map_err(|error| WsError::Transport(error.to_string()))
    }

    /// Send a close frame with the given code and reason, then drop
    /// the socket.
    pub async fn close(&self, code: u16, reason: &str) -> Result<(), WsError> {
        self.send_close(code, reason).await
    }

    async fn send_close(&self, code: u16, reason: &str) -> Result<(), WsError> {
        let mut guard = self.inner.lock().await;
        guard
            .send(Message::Close(Some(axum::extract::ws::CloseFrame {
                code,
                reason: reason.to_string().into(),
            })))
            .await
            .map_err(|error| WsError::Transport(error.to_string()))
    }
}

#[derive(Debug, Clone)]
pub enum WsMessage {
    Text(String),
    Binary(Vec<u8>),
}

#[derive(Debug)]
pub enum WsError {
    /// The peer or transport returned an error mid-stream.
    Transport(String),
    /// A frame exceeded `max_message_bytes`.
    MessageTooBig,
    /// Connection has already closed.
    Closed,
}

impl std::fmt::Display for WsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Transport(message) => write!(f, "ws transport error: {message}"),
            Self::MessageTooBig => write!(f, "ws message too big"),
            Self::Closed => write!(f, "ws connection closed"),
        }
    }
}

impl std::error::Error for WsError {}

/// Build a route handler that performs WebSocket upgrade and dispatches
/// accepted sessions to `handler`.
///
/// Generic over the outer router's state `S` so the result composes
/// directly with `Router<S>::route(path, ws_route(...))`. The WS
/// machinery itself is stateless from axum's perspective — per-route
/// configuration and the handler closure are captured in the inner
/// closure rather than threaded through `with_state`.
pub fn ws_route<S, F, Fut>(handler: F, config: WsConfig) -> MethodRouter<S>
where
    S: Clone + Send + Sync + 'static,
    F: Fn(WsSession) -> Fut + Clone + Send + Sync + 'static,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let config = Arc::new(config);
    let handler: Arc<dyn Fn(WsSession) -> WsHandlerFuture + Send + Sync + 'static> =
        Arc::new(move |session| Box::pin(handler(session)) as _);
    any(move |headers: HeaderMap, upgrade: WebSocketUpgrade| {
        let config = config.clone();
        let handler = handler.clone();
        ws_dispatch(config, handler, headers, upgrade)
    })
}

type WsHandlerFuture = std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>>;

async fn ws_dispatch(
    config: Arc<WsConfig>,
    handler: Arc<dyn Fn(WsSession) -> WsHandlerFuture + Send + Sync + 'static>,
    headers: HeaderMap,
    upgrade: WebSocketUpgrade,
) -> Response {
    let negotiated = negotiate_subprotocol(&headers, &config.subprotocols);
    let mut upgrade = upgrade.max_message_size(config.max_message_bytes);
    if let Some(subprotocol) = negotiated.clone() {
        upgrade = upgrade.protocols([subprotocol]);
    }

    upgrade.on_upgrade(move |socket| async move {
        let session = WsSession {
            inner: Arc::new(Mutex::new(socket)),
            negotiated_subprotocol: negotiated,
            max_message_bytes: config.max_message_bytes,
        };

        if let Some(interval) = config.idle_ping {
            let ping_socket = session.inner.clone();
            let ping_task = tokio::spawn(async move {
                let mut ticker = tokio::time::interval(interval);
                ticker.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Skip);
                ticker.tick().await; // skip the immediate first tick
                loop {
                    ticker.tick().await;
                    let mut guard = ping_socket.lock().await;
                    if guard.send(Message::Ping(Default::default())).await.is_err() {
                        break;
                    }
                }
            });

            handler(session).await;
            ping_task.abort();
        } else {
            handler(session).await;
        }
    })
}

fn negotiate_subprotocol(headers: &HeaderMap, offered: &[String]) -> Option<String> {
    if offered.is_empty() {
        return None;
    }
    let raw = headers.get("sec-websocket-protocol")?.to_str().ok()?;
    for client_choice in raw.split(',').map(str::trim) {
        if let Some(matched) = offered.iter().find(|name| name.as_str() == client_choice) {
            return Some(matched.clone());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::http::HeaderValue;
    use axum::Router;
    use futures::{SinkExt, StreamExt};
    use std::net::SocketAddr;
    use tokio::net::TcpListener;
    use tokio_tungstenite::tungstenite::protocol::Message as TungMessage;

    async fn echo(session: WsSession) {
        while let Ok(Some(message)) = session.recv().await {
            match message {
                WsMessage::Text(text) => {
                    if session.send(text).await.is_err() {
                        break;
                    }
                }
                WsMessage::Binary(bytes) => {
                    if session.send_binary(bytes).await.is_err() {
                        break;
                    }
                }
            }
        }
    }

    async fn spawn_server() -> SocketAddr {
        let app = Router::new().route("/ws", ws_route(echo, WsConfig::default()));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });
        addr
    }

    #[tokio::test]
    async fn ws_echo_roundtrip() {
        let addr = spawn_server().await;
        let url = format!("ws://{addr}/ws");
        let (mut socket, _response) = tokio_tungstenite::connect_async(url).await.unwrap();

        socket
            .send(TungMessage::Text("hello".into()))
            .await
            .unwrap();
        let echoed = socket.next().await.unwrap().unwrap();
        assert_eq!(echoed, TungMessage::Text("hello".into()));

        socket
            .send(TungMessage::Binary(vec![1, 2, 3, 4].into()))
            .await
            .unwrap();
        let echoed = socket.next().await.unwrap().unwrap();
        assert_eq!(echoed, TungMessage::Binary(vec![1, 2, 3, 4].into()));

        socket.send(TungMessage::Close(None)).await.unwrap();
    }

    #[tokio::test]
    async fn ws_subprotocol_negotiation_picks_first_match() {
        let config = WsConfig {
            subprotocols: vec!["v1.harn".into(), "v2.harn".into()],
            ..WsConfig::default()
        };
        let app = Router::new().route("/ws", ws_route(echo, config));
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let addr = listener.local_addr().unwrap();
        tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let url = format!("ws://{addr}/ws");
        let request =
            tokio_tungstenite::tungstenite::client::IntoClientRequest::into_client_request(url)
                .unwrap();
        let mut request = request;
        request.headers_mut().insert(
            "Sec-WebSocket-Protocol",
            "unsupported, v2.harn".parse().unwrap(),
        );
        let (mut socket, response) = tokio_tungstenite::connect_async(request).await.unwrap();
        assert_eq!(
            response
                .headers()
                .get("sec-websocket-protocol")
                .map(|v| v.to_str().unwrap()),
            Some("v2.harn"),
        );
        socket.send(TungMessage::Close(None)).await.unwrap();
    }

    #[test]
    fn negotiate_subprotocol_picks_first_match() {
        let mut headers = HeaderMap::new();
        headers.insert(
            "sec-websocket-protocol",
            HeaderValue::from_static("a, b, c"),
        );
        assert_eq!(
            negotiate_subprotocol(&headers, &["c".into(), "a".into()]),
            Some("a".into())
        );
    }

    #[test]
    fn negotiate_subprotocol_returns_none_when_no_overlap() {
        let mut headers = HeaderMap::new();
        headers.insert("sec-websocket-protocol", HeaderValue::from_static("x, y"));
        assert_eq!(
            negotiate_subprotocol(&headers, &["a".into(), "b".into()]),
            None
        );
    }

    #[test]
    fn negotiate_subprotocol_returns_none_when_offered_empty() {
        let mut headers = HeaderMap::new();
        headers.insert("sec-websocket-protocol", HeaderValue::from_static("a"));
        assert_eq!(negotiate_subprotocol(&headers, &[]), None);
    }
}
