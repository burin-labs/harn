//! Fixtures shared by the `harn serve site` suites (`site_*.rs`): canned
//! [`SiteAuth`] hooks, a temp-dir script writer, router assembly, and the
//! `tower::ServiceExt::oneshot` request/response helpers.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use axum::body::{to_bytes, Body};
use axum::http::request::Parts;
use axum::http::{Request, StatusCode};
use axum::response::{IntoResponse, Response};
use axum::{Json, Router};
use harn_serve::{
    DispatchCore, DispatchCoreConfig, NoReplayCache, RouteSpec, SiteAuth, SiteAuthContext,
    SiteAuthOutcome, SiteServer, SiteServerConfig,
};
use harn_vm::TenantId;
use serde_json::{json, Value};
use tower::ServiceExt;

/// `SiteAuth` hook that always admits with a fixed identity.
#[derive(Default)]
pub(crate) struct AllowAuth {
    pub(crate) tenant: Option<&'static str>,
    pub(crate) scopes: &'static [&'static str],
    pub(crate) subject: Option<&'static str>,
    pub(crate) kind: Option<&'static str>,
    pub(crate) context: Option<Value>,
}

impl AllowAuth {
    /// Admit under `tenant` with `scopes` and no further identity.
    pub(crate) fn scoped(
        tenant: Option<&'static str>,
        scopes: &'static [&'static str],
    ) -> Arc<Self> {
        Arc::new(Self {
            tenant,
            scopes,
            ..Default::default()
        })
    }
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

/// `SiteAuth` hook that always refuses with an embedder-shaped 401.
pub(crate) struct DenyAuth;

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

/// Dispatch config for `path` that re-runs the handler on every request,
/// as an HTTP host must.
pub(crate) fn core_config(path: &Path) -> DispatchCoreConfig {
    let mut config = DispatchCoreConfig::for_script(path);
    config.replay_cache = Arc::new(NoReplayCache);
    config
}

pub(crate) fn build_core(path: &Path) -> DispatchCore {
    DispatchCore::new(core_config(path)).expect("dispatch core")
}

/// Write `script` to `site.harn` in a fresh temp dir. Keep the dir alive
/// as long as any router over it: the dispatcher reads the script on
/// every request.
pub(crate) fn write_site(script: &str) -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("site.harn");
    std::fs::write(&path, script).expect("write script");
    (dir, path)
}

/// Mount the site router for `config`, optionally behind an auth hook.
pub(crate) fn mount(config: SiteServerConfig, auth: Option<Arc<dyn SiteAuth>>) -> Router {
    let config = match auth {
        Some(auth) => config.with_auth(auth),
        None => config,
    };
    SiteServer::new(config).router().expect("site router")
}

/// Write `script` and mount a plain site router over it.
pub(crate) fn script_router(script: &str) -> (tempfile::TempDir, Router) {
    let (dir, path) = write_site(script);
    let router = mount(SiteServerConfig::new(build_core(&path)), None);
    (dir, router)
}

pub(crate) async fn get(router: Router, uri: &str) -> Response {
    request(router, "GET", uri).await
}

pub(crate) async fn request(router: Router, method: &str, uri: &str) -> Response {
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

/// Status plus JSON body (`Null` when empty or not JSON).
pub(crate) async fn read_json(response: Response) -> (StatusCode, Value) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    let value = if bytes.is_empty() {
        Value::Null
    } else {
        serde_json::from_slice(&bytes).unwrap_or(Value::Null)
    };
    (status, value)
}

/// Status plus body decoded as (lossy) UTF-8.
pub(crate) async fn read_body(response: Response) -> (StatusCode, String) {
    let status = response.status();
    let bytes = to_bytes(response.into_body(), usize::MAX).await.unwrap();
    (status, String::from_utf8_lossy(&bytes).into_owned())
}

/// Serve `router` on a loopback listener in the background and return its
/// address — for cases that need a live socket (WebSocket handshakes).
pub(crate) async fn serve_loopback(router: Router) -> std::net::SocketAddr {
    let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let _ = axum::serve(listener, router).await;
    });
    addr
}
