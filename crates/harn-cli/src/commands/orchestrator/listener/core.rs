use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use axum::extract::{DefaultBodyLimit, Extension};
use axum::http::StatusCode;
use axum::middleware;
use axum::response::{IntoResponse, Response};
use axum::routing::{get, post};
use axum::Router;

use harn_vm::event_log::{AnyEventLog, Topic};
use harn_vm::secrets::SecretProvider;

use super::acp_hub::{
    acp_retained_session_duration_from_env, acp_websocket_endpoint, AcpWebSocketHub,
    AcpWebSocketState, ACP_PATH,
};
use super::admin::{admin_reload_endpoint, AdminReloadHandle, AdminReloadState, ADMIN_RELOAD_PATH};
use super::routes::{
    ingest_trigger, test_file_from_env, ListenerAuth, RouteConfig, RouteRegistry, TestRequestGate,
    TriggerMetricSnapshot, PENDING_TOPIC,
};
use crate::commands::orchestrator::errors::OrchestratorError;
use crate::commands::orchestrator::origin_guard::{enforce_allowed_origin, OriginAllowList};
use crate::commands::orchestrator::tls::{ServerRuntime, TlsFiles};

pub(super) const DEFAULT_MAX_BODY_BYTES: usize = 10 * 1024 * 1024;
pub(super) const REQUEST_ENTERED_FILE_ENV: &str = "HARN_ORCHESTRATOR_TEST_REQUEST_ENTERED_FILE";
pub(super) const REQUEST_RELEASE_FILE_ENV: &str = "HARN_ORCHESTRATOR_TEST_REQUEST_RELEASE_FILE";

#[derive(Clone)]
pub(crate) struct ListenerConfig {
    pub(crate) bind: std::net::SocketAddr,
    pub(crate) tls: Option<TlsFiles>,
    pub(crate) event_log: Arc<AnyEventLog>,
    pub(crate) secrets: Arc<dyn SecretProvider>,
    pub(crate) allowed_origins: OriginAllowList,
    pub(crate) max_body_bytes: usize,
    pub(crate) metrics_registry: Arc<harn_vm::MetricsRegistry>,
    pub(crate) admin_reload: Option<AdminReloadHandle>,
    pub(crate) mcp_router: Option<Router>,
    pub(crate) routes: Vec<RouteConfig>,
    pub(crate) tenant_store: Option<Arc<harn_vm::TenantStore>>,
    pub(crate) session_store: Option<Arc<harn_vm::SessionStore>>,
}

impl ListenerConfig {
    pub(crate) fn max_body_bytes_or_default(max_body_bytes: Option<usize>) -> usize {
        max_body_bytes.unwrap_or(DEFAULT_MAX_BODY_BYTES)
    }
}

pub(crate) struct ListenerRuntime {
    server: ServerRuntime,
    routes: Arc<RouteRegistry>,
    readiness: Arc<ListenerReadiness>,
    #[cfg(test)]
    acp_hub: Arc<AcpWebSocketHub>,
}

#[derive(Default)]
struct ListenerReadiness {
    ready: AtomicBool,
}

impl ListenerReadiness {
    fn mark_ready(&self) {
        self.ready.store(true, Ordering::Release);
    }

    fn mark_not_ready(&self) {
        self.ready.store(false, Ordering::Release);
    }

    fn is_ready(&self) -> bool {
        self.ready.load(Ordering::Acquire)
    }
}

impl ListenerRuntime {
    pub(crate) async fn start(config: ListenerConfig) -> Result<Self, OrchestratorError> {
        let pending_topic =
            Topic::new(PENDING_TOPIC).map_err(|error| format!("invalid pending topic: {error}"))?;
        let inbox_metrics = Arc::new(harn_vm::MetricsRegistry::default());
        let inbox = Arc::new(
            harn_vm::InboxIndex::new(config.event_log.clone(), inbox_metrics)
                .await
                .map_err(|error| format!("failed to initialize inbox index: {error}"))?,
        );
        let requires_auth = config
            .routes
            .iter()
            .any(|route| route.auth_mode.requires_credentials());
        let auth = Arc::new(ListenerAuth::from_env(
            requires_auth,
            config.session_store.clone(),
        )?);
        let request_gate = TestRequestGate {
            entered_file: test_file_from_env(REQUEST_ENTERED_FILE_ENV),
            release_file: test_file_from_env(REQUEST_RELEASE_FILE_ENV),
        };
        let origin_state = Arc::new(config.allowed_origins.clone());
        let admin_state = config.admin_reload.clone().map(|reload| {
            Arc::new(AdminReloadState {
                event_log: config.event_log.clone(),
                auth: auth.clone(),
                reload,
            })
        });
        let acp_hub = AcpWebSocketHub::new(
            config.event_log.clone(),
            acp_retained_session_duration_from_env(),
        );
        let acp_hub_sweeper = acp_hub.clone();
        tokio::spawn(async move {
            acp_hub_sweeper.run_expiry_sweeper().await;
        });
        let acp_state = Arc::new(AcpWebSocketState {
            event_log: config.event_log.clone(),
            auth: auth.clone(),
            pipeline: None,
            hub: acp_hub.clone(),
        });
        let routes = Arc::new(RouteRegistry::new(
            config.routes,
            config.event_log.clone(),
            inbox,
            config.secrets.clone(),
            config.metrics_registry.clone(),
            auth.clone(),
            pending_topic.clone(),
            request_gate,
            config.tenant_store.clone(),
        )?);
        let readiness = Arc::new(ListenerReadiness::default());
        let mut app = Router::new()
            .route(
                "/health",
                get(|| async move { (StatusCode::OK, "ok").into_response() }),
            )
            .route(
                "/healthz",
                get(|| async move { (StatusCode::OK, "ok").into_response() }),
            )
            .route(
                "/readyz",
                get(readyz_endpoint).layer(Extension(readiness.clone())),
            )
            .route(
                "/metrics",
                get(metrics_endpoint).layer(Extension(config.metrics_registry.clone())),
            );
        app = app.route(
            ACP_PATH,
            get(acp_websocket_endpoint).layer(Extension(acp_state)),
        );
        if let Some(admin_state) = admin_state {
            app = app.route(
                ADMIN_RELOAD_PATH,
                post(admin_reload_endpoint).layer(Extension(admin_state)),
            );
        }
        if let Some(mcp_router) = config.mcp_router {
            app = app.merge(mcp_router);
        }
        let app = app.route(
            "/{*path}",
            post(ingest_trigger).layer(Extension(routes.clone())),
        );

        let app = app
            .layer(DefaultBodyLimit::max(config.max_body_bytes))
            .layer(middleware::from_fn_with_state(
                origin_state.clone(),
                enforce_allowed_origin,
            ));

        let server = ServerRuntime::start(config.bind, app, config.tls.as_ref()).await?;
        Ok(Self {
            server,
            routes,
            readiness,
            #[cfg(test)]
            acp_hub,
        })
    }

    pub(crate) fn local_addr(&self) -> std::net::SocketAddr {
        self.server.local_addr()
    }

    #[cfg(test)]
    pub(crate) fn acp_session_is_detached_for_test(&self, session_id: &str) -> bool {
        self.acp_hub.session_is_detached_for_test(session_id)
    }

    #[cfg(test)]
    pub(crate) async fn sweep_expired_acp_workers_for_test(&self) {
        self.acp_hub.sweep_expired_once_for_test().await;
    }

    pub(crate) fn scheme(&self) -> &'static str {
        if self.server.tls_enabled() {
            "https"
        } else {
            "http"
        }
    }

    pub(crate) fn url(&self) -> String {
        format!("{}://{}", self.scheme(), self.local_addr())
    }

    pub(crate) fn mark_ready(&self) {
        self.readiness.mark_ready();
    }

    pub(crate) fn mark_not_ready(&self) {
        self.readiness.mark_not_ready();
    }

    pub(crate) fn trigger_metrics(&self) -> BTreeMap<String, TriggerMetricSnapshot> {
        self.routes.snapshot_metrics()
    }

    pub(crate) fn reload_routes(&self, routes: Vec<RouteConfig>) -> Result<(), OrchestratorError> {
        self.routes.reload(routes)
    }

    pub(crate) async fn shutdown(
        self,
        timeout: Duration,
    ) -> Result<BTreeMap<String, TriggerMetricSnapshot>, OrchestratorError> {
        let Self { server, routes, .. } = self;
        server.shutdown(timeout).await?;
        Ok(routes.snapshot_metrics())
    }
}

async fn metrics_endpoint(
    Extension(metrics): Extension<Arc<harn_vm::MetricsRegistry>>,
) -> impl IntoResponse {
    (
        StatusCode::OK,
        [(
            axum::http::header::CONTENT_TYPE,
            "text/plain; version=0.0.4; charset=utf-8",
        )],
        metrics.render_prometheus(),
    )
}

async fn readyz_endpoint(Extension(readiness): Extension<Arc<ListenerReadiness>>) -> Response {
    if readiness.is_ready() {
        (StatusCode::OK, "ready").into_response()
    } else {
        (StatusCode::SERVICE_UNAVAILABLE, "starting").into_response()
    }
}
