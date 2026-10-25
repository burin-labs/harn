use serde_json::{json, Value as JsonValue};
use uuid::Uuid;

use harn_vm_core::value::VmError;

use super::{display_path, GitCommand, GitMutation};

pub(super) async fn enforce_git_approval(
    ctx: Option<&harn_vm_core::vm::AsyncBuiltinCtx>,
    command: &GitCommand,
) -> Result<Option<JsonValue>, VmError> {
    let args = json!({
        "operation": command.operation,
        "argv": command.argv,
        "cwd": display_path(&command.cwd),
        "affected_paths": command.affected_paths,
    });
    let policy_decision = harn_vm::orchestration::current_run_approval_policy()
        .map(|policy| policy.evaluate_detailed_with_repeat(command.operation, &args, 0));
    if let Some(decision) = policy_decision
        .as_ref()
        .filter(|decision| decision.is_deny())
    {
        return Err(VmError::CategorizedError {
            message: decision.reason.clone(),
            category: harn_vm_core::value::ErrorCategory::ToolRejected,
        });
    }
    if command.mutation == GitMutation::Risky {
        if let Some(approval) = harn_vm::orchestration::current_operator_approval_grant()
            .and_then(|grant| grant.receipt_for(command.operation))
        {
            return Ok(Some(approval));
        }
        return request_permission(
            ctx,
            command.operation,
            &args,
            policy_decision.map(|decision| decision.receipt),
        )
        .await
        .map(Some);
    }
    let Some(decision) = policy_decision else {
        return Ok(None);
    };
    if decision.is_allow() {
        Ok(None)
    } else {
        request_permission(ctx, command.operation, &args, Some(decision.receipt))
            .await
            .map(Some)
    }
}

async fn request_permission(
    ctx: Option<&harn_vm_core::vm::AsyncBuiltinCtx>,
    operation: &str,
    args: &JsonValue,
    policy_decision: Option<JsonValue>,
) -> Result<JsonValue, VmError> {
    use harn_vm::native_vm::NativeVmState;
    let Some(bridge) = ctx.and_then(|ctx| ctx.child_vm().native().bridge.clone()) else {
        return Err(VmError::CategorizedError {
            message: format!("{operation}: approval required but no host bridge is attached"),
            category: harn_vm_core::value::ErrorCategory::ToolRejected,
        });
    };
    let approval_id = format!("git-{}", Uuid::now_v7());
    let approval_request = harn_vm::stdlib::hitl::approval_request_for_host_permission(
        approval_id.clone(),
        operation.to_string(),
        args.clone(),
        harn_vm::llm::current_agent_session_id().unwrap_or_else(|| "harn".to_string()),
        Vec::new(),
        policy_decision
            .as_ref()
            .map(|decision| json!({"policy_decision": decision}))
            .unwrap_or(JsonValue::Null),
        vec![format!("stdlib.{operation}")],
    );
    let approval_request_json = serde_json::to_value(&approval_request).unwrap_or(JsonValue::Null);
    let response = bridge
        .call(
            harn_vm::llm::acp_permission::METHOD_REQUEST_PERMISSION,
            harn_vm::llm::acp_permission::request_params(
                harn_vm::llm::current_agent_session_id().as_deref(),
                &approval_id,
                operation,
                args,
                approval_request_json,
                &policy_decision.clone().unwrap_or(JsonValue::Null),
                None,
                None,
            ),
        )
        .await?;
    match harn_vm::llm::acp_permission::parse_response(&response) {
        harn_vm::llm::acp_permission::WireOutcome::Allowed { .. } => Ok(response),
        harn_vm::llm::acp_permission::WireOutcome::Rejected { reason, .. } => {
            Err(VmError::CategorizedError {
                message: format!("{operation}: approval denied: {reason}"),
                category: harn_vm_core::value::ErrorCategory::ToolRejected,
            })
        }
    }
}
