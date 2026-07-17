pub(in crate::llm) fn approval_unavailable_tool_result(
    tool_name: &str,
    reason: impl Into<String>,
    denial_class: Option<&str>,
    class_repeat_count: Option<u64>,
) -> serde_json::Value {
    let reason = reason.into();
    let class = denial_class.unwrap_or("approval_required");
    let repeat = class_repeat_count.unwrap_or(1);
    let next_step = if repeat <= 1 {
        format!(
            "Approval is unavailable in this run for risk class `{class}`. Do not retry \
             `{tool_name}` or any argument/command variant with this same approval-required \
             risk class; it will be denied again. Choose an approach that stays within the \
             currently allowed tools and policy, or briefly report the blocked permission as \
             the reason you cannot continue."
        )
    } else {
        format!(
            "Risk class `{class}` was already marked terminal because approval is unavailable \
             in this run. Stop trying `{tool_name}` or equivalent variants in that class and \
             pivot to allowed work or report the missing permission."
        )
    };
    serde_json::json!({
        "error": "permission_denied",
        "tool": tool_name,
        "reason": reason,
        "denial_class": class,
        "class_repeat_count": repeat,
        "next_step": next_step,
    })
}
