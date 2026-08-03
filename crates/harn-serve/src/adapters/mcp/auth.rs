//! Transport auth metadata and request validation helpers.
use super::*;

pub(super) fn attach_http_headers(response: &mut Response, protocol: &str) {
    if let Ok(value) = HeaderValue::from_str(protocol) {
        response.headers_mut().insert(
            HeaderName::from_bytes(MCP_PROTOCOL_HEADER.as_bytes())
                .expect("SDK MCP protocol header is valid"),
            value,
        );
    }
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

/// Cross-check the stable-required `Mcp-Method` / `Mcp-Name` headers against
/// the parsed JSON-RPC body. A mismatch is the stable MCP `-32020` error so
/// the caller can ship it back as either an HTTP 200 with the error body
/// (stable spec) or an HTTP 400.
pub(super) fn validate_standard_routing_headers(
    headers: &HeaderMap,
    request: &JsonValue,
) -> Result<(), JsonValue> {
    let id = request.get("id").cloned().unwrap_or(JsonValue::Null);
    let method = request.get("method").and_then(JsonValue::as_str);
    let params = request.get("params").cloned().unwrap_or_else(|| json!({}));
    let name = method.and_then(|m| standard_name_header_value(m, &params));
    negotiate_http_request(
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
