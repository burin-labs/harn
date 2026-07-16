use crate::agent_events::{ToolCallErrorCategory, ToolDenial, ToolMutationStatus};

pub(super) fn agent_primitive_denied_tool(
    tool_name: &str,
    tool_call_id: &str,
    tool_args: &serde_json::Value,
    reason: impl Into<String>,
    category: ToolCallErrorCategory,
    denial: Option<&ToolDenial>,
    resolved_repair: Option<serde_json::Value>,
) -> serde_json::Value {
    let reason = reason.into();
    // Recoverable argument failures coach a corrected retry; hard denials do not.
    // Tool ceilings are name resolution: repair unique calls or list callable names.
    let retryable_denial = denial.is_some_and(|denial| denial.retryable);
    let tool_ceiling_denial =
        denial.is_some_and(|denial| denial.gate == crate::agent_events::DenialGate::ToolCeiling);
    let approval_unavailable_denial =
        denial.filter(|denial| denial.gate == crate::agent_events::DenialGate::ApprovalUnavailable);
    // `deny_tool_call` is the sole owner of resolving repairs and normalizing
    // their typed denial. This result builder only projects that contract.
    let denial_json = denial.map(ToolDenial::to_json);
    let mut result = if let Some(repair) = resolved_repair {
        repair
    } else if category.is_recoverable() || retryable_denial {
        super::agent_tools::recoverable_tool_result(tool_name, reason.clone())
    } else if tool_ceiling_denial {
        super::agent_tools::unavailable_tool_result(tool_name, reason.clone())
    } else if let Some(denial) = approval_unavailable_denial {
        super::agent_tools::approval_denials::approval_unavailable_tool_result(
            tool_name,
            reason.clone(),
            denial.denial_class.as_deref(),
            denial.class_repeat_count,
        )
    } else {
        super::agent_tools::denied_tool_result(tool_name, reason.clone())
    };
    // Mirror the denial into the result and envelope. A successful name repair
    // makes both copies retryable, matching the corrected next step.
    if let Some(denial_json) = denial_json.clone() {
        if let Some(obj) = result.as_object_mut() {
            obj.insert("denial".to_string(), denial_json);
        }
    }
    let rendered = super::agent_tools::render_tool_result(&result);
    let observation = format!("[result of {tool_name}]\n{rendered}\n[end of {tool_name} result]\n");
    serde_json::json!({
        "ok": false,
        "status": "error",
        "tool_name": tool_name,
        "tool_call_id": tool_call_id,
        "arguments": tool_args,
        "result": result,
        "rendered_result": rendered,
        "observation": observation,
        "error": reason,
        "error_category": category.as_str(),
        "mutation_status": ToolMutationStatus::NotApplied.as_str(),
        "denial": denial_json,
        "executor": null,
    })
}
