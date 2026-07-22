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
import { require_policy } from "std/harness/policy"

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

@scopes("personas:read")
@route("GET", "/whoami")
pub fn whoami_route(harness: Harness, req: dict) -> dict {
  return http_ok({
    "authed": harness.auth.is_authenticated(),
    "subject": harness.auth.try_subject(),
    "kind": harness.auth.kind(),
    "has_read": harness.auth.has_scope("personas:read"),
    "has_admin": harness.auth.has_scope("admin:write"),
    "scopes": harness.auth.scopes(),
  })
}

@scopes("personas:read")
@policy(kinds: "operator")
@route("POST", "/operator/action")
pub fn operator_action(req: dict) -> dict {
  return http_ok({ "ok": true })
}

@scopes("resources:write")
@policy(kinds: "tenant", matches: "tenant owner")
@route("POST", "/tenants/{tenant}/resources")
pub fn resource_policy_route(req: dict) -> dict {
  let denial = require_policy({
    kinds: ["tenant"],
    scopes: ["resources:write"],
    matches: [
      {
        kind: "tenant_mismatch",
        label: "tenant",
        left: "req.path_params.tenant",
        right: "tenant.id",
        message: "tenant does not match route"
      },
      {
        kind: "resource_mismatch",
        label: "owner",
        left: "body.owner",
        right: "auth.subject",
        message: "principal does not own resource"
      }
    ]
  }, req)
  if denial != nil {
    return denial
  }
  return http_ok({ "ok": true })
}

@policy(methods: "doc.read doc.write")
@route("POST", "/rpc")
pub fn rpc_policy_route(req: dict) -> dict {
  let denial = require_policy({
    method_path: "body.method",
    methods: {
      "doc.read": {
        scopes: ["doc:read"]
      },
      "doc.write": {
        kinds: ["operator"],
        scopes: ["doc:write"],
        matches: [
          {
            kind: "resource_mismatch",
            label: "doc",
            left: "body.owner",
            right: "auth.subject"
          }
        ]
      }
    }
  }, req)
  if denial != nil {
    return denial
  }
  return http_ok({ "ok": true })
}
"#;

/// `SiteAuth` hook that always admits with a fixed identity.
#[derive(Default)]
struct AllowAuth {
    tenant: Option<&'static str>,
    scopes: &'static [&'static str],
    subject: Option<&'static str>,
    kind: Option<&'static str>,
    context: Option<Value>,
}

#[async_trait::async_trait]
impl SiteAuth for AllowAuth {
    async fn authenticate(&self, _parts: &Parts, _route: &RouteSpec) -> SiteAuthOutcome {
        SiteAuthOutcome::Allow(SiteAuthContext {
            tenant_id: self.tenant.map(TenantId::new),
            scopes: self.scopes.iter().map(|scope| scope.to_string()).collect(),
            subject: self.subject.map(str::to_string),
            scheme: Some("apikey".to_string()),
            kind: self.kind.map(str::to_string),
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
            ..Default::default()
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
        Ok(Some(VmValue::String(arcstr::ArcStr::from(
            rendered.as_str(),
        ))))
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

async fn request_json(router: Router, method: &str, uri: &str, body: Value) -> Response {
    router
        .oneshot(
            Request::builder()
                .method(method)
                .uri(uri)
                .header("content-type", "application/json")
                .body(Body::from(body.to_string()))
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
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
        ..Default::default()
    });
    let (_dir, router) = site_router(Some(hook), None);
    let (status, body) = read_json(request(router, "DELETE", "/per_method").await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
}

/// The hook-resolved identity is threaded to the `.harn` route as the
/// ambient `harness.auth` handle: a route can read the subject, the
/// embedder-assigned principal kind, and test/enumerate the granted
/// scopes — the foundation a `.harn`-side auth policy composes (issue
/// burin-labs/harn#3323; unblocks cloud-platform route-policy adoption).
#[tokio::test]
async fn harness_auth_exposes_bound_principal_to_route() {
    let hook = Arc::new(AllowAuth {
        tenant: Some("acme"),
        scopes: &["personas:read", "sessions:write"],
        subject: Some("k_operator"),
        kind: Some("operator"),
        ..Default::default()
    });
    let (_dir, router) = site_router(Some(hook), None);
    let (status, body) = read_json(get(router, "/whoami").await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["authed"], true);
    assert_eq!(body["subject"], "k_operator");
    assert_eq!(body["kind"], "operator");
    assert_eq!(body["has_read"], true);
    assert_eq!(body["has_admin"], false);
    // `scopes()` returns the granted set as a sorted list.
    let scopes: Vec<&str> = body["scopes"]
        .as_array()
        .expect("scopes array")
        .iter()
        .map(|scope| scope.as_str().expect("scope string"))
        .collect();
    assert_eq!(scopes, vec!["personas:read", "sessions:write"]);
}

/// Without an embedder hook the dispatch is unauthenticated end to end:
/// `harness.auth.is_authenticated()` is `false` and `scopes()` is empty.
/// (The `/whoami` route's own `@scopes` is dropped here so the request
/// reaches the handler under the allow-all default — proving the
/// `harness.auth` view, not the admission gate.)
#[tokio::test]
async fn harness_auth_reports_anonymous_without_hook() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("anon.harn");
    std::fs::write(
        &path,
        r#"
@route("GET", "/whoami")
pub fn whoami_route(harness: Harness, req: dict) -> dict {
  return http_ok({
    "authed": harness.auth.is_authenticated(),
    "subject": harness.auth.try_subject(),
    "scopes": harness.auth.scopes(),
  })
}
"#,
    )
    .expect("write script");
    let config = SiteServerConfig::new(build_core(&path, None));
    let router = SiteServer::new(config).router().expect("site router");

    let (status, body) = read_json(get(router, "/whoami").await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["authed"], false);
    assert_eq!(body["subject"], Value::Null);
    assert_eq!(body["scopes"].as_array().expect("scopes array").len(), 0);
}

/// `@policy(kinds: "operator")` composes with `@scopes`: a principal that
/// carries the scope *and* the allowed kind is admitted.
#[tokio::test]
async fn policy_kinds_admits_matching_principal_kind() {
    let hook = Arc::new(AllowAuth {
        scopes: &["personas:read"],
        kind: Some("operator"),
        ..Default::default()
    });
    let (_dir, router) = site_router(Some(hook), None);
    let (status, body) = read_json(request(router, "POST", "/operator/action").await).await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
}

/// A principal with the required scope but a *disallowed* kind is refused
/// at admission with a tenant-safe `forbidden_principal_kind` envelope
/// that names the allowed kinds but never the caller's own kind.
#[tokio::test]
async fn policy_kinds_denies_wrong_principal_kind() {
    let hook = Arc::new(AllowAuth {
        scopes: &["personas:read"],
        kind: Some("tenant"),
        ..Default::default()
    });
    let (_dir, router) = site_router(Some(hook), None);
    let (status, body) = read_json(request(router, "POST", "/operator/action").await).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["code"], "forbidden");
    assert_eq!(body["details"]["kind"], "forbidden_principal_kind");
    assert_eq!(body["details"]["allowed_kinds"][0], "operator");
    // Tenant-safe: the denial never echoes the caller's own kind.
    let rendered = body.to_string();
    assert!(
        !rendered.contains("tenant"),
        "denial leaked caller kind: {rendered}"
    );
    assert!(body["request_id"].as_str().is_some());
}

/// A principal the embedder did not classify (no `kind`) can never satisfy
/// a non-empty `@policy(kinds:)` allow-set — the gate fails closed.
#[tokio::test]
async fn policy_kinds_denies_unclassified_principal() {
    let hook = Arc::new(AllowAuth {
        scopes: &["personas:read"],
        // no `kind` set
        ..Default::default()
    });
    let (_dir, router) = site_router(Some(hook), None);
    let (status, body) = read_json(request(router, "POST", "/operator/action").await).await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["details"]["kind"], "forbidden_principal_kind");
}

/// Runtime route policies can compare the matched path tenant and JSON body
/// owner against the ambient tenant/auth principal without leaking either
/// side's value in the denial envelope.
#[tokio::test]
async fn require_policy_admits_matching_tenant_resource_owner() {
    let hook = Arc::new(AllowAuth {
        tenant: Some("acme"),
        scopes: &["resources:write"],
        subject: Some("user_1"),
        kind: Some("tenant"),
        ..Default::default()
    });
    let (_dir, router) = site_router(Some(hook), None);
    let (status, body) = read_json(
        request_json(
            router,
            "POST",
            "/tenants/acme/resources",
            json!({ "owner": "user_1" }),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
}

#[tokio::test]
async fn require_policy_denies_tenant_mismatch_without_echoing_values() {
    let hook = Arc::new(AllowAuth {
        tenant: Some("globex"),
        scopes: &["resources:write"],
        subject: Some("user_1"),
        kind: Some("tenant"),
        ..Default::default()
    });
    let (_dir, router) = site_router(Some(hook), None);
    let (status, body) = read_json(
        request_json(
            router,
            "POST",
            "/tenants/acme/resources",
            json!({ "owner": "user_1" }),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["details"]["kind"], "tenant_mismatch");
    let rendered = body.to_string();
    assert!(
        !rendered.contains("globex") && !rendered.contains("acme"),
        "denial leaked tenant values: {rendered}"
    );
}

#[tokio::test]
async fn require_policy_denies_resource_owner_mismatch() {
    let hook = Arc::new(AllowAuth {
        tenant: Some("acme"),
        scopes: &["resources:write"],
        subject: Some("user_2"),
        kind: Some("tenant"),
        ..Default::default()
    });
    let (_dir, router) = site_router(Some(hook), None);
    let (status, body) = read_json(
        request_json(
            router,
            "POST",
            "/tenants/acme/resources",
            json!({ "owner": "user_1" }),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["details"]["kind"], "resource_mismatch");
    let rendered = body.to_string();
    assert!(
        !rendered.contains("user_1") && !rendered.contains("user_2"),
        "denial leaked owner values: {rendered}"
    );
}

#[tokio::test]
async fn require_policy_json_rpc_method_read_uses_body_method_scope() {
    let hook = Arc::new(AllowAuth {
        scopes: &["doc:read"],
        subject: Some("doc_1"),
        kind: Some("tenant"),
        ..Default::default()
    });
    let (_dir, router) = site_router(Some(hook), None);
    let (status, body) = read_json(
        request_json(
            router,
            "POST",
            "/rpc",
            json!({ "method": "doc.read", "owner": "doc_1" }),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
}

#[tokio::test]
async fn require_policy_json_rpc_method_write_requires_write_scope_and_operator_kind() {
    let missing_scope = Arc::new(AllowAuth {
        scopes: &["doc:read"],
        subject: Some("doc_1"),
        kind: Some("operator"),
        ..Default::default()
    });
    let (_dir, router) = site_router(Some(missing_scope), None);
    let (status, body) = read_json(
        request_json(
            router,
            "POST",
            "/rpc",
            json!({ "method": "doc.write", "owner": "doc_1" }),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["details"]["kind"], "missing_scope");
    assert_eq!(body["details"]["missing_scope"], "doc:write");

    let wrong_kind = Arc::new(AllowAuth {
        scopes: &["doc:write"],
        subject: Some("doc_1"),
        kind: Some("tenant"),
        ..Default::default()
    });
    let (_dir, router) = site_router(Some(wrong_kind), None);
    let (status, body) = read_json(
        request_json(
            router,
            "POST",
            "/rpc",
            json!({ "method": "doc.write", "owner": "doc_1" }),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::FORBIDDEN);
    assert_eq!(body["details"]["kind"], "forbidden_principal_kind");

    let allowed = Arc::new(AllowAuth {
        scopes: &["doc:write"],
        subject: Some("doc_1"),
        kind: Some("operator"),
        ..Default::default()
    });
    let (_dir, router) = site_router(Some(allowed), None);
    let (status, body) = read_json(
        request_json(
            router,
            "POST",
            "/rpc",
            json!({ "method": "doc.write", "owner": "doc_1" }),
        )
        .await,
    )
    .await;
    assert_eq!(status, StatusCode::OK);
    assert_eq!(body["ok"], true);
}
