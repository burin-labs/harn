//! Transport auth metadata and request validation helpers.
//!
//! Re-exported as [`super::streamable_http`] so every MCP Streamable HTTP
//! host (this adapter and the `harn mcp serve` CLI) shares one copy.
use super::*;

/// Stamp the negotiated MCP protocol version onto an HTTP response.
pub fn attach_http_headers(response: &mut Response, protocol: &str) {
    if let Ok(value) = HeaderValue::from_str(protocol) {
        response.headers_mut().insert(
            HeaderName::from_bytes(MCP_PROTOCOL_HEADER.as_bytes())
                .expect("SDK MCP protocol header is valid"),
            value,
        );
    }
}

/// Whether a POST answer should be an SSE stream: the client accepts
/// `text/event-stream` and does not also accept JSON.
pub fn should_stream_post_response(headers: &HeaderMap) -> bool {
    accepts_media(headers, "text/event-stream") && !accepts_media(headers, "application/json")
}

/// Whether the `Accept` header lists `media_type`, ignoring parameters such
/// as `q=`. A missing or unreadable header accepts nothing.
pub fn accepts_media(headers: &HeaderMap, media_type: &str) -> bool {
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

/// Cross-check the stable-required `Mcp-Method` / `Mcp-Name` headers against
/// the parsed JSON-RPC body. A mismatch is the stable MCP `-32020` error so
/// the caller can ship it back as either an HTTP 200 with the error body
/// (stable spec) or an HTTP 400.
pub(super) fn validate_standard_routing_headers(
    headers: &HeaderMap,
    request: &JsonValue,
) -> Result<(), JsonValue> {
    negotiate_routing_headers(headers, request).map(|_| ())
}

/// Negotiate the stable routing headers (`Mcp-Method`, `Mcp-Name`,
/// `MCP-Protocol-Version`) against the parsed JSON-RPC body, returning the
/// negotiated outcome or the JSON-RPC error body for a contradiction.
pub fn negotiate_routing_headers(
    headers: &HeaderMap,
    request: &JsonValue,
) -> Result<mcp_protocol::McpHttpHeaderOutcome, JsonValue> {
    let id = request.get("id").cloned().unwrap_or(JsonValue::Null);
    let method = request.get("method").and_then(JsonValue::as_str);
    let params = request.get("params").unwrap_or(&JsonValue::Null);
    let name = method.and_then(|m| standard_name_header_value(m, params));
    negotiate_http_request(
        |key| headers.get(key).and_then(|value| value.to_str().ok()),
        method,
        name.as_deref(),
        &id,
    )
}

/// Refuse (403) a browser `Origin` that is not loopback; requests without
/// an `Origin` header pass.
pub fn validate_origin(headers: &HeaderMap) -> Result<(), Box<Response>> {
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
