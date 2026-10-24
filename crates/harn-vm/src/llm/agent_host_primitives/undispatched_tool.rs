//! The `tool_result` shapes for calls that never produced a real outcome.

/// Shared base `tool_result` shape for a call that never produced a real
/// tool outcome. Extended by [`super::cancelled_tool::agent_primitive_cancelled_tool`] (preempted
/// in-flight) and [`agent_primitive_undispatched_tool`] (never dispatched at
/// all) so every non-dispatch path records the same transcript shape.
pub(super) fn agent_primitive_unexecuted_tool_base(
    tool_name: &str,
    tool_call_id: &str,
    tool_args: &serde_json::Value,
    status: &str,
    rendered: String,
    observation: String,
    error_message: String,
) -> serde_json::Value {
    serde_json::json!({
        "ok": false,
        "status": status,
        "tool_name": tool_name,
        "tool_call_id": tool_call_id,
        "arguments": tool_args,
        "result": serde_json::Value::Null,
        "rendered_result": rendered,
        "observation": observation,
        "error": error_message,
        "error_category": crate::agent_events::ToolCallErrorCategory::Cancelled.as_str(),
        "mutation_status": crate::agent_events::ToolMutationStatus::Unknown.as_str(),
    })
}

/// Build the `tool_result` shape for a call that was persisted as part of an
/// assistant tool_use turn but will never be dispatched — a pre-dispatch
/// interrupt (`status: "interrupted"`), an `agent_await_resumption`
/// suspension (`status: "awaiting_resumption"`), or a sibling call skipped by
/// either (`status: "skipped"`). Mirrors the cancelled-tool shape so hosts
/// and transcripts see one consistent "no real outcome" record.
pub(super) fn agent_primitive_undispatched_tool(
    tool_name: &str,
    tool_call_id: &str,
    tool_args: &serde_json::Value,
    status: &str,
    reason: &str,
) -> serde_json::Value {
    let rendered = if reason.is_empty() {
        format!("[not dispatched ({status}): {tool_name}]")
    } else {
        format!("[not dispatched ({status}): {tool_name}] {reason}")
    };
    let observation = format!(
        "[call to {name} was not dispatched: {status}]\n{reason}\n[end of {name} non-dispatch notice]\n",
        name = tool_name,
        reason = if reason.is_empty() {
            "the call was never executed"
        } else {
            reason
        },
    );
    let error_message = if reason.is_empty() {
        format!("tool call not dispatched ({status}): {tool_name}")
    } else {
        format!("tool call not dispatched ({status}): {reason}")
    };
    let mut result = agent_primitive_unexecuted_tool_base(
        tool_name,
        tool_call_id,
        tool_args,
        status,
        rendered,
        observation,
        error_message,
    );
    if let Some(obj) = result.as_object_mut() {
        obj.insert(
            "mutation_status".to_string(),
            serde_json::json!(crate::agent_events::ToolMutationStatus::NotApplied.as_str()),
        );
        obj.insert("dispatched".to_string(), serde_json::Value::Bool(false));
        obj.insert("skip_reason".to_string(), serde_json::json!(reason));
    }
    result
}
