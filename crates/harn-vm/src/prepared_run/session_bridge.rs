//! Projection of a prepared session's grouped approval onto the existing ACP bridge.

use crate::bridge::HostBridge;
use crate::llm::acp_permission::{
    parse_response, request_params, WireOutcome, METHOD_REQUEST_PERMISSION,
};

use super::{ApprovalBatch, PreparedSessionApprovalDecision};

/// Ask the attached host for one grouped decision. Canonical ACP parsing owns
/// missing, malformed, rejected, and selected responses; the session state
/// machine still verifies the batch fingerprint before granting authority.
pub async fn request_session_approval(
    bridge: &HostBridge,
    session_id: &str,
    batch: &ApprovalBatch,
) -> Result<PreparedSessionApprovalDecision, String> {
    if session_id.is_empty() || bridge.get_session_id() != session_id {
        return Err("prepared approval bridge does not match the attached session".to_string());
    }
    let raw_input = serde_json::to_value(batch).map_err(|error| error.to_string())?;
    let summary = batch
        .groups
        .iter()
        .flat_map(|group| group.summaries.iter())
        .cloned()
        .collect::<Vec<_>>()
        .join("\n");
    let params = request_params(
        Some(session_id),
        &batch.batch_fingerprint,
        "prepared_session.start",
        &raw_input,
        serde_json::json!({"summary": summary, "risk": "prepared_run", "approval_batch": batch}),
        &serde_json::json!({"action": "ask", "reason": "prepared session requires grouped approval"}),
        None,
        crate::tool_annotations::ToolKind::Other,
    );
    let response = bridge
        .call(METHOD_REQUEST_PERMISSION, params)
        .await
        .map_err(|error| error.to_string())?;
    let (approved, resolution) = match parse_response(&response) {
        WireOutcome::Allowed { resolution } => (true, resolution),
        WireOutcome::Rejected { resolution, .. } => (false, resolution),
    };
    Ok(PreparedSessionApprovalDecision {
        batch_fingerprint: batch.batch_fingerprint.clone(),
        approved,
        decider: resolution.decider,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    use std::sync::Arc;

    fn responding_bridge(response: serde_json::Value, calls: Arc<AtomicUsize>) -> HostBridge {
        let pending = Arc::new(tokio::sync::Mutex::new(HashMap::<
            u64,
            tokio::sync::oneshot::Sender<serde_json::Value>,
        >::new()));
        let responses = pending.clone();
        let writer = Arc::new(move |line: &str| {
            let request: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(request["method"], METHOD_REQUEST_PERMISSION);
            assert_eq!(request["params"]["sessionId"], "session-1");
            assert_eq!(
                request["params"]["toolCall"]["rawInput"]["batch_fingerprint"],
                "batch-1"
            );
            assert_eq!(request["params"]["options"][0]["optionId"], "allow");
            calls.fetch_add(1, Ordering::SeqCst);
            let id = request["id"].as_u64().unwrap();
            responses
                .try_lock()
                .unwrap()
                .remove(&id)
                .unwrap()
                .send(serde_json::json!({"id": id, "result": response.clone()}))
                .map_err(|_| "approval response receiver closed".to_string())
        });
        let bridge = HostBridge::from_parts_with_writer(
            pending,
            Arc::new(AtomicBool::new(false)),
            writer,
            1,
        );
        bridge.set_session_id("session-1");
        bridge
    }

    fn batch() -> ApprovalBatch {
        ApprovalBatch {
            batch_fingerprint: "batch-1".to_string(),
            plan_fingerprint: "plan-1".to_string(),
            groups: vec![super::super::ApprovalGroup {
                semantic_group: "workspace".to_string(),
                requirement_fingerprints: vec!["requirement-1".to_string()],
                summaries: vec!["Write the declared workspace".to_string()],
                risk_labels: vec!["workspace".to_string()],
            }],
        }
    }

    #[tokio::test]
    async fn grouped_approval_uses_canonical_allowed_denied_and_malformed_answers() {
        for (response, approved) in [
            (
                serde_json::json!({"outcome":{"outcome":"selected","optionId":"allow"}}),
                true,
            ),
            (
                serde_json::json!({"outcome":{"outcome":"selected","optionId":"reject"}}),
                false,
            ),
            (serde_json::json!({}), false),
            (serde_json::json!({"outcome":"approved"}), false),
            (
                serde_json::json!({"outcome":{"outcome":"selected","optionId":"unknown"}}),
                false,
            ),
        ] {
            let calls = Arc::new(AtomicUsize::new(0));
            let bridge = responding_bridge(response, calls.clone());
            let decision = request_session_approval(&bridge, "session-1", &batch())
                .await
                .unwrap();
            assert_eq!(decision.approved, approved);
            assert_eq!(decision.batch_fingerprint, "batch-1");
            assert_eq!(calls.load(Ordering::SeqCst), 1);
        }
    }

    #[tokio::test]
    async fn another_session_cannot_use_the_approval_bridge() {
        let calls = Arc::new(AtomicUsize::new(0));
        let bridge = responding_bridge(serde_json::json!({}), calls.clone());
        assert!(request_session_approval(&bridge, "other-session", &batch())
            .await
            .is_err());
        assert_eq!(calls.load(Ordering::SeqCst), 0);
    }
}
