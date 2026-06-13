//! Embedder auth hook conformance for `harn serve site` (#3212).
//!
//! Exercises the [`harn_serve::SiteAuth`] route-level hook end-to-end
//! through a real router: the no-hook default is unchanged, an `Allow`
//! threads tenant + scopes into the dispatch (so `@scopes` admission and
//! `harness.tenant.id()` agree with the embedder), a scope shortfall is
//! refused with the canonical `forbidden` envelope, a `Deny` response
//! passes through verbatim, and the opaque embedder context is visible
//! to a [`harn_vm::HostCallBridge`] for the duration of the dispatch.
//!
//! All cases drive the router through `tower::ServiceExt::oneshot`,
//! mirroring `tests/site_hosting.rs`.

use std::path::Path;
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::request::Parts;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use harn_serve::{
    current_auth_context, DispatchCore, DispatchCoreConfig, DispatchError, NoReplayCache,
    RouteSpec, SiteAuth, SiteAuthContext, SiteAuthOutcome, SiteServer, SiteServerConfig,
    VmConfigurator,
};
use harn_vm::{HostCallBridge, TenantId, Vm, VmError, VmValue};
use serde_json::{json, Value};
use tower::ServiceExt;

/// One script backs every case: a public route, a `@scopes`-guarded
/// route that also reports the bound tenant, and a route that asks the
/// embedder's host-call bridge for the ambient auth context.
const SITE_SCRIPT: &str = r#"
@route("GET", "/public")
pub fn open_route(req: dict) -> dict {
  return http_ok({ "ok": true })
}

@scopes("personas:read")
@route("GET", "/scoped")
pub fn scoped_route(harness: Harness, req: dict) -> dict {
  return http_ok({ "tenant": harness.tenant.try_id() })
}

@route("GET", "/ctx")
pub fn ctx_route(req: dict) -> dict {
  return http_ok({ "ctx": host_call("embedder.auth_context", {}) })
}

@scopes("base:read", "PUT personas:write")
@route("*", "/per_method")
pub fn per_method_route(req: dict) -> dict {
  return http_ok({ "ok": true })
}
"#;

/// `SiteAuth` hook that always admits with a fixed identity.
struct AllowAuth {
    tenant: Option<&'static str>,
    scopes: &'static [&'static str],
    context: Option<Value>,
}

#[async_trait::async_trait]
impl SiteAuth for AllowAuth {
    async fn authenticate(&self, _parts: &Parts, _route: &RouteSpec) -> SiteAuthOutcome {
        SiteAuthOutcome::Allow(SiteAuthContext {
            tenant_id: self.tenant.map(TenantId::new),
            scopes: self.scopes.iter().map(|scope| scope.to_string()).collect(),
            context: self.context.clone(),
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

/// `SiteAuth` hook that authenticates off the request head: a bearer
/// header gates admission, and the matched route template is echoed
/// into the context — proof the hook sees both `Parts` and `RouteSpec`.
struct HeaderAuth;

#[async_trait::async_trait]
impl SiteAuth for HeaderAuth {
    async fn authenticate(&self, parts: &Parts, route: &RouteSpec) -> SiteAuthOutcome {
        let authorized = parts
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            == Some("Bearer letmein");
        if !authorized {
            return SiteAuthOutcome::deny(
                (StatusCode::UNAUTHORIZED, Json(json!({ "error": "no_key" }))).into_response(),
            );
        }
        SiteAuthOutcome::Allow(SiteAuthContext {
            tenant_id: None,
            scopes: ["personas:read".to_string()].into(),
            context: Some(json!({ "route": route.path, "method": route.method })),
        })
    }
}

/// Host-call bridge that hands the ambient embedder auth context back
/// to the `.harn` handler as a JSON string (or `"absent"`).
struct ContextEchoBridge;

impl HostCallBridge for ContextEchoBridge {
    fn dispatch(
        &self,
        capability: &str,
        operation: &str,
        _params: &harn_vm::value::DictMap,
    ) -> Result<Option<VmValue>, VmError> {
        if capability != "embedder" || operation != "auth_context" {
            return Ok(None);
        }
        let rendered = current_auth_context()
            .map(|context| context.to_string())
            .unwrap_or_else(|| "absent".to_string());
        Ok(Some(VmValue::String(Arc::from(rendered.as_str()))))
    }
}

/// `VmConfigurator` that installs [`ContextEchoBridge`] on the dispatch
/// thread, the way an embedder wires its host-call bridge in.
struct BridgeConfigurator;

impl VmConfigurator for BridgeConfigurator {
    fn configure(&self, _vm: &mut Vm) -> Result<(), DispatchError> {
        harn_vm::set_host_call_bridge(Arc::new(ContextEchoBridge));
        Ok(())
    }
}

fn build_core(path: &Path, configurator: Option<Arc<dyn VmConfigurator>>) -> DispatchCore {
    let mut config = DispatchCoreConfig::for_script(path);
    // An HTTP host must re-run its handler on every request.
    config.replay_cache = Arc::new(NoReplayCache);
    if let Some(configurator) = configurator {
        config.vm_configurator = configurator;
    }
    DispatchCore::new(config).expect("dispatch core")
}

/// Write the shared script to a temp dir and build a site router over
/// it, optionally with an auth hook and a VM configurator. The temp dir
/// is returned so it outlives the router.
fn site_router(
    auth: Option<Arc<dyn SiteAuth>>,
    configurator: Option<Arc<dyn VmConfigurator>>,
) -> (tempfile::TempDir, Router) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("site.harn");
    std::fs::write(&path, SITE_SCRIPT).expect("write script");
    let mut config = SiteServerConfig::new(build_core(&path, configurator));
    if let Some(auth) = auth {
        config = config.with_auth(auth);
    }
    let router = SiteServer::new(config).router().expect("site router");
    (dir, router)
}

async fn get(router: Router, uri: &str) -> Response {
    request(router, "GET", uri).await
}

async fn request(router: Router, method: &str, uri: &str) -> Response {
    router
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap()
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

/// (a) Without a hook the adapter behaves exactly as before: public
/// routes serve, and a `@scopes` route is refused by the dispatch-level
/// check because the allow-all policy grants no scopes.
#[tokio::test]
async fn default_without_hook_is_unchanged() {
    let (_dir, router) = site_router(None, None);
    let (status, body) = read_json(get(router.clone(), "/public").await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);

    let (status, body) = read_json(get(router, "/scoped").await).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "forbidden");
}

/// (b) An `Allow` carrying tenant + scopes admits the scoped route and
/// the dispatch runs under that tenant (`harness.tenant` sees it).
#[tokio::test]
async fn allow_with_tenant_and_scopes_reaches_scoped_route() {
    let hook = Arc::new(AllowAuth {
        tenant: Some("acme"),
        scopes: &["personas:read", "sessions:write"],
        context: None,
    });
    let (_dir, router) = site_router(Some(hook), None);
    let (status, body) = read_json(get(router, "/scoped").await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["tenant"], "acme");
}

/// (c) An `Allow` whose scopes do not cover the route's `@scopes` is
/// refused at admission with the canonical `forbidden` envelope.
#[tokio::test]
async fn allow_with_missing_scopes_yields_canonical_403() {
    let hook = Arc::new(AllowAuth {
        tenant: Some("acme"),
        scopes: &["sessions:read"],
        context: None,
    });
    let (_dir, router) = site_router(Some(hook), None);
    let (status, body) = read_json(get(router, "/scoped").await).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "forbidden");
    assert_eq!(body["details"]["kind"], "forbidden");
    assert_eq!(body["details"]["missing_scopes"][0], "personas:read");
    assert_eq!(body["details"]["granted_scopes"][0], "sessions:read");
    assert!(body["request_id"].as_str().is_some());
}

/// (d) A `Deny` returns the embedder's response verbatim — status,
/// headers, and body untouched.
#[tokio::test]
async fn deny_passes_embedder_response_through_verbatim() {
    let (_dir, router) = site_router(Some(Arc::new(DenyAuth)), None);
    let response = get(router, "/public").await;
    assert_eq!(response.headers()["x-embedder-deny"], "1");
    let (status, body) = read_json(response).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "custom_embedder_denial");
}

/// (e) A route with no `@scopes` plus an `Allow` with no tenant and no
/// scopes serves fine — public routes need only the hook's blessing.
#[tokio::test]
async fn public_route_admits_tenantless_scopeless_allow() {
    let hook = Arc::new(AllowAuth {
        tenant: None,
        scopes: &[],
        context: None,
    });
    let (_dir, router) = site_router(Some(hook), None);
    let (status, body) = read_json(get(router, "/public").await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
}

/// The opaque embedder context rides the dispatch onto the VM thread,
/// where the embedder's host-call bridge recovers it via
/// `current_auth_context()` while serving a handler's `host_call`.
#[tokio::test]
async fn auth_context_is_visible_to_the_host_call_bridge() {
    let hook = Arc::new(AllowAuth {
        tenant: None,
        scopes: &[],
        context: Some(json!({ "key_id": "k_123", "session": "s_9" })),
    });
    let (_dir, router) = site_router(Some(hook), Some(Arc::new(BridgeConfigurator)));
    let (status, body) = read_json(get(router, "/ctx").await).await;
    assert_eq!(status, StatusCode::OK);
    let echoed: Value =
        serde_json::from_str(body["ctx"].as_str().expect("bridge echoes a string")).unwrap();
    assert_eq!(echoed, json!({ "key_id": "k_123", "session": "s_9" }));
}

/// Without an `Allow`-supplied context the bridge sees no ambient
/// value: nothing leaks across requests on the shared dispatch thread.
#[tokio::test]
async fn absent_auth_context_is_absent_at_the_bridge() {
    let hook = Arc::new(AllowAuth {
        tenant: None,
        scopes: &[],
        context: None,
    });
    let (_dir, router) = site_router(Some(hook), Some(Arc::new(BridgeConfigurator)));
    let (status, body) = read_json(get(router, "/ctx").await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ctx"], "absent");
}

/// The hook authenticates off the request head and sees the matched
/// route: a bearer header gates admission, and the `RouteSpec` handed
/// to the hook round-trips through the context into the bridge.
#[tokio::test]
async fn hook_sees_request_head_and_matched_route() {
    let (_dir, router) = site_router(
        Some(Arc::new(HeaderAuth)),
        Some(Arc::new(BridgeConfigurator)),
    );

    let denied = get(router.clone(), "/ctx").await;
    let (status, body) = read_json(denied).await;
    assert_eq!(status, StatusCode::UNAUTHORIZED);
    assert_eq!(body["error"], "no_key");

    let allowed = router
        .oneshot(
            Request::builder()
                .uri("/ctx")
                .header("authorization", "Bearer letmein")
                .body(Body::empty())
                .unwrap(),
        )
        .await
        .unwrap();
    let (status, body) = read_json(allowed).await;
    assert_eq!(status, StatusCode::OK);
    let echoed: Value =
        serde_json::from_str(body["ctx"].as_str().expect("bridge echoes a string")).unwrap();
    assert_eq!(echoed, json!({ "route": "/ctx", "method": "GET" }));
}

/// Per-method `@scopes`: `/per_method` declares the baseline `base:read`
/// for every method plus `personas:write` only for `PUT`. A `GET` needs
/// just the baseline — an `Allow` carrying `base:read` admits it even
/// without `personas:write`, proving the per-method extra is scoped to
/// `PUT` and `GET` falls back to the baseline alone.
#[tokio::test]
async fn per_method_get_requires_only_the_baseline_scope() {
    let hook = Arc::new(AllowAuth {
        tenant: None,
        scopes: &["base:read"],
        context: None,
    });
    let (_dir, router) = site_router(Some(hook), None);
    let (status, body) = read_json(request(router, "GET", "/per_method").await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
}

/// The same baseline-only credential is *insufficient* for `PUT`, which
/// unions `personas:write` onto the baseline: admission refuses with the
/// canonical envelope, and the missing scope is exactly the per-method
/// extra (not the already-granted baseline).
#[tokio::test]
async fn per_method_put_demands_the_method_scoped_extra() {
    let hook = Arc::new(AllowAuth {
        tenant: None,
        scopes: &["base:read"],
        context: None,
    });
    let (_dir, router) = site_router(Some(hook), None);
    let (status, body) = read_json(request(router, "PUT", "/per_method").await).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "forbidden");
    assert_eq!(body["details"]["missing_scopes"][0], "personas:write");
    assert_eq!(body["details"]["required_scopes"][0], "base:read");
    assert_eq!(body["details"]["required_scopes"][1], "personas:write");
}

/// A credential carrying both the baseline and the per-method extra
/// admits `PUT` — the union is satisfied.
#[tokio::test]
async fn per_method_put_admits_with_baseline_plus_extra() {
    let hook = Arc::new(AllowAuth {
        tenant: None,
        scopes: &["base:read", "personas:write"],
        context: None,
    });
    let (_dir, router) = site_router(Some(hook), None);
    let (status, body) = read_json(request(router, "PUT", "/per_method").await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
}

/// A method with no per-method extra (here `DELETE`) falls back to the
/// baseline requirement — `base:read` alone admits it, same as `GET`.
#[tokio::test]
async fn per_method_unlisted_method_falls_back_to_baseline() {
    let hook = Arc::new(AllowAuth {
        tenant: None,
        scopes: &["base:read"],
        context: None,
    });
    let (_dir, router) = site_router(Some(hook), None);
    let (status, body) = read_json(request(router, "DELETE", "/per_method").await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
}
