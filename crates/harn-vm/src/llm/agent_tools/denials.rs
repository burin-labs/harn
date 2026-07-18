//! User-facing result bodies for terminal policy denials.

use crate::agent_events::SideEffectCeilingDetails;

pub(super) fn denied_tool_result(tool_name: &str, reason: impl Into<String>) -> serde_json::Value {
    let reason = reason.into();
    // A bare `{"error":"permission_denied", ...}` tells the model what was
    // blocked but not what to do instead, so it tends to retry the same denied
    // call. Add a generic, capability-gate-appropriate next step: don't repeat
    // the call, find another way to make progress, or ask for the permission.
    //
    // RESERVED FOR TRUE POLICY/PERMISSION DENIALS (capability gate, sandbox /
    // command policy, host rejection). The "Do not retry the same call" wording
    // is *correct* there — the call is blocked and re-issuing it identically
    // will be blocked again. Recoverable rejections (bad/missing arguments, an
    // empty tool name) must NOT use this body: see `recoverable_tool_result`,
    // which coaches a retry *with the correction* instead.
    // Name the tools that ARE callable so the model can self-correct in one
    // turn instead of re-issuing the denied call or guessing another unlisted
    // name (live fw-gpt-oss-120b transcripts: cheap models emit Codex/container
    // vocab they were never shown, read a bare denial, and thrash). The active
    // policy's allowlist is the source of truth; omit the clause when the
    // surface is unbounded (allow-all) so we never assert a misleading list.
    let allowed = crate::orchestration::current_allowed_tool_names();
    let available_clause = if allowed.is_empty() {
        String::new()
    } else {
        format!(" Available tools: {}.", allowed.join(", "))
    };
    let next_step = format!(
        "The `{tool_name}` tool is not permitted right now. Do not retry the same call. \
         Make progress with the tools you are allowed to use, or if this capability is \
         essential, briefly tell the user what you need permission for and why.\
         {available_clause}"
    );
    serde_json::json!({
        "error": "permission_denied",
        "tool": tool_name,
        "reason": reason,
        "next_step": next_step,
    })
}

/// Build actionable feedback for a hard side-effect ceiling. Unlike a generic
/// permission denial, this names the typed policy facts so the model can
/// choose a non-mutating path or ask an operator to change the owned policy.
pub(super) fn side_effect_ceiling_tool_result(
    tool_name: &str,
    reason: impl Into<String>,
    details: &SideEffectCeilingDetails,
) -> serde_json::Value {
    let reason = reason.into();
    let next_step = format!(
        "`{tool_name}` requires side-effect level `{}`, but this session permits only through `{}`. \
         Do not retry the same call. Choose a non-mutating approach, or ask the operator to raise \
         the session side-effect ceiling to `{}` before retrying.",
        details.required_level.as_str(),
        details.ceiling.as_str(),
        details.required_level.as_str(),
    );
    serde_json::json!({
        "error": "permission_denied",
        "tool": tool_name,
        "reason": reason,
        "next_step": next_step,
    })
}
