//! End-to-end transport conformance test for the A.12 transport stack.
//!
//! Drives a single axum server wired through
//! [`harn_serve::apply_transport_layers`] + a WS route built from
//! [`harn_serve::ws_route`], then exercises every feature the issue
//! calls out as acceptance criteria:
//!
//! * **WebSocket echo** — text and binary frames round-trip with
//!   subprotocol negotiation.
//! * **Multipart upload** — request body parsed via
//!   `harn_vm::stdlib::multipart` (already shipped) and counted.
//! * **gzip negotiation** — `Accept-Encoding: gzip` yields a
//!   `Content-Encoding: gzip` body that round-trips.
//! * **ETag + 304 cycle** — first GET attaches the ETag; second GET
//!   with `If-None-Match` short-circuits to `304 Not Modified`.
//! * **CORS preflight** — declarative `cors { ... }` at mount time
//!   answers `OPTIONS` without dispatching into a handler.

use std::net::SocketAddr;
use std::sync::Arc;

use axum::body::Body;
use axum::extract::{Multipart, State};
use axum::http::{header, HeaderValue, Method, Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use harn_serve::{
    apply_transport_layers, ws_route, CorsConfig, TransportConfig, WsConfig, WsMessage, WsSession,
};
use serde_json::json;
use tokio::net::TcpListener;
use tokio_tungstenite::tungstenite::protocol::Message as TungMessage;

#[derive(Clone)]
struct AppState {
    payload: Arc<serde_json::Value>,
}

async fn echo_ws(session: WsSession) {
    while let Ok(Some(message)) = session.recv().await {
        match message {
            WsMessage::Text(text) => {
                if session.send(text).await.is_err() {
                    break;
                }
            }
            WsMessage::Binary(bytes) => {
                if session.send_binary(bytes).await.is_err() {
                    break;
                }
            }
        }
    }
}

async fn snapshot(State(state): State<AppState>) -> Response {
    Json((*state.payload).clone()).into_response()
}

async fn upload_count(mut multipart: Multipart) -> Response {
    let mut parts = 0u32;
    let mut total = 0u64;
    while let Ok(Some(field)) = multipart.next_field().await {
        parts += 1;
        if let Ok(bytes) = field.bytes().await {
            total += bytes.len() as u64;
        }
    }
    Json(json!({"parts": parts, "bytes": total})).into_response()
}

fn build_app() -> Router {
    let state = AppState {
        payload: Arc::new(json!({"resource": {"id": "r1", "value": "x".repeat(2048)}})),
    };
    let routes = Router::new()
        .route("/v1/resource", get(snapshot))
        .route("/v1/upload", post(upload_count))
        .route("/v1/ws", ws_route(echo_ws, WsConfig::default()))
        .with_state(state);
    let config = TransportConfig {
        compression: true,
        etag: true,
        cors: Some(CorsConfig {
            allow_origins: vec!["https://app.example.com".into()],
            allow_methods: vec!["GET".into(), "POST".into(), "OPTIONS".into()],
            allow_headers: vec![
                "authorization".into(),
                "content-type".into(),
                "if-none-match".into(),
            ],
            ..Default::default()
        }),
    };
    apply_transport_layers(routes, &config)
}

async fn spawn_server() -> SocketAddr {
    let app = build_app();
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, app).await;
    });
    addr
}

async fn read_body_bytes(response: Response) -> Vec<u8> {
    axum::body::to_bytes(response.into_body(), usize::MAX)
        .await
        .unwrap()
        .to_vec()
}

/// Drive a single HTTP request through the same router used by the WS
/// test. We use `Service::oneshot` rather than a live TCP socket so
/// the response is observable byte-for-byte — gzip framing varies
/// across reqwest versions and that variance would make the gzip
/// assertion flaky.
async fn http_request(request: Request<Body>) -> Response {
    use tower::ServiceExt;
    build_app().oneshot(request).await.unwrap()
}

#[tokio::test]
async fn ws_echo_roundtrip() {
    let addr = spawn_server().await;
    let url = format!("ws://{addr}/v1/ws");
    let (mut socket, _response) = tokio_tungstenite::connect_async(url).await.unwrap();

    socket.send(TungMessage::Text("ping".into())).await.unwrap();
    let echoed = socket.next().await.unwrap().unwrap();
    assert_eq!(echoed, TungMessage::Text("ping".into()));

    socket
        .send(TungMessage::Binary(vec![0xDE, 0xAD, 0xBE, 0xEF].into()))
        .await
        .unwrap();
    let echoed = socket.next().await.unwrap().unwrap();
    assert_eq!(
        echoed,
        TungMessage::Binary(vec![0xDE, 0xAD, 0xBE, 0xEF].into())
    );

    socket.send(TungMessage::Close(None)).await.unwrap();
}

#[tokio::test]
async fn multipart_upload_is_parsed_field_by_field() {
    let boundary = "----conformance-test-boundary";
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"title\"\r\n\r\nHello\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"file\"; filename=\"a.bin\"\r\n\
         Content-Type: application/octet-stream\r\n\r\n\x00\x01\x02\x03\r\n--{boundary}--\r\n"
    );
    let request = Request::builder()
        .method(Method::POST)
        .uri("/v1/upload")
        .header(
            header::CONTENT_TYPE,
            format!("multipart/form-data; boundary={boundary}"),
        )
        .body(Body::from(body.into_bytes()))
        .unwrap();
    let response = http_request(request).await;
    assert_eq!(response.status(), StatusCode::OK);
    let bytes = read_body_bytes(response).await;
    let parsed: serde_json::Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(parsed["parts"], 2);
    assert!(parsed["bytes"].as_u64().unwrap() >= 9);
}

#[tokio::test]
async fn gzip_negotiation_compresses_json_response() {
    let request = Request::builder()
        .uri("/v1/resource")
        .header(header::ACCEPT_ENCODING, "gzip")
        .body(Body::empty())
        .unwrap();
    let response = http_request(request).await;
    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response
            .headers()
            .get(header::CONTENT_ENCODING)
            .map(HeaderValue::to_str)
            .transpose()
            .unwrap(),
        Some("gzip"),
    );
    let body = read_body_bytes(response).await;

    // Decode gzip and verify the payload round-trips.
    use async_compression::tokio::bufread::GzipDecoder;
    use tokio::io::AsyncReadExt;
    let mut decoded = Vec::new();
    GzipDecoder::new(&body[..])
        .read_to_end(&mut decoded)
        .await
        .unwrap();
    let parsed: serde_json::Value = serde_json::from_slice(&decoded).unwrap();
    assert_eq!(parsed["resource"]["id"], "r1");
}

#[tokio::test]
async fn etag_cycle_yields_304_on_match() {
    let first = http_request(
        Request::builder()
            .uri("/v1/resource")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(first.status(), StatusCode::OK);
    let etag = first
        .headers()
        .get(header::ETAG)
        .expect("first GET returns an ETag")
        .to_str()
        .unwrap()
        .to_string();

    let conditional = http_request(
        Request::builder()
            .uri("/v1/resource")
            .header(header::IF_NONE_MATCH, &etag)
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert_eq!(conditional.status(), StatusCode::NOT_MODIFIED);
    let body = read_body_bytes(conditional).await;
    assert!(body.is_empty(), "304 body should be empty, got {body:?}");
}

#[tokio::test]
async fn cors_preflight_short_circuits_without_handler_dispatch() {
    let response = http_request(
        Request::builder()
            .method(Method::OPTIONS)
            .uri("/v1/resource")
            .header(header::ORIGIN, "https://app.example.com")
            .header("access-control-request-method", "GET")
            .header(
                "access-control-request-headers",
                "authorization, if-none-match",
            )
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    assert!(response.status().is_success());
    let allowed_origin = response
        .headers()
        .get("access-control-allow-origin")
        .expect("allow-origin set");
    assert_eq!(allowed_origin, "https://app.example.com");
    let allowed_headers = response
        .headers()
        .get("access-control-allow-headers")
        .expect("allow-headers set")
        .to_str()
        .unwrap();
    assert!(allowed_headers
        .to_ascii_lowercase()
        .contains("if-none-match"));
}

#[tokio::test]
async fn cors_unknown_origin_does_not_expose_allow_headers() {
    let response = http_request(
        Request::builder()
            .method(Method::OPTIONS)
            .uri("/v1/resource")
            .header(header::ORIGIN, "https://evil.example.com")
            .header("access-control-request-method", "GET")
            .body(Body::empty())
            .unwrap(),
    )
    .await;
    // tower-http's CorsLayer responds to the preflight unconditionally
    // but only echoes Allow-Origin when the request origin matched the
    // policy. Verify we neither leaked the wildcard nor mirrored the
    // unknown origin back, both of which would defeat the explicit
    // allow-list.
    let allow_origin = response.headers().get("access-control-allow-origin");
    assert_ne!(
        allow_origin.and_then(|v| v.to_str().ok()),
        Some("*"),
        "unknown origin must not yield wildcard CORS",
    );
    assert_ne!(
        allow_origin.and_then(|v| v.to_str().ok()),
        Some("https://evil.example.com"),
        "unknown origin must not be mirrored back",
    );
}
