//! HTTP and SSE transport routing for MCP.
use super::auth::{
    attach_http_headers, should_stream_post_response, validate_origin,
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
    // Stable Streamable HTTP cross-checks required routing headers against the
    // JSON-RPC body before dispatch. Protocol validation failures are HTTP 400
    // while retaining their structured JSON-RPC diagnostic.
    if let Err(error_body) = validate_standard_routing_headers(&headers, &request) {
        let mut http = (StatusCode::BAD_REQUEST, Json(error_body)).into_response();
        attach_http_headers(&mut http, MCP_PROTOCOL_VERSION);
        return http;
    }
    let session = SharedSession::new();
    let auth = AuthRequest::from_http(&method, &state.options.path, body.to_vec(), &headers);
    let response_protocol = MCP_PROTOCOL_VERSION;

    match state.server.process_message(request, session, auth).await {
        ImmediateResult::Accepted => StatusCode::ACCEPTED.into_response(),
        ImmediateResult::Response(response) => {
            let mut http = if mcp_protocol::requires_http_bad_request(&response) {
                (StatusCode::BAD_REQUEST, Json(response)).into_response()
            } else if should_stream_post_response(&headers) {
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
        // The task id is the response; progress notifications still arrive on
        // the stream, and the terminal result is filed against the task rather
        // than written here, so the client collects it with `tasks/get`.
        ImmediateResult::TaskStream { immediate, job } => {
            let stream = spawn_http_stream_with_prelude(state.server.clone(), *job, immediate);
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
    spawn_http_stream_inner(server, job, None)
}

/// Like [`spawn_http_stream`], but writes `prelude` before the job starts.
///
/// A task call's `tools/call` answer has to reach the client immediately -- it
/// carries the id everything else is addressed by -- so it is queued onto the
/// stream before the work begins rather than raced against the job's own
/// notifications.
pub(super) fn spawn_http_stream_with_prelude(
    server: Arc<McpServer>,
    job: StreamJob,
    prelude: JsonValue,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    spawn_http_stream_inner(server, job, Some(prelude))
}

fn spawn_http_stream_inner(
    server: Arc<McpServer>,
    job: StreamJob,
    prelude: Option<JsonValue>,
) -> Sse<impl futures::Stream<Item = Result<Event, Infallible>>> {
    let (tx, rx) = unbounded::<JsonValue>();
    if let Some(prelude) = prelude {
        let _ = tx.unbounded_send(prelude);
    }
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
