use serde_json::{json, Value as JsonValue};
use uuid::Uuid;

use crate::value::VmError;

use super::{display_path, GitCommand, GitMutation};

pub(super) async fn enforce_git_approval(
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    command: &GitCommand,
) -> Result<Option<JsonValue>, VmError> {
    let args = json!({
        "operation": command.operation,
        "argv": command.argv,
        "cwd": display_path(&command.cwd),
        "affected_paths": command.affected_paths,
    });
    let policy_decision = crate::orchestration::current_approval_policy()
        .map(|policy| policy.evaluate_detailed(command.operation, &args));
    if let Some(decision) = policy_decision
        .as_ref()
        .filter(|decision| decision.is_deny())
    {
        return Err(VmError::CategorizedError {
            message: decision.reason.clone(),
            category: crate::value::ErrorCategory::ToolRejected,
        });
    }
    if command.mutation == GitMutation::Risky {
        if let Some(approval) = crate::orchestration::current_operator_approval_grant()
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
    ctx: Option<&crate::vm::AsyncBuiltinCtx>,
    operation: &str,
    args: &JsonValue,
    policy_decision: Option<JsonValue>,
) -> Result<JsonValue, VmError> {
    let Some(bridge) = ctx.and_then(|ctx| ctx.child_vm().bridge.clone()) else {
        return Err(VmError::CategorizedError {
            message: format!("{operation}: approval required but no host bridge is attached"),
            category: crate::value::ErrorCategory::ToolRejected,
        });
    };
    let approval_id = format!("git-{}", Uuid::now_v7());
    let approval_request = crate::stdlib::hitl::approval_request_for_host_permission(
        approval_id.clone(),
        operation.to_string(),
        args.clone(),
        crate::llm::current_agent_session_id().unwrap_or_else(|| "harn".to_string()),
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
            crate::llm::acp_permission::METHOD_REQUEST_PERMISSION,
            crate::llm::acp_permission::request_params(
                crate::llm::current_agent_session_id().as_deref(),
                &approval_id,
                operation,
                args,
                approval_request_json,
                &policy_decision.clone().unwrap_or(JsonValue::Null),
                None,
                crate::tool_annotations::ToolKind::Other,
            ),
        )
        .await?;
    match crate::llm::acp_permission::parse_response(&response) {
        crate::llm::acp_permission::WireOutcome::Allowed => Ok(response),
        crate::llm::acp_permission::WireOutcome::Rejected { reason } => {
            Err(VmError::CategorizedError {
                message: format!("{operation}: approval denied: {reason}"),
                category: crate::value::ErrorCategory::ToolRejected,
            })
        }
    }
}
