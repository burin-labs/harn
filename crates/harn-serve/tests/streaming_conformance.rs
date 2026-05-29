//! End-to-end streaming-upload conformance for the A.12 transport
//! follow-up.
//!
//! Exercises both [`harn_serve::MultipartStream`] and
//! [`harn_serve::RequestBodyChannel`] against a 50 MiB payload — large
//! enough that the buffered
//! [`harn_vm::stdlib::multipart::multipart_parse`] would have to
//! allocate the full body plus a per-field copy (≥100 MiB live), but
//! small enough that test runtime stays under a second on a modest CI
//! box. The handlers never collect the body: each chunk flows straight
//! into a [`sha2::Sha256`] hasher and an `AtomicU64` byte counter, and
//! the test asserts the in-handler **peak chunk size** stays well below
//! the payload size — the practical proof that the consumer never sat
//! on the full upload.
//!
//! The buffered baseline isn't run here (it would defeat the point of
//! the test by allocating the 50 MiB body twice); the comparison
//! against it is documented in [`crate::streaming`]'s module preamble.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{DefaultBodyLimit, Multipart};
use axum::http::{header, Method, Request, StatusCode};
use axum::response::IntoResponse;
use axum::routing::post;
use axum::Router;
use bytes::Bytes;
use futures_util::stream::{self, StreamExt};
use harn_serve::{BodyChannelConfig, MultipartStream, MultipartStreamConfig, RequestBodyChannel};
use sha2::{Digest, Sha256};
use tower::ServiceExt;

const FIFTY_MIB: usize = 50 * 1024 * 1024;
const CHUNK_BYTES: usize = 64 * 1024;
const BOUNDARY: &str = "----streaming-50mib-conformance";

#[derive(Clone, Default)]
struct StreamingMetrics {
    peak_chunk: Arc<AtomicU64>,
    total_bytes: Arc<AtomicU64>,
    /// Hex-encoded final hash, populated when the handler finishes.
    digest_hex: Arc<std::sync::Mutex<Option<String>>>,
}

impl StreamingMetrics {
    fn record(&self, chunk: &[u8]) {
        let len = chunk.len() as u64;
        self.peak_chunk.fetch_max(len, Ordering::Relaxed);
        self.total_bytes.fetch_add(len, Ordering::Relaxed);
    }

    fn finish(&self, hasher: Sha256) {
        let digest = hex::encode(hasher.finalize());
        *self.digest_hex.lock().unwrap() = Some(digest);
    }

    fn snapshot(&self) -> (u64, u64, Option<String>) {
        (
            self.peak_chunk.load(Ordering::Relaxed),
            self.total_bytes.load(Ordering::Relaxed),
            self.digest_hex.lock().unwrap().clone(),
        )
    }
}

fn deterministic_payload(size: usize) -> Vec<u8> {
    // PCG-style xorshift to produce a payload that compresses badly and
    // gives a non-trivial hash — both important so the test cannot pass
    // by accident on an all-zero body.
    let mut state: u64 = 0x9E3779B97F4A7C15;
    let mut out = Vec::with_capacity(size);
    while out.len() < size {
        state = state
            .wrapping_mul(6364136223846793005)
            .wrapping_add(1442695040888963407);
        out.extend_from_slice(&state.to_le_bytes());
    }
    out.truncate(size);
    out
}

fn expected_digest(payload: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(payload);
    hex::encode(hasher.finalize())
}

/// Build a `Bytes`-yielding stream that delivers `payload` in
/// [`CHUNK_BYTES`] pieces and returns `Poll::Pending` between each via
/// `tokio::task::yield_now`. This mirrors how a real TCP connection
/// stages chunks — multer's `poll_stream` only stops accumulating when
/// the source goes pending, so without the explicit yield it would
/// swallow the whole payload in one tick and the streaming property
/// would be invisible to the consumer.
fn make_paced_chunk_stream(
    payload: Vec<u8>,
) -> impl futures_util::Stream<Item = Result<Bytes, std::convert::Infallible>> {
    let chunks: Vec<Bytes> = payload
        .chunks(CHUNK_BYTES)
        .map(Bytes::copy_from_slice)
        .collect();
    stream::iter(chunks).then(|chunk| async move {
        tokio::task::yield_now().await;
        Ok::<_, std::convert::Infallible>(chunk)
    })
}

fn multipart_envelope(field_name: &str, body: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(body.len() + 256);
    out.extend_from_slice(format!("--{BOUNDARY}\r\n").as_bytes());
    out.extend_from_slice(
        format!(
            "Content-Disposition: form-data; name=\"{field_name}\"; filename=\"payload.bin\"\r\n"
        )
        .as_bytes(),
    );
    out.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
    out.extend_from_slice(body);
    out.extend_from_slice(b"\r\n");
    out.extend_from_slice(format!("--{BOUNDARY}--\r\n").as_bytes());
    out
}

#[tokio::test]
async fn multipart_stream_processes_50_mib_without_buffering() {
    let payload = deterministic_payload(FIFTY_MIB);
    let expected = expected_digest(&payload);
    let metrics = StreamingMetrics::default();

    let handler_metrics = metrics.clone();
    let handler = move |multipart: Multipart| {
        let metrics = handler_metrics.clone();
        async move {
            let mut stream = MultipartStream::start(multipart, MultipartStreamConfig::default());
            let mut hasher = Sha256::new();
            while let Some(mut field) = stream
                .next_field()
                .await
                .expect("multipart parse should succeed")
            {
                while let Some(chunk_result) = field.bytes.recv().await {
                    let chunk = chunk_result.expect("field bytes should arrive intact");
                    metrics.record(&chunk);
                    hasher.update(&chunk);
                }
            }
            metrics.finish(hasher);
            StatusCode::OK.into_response()
        }
    };
    // axum applies a 2 MiB DefaultBodyLimit to every request. The
    // streaming primitive doesn't need it — its own per-field cap is
    // what stops malicious uploads — so disable the request-wide cap
    // for this route and prove the producer can walk the full 50 MiB.
    let app = Router::new()
        .route("/upload", post(handler))
        .layer(DefaultBodyLimit::disable());

    // Feed the multipart envelope as a chunked stream so the producer
    // task sees wire-shaped chunks the same way it would on a real
    // connection. `Body::from(Vec<u8>)` is a single-chunk body —
    // multer can only emit what hyper hands it, so the field would
    // look like a 50 MiB chunk and the streaming property would be
    // invisible.
    //
    // Multer's `poll_stream` aggressively drains the source stream
    // until it sees `Pending`, so we also have to interleave a
    // `yield_now` between chunks. `stream::iter` always returns
    // `Ready` and would let multer accumulate the whole 50 MiB into
    // its internal buffer before yielding the first chunk — also
    // defeating the streaming proof.
    let body = multipart_envelope("payload", &payload);
    let chunked = Body::from_stream(make_paced_chunk_stream(body));
    let request = Request::builder()
        .method(Method::POST)
        .uri("/upload")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={BOUNDARY}"),
        )
        .body(chunked)
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let (peak_chunk, total_bytes, digest) = metrics.snapshot();
    assert_eq!(
        total_bytes, FIFTY_MIB as u64,
        "handler should have observed every byte"
    );
    assert_eq!(
        digest.as_deref(),
        Some(expected.as_str()),
        "hash mismatch — handler did not see the full payload"
    );
    // multer can coalesce small chunks while scanning for the next
    // boundary, but it cannot legally hand the consumer more than a
    // small multiple of the wire chunk size in one go without
    // buffering the whole field — which is exactly what we're proving
    // it doesn't do. 4× the input chunk size is comfortable headroom.
    assert!(
        peak_chunk <= (4 * CHUNK_BYTES) as u64,
        "peak streamed chunk was {peak_chunk} bytes — multer should hand off in wire-shaped \
         pieces, not the full 50 MiB"
    );
}

#[tokio::test]
async fn body_channel_processes_50_mib_chunked_upload() {
    let payload = deterministic_payload(FIFTY_MIB);
    let expected = expected_digest(&payload);
    let metrics = StreamingMetrics::default();

    let handler_metrics = metrics.clone();
    let handler = move |req: Request<Body>| {
        let metrics = handler_metrics.clone();
        async move {
            let (_parts, body) = req.into_parts();
            let mut channel = RequestBodyChannel::start(body, BodyChannelConfig::default());
            let mut hasher = Sha256::new();
            while let Some(chunk) = channel
                .recv()
                .await
                .expect("body stream should arrive intact")
            {
                metrics.record(&chunk);
                hasher.update(&chunk);
            }
            metrics.finish(hasher);
            StatusCode::OK.into_response()
        }
    };
    let app = Router::new()
        .route("/upload", post(handler))
        .layer(DefaultBodyLimit::disable());

    // Build the body as a chunked stream so axum sees the upload arrive
    // in pieces rather than as a single buffered payload — that's the
    // shape `Transfer-Encoding: chunked` hits in production. The
    // pacing helper interleaves `yield_now` so each chunk is delivered
    // on its own task tick rather than coalesced.
    let chunked = Body::from_stream(make_paced_chunk_stream(payload.clone()));

    let request = Request::builder()
        .method(Method::POST)
        .uri("/upload")
        .body(chunked)
        .unwrap();
    let response = app.oneshot(request).await.unwrap();
    assert_eq!(response.status(), StatusCode::OK);

    let (peak_chunk, total_bytes, digest) = metrics.snapshot();
    assert_eq!(total_bytes, FIFTY_MIB as u64);
    assert_eq!(digest.as_deref(), Some(expected.as_str()));
    assert!(
        peak_chunk <= (2 * CHUNK_BYTES) as u64,
        "peak streamed chunk was {peak_chunk} bytes — body must be delivered in wire-size \
         chunks, not the full 50 MiB"
    );
}
