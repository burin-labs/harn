use super::client_v1::dispatch_to_v1;
use super::*;

#[tokio::test]
async fn v1_dispatch_handles_direct_messages_and_rejects_invalid_unions() {
    let (outcome, _) = dispatch_to_v1(json!({"message": {
        "messageId": "message-1", "contextId": "ctx-1", "role": "ROLE_AGENT", "parts": [{"text": "done"}]
    }})).await;
    assert!(
        matches!(outcome.unwrap().1, harn_vm::a2a::DispatchOutcome::InlineMessage { ref message_id, ref result }
        if message_id == "message-1" && result == &json!("done"))
    );
    let (invalid, _) = dispatch_to_v1(json!({"task": {}, "message": {}})).await;
    assert!(matches!(
        invalid,
        Err(harn_vm::a2a::A2aClientError::Protocol(_))
    ));
}

#[tokio::test]
async fn v1_pending_handles_retain_protocol_and_tenant_and_failures_do_not_succeed() {
    let (pending, _) = dispatch_to_v1(json!({"task": {
        "id": "task-pending", "status": {"state": "TASK_STATE_WORKING"}
    }}))
    .await;
    let harn_vm::a2a::DispatchOutcome::PendingTask { state, handle, .. } = pending.unwrap().1
    else {
        panic!("expected pending task");
    };
    assert_eq!(state, "working");
    assert_eq!(handle["protocol_version"], "1.0");
    assert_eq!(handle["tenant"], "team-a");
    let (failed, _) = dispatch_to_v1(json!({"task": {
        "id": "task-failed", "status": {"state": "TASK_STATE_FAILED"}
    }}))
    .await;
    assert!(matches!(
        failed,
        Err(harn_vm::a2a::A2aClientError::Protocol(_))
    ));
}
