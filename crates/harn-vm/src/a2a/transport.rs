use serde_json::Value;
use tokio::sync::broadcast;

use super::{recv_cancel, A2aClientError, A2aWireVersion};

pub(super) async fn send_jsonrpc(
    rpc_url: &str,
    request: &Value,
    trace_id: &str,
    protocol_version: A2aWireVersion,
    cancel_rx: &mut broadcast::Receiver<()>,
) -> Result<Value, A2aClientError> {
    let response = send_http(
        crate::llm::shared_blocking_client()
            .post(rpc_url)
            .header(reqwest::header::CONTENT_TYPE, "application/json")
            .header("A2A-Version", protocol_version.header())
            .header("A2A-Trace-Id", trace_id)
            .json(request),
        cancel_rx,
        "A2A task dispatch cancelled",
    )
    .await?;
    if matches!(
        response.status(),
        reqwest::StatusCode::UNAUTHORIZED | reqwest::StatusCode::FORBIDDEN
    ) {
        return Err(A2aClientError::Denied(format!(
            "A2A task dispatch returned HTTP {}",
            response.status()
        )));
    }
    if !response.status().is_success() {
        return Err(A2aClientError::Protocol(format!(
            "A2A task dispatch returned HTTP {}",
            response.status()
        )));
    }
    response
        .json::<Value>()
        .await
        .map_err(|error| A2aClientError::Protocol(format!("parse A2A dispatch response: {error}")))
}

pub(super) async fn send_http(
    request: reqwest::RequestBuilder,
    cancel_rx: &mut broadcast::Receiver<()>,
    cancelled_message: &'static str,
) -> Result<reqwest::Response, A2aClientError> {
    tokio::select! {
        response = request.send() => response.map_err(|error| {
            if error.is_timeout() {
                A2aClientError::Timeout(format!(
                    "A2A HTTP request timed out: {}",
                    crate::egress::redact_reqwest_error(&error)
                ))
            } else {
                A2aClientError::Transport(format!(
                    "A2A HTTP request failed: {}",
                    crate::egress::redact_reqwest_error(&error)
                ))
            }
        }),
        _ = recv_cancel(cancel_rx) => Err(A2aClientError::Cancelled(cancelled_message.to_string())),
    }
}
