use std::sync::Arc;

use axum::body::Bytes;
use axum::extract::{Extension, OriginalUri};
use axum::http::{HeaderMap, Method, StatusCode};
use axum::response::{IntoResponse, Response};
use serde_json::{json, Value as JsonValue};
use tokio::sync::{mpsc, oneshot};

use harn_vm::event_log::AnyEventLog;

use super::routes::{normalize_headers, HttpError, ListenerAuth};
use crate::commands::orchestrator::errors::OrchestratorError;

pub(super) const ADMIN_RELOAD_PATH: &str = "/admin/reload";

pub(crate) struct AdminReloadRequest {
    pub(crate) source: String,
    pub(crate) response_tx: Option<oneshot::Sender<Result<JsonValue, OrchestratorError>>>,
}

#[derive(Clone)]
pub struct AdminReloadHandle {
    tx: mpsc::UnboundedSender<AdminReloadRequest>,
}

impl AdminReloadHandle {
    pub(crate) fn channel() -> (Self, mpsc::UnboundedReceiver<AdminReloadRequest>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, rx)
    }

    pub(crate) fn trigger(&self, source: impl Into<String>) -> Result<(), OrchestratorError> {
        self.tx
            .send(AdminReloadRequest {
                source: source.into(),
                response_tx: None,
            })
            .map_err(|_| OrchestratorError::Listener("reload channel is closed".to_string()))
    }

    pub(crate) async fn request(
        &self,
        source: impl Into<String>,
    ) -> Result<JsonValue, OrchestratorError> {
        let (tx, rx) = oneshot::channel();
        self.tx
            .send(AdminReloadRequest {
                source: source.into(),
                response_tx: Some(tx),
            })
            .map_err(|_| "reload channel is closed".to_string())?;
        rx.await
            .map_err(|_| "reload response channel closed".to_string())?
    }
}

#[derive(Clone)]
pub(super) struct AdminReloadState {
    pub(super) event_log: Arc<AnyEventLog>,
    pub(super) auth: Arc<ListenerAuth>,
    pub(super) reload: AdminReloadHandle,
}

pub(super) async fn admin_reload_endpoint(
    Extension(state): Extension<Arc<AdminReloadState>>,
    method: Method,
    OriginalUri(uri): OriginalUri,
    headers: HeaderMap,
    body: Bytes,
) -> Response {
    let normalized_headers = normalize_headers(&headers);
    if state
        .auth
        .authorize(
            state.event_log.as_ref(),
            method.as_str(),
            uri.path(),
            &normalized_headers,
            &body,
        )
        .await
        .is_err()
    {
        return HttpError::unauthorized("auth failed").into_response();
    }
    let source = serde_json::from_slice::<JsonValue>(&body)
        .ok()
        .and_then(|value| {
            value
                .get("source")
                .and_then(JsonValue::as_str)
                .map(ToString::to_string)
        })
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| "admin_api".to_string());
    match state.reload.request(source.clone()).await {
        Ok(summary) => (
            StatusCode::OK,
            axum::Json(json!({
                "status": "ok",
                "source": source,
                "summary": summary,
            })),
        )
            .into_response(),
        Err(error) => (
            StatusCode::INTERNAL_SERVER_ERROR,
            axum::Json(json!({
                "status": "error",
                "source": source,
                "error": error.to_string(),
            })),
        )
            .into_response(),
    }
}
