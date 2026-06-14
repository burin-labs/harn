//! Raw/binary request + response bodies through `harn serve site` (#3214).
//!
//! Two halves, one seam:
//!
//! * **Responses** — HS-2's `@stream` + [`harn_serve::SiteStreamProvider`]
//!   already generalizes to binary downloads: the provider returns *any*
//!   axum `Response`, which the adapter forwards verbatim. The first test
//!   proves it — a deliberately non-UTF-8 `.harnpack`-shaped body
//!   round-trips byte-exact, `Content-Type` and `Content-Disposition`
//!   intact, never touching the utf8-lossy JSON envelope.
//! * **Requests** — the new bare `@raw` marker: like `@stream` the route
//!   is answered by the provider after admission, but the request body
//!   *is* buffered (up to the body limit) and handed over as exact
//!   [`Bytes`] — the channel for binary/multipart uploads (pack publish).
//!
//! Admission — the [`harn_serve::SiteAuth`] hook and the route's
//! `@scopes` — still gates `@raw` routes *before* the body is read or the
//! provider is consulted, and a `@raw` route without a provider refuses
//! to build. All cases drive the router through
//! `tower::ServiceExt::oneshot`, mirroring `tests/site_streaming.rs`.

use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};

use axum::body::{to_bytes, Body, Bytes};
use axum::http::header::{CONTENT_DISPOSITION, CONTENT_TYPE};
use axum::http::request::Parts;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use harn_serve::{
    DispatchCore, DispatchCoreConfig, NoReplayCache, RouteSpec, SiteAuth, SiteAuthContext,
    SiteAuthOutcome, SiteServer, SiteServerConfig, SiteStreamProvider,
};
use harn_vm::TenantId;
use serde_json::{json, Value};
use tower::ServiceExt;

/// One script backs every case: an unscoped `@stream` download route
/// (binary response half) and a `@scopes`-guarded `@raw` publish route
/// (binary request half). Both bodies are declaration-only stubs — if
/// either ever runs, the VM dispatched where the provider should have
/// answered, and the assertions below catch the buffered sentinel.
const SITE_SCRIPT: &str = r#"
@stream
@route("GET", "/packs/{name}/download")
pub fn pack_download(req: dict) -> dict {
  return http_ok({ "buffered_stub": true })
}

@raw
@scopes("packs:write")
@route("POST", "/packs/publish")
pub fn pack_publish(req: dict) -> dict {
  return http_ok({ "buffered_stub": true })
}
"#;

const PACK_CONTENT_TYPE: &str = "application/vnd.harn.harnpack";
const PACK_DISPOSITION: &str = "attachment; filename=\"demo.harnpack\"";

/// A `.harnpack`-shaped payload that is deliberately *not* valid UTF-8:
/// a gzip-ish magic prefix, an embedded NUL, lone continuation bytes,
/// and every byte value once. If any utf8-lossy conversion touches it,
/// byte equality fails.
fn pack_bytes() -> Vec<u8> {
    let mut bytes = vec![0x1f, 0x8b, 0x00, 0xff, 0xfe, 0x80];
    bytes.extend(0u8..=255);
    bytes
}

/// A multipart/form-data payload whose `bundle` part is the same
/// non-UTF-8 binary — the wire shape of a pack publish.
fn multipart_publish_body() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(b"--BOUNDARY\r\n");
    body.extend_from_slice(b"Content-Disposition: form-data; name=\"owner\"\r\n\r\n");
    body.extend_from_slice(b"acme\r\n");
    body.extend_from_slice(b"--BOUNDARY\r\n");
    body.extend_from_slice(
        b"Content-Disposition: form-data; name=\"bundle\"; filename=\"demo.harnpack\"\r\n",
    );
    body.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
    body.extend_from_slice(&pack_bytes());
    body.extend_from_slice(b"\r\n--BOUNDARY--\r\n");
    body
}

/// What the provider saw for one `open(...)` call. `body` stays `None`
/// both before any call and for a `@stream` call (which receives no
/// body) — the `calls` counter is the called-at-all signal.
#[derive(Default)]
struct SeenRequest {
    request: Option<Value>,
    body: Option<Bytes>,
}

/// Test provider answering both routes: the download route returns the
/// binary pack verbatim; the publish route records the exact request +
/// body it was handed and acknowledges with JSON.
struct PackProvider {
    calls: Arc<AtomicUsize>,
    seen: Arc<Mutex<SeenRequest>>,
}

#[async_trait::async_trait]
impl SiteStreamProvider for PackProvider {
    async fn open(
        &self,
        route: &RouteSpec,
        _auth: Option<&SiteAuthContext>,
        request: Value,
        body: Option<Bytes>,
    ) -> Response {
        self.calls.fetch_add(1, Ordering::SeqCst);
        {
            let mut seen = self.seen.lock().unwrap();
            seen.request = Some(request);
            seen.body = body.clone();
        }
        if route.path.ends_with("/download") {
            // Binary response half: an embedder-shaped download, exactly
            // how harn-cloud serves a `.harnpack` / CAS blob.
            return (
                StatusCode::OK,
                [
                    (CONTENT_TYPE, PACK_CONTENT_TYPE),
                    (CONTENT_DISPOSITION, PACK_DISPOSITION),
                ],
                pack_bytes(),
            )
                .into_response();
        }
        (
            StatusCode::CREATED,
            Json(json!({ "received_bytes": body.map(|b| b.len()).unwrap_or(0) })),
        )
            .into_response()
    }
}

/// `SiteAuth` hook that always admits with a fixed identity.
struct AllowAuth {
    scopes: &'static [&'static str],
}

#[async_trait::async_trait]
impl SiteAuth for AllowAuth {
    async fn authenticate(&self, _parts: &Parts, _route: &RouteSpec) -> SiteAuthOutcome {
        SiteAuthOutcome::Allow(SiteAuthContext {
            tenant_id: Some(TenantId::new("acme")),
            scopes: self.scopes.iter().map(|scope| scope.to_string()).collect(),
            context: None,
            ..Default::default()
        })
    }
}

/// `SiteAuth` hook that always refuses with an embedder-shaped response.
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
    // An HTTP host must re-run its handler on every request.
    config.replay_cache = Arc::new(NoReplayCache);
    DispatchCore::new(config).expect("dispatch core")
}

/// Write the shared script to a temp dir and build a site router over it
/// with the recording provider, optionally behind an auth hook. The temp
/// dir is returned so it outlives the router.
fn pack_router(
    auth: Option<Arc<dyn SiteAuth>>,
) -> (
    tempfile::TempDir,
    Router,
    Arc<AtomicUsize>,
    Arc<Mutex<SeenRequest>>,
) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("site.harn");
    std::fs::write(&path, SITE_SCRIPT).expect("write script");
    let calls = Arc::new(AtomicUsize::new(0));
    let seen = Arc::new(Mutex::new(SeenRequest::default()));
    let mut config =
        SiteServerConfig::new(build_core(&path)).with_stream_provider(Arc::new(PackProvider {
            calls: calls.clone(),
            seen: seen.clone(),
        }));
    if let Some(auth) = auth {
        config = config.with_auth(auth);
    }
    let router = SiteServer::new(config).router().expect("site router");
    (dir, router, calls, seen)
}

fn publish_request(body: Vec<u8>) -> Request<Body> {
    Request::builder()
        .method("POST")
        .uri("/packs/publish")
        .header("content-type", "multipart/form-data; boundary=BOUNDARY")
        .body(Body::from(body))
        .unwrap()
}

/// The response half of #3214 needs no new machinery: `@stream` already
/// hands the route to a provider whose `Response` is forwarded verbatim,
/// and a binary download is just such a response. This proves it
/// byte-exact: a payload that is *not* valid UTF-8 (any lossy conversion
/// would corrupt it) comes back identical, with `Content-Type` and
/// `Content-Disposition` preserved — and the `.harn` stub never runs.
#[tokio::test]
async fn stream_route_binary_response_round_trips_byte_exact() {
    let expected = pack_bytes();
    assert!(
        String::from_utf8(expected.clone()).is_err(),
        "test payload must be invalid UTF-8 to prove no lossy conversion happens"
    );

    let (_dir, router, calls, _seen) = pack_router(None);
    let response = router
        .oneshot(
            Request::builder()
                .uri("/packs/demo/download")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::OK);
    assert_eq!(response.headers()[CONTENT_TYPE], PACK_CONTENT_TYPE);
    assert_eq!(response.headers()[CONTENT_DISPOSITION], PACK_DISPOSITION);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    assert_eq!(
        body.as_ref(),
        expected.as_slice(),
        "binary response must round-trip byte-exact"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);
}

/// The request half: a multipart publish whose bundle part is invalid
/// UTF-8 reaches the provider as the exact bytes that were sent — no
/// utf8-lossy view, no base64 envelope — while the JSON head dict stays
/// body-free and keeps the `content-type` boundary the provider needs to
/// parse the multipart itself. The `.harn` stub never runs.
#[tokio::test]
async fn raw_route_hands_exact_request_bytes_to_provider() {
    let payload = multipart_publish_body();
    assert!(
        String::from_utf8(payload.clone()).is_err(),
        "test payload must be invalid UTF-8 to prove no lossy conversion happens"
    );

    let hook = Arc::new(AllowAuth {
        scopes: &["packs:write"],
    });
    let (_dir, router, calls, seen) = pack_router(Some(hook));
    let response = router
        .oneshot(publish_request(payload.clone()))
        .await
        .unwrap();

    assert_eq!(response.status(), StatusCode::CREATED);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let ack: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(ack["received_bytes"], payload.len());
    assert!(
        !body.windows(13).any(|w| w == b"buffered_stub"),
        "the .harn stub body must never dispatch for a @raw route"
    );
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    let seen = seen.lock().unwrap();
    let received = seen
        .body
        .as_ref()
        .expect("@raw route must hand the provider Some(body)");
    assert_eq!(
        received.as_ref(),
        payload.as_slice(),
        "request bytes must reach the provider uncorrupted"
    );
    let request = seen.request.as_ref().expect("provider saw the head dict");
    assert_eq!(
        request["headers"]["content-type"],
        "multipart/form-data; boundary=BOUNDARY"
    );
    // The raw bytes travel only through the `body` parameter — the JSON
    // head dict never carries (a lossy view of) the payload.
    assert_eq!(request["body"], "");
    assert_eq!(request["body_base64"], "");
}

/// A `SiteAuth` `Deny` gates a `@raw` route exactly as it gates every
/// other route: the embedder response comes back verbatim and the
/// provider is never consulted (so the body is never even read).
#[tokio::test]
async fn auth_deny_refuses_raw_route_before_provider_opens() {
    let (_dir, router, calls, _seen) = pack_router(Some(Arc::new(DenyAuth)));
    let response = router
        .oneshot(publish_request(multipart_publish_body()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::UNAUTHORIZED);
    assert_eq!(response.headers()["x-embedder-deny"], "1");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// An admitted identity whose scopes do not cover the route's `@scopes`
/// is refused with the canonical `forbidden` envelope — again before the
/// provider is consulted.
#[tokio::test]
async fn scope_shortfall_yields_403_before_provider_opens() {
    let hook = Arc::new(AllowAuth {
        scopes: &["packs:read"],
    });
    let (_dir, router, calls, _seen) = pack_router(Some(hook));
    let response = router
        .oneshot(publish_request(multipart_publish_body()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let envelope: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(envelope["code"], "forbidden");
    assert_eq!(envelope["details"]["missing_scopes"][0], "packs:write");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// Without a hook there is no identity and no granted scopes, so a
/// scoped `@raw` route refuses with the canonical 403 at admission —
/// same back-stop HS-2 established for `@stream` routes.
#[tokio::test]
async fn scoped_raw_route_without_hook_is_refused_at_admission() {
    let (_dir, router, calls, _seen) = pack_router(None);
    let response = router
        .oneshot(publish_request(multipart_publish_body()))
        .await
        .unwrap();
    assert_eq!(response.status(), StatusCode::FORBIDDEN);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// A body over the configured limit is refused with the canonical 413
/// envelope; the provider never sees a partial payload.
#[tokio::test]
async fn raw_route_body_over_limit_is_refused_with_413() {
    let hook = Arc::new(AllowAuth {
        scopes: &["packs:write"],
    });
    let (_dir, router, calls, _seen) = pack_router(Some(hook));
    let oversized = vec![0u8; harn_serve::DEFAULT_HTTP_BODY_LIMIT_BYTES + 1];
    let response = router.oneshot(publish_request(oversized)).await.unwrap();
    assert_eq!(response.status(), StatusCode::PAYLOAD_TOO_LARGE);
    let body = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let envelope: Value = serde_json::from_slice(&body).unwrap();
    assert_eq!(envelope["code"], "request_body_too_large");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// Declaring a `@raw` route without installing a provider is a
/// configuration error surfaced at router-build time, not a 500 at
/// request time — mirroring the `@stream` rule.
#[tokio::test]
async fn raw_route_without_provider_refuses_to_build() {
    const RAW_ONLY_SCRIPT: &str = r#"
@raw
@route("POST", "/packs/publish")
pub fn pack_publish(req: dict) -> dict {
  return http_ok({ "buffered_stub": true })
}
"#;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("site.harn");
    std::fs::write(&path, RAW_ONLY_SCRIPT).expect("write script");
    let config = SiteServerConfig::new(build_core(&path));
    let error = SiteServer::new(config).router().expect_err("must refuse");
    assert!(
        error.contains("@raw") && error.contains("with_stream_provider"),
        "unhelpful error: {error}"
    );
}
