//! HTTP and SSE transport routing for MCP.
use super::auth::{
    attach_http_headers, should_stream_post_response, validate_origin, validate_protocol_header,
    validate_standard_routing_headers,
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
    // stable Streamable HTTP cross-checks the routing headers against the
    // JSON-RPC body so a fuzzed or spoofed peer can't smuggle a tools/call
    // past a header-only audit. A mismatch is `-32020`; we ship it as a
    // 200 with the JSON-RPC body so the client sees the diagnostic.
    if let Err(error_body) = validate_standard_routing_headers(&headers, &request) {
        let mut http = Json(error_body).into_response();
        attach_http_headers(&mut http, MCP_PROTOCOL_VERSION);
        return http;
    }
    let session = SharedSession::new();
    let auth = AuthRequest::from_http(&method, &state.options.path, body.to_vec(), &headers);
    let response_protocol = MCP_PROTOCOL_VERSION;

    match state.server.process_message(request, session, auth).await {
        ImmediateResult::Accepted => StatusCode::ACCEPTED.into_response(),
        ImmediateResult::Response(response) => {
            let mut http = if should_stream_post_response(&headers) {
                sse_single_response(response).into_response()
            } else {
                Json(response).into_response()
            };
            attach_http_headers(&mut http, response_protocol);
            http
        }
        ImmediateResult::Stream(job) => {
            let stream = spawn_http_stream(state.server.clone(), *job);
            let mut http = stream.into_response();
            attach_http_headers(&mut http, response_protocol);
            http
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
