use serde_json::Value;
use tokio::sync::broadcast;

use super::{ExternalAgentError, ExternalAgentPeer};

pub(super) async fn send_a2a_rpc(
    peer: &ExternalAgentPeer,
    request_id: String,
    method: &str,
    mut params: Value,
    cancel_rx: &mut broadcast::Receiver<()>,
) -> Result<Value, ExternalAgentError> {
    let (protocol_version, tenant) = crate::a2a::peer_wire_settings(
        &peer.card,
        &peer.card_url,
        &peer.rpc_url,
        &super::normalize_a2a_target(&peer.target),
    )?;
    if let Some(tenant) = tenant.as_ref() {
        params["tenant"] = serde_json::json!(tenant);
    }
    let request = crate::jsonrpc::request(request_id.clone(), method, params);
    let body = crate::a2a::send_jsonrpc_request_versioned(
        &peer.rpc_url,
        &request,
        &request_id,
        protocol_version,
        cancel_rx,
    )
    .await?;
    if let Some(error) = body.get("error") {
        let message = error
            .get("message")
            .and_then(Value::as_str)
            .unwrap_or("unknown external agent error");
        return Err(ExternalAgentError::Protocol(format!(
            "{method} failed: {message}"
        )));
    }
    body.get("result")
        .cloned()
        .ok_or_else(|| ExternalAgentError::Protocol(format!("{method} response missing result")))
}
