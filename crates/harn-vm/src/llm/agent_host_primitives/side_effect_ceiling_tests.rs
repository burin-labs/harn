use std::collections::HashMap;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::Mutex;

use super::host_agent_dispatch_tool_call;
use super::side_effect_ceiling::{request_side_effect_permission, SideEffectPermissionOutcome};
use crate::agent_events::{DenialGate, SideEffectCeilingRemedy};
use crate::bridge::HostBridge;
use crate::orchestration::SideEffectCeilingViolation;
use crate::tool_annotations::SideEffectLevel;

struct HostBridgeGuard {
    previous: Option<Arc<HostBridge>>,
}

impl HostBridgeGuard {
    fn replace(bridge: Option<Arc<HostBridge>>) -> Self {
        Self {
            previous: crate::llm::swap_current_host_bridge(bridge),
        }
    }
}

impl Drop for HostBridgeGuard {
    fn drop(&mut self) {
        let _ = crate::llm::swap_current_host_bridge(self.previous.take());
    }
}

fn policy_options(session_id: &str) -> crate::value::DictMap {
    let mut options = crate::value::DictMap::new();
    options.insert(
        crate::value::intern_key("session_id"),
        crate::stdlib::json_to_vm_value(&serde_json::json!(session_id)),
    );
    options.insert(
        crate::value::intern_key("policy"),
        crate::stdlib::json_to_vm_value(&serde_json::json!({
            "tools": ["read_file"],
            "side_effect_level": "read_only",
            "tool_annotations": {
                "read_file": {
                    "kind": "read",
                    "side_effect_level": "process_exec"
                }
            }
        })),
    );
    options
}

async fn dispatch_read_file(
    path: &std::path::Path,
    options: &crate::value::DictMap,
) -> serde_json::Value {
    let call = crate::stdlib::json_to_vm_value(&serde_json::json!({
        "id": "side-effect-ceiling-call",
        "name": "read_file",
        "arguments": {"path": path},
    }));
    let result = host_agent_dispatch_tool_call(
        crate::vm::AsyncBuiltinCtx::for_test(crate::vm::Vm::new()),
        call,
        None,
        options,
    )
    .await
    .expect("policy refusal is a normal tool result");
    crate::llm::helpers::vm_value_to_json(&result)
}

fn side_effect_violation() -> SideEffectCeilingViolation {
    SideEffectCeilingViolation {
        ceiling: SideEffectLevel::ReadOnly,
        required_level: SideEffectLevel::ProcessExec,
    }
}

fn responding_bridge(
    outcome: serde_json::Value,
    requests: Arc<StdMutex<Vec<serde_json::Value>>>,
) -> Arc<HostBridge> {
    let pending: Arc<Mutex<HashMap<u64, tokio::sync::oneshot::Sender<serde_json::Value>>>> =
        Arc::new(Mutex::new(HashMap::new()));
    let response_pending = pending.clone();
    let writer = Arc::new(move |line: &str| {
        let request: serde_json::Value = serde_json::from_str(line)
            .map_err(|error| format!("invalid bridge request: {error}"))?;
        requests
            .lock()
            .map_err(|_| "captured request mutex poisoned".to_string())?
            .push(request.clone());
        let id = request
            .get("id")
            .and_then(serde_json::Value::as_u64)
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
                "result": outcome.clone(),
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

#[tokio::test]
async fn side_effect_ceiling_without_host_is_terminal_and_actionable() {
    crate::orchestration::clear_execution_policy_stacks();
    let _bridge_guard = HostBridgeGuard::replace(None);
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("proof.txt");
    std::fs::write(&path, "should not be read").expect("fixture");

    let result = dispatch_read_file(&path, &policy_options("side-effect-no-host")).await;

    assert_eq!(result["ok"], serde_json::json!(false));
    assert_eq!(
        result["denial"]["gate"],
        serde_json::json!("side_effect_ceiling")
    );
    assert_eq!(
        result["denial"]["side_effect_ceiling"]["ceiling"],
        serde_json::json!("read_only")
    );
    assert_eq!(
        result["denial"]["side_effect_ceiling"]["required_level"],
        serde_json::json!("process_exec")
    );
    assert_eq!(
        result["denial"]["side_effect_ceiling"]["remedy"],
        serde_json::json!("raise_side_effect_ceiling")
    );
    let next_step = result["result"]["next_step"]
        .as_str()
        .expect("actionable next step");
    assert!(next_step.contains("`process_exec`"));
    assert!(next_step.contains("`read_only`"));
    assert!(next_step.contains("non-mutating approach"));
    assert!(next_step.contains("raise"));
    assert_eq!(result["executor"], serde_json::Value::Null);
}

#[tokio::test]
async fn side_effect_ceiling_allow_runs_exactly_the_approved_dispatch() {
    crate::orchestration::clear_execution_policy_stacks();
    let captured = Arc::new(StdMutex::new(Vec::new()));
    let bridge = responding_bridge(
        crate::llm::acp_permission::allow_response(),
        captured.clone(),
    );
    let _bridge_guard = HostBridgeGuard::replace(Some(bridge));
    let directory = tempfile::tempdir().expect("tempdir");
    let path = directory.path().join("proof.txt");
    std::fs::write(&path, "approved fixture").expect("fixture");

    let result = dispatch_read_file(&path, &policy_options("side-effect-allow")).await;

    assert_eq!(result["ok"], serde_json::json!(true));
    assert_eq!(
        result["rendered_result"],
        serde_json::json!("1\tapproved fixture")
    );
    let requests = captured.lock().expect("captured requests");
    assert_eq!(requests.len(), 1);
    assert_eq!(
        requests[0]["method"],
        serde_json::json!(crate::llm::acp_permission::METHOD_REQUEST_PERMISSION)
    );
    assert_eq!(
        requests[0]["params"]["toolCall"]["_meta"]["harn"]["policyDecision"]["scope"],
        serde_json::json!("once")
    );
    assert_eq!(
        requests[0]["params"]["options"][0]["kind"],
        serde_json::json!("allow_once")
    );
}

#[tokio::test]
async fn side_effect_ceiling_rejection_stays_terminal() {
    let captured = Arc::new(StdMutex::new(Vec::new()));
    let bridge = responding_bridge(
        crate::llm::acp_permission::reject_response(Some("user declined".to_string())),
        captured,
    );
    let outcome = request_side_effect_permission(
        Some(&bridge),
        "side-effect-reject",
        "call-reject",
        "read_file",
        &serde_json::json!({"path": "proof.txt"}),
        side_effect_violation(),
        "side effect blocked".to_string(),
        None,
    )
    .await;

    match outcome {
        SideEffectPermissionOutcome::Denied {
            denial, escalated, ..
        } => {
            assert!(escalated);
            assert_eq!(denial.gate, DenialGate::HostRejected);
            assert!(!denial.retryable);
            assert_eq!(
                denial.side_effect_ceiling.expect("typed details").remedy,
                SideEffectCeilingRemedy::RequestPermission
            );
        }
        SideEffectPermissionOutcome::Allowed { .. } => panic!("rejection must not allow dispatch"),
    }
}

#[tokio::test]
async fn side_effect_ceiling_transport_failure_stays_terminal() {
    let bridge = Arc::new(HostBridge::from_parts_with_writer(
        Arc::new(Mutex::new(HashMap::new())),
        Arc::new(AtomicBool::new(false)),
        Arc::new(|_| Err("simulated transport failure".to_string())),
        1,
    ));
    let outcome = request_side_effect_permission(
        Some(&bridge),
        "side-effect-transport",
        "call-transport",
        "read_file",
        &serde_json::json!({"path": "proof.txt"}),
        side_effect_violation(),
        "side effect blocked".to_string(),
        None,
    )
    .await;

    match outcome {
        SideEffectPermissionOutcome::Denied {
            denial, escalated, ..
        } => {
            assert!(escalated);
            assert_eq!(denial.gate, DenialGate::ApprovalUnavailable);
            assert!(!denial.retryable);
            assert_eq!(
                denial.side_effect_ceiling.expect("typed details").remedy,
                SideEffectCeilingRemedy::RequestPermission
            );
        }
        SideEffectPermissionOutcome::Allowed { .. } => {
            panic!("transport failure must not allow dispatch")
        }
    }
}
