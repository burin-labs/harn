//! End-to-end conformance for `harn serve site` (#2574): a `.harn`
//! handler — not a Rust handler — reached over a real HTTP route.
//!
//! Exercises the acceptance criteria from the issue: route resolution
//! (explicit `@route` + the `handler_*` convention), JSON request/response
//! round-trips, a binary multipart upload observed by a `.harn` handler, a
//! `304 Not Modified` produced via `http_not_modified`, and a WebSocket
//! upgrade whose frames are echoed by a `.harn` `on_message` handler.
//!
//! Plain HTTP cases drive the router through `tower::ServiceExt::oneshot`
//! so responses are observable byte-for-byte without a socket. The
//! WebSocket case needs a live upgrade, so it binds a loopback listener.

use std::path::Path;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::{header, Method, Request, StatusCode};
use axum::response::Response;
use axum::Router;
use harn_serve::{DispatchCore, DispatchCoreConfig, NoReplayCache, SiteServer, SiteServerConfig};
use serde_json::Value;
use tower::ServiceExt;

/// One script backs every case: each `pub fn` is a route, except
/// `ws_echo`, which has no route and is reached only as the WebSocket
/// `on_message` callback.
const SITE_SCRIPT: &str = r#"
@route("GET", "/hello")
pub fn hello(req: dict) -> dict {
  return http_ok({ "msg": "hi", "method": req.method })
}

@route("POST", "/echo")
pub fn echo(req: dict) -> dict {
  let parsed = json_parse(req.body)
  return http_ok({ "you_sent": parsed })
}

// Conditional GET: derive a strong ETag, reply 304 when the client's
// validator matches, otherwise 200 with the ETag attached.
@route("GET", "/conditional")
pub fn conditional(req: dict) -> dict {
  let body = "{\"id\":\"r1\"}"
  let etag = http_etag(body)
  let inm = req.headers["if-none-match"]
  if inm == etag {
    return http_not_modified(etag, {})
  }
  return http_response(200, json_parse(body), { "ETag": etag })
}

// Binary-safe multipart: the raw bytes survive the JSON dispatch boundary
// as base64 and are recovered with bytes_from_base64.
@route("POST", "/upload")
pub fn upload(req: dict) -> dict {
  let raw = bytes_from_base64(req.body_base64)
  let parsed = multipart_parse(raw, req.headers["content-type"])
  return http_ok({ "parts": parsed.field_count })
}

// Zero-config: the handler_* convention mounts this at /ping.
pub fn handler_ping(req: dict) -> dict {
  return http_ok({ "pong": true })
}

@route("GET", "/ws")
pub fn ws(req: dict) -> dict {
  return http_upgrade_ws(req, { "subprotocols": ["chat.harn"], "on_message": "ws_echo" })
}

// No @route, no handler_ prefix: dispatch-only, reached as the WS callback.
pub fn ws_echo(msg: dict) -> string {
  return "echo:" + msg.data
}
"#;

/// Write the shared script to a temp dir and build a site router over it.
/// The temp dir is returned so it outlives the router (the dispatcher
/// reads the script file on every request).
fn site_router() -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("site.harn");
    std::fs::write(&path, SITE_SCRIPT).expect("write script");
    let router = build_router(&path);
    (dir, router)
}

fn build_core(path: &Path) -> DispatchCore {
    let mut config = DispatchCoreConfig::for_script(path);
    // An HTTP host must re-run its handler on every request.
    config.replay_cache = Arc::new(NoReplayCache);
    DispatchCore::new(config).expect("dispatch core")
}

fn build_router(path: &Path) -> Router {
    SiteServer::new(SiteServerConfig::new(build_core(path)))
        .router()
        .expect("site router")
}

async fn read_json(response: Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

#[tokio::test]
async fn get_route_dispatches_into_harn_handler() {
    let (_dir, router) = site_router();
    let response = router
        .oneshot(
            Request::builder()
                .uri("/hello")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = read_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["msg"], "hi");
    assert_eq!(body["method"], "GET");
}

#[tokio::test]
async fn post_route_sees_request_body() {
    let (_dir, router) = site_router();
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/echo")
                .header(header::CONTENT_TYPE, "application/json")
                .body(Body::from(r#"{"name":"ada"}"#))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = read_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["you_sent"]["name"], "ada");
}

#[tokio::test]
async fn handler_naming_convention_mounts_route() {
    let (_dir, router) = site_router();
    let response = router
        .oneshot(Request::builder().uri("/ping").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (status, body) = read_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["pong"], true);
}

#[tokio::test]
async fn unknown_method_on_known_path_yields_405() {
    let (_dir, router) = site_router();
    // /hello is GET-only; DELETE has no handler.
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::DELETE)
                .uri("/hello")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::METHOD_NOT_ALLOWED);
    assert!(response.headers().contains_key(header::ALLOW));
}

#[tokio::test]
async fn conditional_get_returns_304_via_http_not_modified() {
    let (dir, router) = site_router();
    let path = dir.path().join("site.harn");

    // First GET: 200 with the handler-set ETag.
    let first = build_router(&path)
        .oneshot(
            Request::builder()
                .uri("/conditional")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(first.status(), StatusCode::OK);
    let etag = first
        .headers()
        .get(header::ETAG)
        .expect("handler attaches an ETag")
        .to_str()
        .unwrap()
        .to_string();

    // Second GET with the matching validator: the handler short-circuits
    // to 304 through http_not_modified.
    let second = router
        .oneshot(
            Request::builder()
                .uri("/conditional")
                .header(header::IF_NONE_MATCH, &etag)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(second.status(), StatusCode::NOT_MODIFIED);
    let body = to_bytes(second.into_body(), usize::MAX).await.unwrap();
    assert!(body.is_empty(), "304 body must be empty");
}

#[tokio::test]
async fn multipart_upload_is_observed_by_harn_handler() {
    let (_dir, router) = site_router();
    let boundary = "----site-host-boundary";
    // Two fields, the second binary (a NUL byte) — proof the bytes survive
    // the base64 round-trip into the handler intact.
    let body = format!(
        "--{boundary}\r\nContent-Disposition: form-data; name=\"title\"\r\n\r\nhello\r\n\
         --{boundary}\r\nContent-Disposition: form-data; name=\"blob\"; filename=\"a.bin\"\r\n\
         Content-Type: application/octet-stream\r\n\r\n\x00\x01\x02\r\n--{boundary}--\r\n"
    );
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/upload")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(body.into_bytes()))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, parsed) = read_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parsed["parts"], 2);
}

#[tokio::test]
async fn websocket_upgrade_echoes_through_on_message_handler() {
    use futures_util::{SinkExt, StreamExt};
    use tokio_tungstenite::tungstenite::protocol::Message as TungMessage;

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("site.harn");
    std::fs::write(&path, SITE_SCRIPT).expect("write script");
    let router = build_router(&path);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let url = format!("ws://{addr}/ws");
    let (mut socket, _response) = tokio_tungstenite::connect_async(url).await.unwrap();
    socket.send(TungMessage::Text("ping".into())).await.unwrap();
    let echoed = socket.next().await.unwrap().unwrap();
    assert_eq!(echoed, TungMessage::Text("echo:ping".into()));
    socket.send(TungMessage::Close(None)).await.unwrap();
    // Keep the temp dir alive until the socket round-trip is done.
    drop(dir);
}

/// The #2574 acceptance bug: `harn_serve::compute_strong_etag` and the
/// `harn_vm` `http_etag` builtin must derive the *same* validator for the
/// same bytes, so a handler-set ETag and the transport layer's ETag never
/// disagree. Both are `"<hex sha256>"`; this pins the shared value.
#[test]
fn strong_etag_matches_http_etag_builtin() {
    assert_eq!(
        harn_serve::compute_strong_etag(b"hello"),
        "\"2cf24dba5fb0a30e26e83b2ac5b9e29e1b161e5c1fa7425e73043362938b9824\"",
    );
}
