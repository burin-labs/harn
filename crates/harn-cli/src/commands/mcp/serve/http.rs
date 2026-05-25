use std::collections::{BTreeMap, HashMap};
use std::convert::Infallible;
use std::sync::{Arc, Mutex};

use axum::body::Bytes;
use axum::extract::{Query, State};
use axum::http::header::{ACCEPT, AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::channel::mpsc::{unbounded, UnboundedReceiver};
use futures::{stream, StreamExt};
use serde_json::Value as JsonValue;
use uuid::Uuid;

use harn_vm::mcp_protocol;

use crate::cli::McpServeArgs;
#[cfg(test)]
use crate::cli::OrchestratorLocalArgs;

use super::super::oauth_resource::{OAuthChallengeError, OAuthResourceServer, OAuthTokenError};
use super::types::{HttpSession, HttpState, McpOrchestratorService, RpcBridge};
use super::util::{auth_event_log, normalized_headers};
use super::watchers::{
    spawn_list_notification_forwarder, spawn_log_notification_forwarder,
    spawn_resource_notification_forwarder, spawn_task_notification_forwarder,
};
use super::{DEPRECATION_HEADER, MCP_PROTOCOL_HEADER, MCP_PROTOCOL_VERSION, MCP_SESSION_HEADER};

pub(super) async fn run_http(
    service: Arc<McpOrchestratorService>,
    args: &McpServeArgs,
) -> Result<(), String> {
    let router = http_router(
        service,
        args.path.clone(),
        args.sse_path.clone(),
        args.messages_path.clone(),
    );
    serve_http_router(router, args.bind, &args.path).await
}

#[cfg(test)]
pub(crate) fn http_router_for_local(
    local: OrchestratorLocalArgs,
    path: String,
    sse_path: String,
    messages_path: String,
) -> Result<Router, String> {
    let service = Arc::new(McpOrchestratorService::new_local(local)?);
    Ok(http_router_for_service(
        service,
        path,
        sse_path,
        messages_path,
    ))
}

pub(crate) fn http_router_for_service(
    service: Arc<McpOrchestratorService>,
    path: String,
    sse_path: String,
    messages_path: String,
) -> Router {
    http_router(service, path, sse_path, messages_path)
}

fn http_router(
    service: Arc<McpOrchestratorService>,
    path: String,
    sse_path: String,
    messages_path: String,
) -> Router {
    let rpc = RpcBridge::start(service.clone());
    let state = HttpState {
        service,
        rpc,
        sessions: Arc::new(Mutex::new(HashMap::new())),
        mcp_path: path.clone(),
        sse_path: sse_path.clone(),
        messages_path: messages_path.clone(),
    };
    Router::new()
        .route(
            "/.well-known/oauth-protected-resource",
            get(oauth_protected_resource_metadata),
        )
        .route(
            "/.well-known/oauth-protected-resource/{*path}",
            get(oauth_protected_resource_metadata),
        )
        .route(
            &path,
            post(http_post_request)
                .get(http_get_stream)
                .delete(http_delete_session),
        )
        .route(&sse_path, get(legacy_sse_stream))
        .route(&messages_path, post(legacy_sse_message))
        .with_state(state)
}

async fn serve_http_router(
    router: Router,
    bind: std::net::SocketAddr,
    path: &str,
) -> Result<(), String> {
    let listener = tokio::net::TcpListener::bind(bind)
        .await
        .map_err(|error| format!("failed to bind {bind}: {error}"))?;
    let local_addr = listener
        .local_addr()
        .map_err(|error| format!("failed to read local addr: {error}"))?;
    eprintln!("[harn] MCP HTTP listener ready on http://{local_addr}{path}");
    axum::serve(listener, router)
        .await
        .map_err(|error| format!("MCP HTTP server failed: {error}"))
}

async fn oauth_protected_resource_metadata(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Response {
    let Some(oauth) = &state.service.oauth else {
        return StatusCode::NOT_FOUND.into_response();
    };
    Json(oauth.metadata(&headers, &state.mcp_path)).into_response()
}

async fn http_post_request(
    State(state): State<HttpState>,
    method: Method,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = validate_origin(&headers) {
        return *response;
    }
    if let Err(response) = validate_protocol_header(&headers) {
        return *response;
    }

    let authenticated = match authorize_http_request(
        &state,
        method.as_str(),
        &state.mcp_path,
        &headers,
        body.as_ref(),
    )
    .await
    {
        Ok(authenticated) => authenticated,
        Err(response) => return response,
    };

    let request: JsonValue = match serde_json::from_slice(body.as_ref()) {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                format!("invalid JSON-RPC request body: {error}"),
            )
                .into_response()
        }
    };

    // Validate the RC HTTP-level headers (`Mcp-Method`, `Mcp-Name`,
    // explicit `MCP-Protocol-Version`) against the JSON-RPC body. The
    // helper returns a JSON-RPC error body when the headers contradict
    // the body so we can return the standard `-32004` /  `-32600` shapes
    // rather than an opaque HTTP 400.
    let body_method = request.get("method").and_then(JsonValue::as_str);
    let body_params = request.get("params");
    let body_name = body_method.and_then(|method| {
        mcp_protocol::rc_name_header_value(method, body_params.unwrap_or(&JsonValue::Null))
    });
    let body_id = request.get("id").cloned().unwrap_or(JsonValue::Null);
    let rc_outcome = match mcp_protocol::negotiate_rc_http_request(
        |key| headers.get(key).and_then(|value| value.to_str().ok()),
        body_method,
        body_name.as_deref(),
        &body_id,
    ) {
        Ok(outcome) => outcome,
        Err(error_response) => return Json(error_response).into_response(),
    };
    let rc_session_optional = rc_outcome.mode.is_modern();

    let header_session = headers
        .get(MCP_SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let (session_id, session, created) =
        match lookup_or_create_session(&state, &request, header_session, rc_session_optional) {
            Ok(value) => value,
            Err(response) => return response,
        };

    let mut current = session.state.lock().expect("HTTP session poisoned").clone();
    if authenticated {
        current.authenticated = true;
    }
    if rc_outcome.mode.is_modern() {
        // The RC HTTP path is stateless: the orchestrator must not lean
        // on a sticky `MCP-Session-Id` for either initialization or
        // capability negotiation. Stamping the session up-front keeps
        // the per-request dispatcher's invariants intact while letting
        // the handler return without ever advertising a session id.
        current.protocol_mode = mcp_protocol::McpProtocolMode::Modern;
        current.initialized = true;
        current.authenticated = true;
        current.protocol_version = mcp_protocol::DRAFT_PROTOCOL_VERSION.to_string();
    }
    // If the client opened a session-wide SSE (GET /mcp), wire the
    // active progress bus to it so per-request progress notifications
    // stream through the same channel as broadcast notifications.
    // Without an open SSE, progress is silently dropped (the spec
    // permits this — clients that want updates open the stream).
    let progress_sender = session
        .sse_tx
        .lock()
        .expect("HTTP session SSE sender poisoned")
        .clone();
    let (updated, response_json) = match state
        .rpc
        .call_with_progress(current, request, progress_sender)
        .await
    {
        Ok(result) => result,
        Err(error) => return (StatusCode::INTERNAL_SERVER_ERROR, error).into_response(),
    };
    *session.state.lock().expect("HTTP session poisoned") = updated;
    // Do not surface a session id to RC clients: the modern profile
    // promises stateless POSTs, and the only reason we keep an internal
    // session record is so list-change broadcasts have somewhere to
    // attach when the client also opens a GET stream.
    let session_id_to_emit = if rc_outcome.mode.is_modern() {
        None
    } else {
        created.then_some(session_id.as_str())
    };
    let negotiated_version = rc_outcome
        .protocol_version
        .as_deref()
        .unwrap_or(MCP_PROTOCOL_VERSION);
    if response_json.is_null() {
        let mut response = StatusCode::ACCEPTED.into_response();
        attach_streamable_headers(&mut response, session_id_to_emit, negotiated_version);
        return response;
    }

    let mut response = if should_stream_post_response(&headers) {
        sse_single_response(response_json).into_response()
    } else {
        Json(response_json).into_response()
    };
    attach_streamable_headers(&mut response, session_id_to_emit, negotiated_version);
    response
}

async fn http_get_stream(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    if let Err(response) = validate_origin(&headers) {
        return *response;
    }
    if let Err(response) = validate_protocol_header(&headers) {
        return *response;
    }
    if let Err(response) =
        authorize_http_request(&state, "GET", &state.mcp_path, &headers, &[]).await
    {
        return response;
    }
    if !accepts_media(&headers, "text/event-stream") {
        return StatusCode::NOT_ACCEPTABLE.into_response();
    }

    // RC clients open a GET stream without an `MCP-Session-Id`: we mint
    // a transient session record so the broadcast forwarders have a
    // place to attach. Legacy clients still must look up their session
    // by id, which preserves the existing error semantics.
    let rc_mode = headers
        .get(MCP_PROTOCOL_HEADER)
        .and_then(|value| value.to_str().ok())
        == Some(mcp_protocol::DRAFT_PROTOCOL_VERSION);
    let header_session = headers
        .get(MCP_SESSION_HEADER)
        .and_then(|value| value.to_str().ok());
    let (session, negotiated_version) = match header_session {
        Some(session_id) => {
            let Some(session) = state
                .sessions
                .lock()
                .expect("MCP sessions poisoned")
                .get(session_id)
                .cloned()
            else {
                return StatusCode::NOT_FOUND.into_response();
            };
            (session, MCP_PROTOCOL_VERSION)
        }
        None if rc_mode => {
            let session = Arc::new(HttpSession::default());
            {
                let mut guard = session.state.lock().expect("HTTP session poisoned");
                guard.initialized = true;
                guard.authenticated = true;
                guard.protocol_mode = mcp_protocol::McpProtocolMode::Modern;
                guard.protocol_version = mcp_protocol::DRAFT_PROTOCOL_VERSION.to_string();
            }
            // Keep the transient session reachable from the registry so
            // resource-subscription bookkeeping in the forwarders has a
            // stable record to mutate. The RC client never learns of an
            // id; the registry entry exists purely for the broadcasters.
            let session_id = Uuid::now_v7().to_string();
            state
                .sessions
                .lock()
                .expect("MCP sessions poisoned")
                .insert(session_id, session.clone());
            (session, mcp_protocol::DRAFT_PROTOCOL_VERSION)
        }
        None => return StatusCode::BAD_REQUEST.into_response(),
    };

    let (tx, rx) = unbounded::<JsonValue>();
    *session.sse_tx.lock().expect("SSE sender poisoned") = Some(tx.clone());
    spawn_list_notification_forwarder(state.service.clone(), tx.clone());
    spawn_resource_notification_forwarder(state.service.clone(), tx.clone(), session.clone());
    spawn_task_notification_forwarder(state.service.clone(), tx.clone(), session.clone());
    spawn_log_notification_forwarder(state.service.clone(), tx, session);
    let mut response = sse_response(rx).into_response();
    attach_streamable_headers(&mut response, None, negotiated_version);
    response
}

async fn http_delete_session(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    if let Err(response) = validate_origin(&headers) {
        return *response;
    }
    if let Err(response) = validate_protocol_header(&headers) {
        return *response;
    }
    if let Err(response) =
        authorize_http_request(&state, "DELETE", &state.mcp_path, &headers, &[]).await
    {
        return response;
    }
    let Some(session_id) = headers
        .get(MCP_SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let removed = state
        .sessions
        .lock()
        .expect("MCP sessions poisoned")
        .remove(session_id);
    let mut response = if removed.is_some() {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    };
    attach_streamable_headers(&mut response, None, MCP_PROTOCOL_VERSION);
    response
}

async fn legacy_sse_stream(State(state): State<HttpState>, headers: HeaderMap) -> Response {
    if let Err(response) = validate_origin(&headers) {
        return *response;
    }
    let authenticated =
        match authorize_http_request(&state, "GET", &state.sse_path, &headers, &[]).await {
            Ok(authenticated) => authenticated,
            Err(mut response) => {
                attach_legacy_deprecation_headers(&mut response);
                return response;
            }
        };

    if authenticated {
        eprintln!(
            "[harn] warning: legacy MCP SSE transport is deprecated; use Streamable HTTP at {}",
            state.mcp_path
        );
    }

    let session_id = Uuid::now_v7().to_string();
    let session = Arc::new(HttpSession::default());
    if authenticated {
        session
            .state
            .lock()
            .expect("legacy SSE session poisoned")
            .authenticated = true;
    }
    let (tx, rx) = unbounded::<JsonValue>();
    *session.sse_tx.lock().expect("SSE sender poisoned") = Some(tx);
    let list_tx = session
        .sse_tx
        .lock()
        .expect("SSE sender poisoned")
        .as_ref()
        .cloned();
    if let Some(list_tx) = list_tx {
        spawn_list_notification_forwarder(state.service.clone(), list_tx);
    }
    let resource_tx = session
        .sse_tx
        .lock()
        .expect("legacy SSE sender poisoned")
        .as_ref()
        .cloned();
    if let Some(resource_tx) = resource_tx {
        spawn_resource_notification_forwarder(state.service.clone(), resource_tx, session.clone());
    }
    let task_tx = session
        .sse_tx
        .lock()
        .expect("legacy SSE sender poisoned")
        .as_ref()
        .cloned();
    if let Some(task_tx) = task_tx {
        spawn_task_notification_forwarder(state.service.clone(), task_tx, session.clone());
    }
    let log_tx = session
        .sse_tx
        .lock()
        .expect("legacy SSE sender poisoned")
        .as_ref()
        .cloned();
    if let Some(log_tx) = log_tx {
        spawn_log_notification_forwarder(state.service.clone(), log_tx, session.clone());
    }
    state
        .sessions
        .lock()
        .expect("MCP sessions poisoned")
        .insert(session_id.clone(), session);
    let endpoint = format!("{}?session_id={session_id}", state.messages_path);
    let endpoint_event = Event::default().event("endpoint").data(endpoint);
    let stream = stream::once(async move { Ok::<Event, Infallible>(endpoint_event) }).chain(
        rx.map(|message| {
            Ok(Event::default()
                .id(Uuid::now_v7().to_string())
                .event("message")
                .data(serde_json::to_string(&message).unwrap_or_else(|_| "{}".to_string())))
        }),
    );
    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response();
    attach_legacy_deprecation_headers(&mut response);
    response
}

async fn legacy_sse_message(
    State(state): State<HttpState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = validate_origin(&headers) {
        return *response;
    }
    let authenticated = match authorize_http_request(
        &state,
        "POST",
        &state.messages_path,
        &headers,
        body.as_ref(),
    )
    .await
    {
        Ok(authenticated) => authenticated,
        Err(mut response) => {
            attach_legacy_deprecation_headers(&mut response);
            return response;
        }
    };
    let Some(session_id) = query.get("session_id") else {
        let mut response = (StatusCode::BAD_REQUEST, "missing session_id").into_response();
        attach_legacy_deprecation_headers(&mut response);
        return response;
    };
    let Some(session) = state
        .sessions
        .lock()
        .expect("MCP sessions poisoned")
        .get(session_id)
        .cloned()
    else {
        let mut response = (StatusCode::NOT_FOUND, "unknown session").into_response();
        attach_legacy_deprecation_headers(&mut response);
        return response;
    };
    let request: JsonValue = match serde_json::from_slice(body.as_ref()) {
        Ok(value) => value,
        Err(error) => {
            let mut response = (
                StatusCode::BAD_REQUEST,
                format!("invalid JSON-RPC request body: {error}"),
            )
                .into_response();
            attach_legacy_deprecation_headers(&mut response);
            return response;
        }
    };
    let mut current = session
        .state
        .lock()
        .expect("legacy SSE session poisoned")
        .clone();
    if authenticated {
        current.authenticated = true;
    }
    let (updated, response) = match state.rpc.call(current, request).await {
        Ok(result) => result,
        Err(error) => {
            let mut response = (StatusCode::INTERNAL_SERVER_ERROR, error).into_response();
            attach_legacy_deprecation_headers(&mut response);
            return response;
        }
    };
    *session.state.lock().expect("legacy SSE session poisoned") = updated;
    if response.is_null() {
        let mut response = StatusCode::ACCEPTED.into_response();
        attach_legacy_deprecation_headers(&mut response);
        return response;
    }
    let Some(sender) = session
        .sse_tx
        .lock()
        .expect("legacy SSE sender poisoned")
        .as_ref()
        .cloned()
    else {
        let mut response = (StatusCode::GONE, "session stream closed").into_response();
        attach_legacy_deprecation_headers(&mut response);
        return response;
    };
    if sender.unbounded_send(response).is_err() {
        let mut response = (StatusCode::GONE, "session stream closed").into_response();
        attach_legacy_deprecation_headers(&mut response);
        return response;
    }
    let mut response = StatusCode::ACCEPTED.into_response();
    attach_legacy_deprecation_headers(&mut response);
    response
}

#[allow(clippy::result_large_err)] // axum::Response is large but short-lived on the error path.
fn lookup_or_create_session(
    state: &HttpState,
    request: &JsonValue,
    header_session: Option<String>,
    session_optional: bool,
) -> Result<(String, Arc<HttpSession>, bool), Response> {
    let method = request
        .get("method")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    let mut sessions = state.sessions.lock().expect("MCP sessions poisoned");
    if let Some(session_id) = header_session {
        if let Some(session) = sessions.get(&session_id).cloned() {
            return Ok((session_id, session, false));
        }
        // Modern clients are not required to mint or reuse a session;
        // an unknown `MCP-Session-Id` quietly falls back to a fresh
        // transient session so the same client can mix legacy and RC
        // calls during the transition window.
        if !session_optional {
            return Err((StatusCode::NOT_FOUND, "unknown MCP session").into_response());
        }
    }
    if !session_optional
        && method != "initialize"
        && method != harn_vm::mcp_protocol::METHOD_SERVER_DISCOVER
    {
        return Err((StatusCode::BAD_REQUEST, "missing MCP session").into_response());
    }
    let session_id = Uuid::now_v7().to_string();
    let session = Arc::new(HttpSession::default());
    sessions.insert(session_id.clone(), session.clone());
    Ok((session_id, session, true))
}

pub(super) fn sse_single_response(
    message: JsonValue,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let prime = Event::default().id(Uuid::now_v7().to_string()).data("");
    let message = Event::default()
        .id(Uuid::now_v7().to_string())
        .event("message")
        .data(serde_json::to_string(&message).unwrap_or_else(|_| "{}".to_string()));
    Sse::new(stream::iter([
        Ok::<Event, Infallible>(prime),
        Ok::<Event, Infallible>(message),
    ]))
    .keep_alive(KeepAlive::default())
}

pub(super) fn sse_response(
    rx: UnboundedReceiver<JsonValue>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let prime = Event::default().id(Uuid::now_v7().to_string()).data("");
    let stream =
        stream::once(async move { Ok::<Event, Infallible>(prime) }).chain(rx.map(|message| {
            Ok(Event::default()
                .id(Uuid::now_v7().to_string())
                .event("message")
                .data(serde_json::to_string(&message).unwrap_or_else(|_| "{}".to_string())))
        }));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

pub(super) fn attach_streamable_headers(
    response: &mut Response,
    session_id: Option<&str>,
    protocol: &str,
) {
    if let Some(session_id) = session_id {
        if let Ok(value) = HeaderValue::from_str(session_id) {
            response
                .headers_mut()
                .insert(HeaderName::from_static(MCP_SESSION_HEADER), value);
        }
    }
    if let Ok(value) = HeaderValue::from_str(protocol) {
        response
            .headers_mut()
            .insert(HeaderName::from_static(MCP_PROTOCOL_HEADER), value);
    }
}

fn attach_legacy_deprecation_headers(response: &mut Response) {
    response.headers_mut().insert(
        HeaderName::from_static(DEPRECATION_HEADER),
        HeaderValue::from_static("true"),
    );
}

fn should_stream_post_response(headers: &HeaderMap) -> bool {
    accepts_media(headers, "text/event-stream") && !accepts_media(headers, "application/json")
}

fn accepts_media(headers: &HeaderMap, media_type: &str) -> bool {
    let Some(value) = headers.get(ACCEPT).and_then(|value| value.to_str().ok()) else {
        return false;
    };
    value.split(',').any(|entry| {
        let media = entry
            .split(';')
            .next()
            .unwrap_or_default()
            .trim()
            .to_ascii_lowercase();
        media == media_type || media == "*/*"
    })
}

fn validate_protocol_header(headers: &HeaderMap) -> Result<(), Box<Response>> {
    let Some(value) = headers
        .get(MCP_PROTOCOL_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return Ok(());
    };
    if mcp_protocol::is_supported_protocol_version(value) {
        Ok(())
    } else {
        Err(Box::new(StatusCode::BAD_REQUEST.into_response()))
    }
}

fn validate_origin(headers: &HeaderMap) -> Result<(), Box<Response>> {
    let Some(origin) = headers.get("origin").and_then(|value| value.to_str().ok()) else {
        return Ok(());
    };
    let Ok(url) = url::Url::parse(origin) else {
        return Err(Box::new(StatusCode::FORBIDDEN.into_response()));
    };
    match url.host_str() {
        Some("127.0.0.1") | Some("localhost") | Some("[::1]") | Some("::1") => Ok(()),
        _ => Err(Box::new(StatusCode::FORBIDDEN.into_response())),
    }
}

async fn authorize_http_request(
    state: &HttpState,
    method: &str,
    path: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<bool, Response> {
    if state.service.auth.has_api_keys()
        && authorize_legacy_http_request(state, method, path, headers, body)
            .await
            .is_ok()
    {
        return Ok(true);
    }

    if let Some(oauth) = &state.service.oauth {
        let Some(token) = bearer_token(headers) else {
            return Err(oauth_challenge_response(
                oauth,
                headers,
                &state.mcp_path,
                None,
                StatusCode::UNAUTHORIZED,
            ));
        };
        return match oauth.validate_bearer(token, headers, &state.mcp_path).await {
            Ok(()) => Ok(true),
            Err(OAuthTokenError::InsufficientScope) => Err(oauth_challenge_response(
                oauth,
                headers,
                &state.mcp_path,
                Some(OAuthChallengeError::InsufficientScope),
                StatusCode::FORBIDDEN,
            )),
            Err(OAuthTokenError::InvalidToken(error)) => Err(oauth_challenge_response(
                oauth,
                headers,
                &state.mcp_path,
                Some(OAuthChallengeError::InvalidToken(error)),
                StatusCode::UNAUTHORIZED,
            )),
        };
    }

    if state.service.auth.has_api_keys() {
        return Err((StatusCode::UNAUTHORIZED, "auth failed").into_response());
    }

    Ok(false)
}

async fn authorize_legacy_http_request(
    state: &HttpState,
    method: &str,
    path: &str,
    headers: &HeaderMap,
    body: &[u8],
) -> Result<(), Response> {
    let auth_log = auth_event_log(&state.service.state_dir)
        .map_err(|error| (StatusCode::INTERNAL_SERVER_ERROR, error).into_response())?;
    state
        .service
        .auth
        .authorize(
            auth_log.as_ref(),
            method,
            path,
            &normalized_headers(headers),
            body,
        )
        .await
        .map_err(|()| (StatusCode::UNAUTHORIZED, "auth failed").into_response())
}

fn oauth_challenge_response(
    oauth: &OAuthResourceServer,
    headers: &HeaderMap,
    mcp_path: &str,
    error: Option<OAuthChallengeError>,
    status: StatusCode,
) -> Response {
    let mut response = status.into_response();
    response.headers_mut().insert(
        WWW_AUTHENTICATE,
        oauth.challenge_header(headers, mcp_path, error),
    );
    response
}

fn bearer_token(headers: &HeaderMap) -> Option<&str> {
    let authorization = headers
        .get(AUTHORIZATION)
        .and_then(|value| value.to_str().ok())?;
    let (scheme, value) = authorization.split_once(' ')?;
    if scheme.eq_ignore_ascii_case("bearer") {
        let value = value.trim();
        (!value.is_empty()).then_some(value)
    } else {
        None
    }
}

pub(super) fn initialize_api_key(params: &JsonValue) -> Option<&str> {
    params
        .pointer("/capabilities/harn/apiKey")
        .and_then(JsonValue::as_str)
        .or_else(|| {
            params
                .pointer("/_meta/harn/apiKey")
                .and_then(JsonValue::as_str)
        })
        .or_else(|| {
            params
                .pointer("/capabilities/experimental/harn/apiKey")
                .and_then(JsonValue::as_str)
        })
}
