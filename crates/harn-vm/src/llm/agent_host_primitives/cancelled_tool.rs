//! The `tool_result` for a call `cancel_in_flight_tool_call` preempted.

/// Build the `tool_result` shape used when a call was preempted by
/// `cancel_in_flight_tool_call`. Distinct from `agent_primitive_denied_tool`
/// so the model can tell "user stopped me mid-run" from "the tool errored".
pub(super) fn agent_primitive_cancelled_tool(
    tool_name: &str,
    tool_call_id: &str,
    tool_args: &serde_json::Value,
    reason: &str,
    executor: Option<serde_json::Value>,
    execution_duration_ms: u64,
    approval_status: Option<&'static str>,
) -> serde_json::Value {
    let rendered = if reason.is_empty() {
        format!("[cancelled in-flight: {tool_name}]")
    } else {
        format!("[cancelled in-flight: {tool_name}] {reason}")
    };
    let observation = format!(
        "[cancelled call to {name}]\n{reason}\n[end of {name} cancellation]\n",
        name = tool_name,
        reason = if reason.is_empty() {
            "cancelled by host"
        } else {
            reason
        },
    );
    let error_message = if reason.is_empty() {
        format!("tool call cancelled in-flight: {tool_name}")
    } else {
        format!("tool call cancelled in-flight: {reason}")
    };
    let mut result = super::agent_primitive_unexecuted_tool_base(
        tool_name,
        tool_call_id,
        tool_args,
        "cancelled",
        rendered,
        observation,
        error_message,
    );
    if let Some(obj) = result.as_object_mut() {
        obj.insert(
            "executor".to_string(),
            executor.unwrap_or(serde_json::Value::Null),
        );
        obj.insert("approval".to_string(), serde_json::json!(approval_status));
        obj.insert(
            "execution_duration_ms".to_string(),
            serde_json::json!(execution_duration_ms),
        );
        obj.insert("cancelled".to_string(), serde_json::Value::Bool(true));
        obj.insert("cancellation_reason".to_string(), serde_json::json!(reason));
    }
    result
}

/// Mark a cancelled result applied when its handler finished a workspace
/// mutation before the cancel: a known change, not `unknown` (harn#9611).
pub(super) fn with_applied_mutations(
    mut result: serde_json::Value,
    session_id: &str,
    tool_call_id: &str,
) -> serde_json::Value {
    let applied = crate::tool_call_mutations::take(session_id, tool_call_id);
    if let Some(obj) = result.as_object_mut().filter(|_| !applied.is_empty()) {
        obj.insert(
            "mutation_status".to_string(),
            serde_json::json!(crate::agent_events::ToolMutationStatus::Applied.as_str()),
        );
        obj.insert("changed_paths".to_string(), serde_json::json!(applied));
    }
    result
}
