//! The one owner for "is this transcript message a tool result".
//!
//! Durable history records a tool result as `role: "tool_result"` on the native
//! channel and as a framed `role: "user"` string on the text channel; both carry
//! `_harn.kind == "tool_result"` facts. Imported transcripts and provider
//! projections may instead use `role: "tool"` or Anthropic `tool_result` blocks.

/// The durable message roles that carry a tool result.
pub fn is_tool_result_role(role: &str) -> bool {
    matches!(role, "tool" | "tool_result")
}

/// True when the message carries a provider-paired tool result: a
/// [`is_tool_result_role`] role, or a user message whose content blocks include
/// a `tool_result`. Text-channel results don't match; they have no
/// provider-level pairing to protect.
pub fn is_tool_result_message(message: &serde_json::Value) -> bool {
    match message.get("role").and_then(|role| role.as_str()) {
        Some(role) if is_tool_result_role(role) => true,
        Some("user") => message
            .get("content")
            .and_then(|content| content.as_array())
            .is_some_and(|blocks| {
                blocks.iter().any(|block| {
                    block.get("type").and_then(|value| value.as_str()) == Some("tool_result")
                })
            }),
        _ => false,
    }
}

/// True when the message carries tool output on any channel: its durable
/// `_harn` facts say so, or it is an [`is_tool_result_message`]. The text frame
/// alone is not provenance: a person or an imported transcript can write it.
pub fn is_tool_output_message(message: &serde_json::Value) -> bool {
    message
        .pointer("/_harn/kind")
        .and_then(|kind| kind.as_str())
        == Some("tool_result")
        || is_tool_result_message(message)
}

/// True for `[result of NAME]\n...\n[end of NAME result]` or the matching
/// `[error from NAME]` ... `[end of NAME error]` pair, as dispatch frames them.
pub fn is_framed_tool_observation(text: &str) -> bool {
    let Some((first_line, _)) = text.split_once('\n') else {
        return false;
    };
    let body_end = text.trim_end_matches('\n');
    [("[result of ", "result"), ("[error from ", "error")]
        .iter()
        .any(|(open, kind)| {
            first_line
                .strip_prefix(open)
                .and_then(|rest| rest.strip_suffix(']'))
                .filter(|name| !name.is_empty())
                .is_some_and(|name| body_end.ends_with(&format!("\n[end of {name} {kind}]")))
        })
}
