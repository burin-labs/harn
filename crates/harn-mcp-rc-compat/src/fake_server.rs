//! Fake RC MCP servers used to validate Harn's MCP client behavior.
//!
//! Both transports (HTTP Streamable and stdio) are implemented in-process
//! so tests can exercise the full request/response cycle without
//! standing up a real network service. The behavior matrix is keyed by
//! [`FakeServerBehavior`] so a single test driver can fan out across the
//! seven RC compatibility cases (modern success, unsupported-version
//! retry, server/discover, cache hints, MRTR/input-required, header
//! mismatch, no-session HTTP) plus the legacy 2025-11-25 baseline.

use std::collections::BTreeMap;
use std::sync::Arc;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::post;
use axum::{Json, Router};
use harn_vm::mcp_protocol::{
    self, server_discover_result, DRAFT_PROTOCOL_VERSION, PROTOCOL_VERSION, RC_HEADER_METHOD,
    RC_HEADER_NAME, RC_HEADER_PROTOCOL_VERSION, RC_META_KEY_PROTOCOL_VERSION,
};
use serde_json::{json, Value as JsonValue};
use tokio::net::TcpListener;
use tokio::sync::{mpsc, oneshot, Mutex};
use tokio::task::JoinHandle;

use crate::recursive_schema::recursive_tree_input_schema;

/// Behaviors the fake server can emulate. Each is a slice of the RC's
/// observable surface; combining several into one fixture is fine, but
/// the individual tests typically pick one per scenario to keep the
/// failure attribution sharp.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum FakeServerBehavior {
    /// Accept `server/discover`, return Modern `tools/list`/`tools/call`
    /// results with the RC envelope (resultType + cache hint).
    ModernSuccess,
    /// First `server/discover` answers with `-32004` and a `supported`
    /// list; second discover with the legacy version succeeds.
    UnsupportedVersionRetry,
    /// `server/discover` advertises both supported versions; client
    /// chooses Modern.
    ServerDiscover,
    /// `tools/list` answers with explicit `ttlMs` + `cacheScope` cache
    /// hints (overrides the conservative defaults).
    CacheHints,
    /// First `tools/call` returns `resultType: input_required` with an
    /// elicitation request; client must resolve the elicitation and
    /// re-issue the call carrying `inputResponses`.
    InputRequired,
    /// HTTP server returns `-32600` when `Mcp-Method` header disagrees
    /// with the body method.
    HeaderMismatch,
    /// HTTP server refuses to emit `Mcp-Session-Id` and rejects any
    /// request that includes one. Validates session-less RC routing.
    NoSessionHttp,
    /// Pre-RC `2025-11-25` server: only `initialize` works, no
    /// `server/discover`, no envelope, no cache hints. Used for legacy
    /// compat regression.
    Legacy202511,
    /// Modern tool schema includes a recursive `$defs` reference so the
    /// client must accept JSON Schema 2020-12 dialect.
    RecursiveDefsTool,
}

/// One inbound request observed by the fake server. Tests assert on
/// these to verify the client emitted the right headers, body, and
/// metadata.
#[derive(Clone, Debug)]
pub struct RecordedRequest {
    pub headers: BTreeMap<String, String>,
    pub body: JsonValue,
}

#[derive(Default)]
struct ServerState {
    behavior: Option<FakeServerBehavior>,
    requests: Vec<RecordedRequest>,
    /// Tracks how many `server/discover` calls have been seen so the
    /// retry case can answer the first with `-32004` and the second
    /// with a success.
    discover_count: usize,
    /// Tracks how many `tools/call` calls have been seen so the
    /// input-required case can hand the second call back as completed.
    call_count: usize,
}

/// Handle returned from [`spawn_fake_http_server`]. Drop it (or call
/// [`Self::shutdown`]) to tear the server down.
pub struct FakeHttpServer {
    pub base_url: String,
    state: Arc<Mutex<ServerState>>,
    shutdown: Option<oneshot::Sender<()>>,
    join: Option<JoinHandle<()>>,
}

impl FakeHttpServer {
    pub async fn recorded(&self) -> Vec<RecordedRequest> {
        self.state.lock().await.requests.clone()
    }

    pub async fn shutdown(mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        if let Some(handle) = self.join.take() {
            let _ = handle.await;
        }
    }
}

impl Drop for FakeHttpServer {
    fn drop(&mut self) {
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
    }
}

/// Spawn a fake RC HTTP MCP server on `127.0.0.1:0`. Returns the URL
/// of the `/mcp` endpoint plus a [`FakeHttpServer`] handle for
/// observation and shutdown.
pub async fn spawn_fake_http_server(behavior: FakeServerBehavior) -> FakeHttpServer {
    let state = Arc::new(Mutex::new(ServerState {
        behavior: Some(behavior),
        ..ServerState::default()
    }));
    let router = Router::new()
        .route("/mcp", post(post_mcp).get(get_mcp))
        .with_state(state.clone());
    let listener = TcpListener::bind("127.0.0.1:0")
        .await
        .expect("bind fake server");
    let local = listener.local_addr().expect("local addr");
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let join = tokio::spawn(async move {
        axum::serve(listener, router)
            .with_graceful_shutdown(async move {
                let _ = shutdown_rx.await;
            })
            .await
            .expect("fake server serve");
    });
    FakeHttpServer {
        base_url: format!("http://{local}"),
        state,
        shutdown: Some(shutdown_tx),
        join: Some(join),
    }
}

async fn post_mcp(
    State(state): State<Arc<Mutex<ServerState>>>,
    headers: HeaderMap,
    Json(body): Json<JsonValue>,
) -> Response {
    let recorded = RecordedRequest {
        headers: headers
            .iter()
            .filter_map(|(name, value)| {
                value
                    .to_str()
                    .ok()
                    .map(|value| (name.as_str().to_ascii_lowercase(), value.to_string()))
            })
            .collect(),
        body: body.clone(),
    };
    let mut guard = state.lock().await;
    guard.requests.push(recorded.clone());
    let behavior = guard.behavior.expect("behavior set");
    let id = body.get("id").cloned().unwrap_or(JsonValue::Null);
    let method = body
        .get("method")
        .and_then(JsonValue::as_str)
        .unwrap_or_default()
        .to_string();
    if behavior == FakeServerBehavior::HeaderMismatch {
        // Strict-server slice: validate the RC `Mcp-Method` / `Mcp-Name`
        // headers against the body exactly like a real RC server and
        // answer `-32600` on disagreement. Reuses the production
        // negotiation helper so the fake can't drift from the spec.
        let header_lookup = |name: &str| headers.get(name).and_then(|value| value.to_str().ok());
        let body_name = body
            .pointer("/params/name")
            .and_then(JsonValue::as_str)
            .filter(|name| !name.is_empty());
        if let Err(error_body) = mcp_protocol::negotiate_rc_http_request(
            header_lookup,
            body.get("method").and_then(JsonValue::as_str),
            body_name,
            &id,
        ) {
            drop(guard);
            return json_response_with_protocol(error_body, DRAFT_PROTOCOL_VERSION);
        }
    }
    let response = handle_request(&mut guard, behavior, &id, &method, &body);
    drop(guard);
    match response {
        Some(value) => {
            let protocol = if behavior == FakeServerBehavior::Legacy202511 {
                PROTOCOL_VERSION
            } else {
                DRAFT_PROTOCOL_VERSION
            };
            json_response_with_protocol(value, protocol)
        }
        None => StatusCode::ACCEPTED.into_response(),
    }
}

async fn get_mcp() -> Response {
    StatusCode::METHOD_NOT_ALLOWED.into_response()
}

fn json_response_with_protocol(body: JsonValue, protocol: &str) -> Response {
    let mut response = Json(body).into_response();
    if let Ok(value) = HeaderValue::from_str(protocol) {
        response
            .headers_mut()
            .insert(HeaderName::from_static(RC_HEADER_PROTOCOL_VERSION), value);
    }
    response
}

fn handle_request(
    state: &mut ServerState,
    behavior: FakeServerBehavior,
    id: &JsonValue,
    method: &str,
    body: &JsonValue,
) -> Option<JsonValue> {
    body.get("id")?;
    match method {
        m if m == mcp_protocol::METHOD_SERVER_DISCOVER => {
            state.discover_count += 1;
            Some(handle_discover(behavior, id, state.discover_count, body))
        }
        "initialize" => Some(handle_initialize(behavior, id, body)),
        "tools/list" => Some(handle_tools_list(behavior, id)),
        "tools/call" => {
            state.call_count += 1;
            Some(handle_tools_call(behavior, id, body, state.call_count))
        }
        _ => Some(harn_vm::jsonrpc::error_response(
            id.clone(),
            -32601,
            &format!("Method not found: {method}"),
        )),
    }
}

fn handle_discover(
    behavior: FakeServerBehavior,
    id: &JsonValue,
    discover_count: usize,
    body: &JsonValue,
) -> JsonValue {
    match behavior {
        FakeServerBehavior::Legacy202511 => harn_vm::jsonrpc::error_response(
            id.clone(),
            -32601,
            "Method not found: server/discover",
        ),
        FakeServerBehavior::UnsupportedVersionRetry if discover_count == 1 => {
            let requested = body
                .pointer("/params/_meta")
                .and_then(JsonValue::as_object)
                .and_then(|meta| meta.get(RC_META_KEY_PROTOCOL_VERSION))
                .and_then(JsonValue::as_str)
                .unwrap_or(DRAFT_PROTOCOL_VERSION);
            mcp_protocol::unsupported_protocol_version_response(id.clone(), requested)
        }
        _ => harn_vm::jsonrpc::response(
            id.clone(),
            server_discover_result(
                json!({"tools": {}}),
                json!({"name": "fake-rc-server", "version": "0.1.0"}),
                Some("Fake RC server for compat harness."),
            ),
        ),
    }
}

fn handle_initialize(behavior: FakeServerBehavior, id: &JsonValue, body: &JsonValue) -> JsonValue {
    let requested = body
        .pointer("/params/protocolVersion")
        .and_then(JsonValue::as_str)
        .unwrap_or(PROTOCOL_VERSION);
    let target = match behavior {
        FakeServerBehavior::Legacy202511 => PROTOCOL_VERSION,
        _ => DRAFT_PROTOCOL_VERSION,
    };
    if requested != target && !matches!(behavior, FakeServerBehavior::Legacy202511) {
        return mcp_protocol::unsupported_protocol_version_response(id.clone(), requested);
    }
    harn_vm::jsonrpc::response(
        id.clone(),
        json!({
            "protocolVersion": target,
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "fake-rc-server", "version": "0.1.0"},
        }),
    )
}

fn handle_tools_list(behavior: FakeServerBehavior, id: &JsonValue) -> JsonValue {
    let tool = if behavior == FakeServerBehavior::RecursiveDefsTool {
        json!({
            "name": "tree_summarize",
            "title": "Summarize tree",
            "description": "Walk a recursive tree and return a summary.",
            "inputSchema": recursive_tree_input_schema(),
        })
    } else {
        json!({
            "name": "echo",
            "title": "Echo",
            "description": "Return the input string back to the caller.",
            "inputSchema": {
                "$schema": "https://json-schema.org/draft/2020-12/schema",
                "type": "object",
                "properties": {
                    "message": {"type": "string"}
                },
                "required": ["message"],
            },
        })
    };
    let mut result = json!({
        "resultType": "complete",
        "tools": [tool],
    });
    if matches!(
        behavior,
        FakeServerBehavior::ModernSuccess
            | FakeServerBehavior::CacheHints
            | FakeServerBehavior::RecursiveDefsTool
            | FakeServerBehavior::ServerDiscover
            | FakeServerBehavior::InputRequired
            | FakeServerBehavior::HeaderMismatch
            | FakeServerBehavior::NoSessionHttp
            | FakeServerBehavior::UnsupportedVersionRetry
    ) {
        let (ttl, scope) = if behavior == FakeServerBehavior::CacheHints {
            (300_000_u64, "public")
        } else {
            (5_000_u64, "private")
        };
        result["ttlMs"] = json!(ttl);
        result["cacheScope"] = json!(scope);
    }
    if behavior == FakeServerBehavior::Legacy202511 {
        // Legacy responses omit RC envelope fields entirely.
        result = json!({"tools": [tool]});
    }
    harn_vm::jsonrpc::response(id.clone(), result)
}

fn handle_tools_call(
    behavior: FakeServerBehavior,
    id: &JsonValue,
    body: &JsonValue,
    call_count: usize,
) -> JsonValue {
    if behavior == FakeServerBehavior::InputRequired && call_count == 1 {
        // Spec: input-required result returns an opaque `requestState`
        // plus a map of `inputRequests` keyed by client-chosen names.
        return harn_vm::jsonrpc::response(
            id.clone(),
            json!({
                "resultType": "input_required",
                "requestState": "opaque-server-state",
                "inputRequests": {
                    "approve": {
                        "method": "elicitation/create",
                        "params": {
                            "mode": "form",
                            "message": "Approve the call?",
                            "requestedSchema": {
                                "$schema": "https://json-schema.org/draft/2020-12/schema",
                                "type": "object",
                                "properties": {
                                    "approved": {"type": "boolean"}
                                },
                                "required": ["approved"],
                            }
                        }
                    }
                }
            }),
        );
    }
    let args = body
        .pointer("/params/arguments")
        .cloned()
        .unwrap_or_else(|| json!({}));
    let mut result = json!({
        "resultType": "complete",
        "content": [{"type": "text", "text": format!("ok:{}", args)}],
        "isError": false,
    });
    // The CacheHints behavior also exercises tool-call caching so the
    // supervised host primitive (#2504) has a real wire to validate
    // against. List/read hints were already exercised on `tools/list`
    // above; the tool-call hint completes the matrix.
    if behavior == FakeServerBehavior::CacheHints {
        if let Some(obj) = result.as_object_mut() {
            obj.insert("ttlMs".to_string(), json!(60_000_u64));
            obj.insert("cacheScope".to_string(), json!("public"));
        }
    }
    if behavior == FakeServerBehavior::Legacy202511 {
        result = json!({
            "content": [{"type": "text", "text": format!("ok:{}", args)}],
            "isError": false,
        });
    }
    harn_vm::jsonrpc::response(id.clone(), result)
}

// ----- stdio fake server ---------------------------------------------------

/// One stdio fake server: receives lines on `stdin_rx`, emits lines on
/// `stdout_tx`. Drop both channels to shut it down.
pub struct FakeStdioServer {
    pub stdin_tx: mpsc::UnboundedSender<String>,
    pub stdout_rx: mpsc::UnboundedReceiver<String>,
    pub join: JoinHandle<()>,
}

/// Spawn an in-process stdio fake server with the given behavior. The
/// returned channels mimic the stdin/stdout pipes a real child would
/// have, so client-side tests can drive the wire without a subprocess.
pub fn spawn_fake_stdio_server(behavior: FakeServerBehavior) -> FakeStdioServer {
    let (stdin_tx, mut stdin_rx) = mpsc::unbounded_channel::<String>();
    let (stdout_tx, stdout_rx) = mpsc::unbounded_channel::<String>();
    let state = Arc::new(Mutex::new(ServerState {
        behavior: Some(behavior),
        ..ServerState::default()
    }));
    let join = tokio::spawn(async move {
        while let Some(line) = stdin_rx.recv().await {
            let trimmed = line.trim().to_string();
            if trimmed.is_empty() {
                continue;
            }
            let Ok(body) = serde_json::from_str::<JsonValue>(&trimmed) else {
                continue;
            };
            let mut guard = state.lock().await;
            let id = body.get("id").cloned().unwrap_or(JsonValue::Null);
            let method = body
                .get("method")
                .and_then(JsonValue::as_str)
                .unwrap_or_default()
                .to_string();
            if let Some(response) = handle_request(&mut guard, behavior, &id, &method, &body) {
                let mut encoded = serde_json::to_string(&response).unwrap_or_default();
                encoded.push('\n');
                let _ = stdout_tx.send(encoded);
            }
        }
    });
    FakeStdioServer {
        stdin_tx,
        stdout_rx,
        join,
    }
}

/// Header-name constants re-exported so test files don't need to
/// depend on `harn-vm::mcp_protocol` directly for the RC routing
/// headers.
pub mod headers {
    pub const PROTOCOL: &str = super::RC_HEADER_PROTOCOL_VERSION;
    pub const METHOD: &str = super::RC_HEADER_METHOD;
    pub const NAME: &str = super::RC_HEADER_NAME;
}
