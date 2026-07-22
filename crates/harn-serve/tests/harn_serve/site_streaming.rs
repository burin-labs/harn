//! Host-streamed response bodies through `harn serve site` (#3213).
//!
//! Exercises the `@stream` route marker + [`harn_serve::SiteStreamProvider`]
//! seam end-to-end through a real router: an admitted request on a
//! `@stream` route reaches the embedder's provider (which sees the
//! matched route, the hook-resolved identity, and the request head) and
//! the provider's SSE body streams back verbatim; admission — the
//! [`harn_serve::SiteAuth`] hook and the route's `@scopes` — still gates
//! the route, refusing *before* the provider is consulted; a non-stream
//! route on the same script dispatches into the VM unchanged; and a
//! script that declares a `@stream` route refuses to build a router
//! without a provider.
//!
//! All cases drive the router through `tower::ServiceExt::oneshot`,
//! mirroring `tests/site_auth.rs`.

use std::convert::Infallible;
use std::path::Path;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::request::Parts;
use axum::http::{Request, StatusCode};
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use harn_serve::{
    DispatchCore, DispatchCoreConfig, NoReplayCache, RouteSpec, SiteAuth, SiteAuthContext,
    SiteAuthOutcome, SiteServer, SiteServerConfig, SiteStreamProvider,
};
use harn_vm::TenantId;
use serde_json::{json, Value};
use tower::ServiceExt;

/// One script backs every case: a `@scopes`-guarded `@stream` route and
/// a plain dispatch route. The stream route's body is a declaration-only
/// stub — if it ever runs, the VM dispatched where the provider should
/// have answered, and the assertions below catch the buffered sentinel.
const SITE_SCRIPT: &str = r#"
@stream
@scopes("events:read")
@route("GET", "/topics/{topic}/events")
pub fn topic_events(req: dict) -> dict {
  return http_ok({ "buffered_stub": true })
}

@route("GET", "/plain")
pub fn plain(req: dict) -> dict {
  return http_ok({ "plain": true })
}
"#;

/// Test provider: emits three SSE events and ends. The first event
/// echoes what the provider was given (route, tenant, path params,
/// query), so one body read proves the whole contract.
struct ThreeEventProvider {
    calls: Arc<AtomicUsize>,
}

#[async_trait::async_trait]
impl SiteStreamProvider for ThreeEventProvider {
    async fn open(
        &self,
        route: &RouteSpec,
        auth: Option<&SiteAuthContext>,
        request: Value,
        body: Option<axum::body::Bytes>,
    ) -> Response {
        self.calls.fetch_add(1, Ordering::SeqCst);
        // A @stream route never reads the request body.
        assert!(body.is_none(), "stream route must not buffer a body");
        let opened = json!({
            "route": { "method": route.method, "path": route.path },
            "tenant": auth.and_then(|context| context.tenant_id.as_ref()).map(|t| t.0.clone()),
            "topic": request["path_params"]["topic"],
            "since": request["query"]["since"],
        });
        let events = vec![
            Event::default().event("opened").data(opened.to_string()),
            Event::default().data("two"),
            Event::default().data("three"),
        ];
        let stream = futures::stream::iter(events.into_iter().map(Ok::<_, Infallible>));
        Sse::new(stream)
            .keep_alive(KeepAlive::default())
            .into_response()
    }
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
/// with the counting provider, optionally behind an auth hook. The temp
/// dir is returned so it outlives the router.
fn streaming_router(
    auth: Option<Arc<dyn SiteAuth>>,
) -> (tempfile::TempDir, Router, Arc<AtomicUsize>) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("site.harn");
    std::fs::write(&path, SITE_SCRIPT).expect("write script");
    let calls = Arc::new(AtomicUsize::new(0));
    let mut config = SiteServerConfig::new(build_core(&path)).with_stream_provider(Arc::new(
        ThreeEventProvider {
            calls: calls.clone(),
        },
    ));
    if let Some(auth) = auth {
        config = config.with_auth(auth);
    }
    let router = SiteServer::new(config).router().expect("site router");
    (dir, router, calls)
}

async fn get(router: Router, uri: &str) -> Response {
    router
        .oneshot(Request::builder().uri(uri).body(Body::empty()).unwrap())
        .await
        .unwrap()
}

async fn read_body(response: Response) -> (StatusCode, String) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// The happy path: an admitted request on the `@stream` route receives
/// the provider's three SSE events — and the first event proves the
/// provider saw the matched route, the hook's tenant, the captured path
/// param, and the query string. The `.harn` stub body never runs.
#[tokio::test]
async fn stream_route_delivers_provider_sse_events() {
    let hook = Arc::new(AllowAuth {
        tenant: Some("acme"),
        scopes: &["events:read"],
    });
    let (_dir, router, calls) = streaming_router(Some(hook));
    let response = get(router, "/topics/deploys/events?since=42").await;
    assert_eq!(
        response.headers()["content-type"].to_str().unwrap(),
        "text/event-stream"
    );
    let (status, body) = read_body(response).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    // Three events arrived, in order, SSE-framed.
    assert!(
        body.contains("event: opened"),
        "missing first event: {body}"
    );
    assert!(body.contains("data: two"), "missing second event: {body}");
    assert!(body.contains("data: three"), "missing third event: {body}");
    assert!(
        !body.contains("buffered_stub"),
        "the .harn stub body must never dispatch for a @stream route: {body}"
    );

    // The first event echoes the provider's inputs.
    let opened_line = body
        .lines()
        .find(|line| line.starts_with("data: {"))
        .expect("opened event data line");
    let opened: Value = serde_json::from_str(opened_line.trim_start_matches("data: ")).unwrap();
    assert_eq!(opened["route"]["method"], "GET");
    assert_eq!(opened["route"]["path"], "/topics/{topic}/events");
    assert_eq!(opened["tenant"], "acme");
    assert_eq!(opened["topic"], "deploys");
    assert_eq!(opened["since"], "42");
}

/// A `SiteAuth` `Deny` gates the stream route exactly as it gates a
/// dispatch route: the embedder response comes back verbatim and the
/// provider is never consulted.
#[tokio::test]
async fn auth_deny_refuses_stream_route_before_provider_opens() {
    let (_dir, router, calls) = streaming_router(Some(Arc::new(DenyAuth)));
    let response = get(router, "/topics/deploys/events").await;
    assert_eq!(response.headers()["x-embedder-deny"], "1");
    let (status, body) = read_body(response).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert!(body.contains("custom_embedder_denial"));
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// An admitted identity whose scopes do not cover the route's `@scopes`
/// is refused with the canonical `forbidden` envelope — again before the
/// provider is consulted.
#[tokio::test]
async fn scope_shortfall_yields_403_before_provider_opens() {
    let hook = Arc::new(AllowAuth {
        tenant: Some("acme"),
        scopes: &["events:write"],
    });
    let (_dir, router, calls) = streaming_router(Some(hook));
    let (status, body) = read_body(get(router, "/topics/deploys/events").await).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let envelope: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(envelope["code"], "forbidden");
    assert_eq!(envelope["details"]["missing_scopes"][0], "events:read");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// A non-stream route on the same script is untouched by the provider:
/// it dispatches into the VM and renders its buffered JSON reply.
#[tokio::test]
async fn non_stream_route_still_dispatches_into_the_vm() {
    let hook = Arc::new(AllowAuth {
        tenant: None,
        scopes: &[],
    });
    let (_dir, router, calls) = streaming_router(Some(hook));
    let (status, body) = read_body(get(router, "/plain").await).await;
    assert_eq!(status, StatusCode::OK);
    let value: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(value["plain"], true);
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// A `@stream` route never builds a `CallRequest`, so the dispatch-level
/// scope check cannot back-stop its `@scopes`. The adapter enforces them
/// itself: with no hook installed there is no identity and no granted
/// scopes, so a scoped stream route refuses with the canonical 403
/// before the provider opens — same outcome a scoped plain route gets
/// from the allow-all default at dispatch.
#[tokio::test]
async fn scoped_stream_route_without_hook_is_refused_at_admission() {
    let (_dir, router, calls) = streaming_router(None);
    let (status, body) = read_body(get(router, "/topics/deploys/events").await).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    let envelope: Value = serde_json::from_str(&body).unwrap();
    assert_eq!(envelope["code"], "forbidden");
    assert_eq!(calls.load(Ordering::SeqCst), 0);
}

/// Declaring a `@stream` route without installing a provider is a
/// configuration error surfaced at router-build time, not a 500 at
/// request time.
#[tokio::test]
async fn stream_route_without_provider_refuses_to_build() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("site.harn");
    std::fs::write(&path, SITE_SCRIPT).expect("write script");
    let config = SiteServerConfig::new(build_core(&path));
    let error = SiteServer::new(config).router().expect_err("must refuse");
    assert!(
        error.contains("@stream") && error.contains("with_stream_provider"),
        "unhelpful error: {error}"
    );
}
