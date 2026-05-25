//! HTTP and SSE transport routing for MCP.
use super::auth::{
    accepts_media, attach_http_headers, attach_legacy_deprecation_headers, http_auth_request,
    lookup_or_create_session, should_stream_post_response, validate_origin,
    validate_protocol_header, validate_rc_routing_headers,
};
use super::schema::parse_error_response;
use super::*;

pub(super) async fn http_post_request(
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

    let request = match serde_json::from_slice::<JsonValue>(body.as_ref()) {
        Ok(value) => value,
        Err(error) => {
            return (
                StatusCode::BAD_REQUEST,
                Json(parse_error_response(&error.to_string())),
            )
                .into_response()
        }
    };
    // RC Streamable HTTP cross-checks the routing headers against the
    // JSON-RPC body so a fuzzed or spoofed peer can't smuggle a tools/call
    // past a header-only audit. A mismatch is `-32600`; we ship it as a
    // 200 with the JSON-RPC body so the client sees the diagnostic.
    if let Err(error_body) = validate_rc_routing_headers(&headers, &request) {
        let mut http = Json(error_body).into_response();
        attach_http_headers(&mut http, None, MCP_PROTOCOL_VERSION);
        return http;
    }
    let header_session = headers
        .get(MCP_SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
        .map(str::to_string);
    let (session_id, session, created) =
        match lookup_or_create_session(&state, &request, header_session) {
            Ok(value) => value,
            Err(response) => return *response,
        };
    let auth = http_auth_request(method, &state.options.path, body.to_vec(), &headers);
    let response_protocol = response_protocol_version(&headers, &request);

    match state.server.process_message(request, session, auth).await {
        ImmediateResult::Accepted => StatusCode::ACCEPTED.into_response(),
        ImmediateResult::Response(response) => {
            let mut http = if should_stream_post_response(&headers) {
                sse_single_response(response).into_response()
            } else {
                Json(response).into_response()
            };
            attach_http_headers(
                &mut http,
                created.then_some(session_id.as_str()),
                response_protocol,
            );
            http
        }
        ImmediateResult::Stream(job) => {
            let stream = spawn_http_stream(state.server.clone(), *job);
            let mut http = stream.into_response();
            attach_http_headers(
                &mut http,
                created.then_some(session_id.as_str()),
                response_protocol,
            );
            http
        }
    }
}

/// Pick the protocol version we echo back on this response. Modern
/// requests (RC `_meta` or RC header) get the draft version so the peer
/// can confirm both sides agreed on the same wire profile.
fn response_protocol_version(headers: &HeaderMap, request: &JsonValue) -> &'static str {
    if let Some(value) = headers
        .get(MCP_PROTOCOL_HEADER)
        .and_then(|value| value.to_str().ok())
    {
        if value == mcp_protocol::DRAFT_PROTOCOL_VERSION {
            return mcp_protocol::DRAFT_PROTOCOL_VERSION;
        }
    }
    if headers.contains_key(MCP_METHOD_HEADER) || headers.contains_key(MCP_NAME_HEADER) {
        return mcp_protocol::DRAFT_PROTOCOL_VERSION;
    }
    if request
        .pointer("/params/_meta")
        .and_then(JsonValue::as_object)
        .and_then(|meta| meta.get(mcp_protocol::RC_META_KEY_PROTOCOL_VERSION))
        .and_then(JsonValue::as_str)
        == Some(mcp_protocol::DRAFT_PROTOCOL_VERSION)
    {
        return mcp_protocol::DRAFT_PROTOCOL_VERSION;
    }
    MCP_PROTOCOL_VERSION
}

pub(super) async fn http_get_stream(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = validate_origin(&headers) {
        return *response;
    }
    if let Err(response) = validate_protocol_header(&headers) {
        return *response;
    }
    if !accepts_media(&headers, "text/event-stream") {
        return StatusCode::NOT_ACCEPTABLE.into_response();
    }
    let Some(session_id) = headers
        .get(MCP_SESSION_HEADER)
        .and_then(|value| value.to_str().ok())
    else {
        return StatusCode::BAD_REQUEST.into_response();
    };
    let Some(session) = state
        .sessions
        .lock()
        .expect("sessions poisoned")
        .get(session_id)
        .cloned()
    else {
        return StatusCode::NOT_FOUND.into_response();
    };
    let (tx, rx) = unbounded::<JsonValue>();
    session.set_stream_tx(Some(tx));
    let mut response = sse_response(rx).into_response();
    attach_http_headers(&mut response, None, MCP_PROTOCOL_VERSION);
    response
}

pub(super) async fn http_delete_session(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = validate_origin(&headers) {
        return *response;
    }
    if let Err(response) = validate_protocol_header(&headers) {
        return *response;
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
        .expect("sessions poisoned")
        .remove(session_id);
    let mut response = if removed.is_some() {
        StatusCode::NO_CONTENT.into_response()
    } else {
        StatusCode::NOT_FOUND.into_response()
    };
    attach_http_headers(&mut response, None, MCP_PROTOCOL_VERSION);
    response
}

pub(super) async fn legacy_sse_stream(
    State(state): State<HttpState>,
    headers: HeaderMap,
) -> Response {
    if let Err(response) = validate_origin(&headers) {
        return *response;
    }
    let session_id = Uuid::now_v7().to_string();
    let session = SharedSession::new();
    let (tx, rx) = unbounded::<JsonValue>();
    session.set_stream_tx(Some(tx));
    state
        .sessions
        .lock()
        .expect("sessions poisoned")
        .insert(session_id.clone(), session);
    let endpoint_event = Event::default().event("endpoint").data(format!(
        "{}?session_id={session_id}",
        state.options.messages_path
    ));
    let stream =
        stream::once(async move { Ok::<Event, Infallible>(endpoint_event) }).chain(sse_events(rx));
    let mut response = Sse::new(stream)
        .keep_alive(KeepAlive::default())
        .into_response();
    attach_legacy_deprecation_headers(&mut response);
    response
}

pub(super) async fn legacy_sse_message(
    State(state): State<HttpState>,
    Query(query): Query<BTreeMap<String, String>>,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    if let Err(response) = validate_origin(&headers) {
        return *response;
    }
    let Some(session_id) = query.get("session_id") else {
        let mut response = StatusCode::BAD_REQUEST.into_response();
        attach_legacy_deprecation_headers(&mut response);
        return response;
    };
    let Some(session) = state
        .sessions
        .lock()
        .expect("sessions poisoned")
        .get(session_id)
        .cloned()
    else {
        let mut response = StatusCode::NOT_FOUND.into_response();
        attach_legacy_deprecation_headers(&mut response);
        return response;
    };
    let request = match serde_json::from_slice::<JsonValue>(body.as_ref()) {
        Ok(value) => value,
        Err(error) => {
            let mut response = (
                StatusCode::BAD_REQUEST,
                Json(parse_error_response(&error.to_string())),
            )
                .into_response();
            attach_legacy_deprecation_headers(&mut response);
            return response;
        }
    };
    let auth = http_auth_request(
        Method::POST,
        &state.options.messages_path,
        body.to_vec(),
        &headers,
    );
    match state
        .server
        .process_message(request, session.clone(), auth)
        .await
    {
        ImmediateResult::Accepted => {
            let mut response = StatusCode::ACCEPTED.into_response();
            attach_legacy_deprecation_headers(&mut response);
            response
        }
        ImmediateResult::Response(response) => {
            if let Some(tx) = session.stream_tx() {
                let _ = tx.unbounded_send(response);
                let mut response = StatusCode::ACCEPTED.into_response();
                attach_legacy_deprecation_headers(&mut response);
                response
            } else {
                let mut response = StatusCode::GONE.into_response();
                attach_legacy_deprecation_headers(&mut response);
                response
            }
        }
        ImmediateResult::Stream(job) => {
            let Some(tx) = session.stream_tx() else {
                let mut response = StatusCode::GONE.into_response();
                attach_legacy_deprecation_headers(&mut response);
                return response;
            };
            tokio::spawn(async move {
                let notifier = notify_channel(move |message| {
                    let _ = tx.unbounded_send(message);
                });
                state.server.execute_streaming_job(*job, notifier).await;
            });
            let mut response = StatusCode::ACCEPTED.into_response();
            attach_legacy_deprecation_headers(&mut response);
            response
        }
    }
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

pub(super) fn spawn_http_stream(
    server: Arc<McpServer>,
    job: StreamJob,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let (tx, rx) = unbounded::<JsonValue>();
    tokio::spawn(async move {
        let notifier = notify_channel(move |message| {
            let _ = tx.unbounded_send(message);
        });
        server.execute_streaming_job(job, notifier).await;
    });
    sse_response(rx)
}

pub(super) fn sse_response(
    rx: UnboundedReceiver<JsonValue>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let prime = Event::default().id(Uuid::now_v7().to_string()).data("");
    let stream = stream::once(async move { Ok::<Event, Infallible>(prime) }).chain(sse_events(rx));
    Sse::new(stream).keep_alive(KeepAlive::default())
}

pub(super) fn sse_events(
    rx: UnboundedReceiver<JsonValue>,
) -> impl futures::Stream<Item = Result<Event, Infallible>> {
    rx.map(|message| {
        Ok(Event::default()
            .id(Uuid::now_v7().to_string())
            .event("message")
            .data(serde_json::to_string(&message).unwrap_or_else(|_| "{}".to_string())))
    })
}

pub(super) fn notify_channel<F>(notify: F) -> Arc<dyn Fn(JsonValue) + Send + Sync>
where
    F: Fn(JsonValue) + Send + Sync + 'static,
{
    Arc::new(notify)
}
