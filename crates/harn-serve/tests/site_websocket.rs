//! Embedder-driven WebSocket upgrades through `harn serve site` (the
//! `@ws` route marker, #3215 family).
//!
//! Exercises the `@ws` marker + [`harn_serve::SiteStreamProvider::upgrade`]
//! seam end-to-end: an admitted request on a `@ws` route reaches the
//! embedder's provider with the matched route, the hook-resolved
//! identity, and the request head; the provider completes the WebSocket
//! upgrade and drives the socket (echoing inbound frames), and the live
//! connection round-trips. Admission — the [`harn_serve::SiteAuth`] hook
//! and the route's `@scopes` — still gates the route, refusing *before*
//! the upgrade is performed (no creds → 401, wrong scope → 403). A
//! non-WebSocket request to a `@ws` route is refused with the correct
//! 4xx by axum's extractor rather than panicking. And a script that
//! declares a `@ws` route refuses to build a router without a provider.
//!
//! The upgrade cases need a live socket, so they bind a loopback
//! listener (mirroring `tests/site_hosting.rs`); the admission /
//! not-a-socket / build cases drive the router through
//! `tower::ServiceExt::oneshot` (mirroring `tests/site_streaming.rs`).

use std::path::Path;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::extract::ws::{Message, WebSocketUpgrade};
use axum::http::request::Parts;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use futures_util::{SinkExt, StreamExt};
use harn_serve::{
    DispatchCore, DispatchCoreConfig, NoReplayCache, RouteSpec, SiteAuth, SiteAuthContext,
    SiteAuthOutcome, SiteServer, SiteServerConfig, SiteStreamProvider,
};
use harn_vm::TenantId;
use serde_json::{json, Value};
use tower::ServiceExt;

/// One script backs every case: a `@scopes`-guarded `@ws` route and a
/// plain dispatch route. The `@ws` route's body is a declaration-only
/// stub — if it ever runs, the VM dispatched where the provider should
/// have upgraded.
const SITE_SCRIPT: &str = r#"
@ws
@scopes("live:read")
@route("GET", "/topics/{topic}/live")
pub fn topic_live(req: dict) -> dict {
  return http_ok({ "buffered_stub": true })
}

@route("GET", "/plain")
pub fn plain(req: dict) -> dict {
  return http_ok({ "plain": true })
}
"#;

/// Test provider: completes the upgrade and echoes each inbound text
/// frame back as `echo:<text>`. Before echoing, it sends one priming
/// frame carrying the route/tenant/path-param it was handed, so a single
/// round-trip proves the whole admission + head-threading contract.
struct EchoUpgradeProvider;

#[async_trait::async_trait]
impl SiteStreamProvider for EchoUpgradeProvider {
    // `open` is never reached on a `@ws` route; a panic here would catch
    // the adapter calling the wrong entry point.
    async fn open(
        &self,
        _route: &RouteSpec,
        _auth: Option<&SiteAuthContext>,
        _request: Value,
        _body: Option<axum::body::Bytes>,
    ) -> Response {
        panic!("a @ws route must call `upgrade`, never `open`");
    }

    async fn upgrade(
        &self,
        route: &RouteSpec,
        auth: Option<&SiteAuthContext>,
        ws: WebSocketUpgrade,
        request: Value,
    ) -> Response {
        let opened = json!({
            "route": { "method": route.method, "path": route.path },
            "tenant": auth.and_then(|c| c.tenant_id.as_ref()).map(|t| t.0.clone()),
            "topic": request["path_params"]["topic"],
        })
        .to_string();
        ws.on_upgrade(move |mut socket| async move {
            // Prime the client with the head dict so the test can assert
            // the provider saw the matched route + identity.
            if socket.send(Message::Text(opened.into())).await.is_err() {
                return;
            }
            while let Some(Ok(message)) = socket.next().await {
                match message {
                    Message::Text(text) => {
                        let reply = format!("echo:{text}");
                        if socket.send(Message::Text(reply.into())).await.is_err() {
                            break;
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        })
    }
}

/// Script for the combined `@ws @stream` route — one route that is both a
/// WebSocket upgrade and an SSE/stream fallback (the gateway `/acp`
/// carve-out). The body is a declaration-only stub; if it ever runs the
/// VM dispatched where the provider should have answered either branch.
const COMBINED_SCRIPT: &str = r#"
@ws
@stream
@scopes("acp:read")
@route("GET", "/acp/{session}")
pub fn acp(req: dict) -> dict {
  return http_ok({ "buffered_stub": true })
}
"#;

/// Provider for the combined route: a genuine handshake upgrades and
/// echoes (`upgrade`); every other request gets a plain `200` carrying
/// the head dict (`open`), proving the non-upgrade branch falls through
/// to the stream path instead of a 4xx.
struct DualProvider;

#[async_trait::async_trait]
impl SiteStreamProvider for DualProvider {
    async fn open(
        &self,
        route: &RouteSpec,
        auth: Option<&SiteAuthContext>,
        request: Value,
        _body: Option<axum::body::Bytes>,
    ) -> Response {
        // Non-upgrade requests land here on the combined route. Shape a
        // marker response that echoes the head so the test proves the
        // stream branch saw the matched route + identity.
        Json(json!({
            "via": "open",
            "route": { "method": route.method, "path": route.path },
            "tenant": auth.and_then(|c| c.tenant_id.as_ref()).map(|t| t.0.clone()),
            "session": request["path_params"]["session"],
        }))
        .into_response()
    }

    async fn upgrade(
        &self,
        _route: &RouteSpec,
        _auth: Option<&SiteAuthContext>,
        ws: WebSocketUpgrade,
        _request: Value,
    ) -> Response {
        ws.on_upgrade(move |mut socket| async move {
            if socket
                .send(Message::Text("via:upgrade".into()))
                .await
                .is_err()
            {
                return;
            }
            while let Some(Ok(message)) = socket.next().await {
                match message {
                    Message::Text(text) => {
                        let reply = format!("echo:{text}");
                        if socket.send(Message::Text(reply.into())).await.is_err() {
                            break;
                        }
                    }
                    Message::Close(_) => break,
                    _ => {}
                }
            }
        })
    }
}

/// Build a site router over [`COMBINED_SCRIPT`] with [`DualProvider`],
/// optionally behind an auth hook.
fn combined_router(auth: Option<Arc<dyn SiteAuth>>) -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("site.harn");
    std::fs::write(&path, COMBINED_SCRIPT).expect("write script");
    let mut config =
        SiteServerConfig::new(build_core(&path)).with_stream_provider(Arc::new(DualProvider));
    if let Some(auth) = auth {
        config = config.with_auth(auth);
    }
    let router = SiteServer::new(config).router().expect("site router");
    (dir, router)
}

/// `SiteAuth` hook that always admits with a fixed identity.
struct AllowAuth {
    tenant: Option<&'static str>,
    scopes: &'static [&'static str],
}

#[async_trait::async_trait]
impl SiteAuth for AllowAuth {
    async fn authenticate(&self, _parts: &Parts, _route: &RouteSpec) -> SiteAuthOutcome {
        SiteAuthOutcome::Allow(SiteAuthContext {
            tenant_id: self.tenant.map(TenantId::new),
            scopes: self.scopes.iter().map(|s| s.to_string()).collect(),
            context: None,
        })
    }
}

/// `SiteAuth` hook that always refuses with an embedder-shaped 401.
struct DenyAuth;

#[async_trait::async_trait]
impl SiteAuth for DenyAuth {
    async fn authenticate(&self, _parts: &Parts, _route: &RouteSpec) -> SiteAuthOutcome {
        SiteAuthOutcome::deny(
            (
                StatusCode::UNAUTHORIZED,
                [("x-embedder-deny", "1")],
                Json(json!({ "error": "custom_embedder_denial" })),
            )
                .into_response(),
        )
    }
}

fn build_core(path: &Path) -> DispatchCore {
    let mut config = DispatchCoreConfig::for_script(path);
    config.replay_cache = Arc::new(NoReplayCache);
    DispatchCore::new(config).expect("dispatch core")
}

/// Write the shared script to a temp dir and build a site router over it
/// with the echo upgrade provider, optionally behind an auth hook. The
/// temp dir is returned so it outlives the router.
fn ws_router(auth: Option<Arc<dyn SiteAuth>>) -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("site.harn");
    std::fs::write(&path, SITE_SCRIPT).expect("write script");
    let mut config = SiteServerConfig::new(build_core(&path))
        .with_stream_provider(Arc::new(EchoUpgradeProvider));
    if let Some(auth) = auth {
        config = config.with_auth(auth);
    }
    let router = SiteServer::new(config).router().expect("site router");
    (dir, router)
}

async fn read_body(response: Response) -> (StatusCode, String) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// The happy path: an admitted request on the `@ws` route upgrades, and
/// the provider's socket echoes a message. The priming frame proves the
/// provider saw the matched route, the hook's tenant, and the captured
/// path param; the `.harn` stub body never runs.
#[tokio::test]
async fn ws_route_upgrades_and_echoes_through_provider() {
    use tokio_tungstenite::tungstenite::protocol::Message as TungMessage;

    let hook = Arc::new(AllowAuth {
        tenant: Some("acme"),
        scopes: &["live:read"],
    });
    let (_dir, router) = ws_router(Some(hook));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let url = format!("ws://{addr}/topics/deploys/live");
    let (mut socket, _response) = tokio_tungstenite::connect_async(url).await.unwrap();

    // First frame is the provider's priming head dict.
    let primed = socket.next().await.unwrap().unwrap();
    let TungMessage::Text(text) = primed else {
        panic!("expected a text priming frame, got {primed:?}");
    };
    let opened: Value = serde_json::from_str(&text).unwrap();
    assert_eq!(opened["route"]["method"], "GET");
    assert_eq!(opened["route"]["path"], "/topics/{topic}/live");
    assert_eq!(opened["tenant"], "acme");
    assert_eq!(opened["topic"], "deploys");

    // Then the socket echoes inbound frames.
    socket.send(TungMessage::Text("ping".into())).await.unwrap();
    let echoed = socket.next().await.unwrap().unwrap();
    assert_eq!(echoed, TungMessage::Text("echo:ping".into()));
    socket.send(TungMessage::Close(None)).await.unwrap();
}

/// A `SiteAuth` `Deny` gates the `@ws` route before the upgrade: the
/// embedder's 401 comes back verbatim and no socket is opened. Driven
/// through `oneshot` with the real upgrade headers so the only reason it
/// is refused is the auth hook, not a missing handshake.
#[tokio::test]
async fn auth_deny_refuses_ws_route_before_upgrade() {
    let (_dir, router) = ws_router(Some(Arc::new(DenyAuth)));
    let request = Request::builder()
        .uri("/topics/deploys/live")
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    assert_eq!(response.headers()["x-embedder-deny"], "1");
    let (status, body) = read_body(response).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("custom_embedder_denial"));
}

/// An admitted identity whose scopes do not cover the route's `@scopes`
/// is refused with the canonical `forbidden` envelope — again before the
/// upgrade, even though the request carried a valid handshake.
#[tokio::test]
async fn scope_shortfall_yields_403_before_upgrade() {
    let hook = Arc::new(AllowAuth {
        tenant: Some("acme"),
        scopes: &["live:write"],
    });
    let (_dir, router) = ws_router(Some(hook));
    let request = Request::builder()
        .uri("/topics/deploys/live")
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .body(Body::empty())
        .unwrap();
    let (status, body) = read_body(router.oneshot(request).await.unwrap()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let envelope: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(envelope["code"], "forbidden");
    assert_eq!(envelope["details"]["missing_scopes"][0], "live:read");
}

/// A scoped `@ws` route with no hook installed has no identity and no
/// granted scopes, so it refuses with the canonical 403 before the
/// upgrade — the same admission back-stop a scoped `@stream` route gets.
#[tokio::test]
async fn scoped_ws_route_without_hook_is_refused_at_admission() {
    let (_dir, router) = ws_router(None);
    let request = Request::builder()
        .uri("/topics/deploys/live")
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .body(Body::empty())
        .unwrap();
    let (status, body) = read_body(router.oneshot(request).await.unwrap()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let envelope: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(envelope["code"], "forbidden");
}

/// A plain (non-WebSocket) GET to a `@ws` route is admitted, then refused
/// by axum's `WebSocketUpgrade` extractor with the correct 4xx — not a
/// panic, and not a stray 101.
#[tokio::test]
async fn non_websocket_request_to_ws_route_is_refused_with_4xx() {
    let hook = Arc::new(AllowAuth {
        tenant: Some("acme"),
        scopes: &["live:read"],
    });
    let (_dir, router) = ws_router(Some(hook));
    // No upgrade headers at all — a plain GET.
    let request = Request::builder()
        .uri("/topics/deploys/live")
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(request).await.unwrap();
    let status = response.status();
    assert!(
        status.is_client_error(),
        "a non-WebSocket request to a @ws route must be a 4xx, got {status}"
    );
    // axum's extractor returns 426 / 400 for a non-upgrade request.
    assert_ne!(status, StatusCode::SWITCHING_PROTOCOLS);
    assert_ne!(status, StatusCode::OK);
}

/// A non-`@ws` route on the same script is untouched: it dispatches into
/// the VM and renders its buffered JSON reply.
#[tokio::test]
async fn non_ws_route_still_dispatches_into_the_vm() {
    let hook = Arc::new(AllowAuth {
        tenant: None,
        scopes: &[],
    });
    let (_dir, router) = ws_router(Some(hook));
    let request = Request::builder()
        .uri("/plain")
        .body(Body::empty())
        .unwrap();
    let (status, body) = read_body(router.oneshot(request).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["plain"], true);
}

/// Declaring a `@ws` route without installing a provider is a
/// configuration error surfaced at router-build time, not a 500 at
/// request time.
#[tokio::test]
async fn ws_route_without_provider_refuses_to_build() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("site.harn");
    std::fs::write(&path, SITE_SCRIPT).expect("write script");
    let config = SiteServerConfig::new(build_core(&path));
    let error = SiteServer::new(config).router().expect_err("must refuse");
    assert!(
        error.contains("@ws") && error.contains("with_stream_provider"),
        "unhelpful error: {error}"
    );
}

/// The default `SiteStreamProvider::upgrade` (a provider that only
/// implements `open`) refuses a `@ws` route with `426 Upgrade Required`
/// rather than 101-ing into a socket nobody drives. Drives a real
/// handshake so the request reaches the default method.
#[tokio::test]
async fn default_upgrade_impl_returns_426() {
    // A provider that implements only `open` (the default `upgrade`
    // applies). `open` must never be reached for a `@ws` route.
    struct OpenOnlyProvider;
    #[async_trait::async_trait]
    impl SiteStreamProvider for OpenOnlyProvider {
        async fn open(
            &self,
            _route: &RouteSpec,
            _auth: Option<&SiteAuthContext>,
            _request: Value,
            _body: Option<axum::body::Bytes>,
        ) -> Response {
            unreachable!("@ws must use the upgrade entry point");
        }
    }

    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("site.harn");
    // No @scopes so admission is unconditional and the request reaches
    // the provider's default `upgrade`.
    std::fs::write(
        &path,
        r#"
@ws
@route("GET", "/socket")
pub fn socket(req: dict) -> dict { return http_ok({}) }
"#,
    )
    .expect("write script");
    let config =
        SiteServerConfig::new(build_core(&path)).with_stream_provider(Arc::new(OpenOnlyProvider));
    let router = SiteServer::new(config).router().expect("site router");

    // A live socket is required: the `WebSocketUpgrade` extractor needs
    // the real `OnUpgrade` connection state to succeed and reach the
    // provider's default `upgrade`, which then refuses with 426.
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let url = format!("ws://{addr}/socket");
    let error = tokio_tungstenite::connect_async(url)
        .await
        .expect_err("the default upgrade impl must refuse the handshake");
    let tokio_tungstenite::tungstenite::Error::Http(response) = error else {
        panic!("expected an HTTP rejection, got {error:?}");
    };
    assert_eq!(response.status(), StatusCode::UPGRADE_REQUIRED);
    let body = response
        .body()
        .as_ref()
        .map(|bytes| String::from_utf8_lossy(bytes).into_owned())
        .unwrap_or_default();
    assert!(
        body.contains("ws_upgrade_unsupported"),
        "unexpected body: {body}"
    );
}

/// Combined `@ws @stream` route, WebSocket branch: a genuine handshake on
/// the route upgrades through `provider.upgrade(...)` and the socket
/// echoes, exactly as a `@ws`-only route would — the `@stream` flag does
/// not steal a real upgrade.
#[tokio::test]
async fn combined_route_upgrades_a_genuine_handshake() {
    use tokio_tungstenite::tungstenite::protocol::Message as TungMessage;

    let hook = Arc::new(AllowAuth {
        tenant: Some("acme"),
        scopes: &["acp:read"],
    });
    let (_dir, router) = combined_router(Some(hook));

    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });

    let url = format!("ws://{addr}/acp/s-1");
    let (mut socket, _response) = tokio_tungstenite::connect_async(url).await.unwrap();

    let primed = socket.next().await.unwrap().unwrap();
    assert_eq!(primed, TungMessage::Text("via:upgrade".into()));
    socket.send(TungMessage::Text("ping".into())).await.unwrap();
    let echoed = socket.next().await.unwrap().unwrap();
    assert_eq!(echoed, TungMessage::Text("echo:ping".into()));
    socket.send(TungMessage::Close(None)).await.unwrap();
}

/// Combined `@ws @stream` route, stream branch: a plain GET to the *same*
/// route is not refused with a 4xx (as a `@ws`-only route would be) — it
/// falls through to `provider.open(...)` and gets the SSE/stream-path
/// response, carrying the matched route + hook identity.
#[tokio::test]
async fn combined_route_falls_through_to_stream_for_plain_get() {
    let hook = Arc::new(AllowAuth {
        tenant: Some("acme"),
        scopes: &["acp:read"],
    });
    let (_dir, router) = combined_router(Some(hook));
    let request = Request::builder()
        .uri("/acp/s-7")
        .body(Body::empty())
        .unwrap();
    let (status, body) = read_body(router.oneshot(request).await.unwrap()).await;
    assert_eq!(
        status,
        StatusCode::OK,
        "a plain GET to a combined @ws @stream route must hit the stream path, not a 4xx"
    );
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["via"], "open");
    assert_eq!(value["route"]["path"], "/acp/{session}");
    assert_eq!(value["tenant"], "acme");
    assert_eq!(value["session"], "s-7");
}

/// A request carrying a half-upgrade header set that is *not* a genuine
/// handshake (no `Sec-WebSocket-Key`) is not treated as an upgrade on a
/// combined route: it falls through to the stream path rather than being
/// handed to the `WebSocketUpgrade` extractor (which would 400 it).
#[tokio::test]
async fn combined_route_partial_upgrade_headers_fall_through_to_stream() {
    let hook = Arc::new(AllowAuth {
        tenant: Some("acme"),
        scopes: &["acp:read"],
    });
    let (_dir, router) = combined_router(Some(hook));
    let request = Request::builder()
        .uri("/acp/s-9")
        // `Upgrade: websocket` but no `Sec-WebSocket-Key`: not a genuine
        // handshake by `is_websocket_upgrade`'s contract.
        .header("upgrade", "websocket")
        .body(Body::empty())
        .unwrap();
    let (status, body) = read_body(router.oneshot(request).await.unwrap()).await;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["via"], "open");
}

/// Auth gates *both* branches of a combined route identically: a denied
/// request never reaches `upgrade` (handshake) or `open` (plain GET).
#[tokio::test]
async fn combined_route_auth_gates_both_branches() {
    // Handshake request with a denying hook → embedder 401, no upgrade.
    let (_dir, router) = combined_router(Some(Arc::new(DenyAuth)));
    let ws_request = Request::builder()
        .uri("/acp/s-1")
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .body(Body::empty())
        .unwrap();
    let response = router.oneshot(ws_request).await.unwrap();
    assert_eq!(response.headers()["x-embedder-deny"], "1");
    let (status, body) = read_body(response).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("custom_embedder_denial"));

    // Plain GET with a denying hook → same embedder 401, no `open`.
    let (_dir, router) = combined_router(Some(Arc::new(DenyAuth)));
    let plain_request = Request::builder()
        .uri("/acp/s-1")
        .body(Body::empty())
        .unwrap();
    let (status, body) = read_body(router.oneshot(plain_request).await.unwrap()).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("custom_embedder_denial"));
}

/// Scope shortfall gates both branches of a combined route before either
/// provider entry point: a handshake *and* a plain GET get the canonical
/// 403, even with a valid identity that lacks the route's `@scopes`.
#[tokio::test]
async fn combined_route_scope_shortfall_gates_both_branches() {
    let hook = || {
        Arc::new(AllowAuth {
            tenant: Some("acme"),
            scopes: &["acp:write"], // not acp:read
        })
    };

    let (_dir, router) = combined_router(Some(hook()));
    let ws_request = Request::builder()
        .uri("/acp/s-1")
        .header("connection", "upgrade")
        .header("upgrade", "websocket")
        .header("sec-websocket-version", "13")
        .header("sec-websocket-key", "dGhlIHNhbXBsZSBub25jZQ==")
        .body(Body::empty())
        .unwrap();
    let (status, body) = read_body(router.oneshot(ws_request).await.unwrap()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["code"],
        "forbidden"
    );

    let (_dir, router) = combined_router(Some(hook()));
    let plain_request = Request::builder()
        .uri("/acp/s-1")
        .body(Body::empty())
        .unwrap();
    let (status, body) = read_body(router.oneshot(plain_request).await.unwrap()).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(
        serde_json::from_str::<Value>(&body).unwrap()["code"],
        "forbidden"
    );
}
