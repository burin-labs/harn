//! Transport auth metadata and request validation helpers.
use super::*;

pub(super) fn http_auth_request(
    method: Method,
    path: &str,
    body: Vec<u8>,
    headers: &HeaderMap,
) -> AuthRequest {
    AuthRequest {
        method: method.as_str().to_string(),
        path: path.to_string(),
        body,
        headers: normalized_headers(headers),
        validated_oauth: None,
    }
}

pub(super) fn normalized_headers(headers: &HeaderMap) -> BTreeMap<String, String> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|value| (name.as_str().to_ascii_lowercase(), value.to_string()))
        })
        .collect()
}

pub(super) fn lookup_or_create_session(
    state: &HttpState,
    request: &JsonValue,
    header_session: Option<String>,
) -> Result<(String, SharedSession, bool), Box<Response>> {
    let method = request
        .get("method")
        .and_then(JsonValue::as_str)
        .unwrap_or_default();
    let mut sessions = state.sessions.lock().expect("sessions poisoned");
    if let Some(session_id) = header_session {
        if let Some(session) = sessions.get(&session_id).cloned() {
            return Ok((session_id, session, false));
        }
        return Err(Box::new(StatusCode::NOT_FOUND.into_response()));
    }
    // Modern clients are session-less: every request carries its own
    // `_meta.protocolVersion` and `Mcp-*` headers, so the server never
    // mints a sticky session id for them. Legacy clients still bootstrap
    // a session on `initialize` and must replay the assigned
    // `Mcp-Session-Id` on subsequent calls.
    if is_session_less_method(method) || is_modern_request(request) {
        let session = SharedSession::new();
        return Ok((String::new(), session, false));
    }
    if method != "initialize" {
        return Err(Box::new(StatusCode::BAD_REQUEST.into_response()));
    }
    let session_id = Uuid::now_v7().to_string();
    let session = SharedSession::new();
    sessions.insert(session_id.clone(), session.clone());
    Ok((session_id, session, true))
}

fn is_session_less_method(method: &str) -> bool {
    method == mcp_protocol::METHOD_SERVER_DISCOVER
}

fn is_modern_request(request: &JsonValue) -> bool {
    // Any request that ships `_meta.protocolVersion` is RC-shaped, even
    // if the named version is one we cannot speak. The dispatch layer
    // emits the canonical `-32004` reply in that case; we must accept
    // the request here so the JSON body actually reaches dispatch
    // instead of getting bounced as a 400.
    request
        .pointer("/params/_meta")
        .and_then(JsonValue::as_object)
        .map(|meta| meta.contains_key(mcp_protocol::RC_META_KEY_PROTOCOL_VERSION))
        .unwrap_or(false)
}

pub(super) fn attach_http_headers(
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

pub(super) fn attach_legacy_deprecation_headers(response: &mut Response) {
    response.headers_mut().insert(
        HeaderName::from_static(DEPRECATION_HEADER),
        HeaderValue::from_static("true"),
    );
}

pub(super) fn should_stream_post_response(headers: &HeaderMap) -> bool {
    accepts_media(headers, "text/event-stream") && !accepts_media(headers, "application/json")
}

pub(super) fn accepts_media(headers: &HeaderMap, media_type: &str) -> bool {
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

pub(super) fn validate_protocol_header(headers: &HeaderMap) -> Result<(), Box<Response>> {
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

/// Cross-check the RC-required `Mcp-Method` / `Mcp-Name` headers against
/// the parsed JSON-RPC body. A mismatch is a JSON-RPC `-32600` error so
/// the caller can ship it back as either an HTTP 200 with the error body
/// (RC spec) or an HTTP 400.
pub(super) fn validate_rc_routing_headers(
    headers: &HeaderMap,
    request: &JsonValue,
) -> Result<(), JsonValue> {
    let id = request.get("id").cloned().unwrap_or(JsonValue::Null);
    let method = request.get("method").and_then(JsonValue::as_str);
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    let name = method.and_then(|m| rc_name_header_value(m, &params));
    negotiate_rc_http_request(
        |key| headers.get(key).and_then(|value| value.to_str().ok()),
        method,
        name.as_deref(),
        &id,
    )
    .map(|_| ())
}

pub(super) fn validate_origin(headers: &HeaderMap) -> Result<(), Box<Response>> {
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
