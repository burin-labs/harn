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

use std::net::SocketAddr;
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
  return http_reply(200, json_parse(body), { "ETag": etag })
}

// Binary-safe multipart: the raw bytes survive the JSON dispatch boundary
// as base64 and are recovered with bytes_from_base64.
@route("POST", "/upload")
pub fn upload(req: dict) -> dict {
  let raw = bytes_from_base64(req.body_base64)
  let parsed = multipart_parse(raw, req.headers["content-type"])
  return http_ok({
    "parts": parsed.field_count,
    "body_is_nil": req.body == nil,
    "body_kind": req.body_kind,
    "body_base64_matches": bytes_to_base64(raw) == req.body_base64,
    "content_length": req.content_length,
  })
}

@route("GET", "/download")
pub fn download(req: dict) -> dict {
  return http_reply(
    200,
    bytes_from_base64("AP/+gA=="),
    {
      "Content-Type": "application/vnd.harn.harnpack",
      "Content-Disposition": "attachment; filename=\"demo.harnpack\"",
    },
  )
}

// Zero-config: the handler_* convention mounts this at /ping.
pub fn handler_ping(req: dict) -> dict {
  return http_ok({ "pong": true })
}

// Echo the adapter-resolved peer/client identity back to the test.
@route("GET", "/whoami")
pub fn whoami(req: dict) -> dict {
  return http_ok({ "client_ip": req.client_ip, "remote_addr": req.remote_addr })
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

/// Build a router whose adapter trusts the given proxy CIDRs for
/// forwarded-header parsing.
fn router_trusting(path: &Path, proxies: &[&str]) -> Router {
    let proxies = proxies.iter().map(|cidr| cidr.parse().unwrap()).collect();
    SiteServer::new(SiteServerConfig::new(build_core(path)).with_trusted_proxies(proxies))
        .router()
        .expect("site router")
}

/// A `GET /whoami` request carrying the given transport peer (mirroring
/// the `ConnectInfo` extension the connect-info make service inserts in
/// production) and an optional spoofable `X-Forwarded-For` header.
fn whoami_request(peer: SocketAddr, forwarded_for: Option<&str>) -> Request<Body> {
    let mut builder = Request::builder().uri("/whoami");
    if let Some(xff) = forwarded_for {
        builder = builder.header("x-forwarded-for", xff);
    }
    let mut request = builder.body(Body::empty()).unwrap();
    request
        .extensions_mut()
        .insert(axum::extract::ConnectInfo(peer));
    request
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
async fn malformed_route_is_diagnosed_and_unmounted_without_breaking_siblings() {
    // A script where one handler's `@route` is malformed (a non-string
    // method position) and a sibling is well-formed. The startup catalog
    // must carry a `HARN-SRV-*` diagnostic and leave the bad handler
    // unmounted (404), while the good handler still serves (200).
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("site.harn");
    std::fs::write(
        &path,
        r#"
pub fn bad_method(req: dict) -> string { return "GET" }

@route(bad_method, "/broken")
pub fn handler_broken(req: dict) -> dict { return http_ok({}) }

@route("GET", "/fine")
pub fn handler_fine(req: dict) -> dict { return http_ok({ "ok": true }) }
"#,
    )
    .expect("write script");

    let core = build_core(&path);
    let diagnostics = core.catalog().diagnostics();
    assert_eq!(diagnostics.len(), 1, "got {diagnostics:?}");
    assert_eq!(diagnostics[0].code, "HARN-SRV-001");
    assert!(diagnostics[0].message.contains("handler_broken"));

    let router = SiteServer::new(SiteServerConfig::new(core))
        .router()
        .expect("site router");

    let broken = router
        .clone()
        .oneshot(
            Request::builder()
                .uri("/broken")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    assert_eq!(broken.status(), StatusCode::NOT_FOUND);

    let fine = router
        .oneshot(Request::builder().uri("/fine").body(Body::empty()).unwrap())
        .await
        .unwrap();
    let (status, body) = read_json(fine).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
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
    // Two fields, the second deliberately invalid UTF-8, so the hosted
    // request must expose the base64 body path instead of a lossy text view.
    let mut body = Vec::new();
    body.extend_from_slice(
        format!(
            "--{boundary}\r\nContent-Disposition: form-data; name=\"title\"\r\n\r\nhello\r\n\
             --{boundary}\r\nContent-Disposition: form-data; name=\"blob\"; filename=\"a.bin\"\r\n\
             Content-Type: application/octet-stream\r\n\r\n"
        )
        .as_bytes(),
    );
    body.extend_from_slice(&[0x00, 0xff, 0xfe, 0x80]);
    body.extend_from_slice(format!("\r\n--{boundary}--\r\n").as_bytes());
    let body_len = body.len();
    let response = router
        .oneshot(
            Request::builder()
                .method(Method::POST)
                .uri("/upload")
                .header(
                    header::CONTENT_TYPE,
                    format!("multipart/form-data; boundary={boundary}"),
                )
                .body(Body::from(body))
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, parsed) = read_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(parsed["parts"], 2);
    assert_eq!(parsed["body_is_nil"], true);
    assert_eq!(parsed["body_kind"], "base64");
    assert_eq!(parsed["body_base64_matches"], true);
    assert_eq!(parsed["content_length"].as_u64().unwrap(), body_len as u64);
}

#[tokio::test]
async fn binary_response_from_harn_handler_round_trips_byte_exact() {
    let (_dir, router) = site_router();
    let response = router
        .oneshot(
            Request::builder()
                .uri("/download")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(
        response.headers()[header::CONTENT_TYPE],
        "application/vnd.harn.harnpack"
    );
    assert_eq!(
        response.headers()[header::CONTENT_DISPOSITION],
        "attachment; filename=\"demo.harnpack\""
    );
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(body.as_ref(), &[0x00, 0xff, 0xfe, 0x80]);
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

#[tokio::test]
async fn remote_addr_reports_the_transport_peer() {
    let (_dir, router) = site_router();
    let peer: SocketAddr = "203.0.113.7:54321".parse().unwrap();
    let response = router.oneshot(whoami_request(peer, None)).await.unwrap();
    let (status, body) = read_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["remote_addr"], "203.0.113.7:54321");
    // With no trusted proxies, client_ip is the peer's IP, not its port.
    assert_eq!(body["client_ip"], "203.0.113.7");
}

#[tokio::test]
async fn forged_forwarded_header_is_ignored_without_trusted_proxies() {
    let (_dir, router) = site_router();
    let peer: SocketAddr = "203.0.113.7:54321".parse().unwrap();
    // A direct client sets X-Forwarded-For hoping to be seen as 1.2.3.4.
    let response = router
        .oneshot(whoami_request(peer, Some("1.2.3.4")))
        .await
        .unwrap();
    let (status, body) = read_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(
        body["client_ip"], "203.0.113.7",
        "forged XFF must be ignored"
    );
}

#[tokio::test]
async fn forwarded_header_is_honored_from_a_trusted_proxy() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("site.harn");
    std::fs::write(&path, SITE_SCRIPT).expect("write script");
    // The request arrives from a proxy in the trusted range; its XFF is
    // believed.
    let router = router_trusting(&path, &["10.0.0.0/8"]);
    let peer: SocketAddr = "10.0.0.5:443".parse().unwrap();
    let response = router
        .oneshot(whoami_request(peer, Some("1.2.3.4")))
        .await
        .unwrap();
    let (status, body) = read_json(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["client_ip"], "1.2.3.4");
    assert_eq!(body["remote_addr"], "10.0.0.5:443");
}

#[tokio::test]
async fn forwarded_chain_peels_off_trusted_hops() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("site.harn");
    std::fs::write(&path, SITE_SCRIPT).expect("write script");
    let router = router_trusting(&path, &["10.0.0.0/8"]);
    let peer: SocketAddr = "10.0.0.5:443".parse().unwrap();
    // client → edge(10.0.0.9) → us(10.0.0.5): the rightmost untrusted hop
    // is the real client, even though an internal proxy follows it.
    let response = router
        .oneshot(whoami_request(peer, Some("1.2.3.4, 10.0.0.9")))
        .await
        .unwrap();
    let (_status, body) = read_json(response).await;
    assert_eq!(body["client_ip"], "1.2.3.4");
}

#[tokio::test]
async fn untrusted_peer_ignores_forwarded_header_even_with_config() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("site.harn");
    std::fs::write(&path, SITE_SCRIPT).expect("write script");
    // Trusted set is configured, but this peer is NOT in it, so its XFF
    // is untrusted and the peer IP wins.
    let router = router_trusting(&path, &["10.0.0.0/8"]);
    let peer: SocketAddr = "203.0.113.7:9000".parse().unwrap();
    let response = router
        .oneshot(whoami_request(peer, Some("1.2.3.4")))
        .await
        .unwrap();
    let (_status, body) = read_json(response).await;
    assert_eq!(body["client_ip"], "203.0.113.7");
}

#[tokio::test]
async fn remote_addr_is_populated_over_a_live_socket() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("site.harn");
    std::fs::write(&path, SITE_SCRIPT).expect("write script");
    let router = build_router(&path);

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        // The connect-info make service is what `serve_router_from_tcp`
        // installs; serving it here proves the peer address reaches the
        // handler over a real socket.
        let _ = axum::serve(
            listener,
            router.into_make_service_with_connect_info::<SocketAddr>(),
        )
        .await;
    });

    let body: Value = reqwest::get(format!("http://{addr}/whoami"))
        .await
        .unwrap()
        .json()
        .await
        .unwrap();
    let remote_addr = body["remote_addr"].as_str().expect("remote_addr present");
    assert!(
        remote_addr.starts_with("127.0.0.1:"),
        "expected loopback peer, got {remote_addr}"
    );
    assert_eq!(body["client_ip"], "127.0.0.1");
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
