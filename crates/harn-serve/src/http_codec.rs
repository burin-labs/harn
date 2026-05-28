//! HTTP response codec for `.harn` handlers hosted on `harn-serve`.
//!
//! Authors return either a plain value (rendered as `200 OK + JSON`)
//! or a tagged `HttpResponse` envelope produced by the `http_*`
//! builtins ([`harn_vm::HttpEnvelope`]). This module bridges the two
//! into an `axum::Response`, applying:
//!
//! * status code (validated 100-599)
//! * caller-supplied headers (single or multi-value)
//! * body codec selection — JSON, streamed chunks, or Server-Sent
//!   Events
//! * a standard error envelope `{ code, message, request_id, details? }`
//!   used both for handler-declared errors (`http_error`) and for
//!   adapter-layer dispatch failures (auth, validation, execution)
//!
//! The codec is intentionally adapter-agnostic — it sees only the
//! `CallResponse.value` (the JSON-serialised handler return) plus a
//! request id. It can therefore back any axum-based HTTP surface
//! built on `harn-serve` without coupling to a particular adapter.

use std::convert::Infallible;

use axum::body::{Body, Bytes};
use axum::http::{header, HeaderMap, HeaderName, HeaderValue, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::Json;
use base64::Engine;
use futures::stream;
use harn_vm::{parse_http_envelope, HttpEnvelope, HttpHeaderValue};
use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::forbidden_data_payload;
use crate::{CallResponse, DispatchError};

/// Return value class produced by the codec — what the caller renders
/// to axum varies by body kind, so the codec exposes the discrete
/// cases rather than a single opaque `Response`.
#[derive(Debug)]
pub enum HttpCodecOutcome {
    /// JSON or empty body. Includes the `204 No Content` shape.
    Json {
        status: StatusCode,
        headers: HeaderMap,
        body: Option<Value>,
    },
    /// Streamed (buffered) body: each chunk is one frame.
    Stream {
        status: StatusCode,
        headers: HeaderMap,
        chunks: Vec<Bytes>,
    },
    /// Server-Sent Events stream.
    Sse {
        status: StatusCode,
        headers: HeaderMap,
        events: Vec<SseEventSpec>,
        retry_ms: Option<u64>,
    },
}

/// Resolved SSE event from a handler's `http_sse(events)` reply.
#[derive(Debug, Clone)]
pub struct SseEventSpec {
    pub id: Option<String>,
    pub event: Option<String>,
    pub data: String,
}

/// Default response: serialise the handler's return value as JSON
/// with a 200 status.
fn default_json_response(value: Value, request_id: &str) -> HttpCodecOutcome {
    let mut headers = HeaderMap::new();
    insert_request_id(&mut headers, request_id);
    HttpCodecOutcome::Json {
        status: StatusCode::OK,
        headers,
        body: Some(value),
    }
}

/// Render a `CallResponse` into an `axum::Response`. Untagged values
/// degrade to `200 OK + application/json`.
///
/// A `.harn` handler that returns an `http_upgrade_ws(...)` envelope
/// cannot be rendered as plain HTTP — the upgrade needs a hijacked
/// connection that the codec does not own. The hosting adapter must
/// detect [`HttpCodecOutcome::WsUpgradeRejected`] from
/// [`decode_call_response`] and route the request through
/// [`crate::ws::ws_route`] instead. To make the failure mode loud
/// rather than silent, rendering a `ws_upgrade` envelope here yields a
/// `500 Internal Server Error` with a `ws_upgrade_not_routed` error
/// code.
pub fn axum_response_from_call(response: CallResponse, request_id: &str) -> Response {
    let outcome = decode_call_response(response, request_id);
    outcome_to_response(outcome)
}

/// Render a `DispatchError` into an `axum::Response` using the
/// standard error envelope.
pub fn axum_response_from_dispatch_error(error: DispatchError, request_id: &str) -> Response {
    let (status, payload) = dispatch_error_payload(error, request_id);
    let mut response = (status, Json(payload)).into_response();
    insert_request_id(response.headers_mut(), request_id);
    response
}

/// Decode a `CallResponse` into a [`HttpCodecOutcome`]. Exposed so
/// adapters that want to inspect the outcome before rendering (e.g.
/// to add custom headers) can do so without re-parsing JSON.
pub fn decode_call_response(response: CallResponse, request_id: &str) -> HttpCodecOutcome {
    let Some(envelope) = parse_http_envelope(&response.value) else {
        return default_json_response(response.value, request_id);
    };
    envelope_to_outcome(envelope, request_id)
}

/// Return `Some(spec)` when the decoded envelope is a `ws_upgrade`
/// directive. Hosting adapters use this to short-circuit out of the
/// plain-HTTP rendering path and dispatch through
/// [`crate::ws::ws_route`] instead. Returns `None` for every other
/// envelope shape (or untagged value).
pub fn classify_ws_upgrade(response: &CallResponse) -> Option<harn_vm::WsUpgradeSpec> {
    let envelope = parse_http_envelope(&response.value)?;
    envelope.ws_upgrade
}

fn envelope_to_outcome(envelope: HttpEnvelope, request_id: &str) -> HttpCodecOutcome {
    if envelope.ws_upgrade.is_some() {
        // The hosting adapter is supposed to detect the upgrade
        // intent via `classify_ws_upgrade` and route to `ws_route`
        // before reaching the codec. Falling through here means
        // somebody asked us to render a 101 over a plain HTTP
        // response, which the WS protocol does not permit. Emit a
        // structured 500 so the misuse surfaces in the access log
        // rather than the client seeing a silent malformed reply.
        let body = json!({
            "code": "ws_upgrade_not_routed",
            "message": "handler returned an http_upgrade_ws envelope but the route is not wired to harn_serve::ws_route",
            "request_id": request_id,
        });
        let mut headers = HeaderMap::new();
        insert_request_id(&mut headers, request_id);
        return HttpCodecOutcome::Json {
            status: StatusCode::INTERNAL_SERVER_ERROR,
            headers,
            body: Some(body),
        };
    }

    let status = StatusCode::from_u16(envelope.status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR);
    let mut headers = http_headers(&envelope.headers);
    insert_request_id(&mut headers, request_id);

    match envelope.body_kind.as_str() {
        "none" => HttpCodecOutcome::Json {
            status,
            headers,
            body: None,
        },
        "stream" => {
            let chunks = body_to_chunks(envelope.body.as_ref());
            HttpCodecOutcome::Stream {
                status,
                headers,
                chunks,
            }
        }
        "sse" => {
            let events = body_to_sse(envelope.body.as_ref());
            HttpCodecOutcome::Sse {
                status,
                headers,
                events,
                retry_ms: envelope.retry_ms,
            }
        }
        // Default: JSON body, with the `is_error` flag merging the
        // standard error envelope fields when set.
        _ => {
            let body = if envelope.is_error {
                Some(error_body_with_request_id(
                    envelope.body.unwrap_or(Value::Null),
                    request_id,
                ))
            } else {
                envelope.body
            };
            HttpCodecOutcome::Json {
                status,
                headers,
                body,
            }
        }
    }
}

fn outcome_to_response(outcome: HttpCodecOutcome) -> Response {
    match outcome {
        HttpCodecOutcome::Json {
            status,
            headers,
            body,
        } => {
            let mut response = match body {
                Some(value) => (status, Json(value)).into_response(),
                None => status.into_response(),
            };
            merge_headers(response.headers_mut(), headers);
            response
        }
        HttpCodecOutcome::Stream {
            status,
            headers,
            chunks,
        } => {
            let stream = stream::iter(
                chunks
                    .into_iter()
                    .map(Ok::<Bytes, Infallible>)
                    .collect::<Vec<_>>(),
            );
            let mut response = Response::builder()
                .status(status)
                .body(Body::from_stream(stream))
                .expect("valid stream response");
            merge_headers(response.headers_mut(), headers);
            if !response.headers().contains_key(header::CONTENT_TYPE) {
                response.headers_mut().insert(
                    header::CONTENT_TYPE,
                    HeaderValue::from_static("application/octet-stream"),
                );
            }
            response
        }
        HttpCodecOutcome::Sse {
            status,
            headers,
            events,
            retry_ms,
        } => {
            let event_stream: futures::stream::Iter<std::vec::IntoIter<Result<Event, Infallible>>> =
                stream::iter(
                    events
                        .into_iter()
                        .map(|spec| Ok(build_sse_event(spec)))
                        .collect::<Vec<_>>(),
                );
            let keep_alive = retry_ms
                .map(|retry| KeepAlive::new().interval(std::time::Duration::from_millis(retry)))
                .unwrap_or_default();
            let sse = Sse::new(event_stream).keep_alive(keep_alive);
            let mut response = sse.into_response();
            // `Sse::into_response` sets status to 200 unconditionally. If the
            // handler overrode the status (e.g. 503 with a final SSE retry
            // hint), preserve it.
            if status != StatusCode::OK {
                *response.status_mut() = status;
            }
            merge_headers(response.headers_mut(), headers);
            response
        }
    }
}

fn build_sse_event(spec: SseEventSpec) -> Event {
    let mut event = Event::default().data(spec.data);
    if let Some(id) = spec.id {
        event = event.id(id);
    }
    if let Some(name) = spec.event {
        event = event.event(name);
    }
    event
}

fn http_headers(map: &std::collections::BTreeMap<String, HttpHeaderValue>) -> HeaderMap {
    let mut headers = HeaderMap::new();
    for (name, value) in map {
        let Ok(name) = HeaderName::try_from(name.as_str()) else {
            continue;
        };
        match value {
            HttpHeaderValue::Single(raw) => {
                if let Ok(header_value) = HeaderValue::from_str(raw) {
                    headers.insert(name, header_value);
                }
            }
            HttpHeaderValue::Multi(values) => {
                for raw in values {
                    if let Ok(header_value) = HeaderValue::from_str(raw) {
                        headers.append(name.clone(), header_value);
                    }
                }
            }
        }
    }
    headers
}

/// Merge caller-supplied envelope headers into the response headers
/// emitted by the underlying axum primitive. Envelope headers win over
/// defaults — `Sse`/`Json`/`Body::from_stream` each pre-set a small
/// number of headers (`content-type`, `cache-control` for SSE) which a
/// caller-supplied value should be free to override.
fn merge_headers(target: &mut HeaderMap, source: HeaderMap) {
    let mut seen_in_source: std::collections::HashSet<HeaderName> =
        std::collections::HashSet::new();
    for (name, value) in source {
        let Some(name) = name else { continue };
        if seen_in_source.insert(name.clone()) {
            // First occurrence: clear any default the underlying
            // primitive may have set, then insert the caller's value.
            target.insert(name.clone(), value);
        } else {
            // Repeats (e.g. multi-value Set-Cookie) append.
            target.append(name, value);
        }
    }
}

fn insert_request_id(headers: &mut HeaderMap, request_id: &str) {
    if headers.contains_key("x-request-id") {
        return;
    }
    if let Ok(value) = HeaderValue::from_str(request_id) {
        headers.insert(HeaderName::from_static("x-request-id"), value);
    }
}

fn body_to_chunks(body: Option<&Value>) -> Vec<Bytes> {
    let Some(Value::Array(items)) = body else {
        return Vec::new();
    };
    items.iter().filter_map(value_to_bytes).collect()
}

/// Convert a JSON value to a `Bytes` chunk:
/// - String: UTF-8 bytes
/// - `{"$bytes_b64": "..."}`: base64-decoded bytes (matches the VM's
///   tagged-bytes JSON form)
/// - Array of small ints: byte array
/// - Anything else: JSON-serialised
fn value_to_bytes(value: &Value) -> Option<Bytes> {
    match value {
        Value::String(text) => Some(Bytes::from(text.clone().into_bytes())),
        Value::Object(map) => {
            if let Some(b64) = map.get("$bytes_b64").and_then(Value::as_str) {
                return base64::engine::general_purpose::STANDARD
                    .decode(b64)
                    .ok()
                    .map(Bytes::from);
            }
            serde_json::to_vec(value).ok().map(Bytes::from)
        }
        Value::Array(values) => {
            let mut bytes = Vec::with_capacity(values.len());
            for v in values {
                let Some(n) = v.as_u64() else {
                    return serde_json::to_vec(value).ok().map(Bytes::from);
                };
                if n > 0xFF {
                    return serde_json::to_vec(value).ok().map(Bytes::from);
                }
                bytes.push(n as u8);
            }
            Some(Bytes::from(bytes))
        }
        Value::Null => None,
        other => serde_json::to_vec(other).ok().map(Bytes::from),
    }
}

fn body_to_sse(body: Option<&Value>) -> Vec<SseEventSpec> {
    let Some(Value::Array(items)) = body else {
        return Vec::new();
    };
    items
        .iter()
        .filter_map(|item| {
            let object = item.as_object()?;
            // Accept either {data: "..."} where data is a string, or
            // {data: <any>} where the codec stringifies it as JSON.
            let data = match object.get("data") {
                Some(Value::String(s)) => s.clone(),
                Some(other) => serde_json::to_string(other).ok()?,
                None => serde_json::to_string(item).ok()?,
            };
            let id = object
                .get("id")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            let event = object
                .get("event")
                .and_then(|v| v.as_str())
                .map(str::to_string);
            Some(SseEventSpec { id, event, data })
        })
        .collect()
}

fn error_body_with_request_id(body: Value, request_id: &str) -> Value {
    let mut map = body
        .as_object()
        .cloned()
        .unwrap_or_else(serde_json::Map::new);
    map.entry("request_id")
        .or_insert(Value::String(request_id.to_string()));
    Value::Object(map)
}

/// Convert a `DispatchError` to `(status, body)` for the standard
/// error envelope. Adapters can use this directly to render
/// pre-dispatch failures (auth, validation) the same way handler
/// errors render.
pub fn dispatch_error_payload(error: DispatchError, request_id: &str) -> (StatusCode, Value) {
    let (status, code, message, details) = match error {
        DispatchError::Unauthorized(message) => (
            StatusCode::UNAUTHORIZED,
            "unauthorized",
            message,
            Value::Null,
        ),
        DispatchError::Forbidden { required, granted } => {
            let payload = forbidden_data_payload(&required, &granted);
            let message = crate::error::forbidden_message(&required, &granted);
            (StatusCode::FORBIDDEN, "forbidden", message, payload)
        }
        DispatchError::Validation(message) => (
            StatusCode::BAD_REQUEST,
            "invalid_request",
            message,
            Value::Null,
        ),
        DispatchError::MissingExport(message) => {
            (StatusCode::NOT_FOUND, "not_found", message, Value::Null)
        }
        DispatchError::Cancelled(message) => (
            StatusCode::from_u16(499).unwrap_or(StatusCode::BAD_REQUEST),
            "cancelled",
            message,
            Value::Null,
        ),
        DispatchError::Execution(message) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "execution_error",
            message,
            Value::Null,
        ),
        DispatchError::Io(message) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "io_error",
            message,
            Value::Null,
        ),
        DispatchError::Cache(message) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            "cache_error",
            message,
            Value::Null,
        ),
    };
    let mut body = json!({
        "code": code,
        "message": message,
        "request_id": request_id,
    });
    if !matches!(details, Value::Null) {
        body["details"] = details;
    }
    (status, body)
}

/// Generate a fresh request id. Adapters that already track one
/// (e.g. honouring an incoming `X-Request-Id`) should pass it
/// through; for the rest, this is the default.
pub fn fresh_request_id() -> String {
    format!("req_{}", Uuid::now_v7())
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::to_bytes;
    use harn_vm::TraceId;

    fn synth_call(value: Value) -> CallResponse {
        CallResponse {
            function: "test".into(),
            value,
            printed_output: String::new(),
            trace_id: TraceId::default(),
            cached: false,
            duration_ms: 0,
        }
    }

    fn make_response(value: Value) -> Response {
        axum_response_from_call(synth_call(value), "req_test")
    }

    async fn body_text(response: Response) -> String {
        let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn untagged_value_defaults_to_200_json() {
        let response = make_response(json!({"ok": true}));
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "application/json"
        );
        assert_eq!(response.headers().get("x-request-id").unwrap(), "req_test");
        assert_eq!(body_text(response).await, r#"{"ok":true}"#);
    }

    #[tokio::test]
    async fn tagged_ok_envelope_renders_status_and_body() {
        let envelope = json!({
            "__http_response__": "v1",
            "status": 201,
            "body_kind": "json",
            "headers": {"Location": "/v1/sessions/sess_1"},
            "body": {"id": "sess_1"},
        });
        let response = make_response(envelope);
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            "/v1/sessions/sess_1"
        );
        assert_eq!(body_text(response).await, r#"{"id":"sess_1"}"#);
    }

    #[tokio::test]
    async fn no_content_envelope_omits_body() {
        let envelope = json!({
            "__http_response__": "v1",
            "status": 204,
            "body_kind": "none",
            "headers": {},
        });
        let response = make_response(envelope);
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(body_text(response).await, "");
    }

    #[tokio::test]
    async fn error_envelope_injects_request_id() {
        let envelope = json!({
            "__http_response__": "v1",
            "status": 422,
            "body_kind": "json",
            "headers": {},
            "is_error": true,
            "body": {"code": "bad_payload", "message": "boom"},
        });
        let response = make_response(envelope);
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let text = body_text(response).await;
        let parsed: Value = serde_json::from_str(&text).unwrap();
        assert_eq!(parsed["code"], "bad_payload");
        assert_eq!(parsed["message"], "boom");
        assert_eq!(parsed["request_id"], "req_test");
    }

    #[tokio::test]
    async fn stream_envelope_concatenates_chunks() {
        let envelope = json!({
            "__http_response__": "v1",
            "status": 200,
            "body_kind": "stream",
            "headers": {"Content-Type": "text/plain"},
            "body": ["alpha", "bravo", "charlie"],
        });
        let response = make_response(envelope);
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/plain"
        );
        assert_eq!(body_text(response).await, "alphabravocharlie");
    }

    #[tokio::test]
    async fn sse_envelope_emits_named_events() {
        let envelope = json!({
            "__http_response__": "v1",
            "status": 200,
            "body_kind": "sse",
            "headers": {},
            "body": [
                {"event": "ping", "data": "1"},
                {"event": "ping", "data": "2", "id": "evt_2"},
            ],
        });
        let response = make_response(envelope);
        assert_eq!(response.status(), StatusCode::OK);
        let text = body_text(response).await;
        // axum's SSE serializes `data:` last; ordering is per the spec.
        assert!(text.contains("data: 1"), "got: {text}");
        assert!(text.contains("data: 2"), "got: {text}");
        assert!(text.contains("event: ping"), "got: {text}");
        assert!(text.contains("id: evt_2"), "got: {text}");
    }

    #[tokio::test]
    async fn dispatch_error_renders_standard_envelope() {
        let response = axum_response_from_dispatch_error(
            DispatchError::Validation("missing field".into()),
            "req_xyz",
        );
        assert_eq!(response.status(), StatusCode::BAD_REQUEST);
        assert_eq!(response.headers().get("x-request-id").unwrap(), "req_xyz");
        let parsed: Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(parsed["code"], "invalid_request");
        assert_eq!(parsed["message"], "missing field");
        assert_eq!(parsed["request_id"], "req_xyz");
    }

    #[tokio::test]
    async fn dispatch_error_forbidden_includes_scope_details() {
        let response = axum_response_from_dispatch_error(
            DispatchError::Forbidden {
                required: std::iter::once("sessions:write".to_string()).collect(),
                granted: std::iter::once("sessions:read".to_string()).collect(),
            },
            "req_xyz",
        );
        assert_eq!(response.status(), StatusCode::FORBIDDEN);
        let parsed: Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(parsed["code"], "forbidden");
        assert_eq!(parsed["details"]["missing_scopes"][0], "sessions:write");
    }

    #[tokio::test]
    async fn stream_decodes_base64_tagged_bytes() {
        let envelope = json!({
            "__http_response__": "v1",
            "status": 200,
            "body_kind": "stream",
            "headers": {"Content-Type": "application/octet-stream"},
            "body": [{"$bytes_b64": "aGVsbG8="}],
        });
        let response = make_response(envelope);
        assert_eq!(body_text(response).await, "hello");
    }

    // --- End-to-end: .harn handler -> DispatchCore -> codec ----------

    use crate::{CallArguments, CallRequest, DispatchCore, DispatchCoreConfig};
    use std::collections::BTreeMap;
    use tempfile::TempDir;

    async fn dispatch_value(script: &str, function: &str) -> Result<Value, DispatchError> {
        let dir = TempDir::new().expect("tempdir");
        let path = dir.path().join("handler.harn");
        std::fs::write(&path, script).expect("write script");
        let core = DispatchCore::new(DispatchCoreConfig::for_script(&path))?;
        let request = CallRequest {
            adapter: "test".into(),
            function: function.into(),
            arguments: CallArguments::Positional(Vec::new()),
            auth: Default::default(),
            caller: "test".into(),
            replay_key: Some(format!("e2e-{function}")),
            trace_id: None,
            parent_span_id: None,
            metadata: BTreeMap::new(),
            cancel_token: None,
            agent_session_id: None,
            progress: None,
            tenant_id: None,
        };
        core.dispatch(request).await.map(|response| response.value)
    }

    #[tokio::test]
    async fn end_to_end_http_ok_handler() {
        let value = dispatch_value(
            r#"
pub fn handler() -> dict {
  return http_ok({greeting: "hi"})
}
"#,
            "handler",
        )
        .await
        .expect("dispatch");
        let response = axum_response_from_call(synth_call(value), "req_e2e");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_text(response).await, r#"{"greeting":"hi"}"#);
    }

    #[tokio::test]
    async fn end_to_end_http_created_with_location() {
        let value = dispatch_value(
            r#"
pub fn handler() -> dict {
  return http_created({id: "sess_42"}, "/v1/sessions/sess_42")
}
"#,
            "handler",
        )
        .await
        .expect("dispatch");
        let response = axum_response_from_call(synth_call(value), "req_e2e");
        assert_eq!(response.status(), StatusCode::CREATED);
        assert_eq!(
            response.headers().get(header::LOCATION).unwrap(),
            "/v1/sessions/sess_42"
        );
    }

    #[tokio::test]
    async fn end_to_end_http_no_content() {
        let value = dispatch_value(
            r"
pub fn handler() -> dict {
  return http_no_content()
}
",
            "handler",
        )
        .await
        .expect("dispatch");
        let response = axum_response_from_call(synth_call(value), "req_e2e");
        assert_eq!(response.status(), StatusCode::NO_CONTENT);
        assert_eq!(body_text(response).await, "");
    }

    #[tokio::test]
    async fn end_to_end_http_error_envelope() {
        let value = dispatch_value(
            r#"
pub fn handler() -> dict {
  return http_error(422, "invalid_input", "field missing", {field: "name"})
}
"#,
            "handler",
        )
        .await
        .expect("dispatch");
        let response = axum_response_from_call(synth_call(value), "req_e2e");
        assert_eq!(response.status(), StatusCode::UNPROCESSABLE_ENTITY);
        let parsed: Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(parsed["code"], "invalid_input");
        assert_eq!(parsed["message"], "field missing");
        assert_eq!(parsed["request_id"], "req_e2e");
        assert_eq!(parsed["details"]["field"], "name");
    }

    #[tokio::test]
    async fn end_to_end_http_stream_from_list() {
        let value = dispatch_value(
            r#"
pub fn handler() -> dict {
  return http_stream(["chunk1\n", "chunk2\n"], "text/plain")
}
"#,
            "handler",
        )
        .await
        .expect("dispatch");
        let response = axum_response_from_call(synth_call(value), "req_e2e");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(
            response.headers().get(header::CONTENT_TYPE).unwrap(),
            "text/plain"
        );
        assert_eq!(body_text(response).await, "chunk1\nchunk2\n");
    }

    #[tokio::test]
    async fn end_to_end_http_sse_from_list() {
        let value = dispatch_value(
            r#"
pub fn handler() -> dict {
  let events = [
    {event: "ping", data: "1"},
    {event: "ping", data: "2", id: "evt_2"},
  ]
  return http_sse(events, 1500)
}
"#,
            "handler",
        )
        .await
        .expect("dispatch");
        let response = axum_response_from_call(synth_call(value), "req_e2e");
        assert_eq!(response.status(), StatusCode::OK);
        let text = body_text(response).await;
        assert!(text.contains("event: ping"), "got: {text}");
        assert!(text.contains("data: 1"), "got: {text}");
        assert!(text.contains("data: 2"), "got: {text}");
        assert!(text.contains("id: evt_2"), "got: {text}");
    }

    #[tokio::test]
    async fn end_to_end_http_stream_from_channel() {
        // Drives the full `http_stream(channel)` path: the handler
        // produces a channel, fills it, closes it, and returns
        // `http_stream(chan)` — the builtin drains the channel before
        // returning, so the codec sees a list of chunks.
        let value = dispatch_value(
            r#"
pub fn handler() -> dict {
  let chan = channel("body", 8)
  send(chan, "first ")
  send(chan, "second")
  close_channel(chan)
  return http_stream(chan, "text/plain")
}
"#,
            "handler",
        )
        .await
        .expect("dispatch");
        let response = axum_response_from_call(synth_call(value), "req_e2e");
        assert_eq!(response.status(), StatusCode::OK);
        assert_eq!(body_text(response).await, "first second");
    }

    #[tokio::test]
    async fn end_to_end_low_level_http_reply_with_headers() {
        let value = dispatch_value(
            r#"
pub fn handler() -> dict {
  return http_reply(202, {accepted: true}, {"X-Job-Id": "job_42"})
}
"#,
            "handler",
        )
        .await
        .expect("dispatch");
        let response = axum_response_from_call(synth_call(value), "req_e2e");
        assert_eq!(response.status(), StatusCode::ACCEPTED);
        assert_eq!(response.headers().get("x-job-id").unwrap(), "job_42");
    }

    #[tokio::test]
    async fn ws_upgrade_envelope_yields_structured_500_when_rendered_as_plain_http() {
        // A handler that returns http_upgrade_ws but the route was not
        // wired through `ws_route` would otherwise emit a 101 over a
        // non-hijacked HTTP connection — silently broken. The codec
        // must instead surface the misuse with a structured error.
        let envelope = json!({
            "__http_response__": "v1",
            "status": 101,
            "body_kind": "none",
            "headers": {"Upgrade": "websocket", "Connection": "Upgrade"},
            "ws_upgrade": {
                "subprotocol": "v1.harn",
                "offered": ["v1.harn"],
            },
        });
        let response = make_response(envelope);
        assert_eq!(response.status(), StatusCode::INTERNAL_SERVER_ERROR);
        let body: Value = serde_json::from_str(&body_text(response).await).unwrap();
        assert_eq!(body["code"], "ws_upgrade_not_routed");
        assert_eq!(body["request_id"], "req_test");
    }

    #[tokio::test]
    async fn classify_ws_upgrade_routes_envelopes_through_ws() {
        let envelope = json!({
            "__http_response__": "v1",
            "status": 101,
            "body_kind": "none",
            "headers": {},
            "ws_upgrade": {
                "subprotocol": "v1.harn",
                "offered": ["v1.harn", "v2.harn"],
            },
        });
        let spec = classify_ws_upgrade(&synth_call(envelope)).expect("upgrade spec");
        assert_eq!(spec.subprotocol.as_deref(), Some("v1.harn"));
        assert_eq!(spec.offered, vec!["v1.harn", "v2.harn"]);

        // Plain envelopes route through the codec as usual.
        let plain = json!({
            "__http_response__": "v1",
            "status": 200,
            "body_kind": "json",
            "headers": {},
            "body": {"ok": true},
        });
        assert!(classify_ws_upgrade(&synth_call(plain)).is_none());

        // Untagged values produce None as well.
        assert!(classify_ws_upgrade(&synth_call(json!({"ok": true}))).is_none());
    }
}
