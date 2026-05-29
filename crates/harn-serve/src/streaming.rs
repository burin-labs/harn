//! Streaming request-body primitives for `harn-serve` adapters.
//!
//! [`harn_vm::stdlib::multipart::multipart_parse`] buffers the entire
//! request body into memory before yielding fields. That's fine for the
//! small forms (1-100 KB) the cloud gateway sees on the auth + admin
//! surfaces, but it OOMs on the file-upload routes: a 200 MB part
//! allocates a 200 MB `Vec<u8>` *plus* a 200 MB hop into the parsed-out
//! field value.
//!
//! This module exposes the same building blocks axum itself uses —
//! [`axum::extract::Multipart`] for per-part streaming, and
//! [`axum::body::Body::into_data_stream`] for raw chunked-body reads —
//! adapted into channel-shaped APIs so a future `.harn` HTTP handler
//! host can pump them through [`harn_vm::value::VmChannelHandle`]
//! exactly like the LLM streaming builtins (`crates/harn-vm/src/llm/
//! stream_builtins.rs`) do today.
//!
//! Two primitives:
//!
//! * [`MultipartStream`] — wraps `axum::extract::Multipart`. The outer
//!   channel yields one [`MultipartField`] per form field; each field
//!   carries its own *inner* bytes channel that streams the field body
//!   in `Bytes` chunks. The producer is single-threaded and walks
//!   fields sequentially, so the consumer **must** drain field N's
//!   bytes channel before reading field N+1 — otherwise the producer
//!   blocks on the inner send and the outer channel stalls.
//! * [`RequestBodyChannel`] — wraps `Body::into_data_stream()`. The
//!   channel yields `Bytes` chunks as they arrive on the wire. Use this
//!   for `Transfer-Encoding: chunked` uploads where the handler wants
//!   to stream straight into a hasher / disk / forwarded request
//!   without materialising the body.
//!
//! Both primitives are reachable from Rust today via the test-only
//! routes wired in `crates/harn-serve/tests/streaming_conformance.rs`.
//! The `.harn` channel bridge — i.e. exposing these as
//! `http.multipart(req) -> Stream<Part>` and `req.body_channel() ->
//! Channel<bytes>` builtins — belongs in the future `.harn` HTTP
//! handler host (the same place [`crate::ws::WsSession`] will land
//! its channel bridge per the `#1870` plan). This module's docs and
//! the conformance test stand in for that bridge until the host
//! lands; the producer task shape is already what the bridge will
//! drive.

use std::sync::Arc;

use axum::body::Body;
use axum::extract::Multipart;
use bytes::Bytes;
use futures::StreamExt;
use tokio::sync::mpsc;

/// Default buffer depth for the outer multipart channel (the one that
/// hands out [`MultipartField`] records). Two slots is enough — at any
/// moment the producer is either parsing the next field header or
/// streaming the current field's body, so a deep buffer just delays
/// backpressure without enabling more work in flight.
pub const DEFAULT_MULTIPART_OUTER_CAPACITY: usize = 2;

/// Default buffer depth for a per-field inner bytes channel. Each slot
/// holds whatever chunk `multer` decided to emit (typically 8 KB), so
/// 16 slots cap the per-field in-flight buffer at ~128 KB — small
/// enough that a thousand concurrent uploads stay under a few hundred
/// megabytes even before route-level limits kick in.
pub const DEFAULT_FIELD_BYTES_CAPACITY: usize = 16;

/// Default buffer depth for [`RequestBodyChannel`]. Same reasoning as
/// the per-field channel: each slot holds one wire-level chunk, and 16
/// slots is enough to keep the producer ahead of a moderate consumer
/// without pinning megabytes per connection.
pub const DEFAULT_BODY_CHANNEL_CAPACITY: usize = 16;

/// Hard cap on per-field bytes. When a field's accumulated body would
/// exceed this, the producer emits [`StreamError::FieldTooLarge`] on the
/// inner channel and drops the rest of the field. The outer channel
/// continues with the next field — the limit is per-field, not
/// per-request, so a malicious payload can't slip a multi-gigabyte
/// chunk through.
///
/// Defaults to 256 MiB. Set this via [`MultipartStreamConfig::max_field_bytes`]
/// when wiring the producer for a specific route.
pub const DEFAULT_MAX_FIELD_BYTES: u64 = 256 * 1024 * 1024;

/// Per-route configuration for [`MultipartStream::start`].
#[derive(Clone, Debug)]
pub struct MultipartStreamConfig {
    /// Outer-channel buffer depth — controls how many fields can be
    /// queued ahead of the consumer. Defaults to
    /// [`DEFAULT_MULTIPART_OUTER_CAPACITY`].
    pub outer_capacity: usize,
    /// Per-field inner-channel buffer depth — controls how many wire
    /// chunks can be queued per field. Defaults to
    /// [`DEFAULT_FIELD_BYTES_CAPACITY`].
    pub field_bytes_capacity: usize,
    /// Hard per-field byte cap. Defaults to
    /// [`DEFAULT_MAX_FIELD_BYTES`].
    pub max_field_bytes: u64,
}

impl Default for MultipartStreamConfig {
    fn default() -> Self {
        Self {
            outer_capacity: DEFAULT_MULTIPART_OUTER_CAPACITY,
            field_bytes_capacity: DEFAULT_FIELD_BYTES_CAPACITY,
            max_field_bytes: DEFAULT_MAX_FIELD_BYTES,
        }
    }
}

/// Configuration for [`RequestBodyChannel::start`].
#[derive(Clone, Debug)]
pub struct BodyChannelConfig {
    /// Buffer depth for the body channel. Defaults to
    /// [`DEFAULT_BODY_CHANNEL_CAPACITY`].
    pub capacity: usize,
}

impl Default for BodyChannelConfig {
    fn default() -> Self {
        Self {
            capacity: DEFAULT_BODY_CHANNEL_CAPACITY,
        }
    }
}

/// Per-field metadata + a bytes channel for the field body.
///
/// Records of this shape are what a future `http.multipart(req)`
/// `.harn` builtin will yield onto its channel. The handler reads
/// metadata from the named fields and drains [`MultipartField::bytes`]
/// to consume the body.
#[derive(Debug)]
pub struct MultipartField {
    /// The `name=` parameter from the part's `Content-Disposition`.
    /// Required by RFC 7578; the producer surfaces a
    /// [`StreamError::MissingFieldName`] before reaching the consumer
    /// when it's absent.
    pub name: String,
    /// The `filename=` parameter from `Content-Disposition`, when
    /// present. Absent for ordinary text fields.
    pub filename: Option<String>,
    /// The part's `Content-Type` header, when present. The producer
    /// passes this through verbatim; it does not validate or default.
    pub content_type: Option<String>,
    /// Channel yielding the field body in wire-arrival chunks. Drops
    /// on `Ok(None)` when the field is fully consumed; an
    /// [`Err`](StreamError) terminates the field and is followed by
    /// the channel closing.
    pub bytes: mpsc::Receiver<Result<Bytes, StreamError>>,
}

/// A streaming multipart parser handed to a route handler.
///
/// Build one with [`MultipartStream::start`]; consume by repeatedly
/// `await`-ing [`MultipartStream::next_field`]. Returning `Ok(None)`
/// signals the body is fully consumed; an [`Err`](StreamError)
/// terminates the stream and is followed by the channel closing.
pub struct MultipartStream {
    receiver: mpsc::Receiver<Result<MultipartField, StreamError>>,
}

impl MultipartStream {
    /// Start the producer task and return a handle the route handler
    /// can drain. The task is spawned via `tokio::spawn`; cancelling
    /// it from the consumer side is implicit — dropping the handle
    /// closes the outer channel, which the producer notices on its
    /// next send and uses to abort the parse.
    pub fn start(multipart: Multipart, config: MultipartStreamConfig) -> Self {
        let (outer_tx, outer_rx) = mpsc::channel(config.outer_capacity.max(1));
        tokio::spawn(drive_multipart(multipart, outer_tx, config));
        Self { receiver: outer_rx }
    }

    /// Receive the next field. Returns `Ok(None)` when the body is
    /// fully consumed.
    pub async fn next_field(&mut self) -> Result<Option<MultipartField>, StreamError> {
        match self.receiver.recv().await {
            Some(Ok(field)) => Ok(Some(field)),
            Some(Err(error)) => Err(error),
            None => Ok(None),
        }
    }
}

/// A streaming raw request body handed to a route handler.
///
/// Build one with [`RequestBodyChannel::start`]; drain via
/// [`RequestBodyChannel::recv`]. The producer drives
/// `Body::into_data_stream()` and pushes each emitted [`Bytes`] chunk
/// onto the channel. Trailers (uncommon in practice but legal on
/// `Transfer-Encoding: chunked`) are discarded — they would need a
/// separate side-channel to surface, which no current adapter needs.
pub struct RequestBodyChannel {
    receiver: mpsc::Receiver<Result<Bytes, StreamError>>,
}

impl RequestBodyChannel {
    /// Start the producer task and return a handle the route handler
    /// can drain. Dropping the handle closes the channel, which aborts
    /// the producer on its next send (analogous to
    /// [`MultipartStream::start`]).
    pub fn start(body: Body, config: BodyChannelConfig) -> Self {
        let (tx, rx) = mpsc::channel(config.capacity.max(1));
        tokio::spawn(drive_body(body, tx));
        Self { receiver: rx }
    }

    /// Receive the next chunk. Returns `Ok(None)` when the body is
    /// fully consumed.
    pub async fn recv(&mut self) -> Result<Option<Bytes>, StreamError> {
        match self.receiver.recv().await {
            Some(Ok(bytes)) => Ok(Some(bytes)),
            Some(Err(error)) => Err(error),
            None => Ok(None),
        }
    }
}

/// Errors surfaced on either the outer multipart channel or any inner
/// per-field channel.
#[derive(Debug, Clone)]
pub enum StreamError {
    /// The multipart parser hit a wire-level error. Wraps the message
    /// reported by `multer`. Most commonly: malformed boundary,
    /// truncated body, or `Content-Type` missing the boundary.
    Multipart(String),
    /// The body stream (chunked or otherwise) hit a transport error.
    /// Wraps the message reported by hyper / axum. Most commonly: peer
    /// reset, timeout, or a length-prefixed body that ran short.
    Body(String),
    /// A multipart part was missing the required `name=` parameter on
    /// its `Content-Disposition` header. RFC 7578 requires it; we
    /// surface this as a structured error rather than skipping the
    /// part silently.
    MissingFieldName,
    /// A field's accumulated body exceeded
    /// [`MultipartStreamConfig::max_field_bytes`]. The producer emits
    /// this on the inner channel, then drops the rest of the field.
    /// The outer channel continues with the next field.
    FieldTooLarge {
        /// The field's `name=` value, when it had one.
        field: Option<String>,
        /// The configured limit that was exceeded.
        limit: u64,
    },
}

impl std::fmt::Display for StreamError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Multipart(message) => write!(f, "multipart parse error: {message}"),
            Self::Body(message) => write!(f, "body stream error: {message}"),
            Self::MissingFieldName => write!(
                f,
                "multipart part is missing the required name= Content-Disposition param"
            ),
            Self::FieldTooLarge { field, limit } => match field {
                Some(name) => write!(
                    f,
                    "multipart field `{name}` exceeded max_field_bytes ({limit})"
                ),
                None => write!(f, "multipart field exceeded max_field_bytes ({limit})"),
            },
        }
    }
}

impl std::error::Error for StreamError {}

async fn drive_multipart(
    mut multipart: Multipart,
    outer_tx: mpsc::Sender<Result<MultipartField, StreamError>>,
    config: MultipartStreamConfig,
) {
    let inner_capacity = config.field_bytes_capacity.max(1);
    let max_field_bytes = config.max_field_bytes;
    loop {
        let next = match multipart.next_field().await {
            Ok(Some(field)) => field,
            Ok(None) => return,
            Err(error) => {
                let _ = outer_tx
                    .send(Err(StreamError::Multipart(error.to_string())))
                    .await;
                return;
            }
        };

        // Snapshot the field metadata before we move the field into
        // the byte-pumping loop — `next_field` reuses the underlying
        // parser state, so the field handle is shape-bound here.
        let name = match next.name() {
            Some(value) => value.to_string(),
            None => {
                if outer_tx
                    .send(Err(StreamError::MissingFieldName))
                    .await
                    .is_err()
                {
                    return;
                }
                continue;
            }
        };
        let filename = next.file_name().map(str::to_string);
        let content_type = next.content_type().map(str::to_string);

        let (inner_tx, inner_rx) = mpsc::channel::<Result<Bytes, StreamError>>(inner_capacity);
        let field = MultipartField {
            name: name.clone(),
            filename,
            content_type,
            bytes: inner_rx,
        };
        if outer_tx.send(Ok(field)).await.is_err() {
            return;
        }

        let inner_tx = Arc::new(inner_tx);
        let outcome = pump_field_bytes(next, inner_tx.clone(), max_field_bytes, &name).await;
        // Drop our reference so the consumer side sees a clean close
        // once the producer is done with this field.
        drop(inner_tx);

        match outcome {
            FieldOutcome::Complete => continue,
            FieldOutcome::ConsumerDropped => return,
            FieldOutcome::ParserFailed(message) => {
                let _ = outer_tx.send(Err(StreamError::Multipart(message))).await;
                return;
            }
        }
    }
}

enum FieldOutcome {
    /// Field fully drained — keep parsing.
    Complete,
    /// Consumer dropped the inner channel — stop early but the outer
    /// channel may still be open, so the producer cooperatively
    /// returns rather than poisoning sibling fields. (axum's parser
    /// can't resume mid-field, so we close the whole stream.)
    ConsumerDropped,
    /// `multer` reported a parse error mid-field. The producer
    /// terminates the stream after surfacing this on the outer
    /// channel.
    ParserFailed(String),
}

async fn pump_field_bytes(
    mut field: axum::extract::multipart::Field<'_>,
    inner_tx: Arc<mpsc::Sender<Result<Bytes, StreamError>>>,
    max_field_bytes: u64,
    field_name: &str,
) -> FieldOutcome {
    let mut bytes_so_far: u64 = 0;
    loop {
        match field.chunk().await {
            Ok(Some(chunk)) => {
                let len = chunk.len() as u64;
                if bytes_so_far.saturating_add(len) > max_field_bytes {
                    let _ = inner_tx
                        .send(Err(StreamError::FieldTooLarge {
                            field: Some(field_name.to_string()),
                            limit: max_field_bytes,
                        }))
                        .await;
                    // Drain the rest of the field so the multer parser
                    // can advance to the next one. If the consumer
                    // dropped, the next send below will detect it.
                    while let Ok(Some(_)) = field.chunk().await {}
                    return FieldOutcome::Complete;
                }
                bytes_so_far += len;
                if inner_tx.send(Ok(chunk)).await.is_err() {
                    return FieldOutcome::ConsumerDropped;
                }
            }
            Ok(None) => return FieldOutcome::Complete,
            Err(error) => return FieldOutcome::ParserFailed(error.to_string()),
        }
    }
}

async fn drive_body(body: Body, tx: mpsc::Sender<Result<Bytes, StreamError>>) {
    let mut stream = body.into_data_stream();
    while let Some(next) = stream.next().await {
        let send_result = match next {
            Ok(chunk) => tx.send(Ok(chunk)).await,
            Err(error) => tx.send(Err(StreamError::Body(error.to_string()))).await,
        };
        if send_result.is_err() {
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use axum::body::Body;
    use axum::extract::Multipart;
    use axum::http::{header, Method, Request, StatusCode};
    use axum::response::{IntoResponse, Response};
    use axum::routing::post;
    use axum::Router;
    use tower::ServiceExt;

    async fn echo_multipart_handler(multipart: Multipart) -> Response {
        let mut stream = MultipartStream::start(multipart, MultipartStreamConfig::default());
        let mut summary = String::new();
        while let Some(mut field) = match stream.next_field().await {
            Ok(field) => field,
            Err(error) => return (StatusCode::BAD_REQUEST, error.to_string()).into_response(),
        } {
            let mut total = 0u64;
            while let Some(chunk_result) = field.bytes.recv().await {
                match chunk_result {
                    Ok(chunk) => total += chunk.len() as u64,
                    Err(error) => {
                        return (StatusCode::PAYLOAD_TOO_LARGE, error.to_string()).into_response();
                    }
                }
            }
            summary.push_str(&format!("{}:{}\n", field.name, total));
        }
        summary.into_response()
    }

    async fn echo_body_handler(req: Request<Body>) -> Response {
        let (_parts, body) = req.into_parts();
        let mut channel = RequestBodyChannel::start(body, BodyChannelConfig::default());
        let mut total: u64 = 0;
        while let Some(chunk_result) = match channel.recv().await {
            Ok(value) => value.map(Ok),
            Err(error) => Some(Err(error)),
        } {
            match chunk_result {
                Ok(chunk) => total += chunk.len() as u64,
                Err(error) => {
                    return (StatusCode::BAD_REQUEST, error.to_string()).into_response();
                }
            }
        }
        total.to_string().into_response()
    }

    fn build_app() -> Router {
        Router::new()
            .route("/multipart", post(echo_multipart_handler))
            .route("/body", post(echo_body_handler))
    }

    fn boundary() -> &'static str {
        "----streaming-unit-test"
    }

    fn multipart_body(fields: &[(&str, &[u8])]) -> Vec<u8> {
        let mut out = Vec::new();
        for (name, value) in fields {
            out.extend_from_slice(format!("--{}\r\n", boundary()).as_bytes());
            out.extend_from_slice(
                format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
            );
            out.extend_from_slice(value);
            out.extend_from_slice(b"\r\n");
        }
        out.extend_from_slice(format!("--{}--\r\n", boundary()).as_bytes());
        out
    }

    async fn read_text(response: Response) -> String {
        let bytes = axum::body::to_bytes(response.into_body(), usize::MAX)
            .await
            .unwrap();
        String::from_utf8(bytes.to_vec()).unwrap()
    }

    #[tokio::test]
    async fn multipart_stream_yields_one_field_at_a_time() {
        let body = multipart_body(&[("a", b"hello"), ("b", b"world!!")]);
        let request = Request::builder()
            .method(Method::POST)
            .uri("/multipart")
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={}", boundary()),
            )
            .body(Body::from(body))
            .unwrap();
        let response = build_app().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let summary = read_text(response).await;
        assert_eq!(summary, "a:5\nb:7\n");
    }

    #[tokio::test]
    async fn multipart_stream_enforces_field_size_cap() {
        async fn capped_handler(multipart: Multipart) -> Response {
            let mut stream = MultipartStream::start(
                multipart,
                MultipartStreamConfig {
                    max_field_bytes: 4,
                    ..Default::default()
                },
            );
            let mut errors = Vec::new();
            while let Some(mut field) = stream.next_field().await.unwrap() {
                while let Some(chunk_result) = field.bytes.recv().await {
                    if let Err(error) = chunk_result {
                        errors.push(error.to_string());
                    }
                }
            }
            errors.join("|").into_response()
        }

        let body = multipart_body(&[("big", b"this body is more than four bytes")]);
        let app = Router::new().route("/c", post(capped_handler));
        let request = Request::builder()
            .method(Method::POST)
            .uri("/c")
            .header(
                header::CONTENT_TYPE,
                format!("multipart/form-data; boundary={}", boundary()),
            )
            .body(Body::from(body))
            .unwrap();
        let response = app.oneshot(request).await.unwrap();
        let summary = read_text(response).await;
        assert!(
            summary.contains("max_field_bytes (4)"),
            "expected size-cap error, got `{summary}`"
        );
        assert!(
            summary.contains("`big`"),
            "expected field name in error, got `{summary}`"
        );
    }

    #[tokio::test]
    async fn body_channel_streams_chunked_upload() {
        let payload = vec![0xABu8; 1024];
        let request = Request::builder()
            .method(Method::POST)
            .uri("/body")
            .body(Body::from(payload.clone()))
            .unwrap();
        let response = build_app().oneshot(request).await.unwrap();
        assert_eq!(response.status(), StatusCode::OK);
        let summary = read_text(response).await;
        assert_eq!(summary, "1024");
    }
}
