//! The model's declared purpose reaches the host's approval request as
//! `toolCall._meta.harn.intent` on both ask paths: the side-effect ceiling
//! escalation and the approval-policy `ask`. The child reaches the parent's
//! bridge fixtures through `use super::*`.

use super::*;

fn with_label(mut options: crate::value::DictMap, label: &str) -> crate::value::DictMap {
    options.insert(
        crate::value::intern_key("_purpose_label"),
        crate::stdlib::json_to_vm_value(&serde_json::json!(label)),
    );
    options
}

fn rejecting_bridge(captured: Arc<StdMutex<Vec<serde_json::Value>>>) -> Arc<HostBridge> {
    responding_bridge(
        crate::llm::acp_permission::reject_response(Some("fixture rejection".to_string())),
        captured,
    )
}

fn only_permission_meta(captured: &Arc<StdMutex<Vec<serde_json::Value>>>) -> serde_json::Value {
    let requests = captured.lock().expect("captured requests");
    assert_eq!(requests.len(), 1, "exactly one permission request");
    assert_eq!(
        requests[0]["method"],
        serde_json::json!(crate::llm::acp_permission::METHOD_REQUEST_PERMISSION)
    );
    requests[0]["params"]["toolCall"]["_meta"]["harn"].clone()
}

async fn side_effect_ask_meta(options: crate::value::DictMap) -> serde_json::Value {
    crate::orchestration::clear_execution_policy_stacks();
    let captured = Arc::new(StdMutex::new(Vec::new()));
    let _bridge_guard = HostBridgeGuard::replace(Some(rejecting_bridge(captured.clone())));
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("proof.txt");
    std::fs::write(&path, "must not run before approval").expect("fixture");
    let result = dispatch_read_file(&path, &options).await;
    assert_eq!(result["denial"]["gate"], serde_json::json!("host_rejected"));
    only_permission_meta(&captured)
}

#[tokio::test]
async fn side_effect_ask_carries_the_normalized_intent() {
    let options = with_label(
        policy_options("intent-side-effect"),
        "  Looking for\n  PR 456 artifacts ",
    );
    let meta = side_effect_ask_meta(options).await;
    assert_eq!(
        meta["intent"],
        serde_json::json!("Looking for PR 456 artifacts")
    );
    assert_eq!(meta["toolName"], serde_json::json!("read_file"));
}

#[tokio::test]
async fn side_effect_ask_without_a_label_has_no_intent_key() {
    let meta = side_effect_ask_meta(policy_options("intent-side-effect-none")).await;
    assert_eq!(meta["toolName"], serde_json::json!("read_file"));
    assert!(meta.get("intent").is_none(), "no label, no key: {meta}");
}

#[tokio::test]
async fn side_effect_ask_drops_a_blank_label() {
    let options = with_label(policy_options("intent-side-effect-blank"), " \n\t ");
    let meta = side_effect_ask_meta(options).await;
    assert!(meta.get("intent").is_none(), "blank label, no key: {meta}");
}

async fn approval_policy_ask_meta(options: crate::value::DictMap) -> serde_json::Value {
    crate::orchestration::clear_execution_policy_stacks();
    crate::orchestration::clear_all_approval_policy_repeat_counts();
    let policy: crate::orchestration::ToolApprovalPolicy =
        serde_json::from_value(serde_json::json!({
            "rules": [{"ask": {"tool": "read_file"}, "reason": "reads ask in this fixture"}]
        }))
        .expect("approval policy");
    crate::orchestration::push_approval_policy(policy);
    let captured = Arc::new(StdMutex::new(Vec::new()));
    let _bridge_guard = HostBridgeGuard::replace(Some(rejecting_bridge(captured.clone())));
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("proof.txt");
    std::fs::write(&path, "must not run before approval").expect("fixture");
    let result = dispatch_read_file(&path, &options).await;
    crate::orchestration::pop_approval_policy();
    crate::orchestration::clear_all_approval_policy_repeat_counts();
    assert_eq!(result["ok"], serde_json::json!(false), "{result}");
    only_permission_meta(&captured)
}

fn session_options(session_id: &str) -> crate::value::DictMap {
    let mut options = crate::value::DictMap::new();
    options.insert(
        crate::value::intern_key("session_id"),
        crate::stdlib::json_to_vm_value(&serde_json::json!(session_id)),
    );
    options
}

#[tokio::test]
async fn approval_policy_ask_carries_a_bounded_intent() {
    let long = format!("Looking for PR 456 artifacts {}", "x".repeat(400));
    let options = with_label(session_options("intent-approval-policy"), &long);
    let meta = approval_policy_ask_meta(options).await;
    let intent = meta["intent"].as_str().expect("intent string");
    assert!(intent.starts_with("Looking for PR 456 artifacts "));
    assert!(intent.ends_with('\u{2026}'));
    assert_eq!(
        intent.chars().count(),
        crate::agent_events::TOOL_CALL_INTENT_MAX_CHARS
    );
    assert_eq!(meta["policyDecision"]["action"], serde_json::json!("ask"));
}

#[tokio::test]
async fn approval_policy_ask_without_a_label_has_no_intent_key() {
    let meta = approval_policy_ask_meta(session_options("intent-approval-policy-none")).await;
    assert!(meta.get("intent").is_none(), "no label, no key: {meta}");
}
