//! One canonical ACP permission-request bridge for agent tool dispatch.
//!
//! Policy owners decide whether a call needs approval. This module owns only
//! the existing ACP request/response transport, so side-effect ceilings and
//! approval-policy rules cannot drift into separate wire implementations.

use std::sync::Arc;

use crate::bridge::HostBridge;
use crate::llm::permissions;

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

/// Append a `PermissionGrant` / `PermissionDeny` / `PermissionEscalation`
/// event to the live transcript for the named session, when one exists.
/// Silent no-op for sessions that haven't been opened (e.g. raw dispatcher
/// calls outside an agent loop).
pub(super) fn emit_permission_event(
    session_id: &str,
    kind: &str,
    tool_name: &str,
    tool_args: &serde_json::Value,
    reason: &str,
    escalated: bool,
) {
    emit_permission_event_with_policy(
        session_id, kind, tool_name, tool_args, reason, escalated, None,
    );
}

pub(super) fn emit_permission_event_with_policy(
    session_id: &str,
    kind: &str,
    tool_name: &str,
    tool_args: &serde_json::Value,
    reason: &str,
    escalated: bool,
    policy_decision: Option<serde_json::Value>,
) {
    if !crate::agent_sessions::exists(session_id) {
        return;
    }
    let event = if let Some(policy_decision) = policy_decision {
        permissions::permission_transcript_event_with_policy(
            kind,
            tool_name,
            tool_args,
            reason,
            escalated,
            Some(policy_decision),
        )
    } else {
        permissions::permission_transcript_event(kind, tool_name, tool_args, reason, escalated)
    };
    let _ = crate::agent_sessions::append_event(session_id, event);
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
        Err(err) => {
            // A `session/cancel` races the outstanding permission call and
            // the bridge unwinds it with a "cancelled" error rather than a
            // host-side rejection or transport failure. Report that as a
            // normal user rejection, not the generic "host doesn't implement
            // this method" Unavailable path — the host DID implement it, the
            // user just cancelled before it resolved.
            if err.to_string().contains("cancelled") {
                HostPermissionOutcome::Rejected {
                    reason: "cancelled by user".to_string(),
                }
            } else {
                HostPermissionOutcome::Unavailable
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::AtomicBool;
    use tokio::sync::Mutex as TokioMutex;

    fn request() -> HostPermissionRequest {
        HostPermissionRequest {
            session_id: "cancel-outcome-test".to_string(),
            tool_call_id: "call_1".to_string(),
            tool_name: "exec".to_string(),
            tool_args: serde_json::json!({ "command": "echo hi" }),
            policy_decision: serde_json::Value::Null,
            request_context: serde_json::Value::Null,
            requested_capabilities: Vec::new(),
            tool_descriptor: None,
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn cancelled_bridge_call_is_rejected_not_unavailable() {
        // A bridge that is already cancelled short-circuits `call` with
        // "Bridge: operation cancelled" before ever writing to the host.
        // This must surface as a normal user rejection, not the
        // trust-killing "host does not implement session/request_permission"
        // Unavailable message — the host never even got asked.
        let cancelled = Arc::new(AtomicBool::new(true));
        let bridge = Arc::new(HostBridge::from_parts(
            Arc::new(TokioMutex::new(HashMap::new())),
            cancelled,
            Arc::new(std::sync::Mutex::new(())),
            1,
        ));

        let outcome = request_host_permission(Some(&bridge), request()).await;

        match outcome {
            HostPermissionOutcome::Rejected { reason } => {
                assert_eq!(reason, "cancelled by user");
            }
            HostPermissionOutcome::Allowed { .. } => panic!("expected Rejected, got Allowed"),
            HostPermissionOutcome::Unavailable => panic!(
                "a cancelled permission request must be Rejected, not the generic \
                 Unavailable trust-killer message"
            ),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn no_bridge_is_unavailable() {
        let outcome = request_host_permission(None, request()).await;
        assert!(matches!(outcome, HostPermissionOutcome::Unavailable));
    }
}
