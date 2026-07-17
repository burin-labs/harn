//! One canonical ACP permission-request bridge for agent tool dispatch.
//!
//! Policy owners decide whether a call needs approval. This module owns only
//! the existing ACP request/response transport, so side-effect ceilings and
//! approval-policy rules cannot drift into separate wire implementations.

use std::sync::Arc;

use crate::bridge::HostBridge;

pub(super) struct HostPermissionRequest {
    pub session_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub tool_args: serde_json::Value,
    pub policy_decision: serde_json::Value,
    pub request_context: serde_json::Value,
    pub requested_capabilities: Vec<String>,
    pub tool_descriptor: Option<serde_json::Value>,
}

pub(super) enum HostPermissionOutcome {
    Allowed { response: serde_json::Value },
    Rejected { reason: String },
    Unavailable,
}

pub(super) async fn request_host_permission(
    bridge: Option<&Arc<HostBridge>>,
    request: HostPermissionRequest,
) -> HostPermissionOutcome {
    let Some(bridge) = bridge else {
        return HostPermissionOutcome::Unavailable;
    };
    let approval_request = crate::stdlib::hitl::approval_request_for_host_permission(
        request.tool_call_id.clone(),
        request.tool_name.clone(),
        request.tool_args.clone(),
        request.session_id.clone(),
        Vec::new(),
        request.request_context,
        request.requested_capabilities,
    );
    let approval_request_json =
        serde_json::to_value(&approval_request).unwrap_or(serde_json::Value::Null);
    match bridge
        .call(
            crate::llm::acp_permission::METHOD_REQUEST_PERMISSION,
            crate::llm::acp_permission::request_params(
                Some(&request.session_id),
                &request.tool_call_id,
                &request.tool_name,
                &request.tool_args,
                approval_request_json,
                &request.policy_decision,
                request.tool_descriptor,
            ),
        )
        .await
    {
        Ok(response) => match crate::llm::acp_permission::parse_response(&response) {
            crate::llm::acp_permission::WireOutcome::Allowed => {
                HostPermissionOutcome::Allowed { response }
            }
            crate::llm::acp_permission::WireOutcome::Rejected { reason } => {
                HostPermissionOutcome::Rejected { reason }
            }
        },
        Err(_) => HostPermissionOutcome::Unavailable,
    }
}
