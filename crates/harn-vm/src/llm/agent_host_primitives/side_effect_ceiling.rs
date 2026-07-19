//! ACP escalation for one exact side-effect ceiling refusal.
//!
//! This is intentionally a dispatch-local approval. It does not persist a
//! grant, alter the session policy, or expose credentials; the durable grants
//! contract remains the separate session-profile owner.

use std::sync::Arc;

use crate::agent_events::{
    DenialGate, SideEffectCeilingDetails, SideEffectCeilingRemedy, ToolDenial,
};
use crate::bridge::HostBridge;
use crate::orchestration::SideEffectCeilingViolation;

use super::host_permission::{
    request_host_permission, HostPermissionOutcome, HostPermissionRequest,
};

// Returned by value exactly once per side-effect-ceiling denial (a cold path),
// so the size asymmetry between `Allowed` and the denial-carrying `Denied`
// variant is not worth an extra heap indirection on `ToolDenial`.
#[allow(clippy::large_enum_variant)]
pub(super) enum SideEffectPermissionOutcome {
    Allowed {
        policy_decision: serde_json::Value,
    },
    Denied {
        denial: ToolDenial,
        escalated: bool,
        policy_decision: serde_json::Value,
    },
}

pub(super) async fn request_side_effect_permission(
    bridge: Option<&Arc<HostBridge>>,
    session_id: &str,
    tool_call_id: &str,
    tool_name: &str,
    tool_args: &serde_json::Value,
    violation: SideEffectCeilingViolation,
    reason: String,
    tool_descriptor: Option<serde_json::Value>,
) -> SideEffectPermissionOutcome {
    let approval_id = if tool_call_id.is_empty() {
        format!("tool_call_{}", uuid::Uuid::now_v7())
    } else {
        tool_call_id.to_string()
    };
    let request_details = details(
        tool_name,
        violation,
        SideEffectCeilingRemedy::RequestPermission,
    );
    let policy_decision = serde_json::json!({
        "action": "ask",
        "source": "side_effect_ceiling",
        "scope": "once",
        "ceiling": violation.ceiling,
        "required_level": violation.required_level,
        "tool": tool_name,
    });
    let request = HostPermissionRequest {
        session_id: session_id.to_string(),
        tool_call_id: approval_id,
        tool_name: tool_name.to_string(),
        tool_args: tool_args.clone(),
        policy_decision: policy_decision.clone(),
        request_context: serde_json::json!({
            "policy_decision": policy_decision.clone(),
            "side_effect_ceiling": request_details.clone(),
        }),
        requested_capabilities: vec![format!("tool.{tool_name}")],
        tool_descriptor,
    };
    match request_host_permission(bridge, request).await {
        HostPermissionOutcome::Allowed { .. } => {
            SideEffectPermissionOutcome::Allowed { policy_decision }
        }
        HostPermissionOutcome::Rejected { reason } => SideEffectPermissionOutcome::Denied {
            denial: ToolDenial::terminal(DenialGate::HostRejected, None, reason)
                .with_side_effect_ceiling(request_details),
            escalated: true,
            policy_decision,
        },
        HostPermissionOutcome::Unavailable if bridge.is_some() => {
            SideEffectPermissionOutcome::Denied {
                denial: ToolDenial::terminal(
                    DenialGate::ApprovalUnavailable,
                    None,
                    "approval request failed or host does not implement session/request_permission",
                )
                .with_side_effect_ceiling(request_details),
                escalated: true,
                policy_decision,
            }
        }
        HostPermissionOutcome::Unavailable => SideEffectPermissionOutcome::Denied {
            denial: ToolDenial::terminal(DenialGate::SideEffectCeiling, None, reason)
                .with_side_effect_ceiling(details(
                    tool_name,
                    violation,
                    SideEffectCeilingRemedy::RaiseSideEffectCeiling,
                )),
            escalated: false,
            policy_decision,
        },
    }
}

fn details(
    tool_name: &str,
    violation: SideEffectCeilingViolation,
    remedy: SideEffectCeilingRemedy,
) -> SideEffectCeilingDetails {
    SideEffectCeilingDetails {
        ceiling: violation.ceiling,
        required_level: violation.required_level,
        tool: tool_name.to_string(),
        remedy,
    }
}
