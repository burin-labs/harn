//! One canonical ACP permission-request bridge for agent tool dispatch.
//!
//! Policy owners decide whether a call needs approval. This module owns only
//! the existing ACP request/response transport, so side-effect ceilings and
//! approval-policy rules cannot drift into separate wire implementations.

use std::sync::Arc;

use crate::bridge::HostBridge;
use crate::llm::permissions;

use crate::orchestration::{
    PolicyEvaluation, ToolPermissionActivityContext, ToolPermissionActivityRecord,
    ToolPermissionPolicyFacts, ToolPermissionPolicyLayer, ToolPermissionPolicyOutcome,
    ToolPermissionResolution,
};

pub(super) struct HostPermissionRequest {
    pub session_id: String,
    pub tool_call_id: String,
    pub tool_name: String,
    pub tool_args: serde_json::Value,
    pub policy_decision: serde_json::Value,
    pub request_context: serde_json::Value,
    pub requested_capabilities: Vec<String>,
    pub tool_descriptor: Option<serde_json::Value>,
    pub tool_annotations: Option<crate::tool_annotations::ToolAnnotations>,
}

pub(super) enum HostPermissionOutcome {
    Allowed {
        response: serde_json::Value,
        resolution: ToolPermissionResolution,
    },
    Rejected {
        reason: String,
        resolution: ToolPermissionResolution,
    },
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

/// Append the portable value-free permission decision beside the existing
/// operational transcript event. Consumers persist only this typed activity;
/// the raw permission event remains model/runtime diagnostics, not authority.
pub(super) fn emit_permission_activity(
    session_id: &str,
    tool_call_id: &str,
    tool_name: &str,
    evaluation: &PolicyEvaluation,
    layer: ToolPermissionPolicyLayer,
    resolution: ToolPermissionResolution,
) {
    if !crate::agent_sessions::exists(session_id) {
        return;
    }
    let request_id = if tool_call_id.is_empty() {
        format!("tool-call-{}", uuid::Uuid::now_v7())
    } else {
        tool_call_id.to_string()
    };
    let model = crate::agent_sessions::pinned_model(session_id)
        .map(|selector| crate::llm_config::resolve_model_info(&selector));
    let context = ToolPermissionActivityContext {
        id: format!("permission-{}", uuid::Uuid::now_v7()),
        request_id,
        session_id: session_id.to_string(),
        agent_id: crate::agent_sessions::actor_chain(session_id)
            .map(|chain| chain.current().to_string()),
        model_provider: model.as_ref().map(|model| model.provider.clone()),
        model_id: model.map(|model| model.id),
        policy_layer: layer,
        occurred_at_ms: std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|duration| duration.as_millis().min(u128::from(u64::MAX)) as u64)
            .unwrap_or(0),
    };
    let policy = ToolPermissionPolicyFacts {
        outcome: if evaluation.is_allow() {
            ToolPermissionPolicyOutcome::Allowed
        } else if evaluation.is_deny() {
            ToolPermissionPolicyOutcome::Denied
        } else {
            ToolPermissionPolicyOutcome::ApprovalRequired
        },
        rule_id: evaluation
            .matched_rule
            .as_ref()
            .and_then(|rule| rule.id.clone()),
        risk_labels: evaluation.risk_labels.clone(),
    };
    let Ok(activity) =
        ToolPermissionActivityRecord::from_policy_facts(tool_name, policy, context, resolution)
    else {
        return;
    };
    let Ok(activity) = serde_json::to_value(activity) else {
        return;
    };
    let event = crate::llm::helpers::transcript_event(
        "PermissionDecision",
        "tool",
        "internal",
        "tool permission decision",
        Some(serde_json::json!({"activity": activity})),
    );
    let _ = crate::agent_sessions::append_event(session_id, event);
}

pub(super) async fn request_host_permission(
    bridge: Option<&Arc<HostBridge>>,
    request: HostPermissionRequest,
) -> HostPermissionOutcome {
    let Some(bridge) = bridge else {
        return HostPermissionOutcome::Unavailable;
    };
    let evidence_refs = crate::llm::permission_preview::capture(
        request.tool_annotations.as_ref(),
        &request.tool_name,
        &request.tool_args,
    );
    let approval_request = crate::stdlib::hitl::approval_request_for_host_permission(
        request.tool_call_id.clone(),
        request.tool_name.clone(),
        request.tool_args.clone(),
        request.session_id.clone(),
        evidence_refs,
        request.request_context,
        request.requested_capabilities,
    );
    let approval_request_json =
        serde_json::to_value(&approval_request).unwrap_or(serde_json::Value::Null);
    let tool_kind = request
        .tool_annotations
        .as_ref()
        .map(|annotations| annotations.kind)
        .unwrap_or_default();
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
                tool_kind,
            ),
        )
        .await
    {
        Ok(response) => match crate::llm::acp_permission::parse_response(&response) {
            crate::llm::acp_permission::WireOutcome::Allowed { resolution } => {
                HostPermissionOutcome::Allowed {
                    response,
                    resolution,
                }
            }
            crate::llm::acp_permission::WireOutcome::Rejected { reason, resolution } => {
                HostPermissionOutcome::Rejected { reason, resolution }
            }
        },
        Err(_) => {
            // A `session/cancel` races the outstanding permission call and
            // the bridge unwinds it with a "cancelled" error rather than a
            // host-side rejection or transport failure. Report that as a
            // normal user rejection, not the generic "host doesn't implement
            // this method" Unavailable path — the host DID implement it, the
            // user just cancelled before it resolved.
            if bridge.is_cancelled() {
                HostPermissionOutcome::Rejected {
                    reason: "cancelled by user".to_string(),
                    resolution: ToolPermissionResolution::terminal(
                        crate::orchestration::ToolPermissionOutcome::Cancelled,
                        crate::orchestration::ToolPermissionDecider::Person,
                    ),
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
    use std::sync::Mutex as StdMutex;
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
            tool_annotations: None,
        }
    }

    fn responding_bridge(requests: Arc<StdMutex<Vec<serde_json::Value>>>) -> Arc<HostBridge> {
        let pending: Arc<
            TokioMutex<HashMap<u64, tokio::sync::oneshot::Sender<serde_json::Value>>>,
        > = Arc::new(TokioMutex::new(HashMap::new()));
        let response_pending = pending.clone();
        let writer = Arc::new(move |line: &str| {
            let request: serde_json::Value = serde_json::from_str(line)
                .map_err(|error| format!("invalid bridge request: {error}"))?;
            requests
                .lock()
                .map_err(|_| "captured request mutex poisoned".to_string())?
                .push(request.clone());
            let id = request["id"]
                .as_u64()
                .ok_or_else(|| "bridge request missing numeric id".to_string())?;
            let sender = response_pending
                .try_lock()
                .map_err(|_| "bridge pending map unexpectedly locked".to_string())?
                .remove(&id)
                .ok_or_else(|| "bridge request was not pending".to_string())?;
            sender
                .send(serde_json::json!({
                    "jsonrpc": "2.0",
                    "id": id,
                    "result": crate::llm::acp_permission::allow_response(),
                }))
                .map_err(|_| "bridge caller dropped before response".to_string())
        });
        Arc::new(HostBridge::from_parts_with_writer(
            pending,
            Arc::new(AtomicBool::new(false)),
            writer,
            1,
        ))
    }

    #[tokio::test(flavor = "current_thread")]
    async fn edit_request_captures_preimage_before_sending_canonical_diff() {
        let directory = tempfile::tempdir().expect("tempdir");
        std::fs::write(directory.path().join("src.rs"), "old\n").expect("fixture");
        crate::stdlib::process::set_thread_execution_context(Some(
            crate::orchestration::RunExecutionRecord {
                cwd: Some(directory.path().to_string_lossy().into_owned()),
                ..Default::default()
            },
        ));
        let captured = Arc::new(StdMutex::new(Vec::new()));
        let bridge = responding_bridge(captured.clone());
        let mut request = request();
        request.tool_name = "edit".to_string();
        request.tool_args = serde_json::json!({
            "action": "exact_patch",
            "path": "src.rs",
            "old_string": "old",
            "new_string": "new"
        });
        request.tool_annotations = Some(crate::tool_annotations::ToolAnnotations {
            kind: crate::tool_annotations::ToolKind::Edit,
            arg_schema: crate::tool_annotations::ToolArgSchema {
                path_params: vec!["path".to_string()],
                ..Default::default()
            },
            ..Default::default()
        });

        let outcome = request_host_permission(Some(&bridge), request).await;
        crate::stdlib::process::set_thread_execution_context(None);

        assert!(matches!(outcome, HostPermissionOutcome::Allowed { .. }));
        let requests = captured.lock().expect("captured requests");
        let diff = &requests[0]["params"]["toolCall"]["content"][0];
        assert_eq!(diff["type"], "diff");
        assert_eq!(diff["oldText"], "old\n");
        assert_eq!(diff["newText"], "new\n");
        assert_eq!(
            requests[0]["params"]["toolCall"]["_meta"]["harn"]["approvalRequest"]["evidence_refs"]
                [0]["source"],
            "pre_approval"
        );
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
            HostPermissionOutcome::Rejected { reason, resolution } => {
                assert_eq!(reason, "cancelled by user");
                assert_eq!(
                    resolution.outcome,
                    crate::orchestration::ToolPermissionOutcome::Cancelled
                );
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

    #[test]
    fn portable_activity_event_excludes_raw_permission_values() {
        let session_id = crate::agent_sessions::open_or_create(Some(
            "permission-activity-event-test".to_string(),
        ));
        let evaluation = PolicyEvaluation {
            action: "ask".to_string(),
            reason: "contains secret-value".to_string(),
            matched_rule: None,
            required_approval: None,
            risk_labels: vec!["network_rule".to_string()],
            receipt: serde_json::json!({
                "context": {"command": "secret-value", "to": "private@example.com"}
            }),
        };

        emit_permission_activity(
            &session_id,
            "call-1",
            "gmail.create_draft",
            &evaluation,
            ToolPermissionPolicyLayer::UserPolicy,
            ToolPermissionResolution::approved(
                crate::orchestration::ToolPermissionDecider::Person,
                crate::orchestration::ToolPermissionGrantScope::Once,
            ),
        );

        let transcript = crate::agent_sessions::transcript(&session_id).expect("transcript");
        let rendered = serde_json::to_string(&crate::llm::helpers::vm_value_to_json(&transcript))
            .expect("json");
        assert!(rendered.contains("PermissionDecision"));
        assert!(rendered.contains("harn.tool_permission_activity.v1"));
        assert!(!rendered.contains("secret-value"));
        assert!(!rendered.contains("private@example.com"));
        assert!(!rendered.contains("arguments"));
        crate::agent_sessions::close(&session_id);
    }
}
