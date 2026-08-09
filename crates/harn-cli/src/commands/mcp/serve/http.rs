use std::convert::Infallible;
use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::State;
use axum::http::header::{ACCEPT, AUTHORIZATION, WWW_AUTHENTICATE};
use axum::http::{HeaderMap, HeaderName, HeaderValue, Method, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures::stream;
use serde_json::Value as JsonValue;
use uuid::Uuid;

use harn_vm::mcp_json_discovery::{McpJsonDescriptor, WELL_KNOWN_MCP_JSON_PATH};
use harn_vm::mcp_protocol;

use crate::cli::McpServeArgs;
#[cfg(test)]
use crate::cli::OrchestratorLocalArgs;

use super::super::oauth_resource::{
    normalize_path, request_origin, OAuthChallengeError, OAuthResourceServer, OAuthTokenError,
};
use super::types::{HttpState, McpOrchestratorService, RpcBridge};
use super::util::{auth_event_log, normalized_headers};
use super::{MCP_PROTOCOL_HEADER, MCP_PROTOCOL_VERSION};

pub(super) async fn run_http(
    service: Arc<McpOrchestratorService>,
    args: &McpServeArgs,
) -> Result<(), String> {
    let router = http_router(service, args.path.clone());
    serve_http_router(router, args.bind, &args.path).await
}

#[cfg(test)]
pub(crate) fn http_router_for_local(
    local: OrchestratorLocalArgs,
    path: String,
) -> Result<Router, String> {
    let service = Arc::new(McpOrchestratorService::new_local(local)?);
    Ok(http_router_for_service(service, path))
}

pub(crate) fn http_router_for_service(
    service: Arc<McpOrchestratorService>,
    path: String,
) -> Router {
    http_router(service, path)
}

fn http_router(service: Arc<McpOrchestratorService>, path: String) -> Router {
    let rpc = RpcBridge::start(service.clone());
    let state = HttpState {
        service,
        rpc,
        mcp_path: path.clone(),
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
        .route(WELL_KNOWN_MCP_JSON_PATH, get(mcp_json_discovery_metadata))
        .route(&path, post(http_post_request))
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

async fn mcp_json_discovery_metadata(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Response {
    Json(McpJsonDescriptor {
        name: "Harn MCP".to_string(),
        description: "Harn orchestrator MCP server.".to_string(),
        icon: Some("https://harnlang.com/favicon.svg".to_string()),
        endpoint: format!(
            "{}{}",
            request_origin(&headers),
            normalize_path(&state.mcp_path)
        ),
    })
    .into_response()
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

    // Validate the stable HTTP-level headers (`Mcp-Method`, `Mcp-Name`,
    // explicit `MCP-Protocol-Version`) against the JSON-RPC body. The
    // helper returns a JSON-RPC error body when the headers contradict
    // the body so we can return the standard `-32022` / `-32020` shapes
    // rather than an opaque HTTP 400.
    let body_method = request.get("method").and_then(JsonValue::as_str);
    let body_params = request.get("params");
    let body_name = body_method.and_then(|method| {
        mcp_protocol::standard_name_header_value(method, body_params.unwrap_or(&JsonValue::Null))
    });
    let body_id = request.get("id").cloned().unwrap_or(JsonValue::Null);
    let protocol_outcome = match mcp_protocol::negotiate_http_request(
        |key| headers.get(key).and_then(|value| value.to_str().ok()),
        body_method,
        body_name.as_deref(),
        &body_id,
    ) {
        Ok(outcome) => outcome,
        Err(error_response) => return Json(error_response).into_response(),
    };
    let mut current = super::types::ConnectionState::default();
    if authenticated {
        current.authenticated = true;
    }
    let (_, response_json) = match state.rpc.call(current, request).await {
        Ok(result) => result,
        Err(error) => return (StatusCode::INTERNAL_SERVER_ERROR, error).into_response(),
    };
    let negotiated_version = protocol_outcome
        .protocol_version
        .as_deref()
        .unwrap_or(MCP_PROTOCOL_VERSION);
    if response_json.is_null() {
        let mut response = StatusCode::ACCEPTED.into_response();
        attach_streamable_headers(&mut response, negotiated_version);
        return response;
    }

    let mut response = if should_stream_post_response(&headers) {
        sse_single_response(response_json).into_response()
    } else {
        Json(response_json).into_response()
    };
    attach_streamable_headers(&mut response, negotiated_version);
    response
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

pub(super) fn attach_streamable_headers(response: &mut Response, protocol: &str) {
    if let Ok(value) = HeaderValue::from_str(protocol) {
        response.headers_mut().insert(
            HeaderName::from_bytes(MCP_PROTOCOL_HEADER.as_bytes())
                .expect("SDK MCP protocol header is valid"),
            value,
        );
    }
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
    if mcp_protocol::is_request_metadata_protocol_version(value) {
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
        && authorize_configured_api_key(state, method, path, headers, body)
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

async fn authorize_configured_api_key(
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
