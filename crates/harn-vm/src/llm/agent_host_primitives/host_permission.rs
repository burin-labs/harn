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
    pub tool_annotations: Option<crate::tool_annotations::ToolAnnotations>,
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
