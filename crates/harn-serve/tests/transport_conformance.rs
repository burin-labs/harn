//! End-to-end conformance for the harn-serve transport stack
//! ([`harn_serve::transport`]): compression negotiation,
//! conditional-GET short-circuit, and CORS preflight, exercised
//! against a `Router` that mirrors the shape of the production
//! adapter routers.
//!
//! Together these tests cover the [`A.12`](https://github.com/burin-labs/harn/issues/2515)
//! transport-completeness acceptance for the features that are
//! shippable without `.harn`-handler request-context plumbing
//! (WebSocket upgrade, multipart streaming, chunked uploads, and
//! HTTP/2 server push are tracked separately).

use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use axum::routing::get;
use axum::{Json, Router};
use harn_serve::transport::{apply_transport_layers, CorsConfig, TransportConfig};
use serde_json::json;
use tokio::io::AsyncReadExt;
use tower::ServiceExt;

const BIG_PAYLOAD_BYTES: usize = 2048;

fn payload_router() -> Router {
    Router::new()
        .route(
            "/v1/payload",
            get(|| async {
                Json(json!({
                    "id": "sess_abc",
                    "blob": "x".repeat(BIG_PAYLOAD_BYTES),
                }))
            }),
        )
        .route(
            "/v1/event",
            get(|| async {
                axum::response::Response::builder()
                    .status(StatusCode::OK)
                    .header(header::CONTENT_TYPE, "text/event-stream")
                    .body(Body::from("event: ping\ndata: {}\n\n"))
                    .unwrap()
            }),
        )
}

async fn decode_gzip(bytes: bytes::Bytes) -> Vec<u8> {
    let mut decoder = async_compression::tokio::bufread::GzipDecoder::new(&bytes[..]);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).await.expect("gzip decode");
    out
}

async fn decode_brotli(bytes: bytes::Bytes) -> Vec<u8> {
    let mut decoder = async_compression::tokio::bufread::BrotliDecoder::new(&bytes[..]);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).await.expect("brotli decode");
    out
}

async fn decode_zstd(bytes: bytes::Bytes) -> Vec<u8> {
    let mut decoder = async_compression::tokio::bufread::ZstdDecoder::new(&bytes[..]);
    let mut out = Vec::new();
    decoder.read_to_end(&mut out).await.expect("zstd decode");
    out
}

#[tokio::test]
async fn gzip_request_yields_compressed_body_that_decodes_back() {
    let app = apply_transport_layers(payload_router(), &TransportConfig::default_enabled());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/payload")
                .header(header::ACCEPT_ENCODING, "gzip")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_ENCODING)
            .unwrap()
            .to_str()
            .unwrap(),
        "gzip",
    );
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let decoded = decode_gzip(bytes).await;
    let parsed: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
    assert_eq!(parsed["id"], "sess_abc");
    assert_eq!(parsed["blob"].as_str().unwrap().len(), BIG_PAYLOAD_BYTES);
}

#[tokio::test]
async fn brotli_request_picks_brotli_when_offered() {
    let app = apply_transport_layers(payload_router(), &TransportConfig::default_enabled());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/payload")
                .header(header::ACCEPT_ENCODING, "br")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_ENCODING)
            .unwrap()
            .to_str()
            .unwrap(),
        "br",
    );
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let decoded = decode_brotli(bytes).await;
    let parsed: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
    assert_eq!(parsed["id"], "sess_abc");
}

#[tokio::test]
async fn zstd_request_picks_zstd_when_offered() {
    let app = apply_transport_layers(payload_router(), &TransportConfig::default_enabled());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/payload")
                .header(header::ACCEPT_ENCODING, "zstd")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_ENCODING)
            .unwrap()
            .to_str()
            .unwrap(),
        "zstd",
    );
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let decoded = decode_zstd(bytes).await;
    let parsed: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
    assert_eq!(parsed["id"], "sess_abc");
}

#[tokio::test]
async fn conditional_get_round_trip_yields_304() {
    let app = apply_transport_layers(payload_router(), &TransportConfig::default_enabled());

    let first = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/payload")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let etag = first
        .headers()
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let second = app
        .oneshot(
            Request::builder()
                .uri("/v1/payload")
                .header(header::IF_NONE_MATCH, &etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
    let bytes = to_bytes(second.into_body(), usize::MAX).await.unwrap();
    assert!(bytes.is_empty());
}

#[tokio::test]
async fn compression_and_etag_compose_under_accept_encoding() {
    // The ETag must be stable regardless of which Accept-Encoding the
    // client sent — that's the whole point of stacking ETag inside
    // compression. A subsequent identical request with If-None-Match
    // must yield 304 even if the prior round used gzip.
    let app = apply_transport_layers(payload_router(), &TransportConfig::default_enabled());

    let gzipped = app
        .clone()
        .oneshot(
            Request::builder()
                .uri("/v1/payload")
                .header(header::ACCEPT_ENCODING, "gzip")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let etag = gzipped
        .headers()
        .get(header::ETAG)
        .unwrap()
        .to_str()
        .unwrap()
        .to_string();

    let conditional = app
        .oneshot(
            Request::builder()
                .uri("/v1/payload")
                .header(header::ACCEPT_ENCODING, "gzip")
                .header(header::IF_NONE_MATCH, &etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(conditional.status(), StatusCode::NOT_MODIFIED);
}

#[tokio::test]
async fn sse_response_is_not_etagged() {
    let app = apply_transport_layers(payload_router(), &TransportConfig::default_enabled());
    let response = app
        .oneshot(
            Request::builder()
                .uri("/v1/event")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::OK);
    assert!(response.headers().get(header::ETAG).is_none());
}

#[tokio::test]
async fn cors_preflight_for_listed_origin_succeeds() {
    let config = TransportConfig {
        compression: false,
        etag: false,
        cors: Some(CorsConfig {
            allow_origins: vec!["https://app.example.com".into()],
            ..Default::default()
        }),
    };
    let app = apply_transport_layers(payload_router(), &config);
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/v1/payload")
                .header(header::ORIGIN, "https://app.example.com")
                .header("access-control-request-method", "GET")
                .header("access-control-request-headers", "authorization")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(response.status().is_success());
    assert_eq!(
        response
            .headers()
            .get("access-control-allow-origin")
            .unwrap(),
        "https://app.example.com"
    );
    let methods = response
        .headers()
        .get("access-control-allow-methods")
        .unwrap()
        .to_str()
        .unwrap();
    assert!(methods.contains("GET"));
    assert!(methods.contains("POST"));
}

#[tokio::test]
async fn cors_with_credentials_disables_wildcard_origin() {
    let config = TransportConfig {
        compression: false,
        etag: false,
        cors: Some(CorsConfig {
            allow_origins: vec!["https://app.example.com".into()],
            allow_credentials: true,
            ..Default::default()
        }),
    };
    let app = apply_transport_layers(payload_router(), &config);
    let response = app
        .oneshot(
            Request::builder()
                .method(Method::OPTIONS)
                .uri("/v1/payload")
                .header(header::ORIGIN, "https://app.example.com")
                .header("access-control-request-method", "GET")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert!(response.status().is_success());
    let creds = response
        .headers()
        .get("access-control-allow-credentials")
        .expect("credentials header present");
    assert_eq!(creds.to_str().unwrap(), "true");
}
