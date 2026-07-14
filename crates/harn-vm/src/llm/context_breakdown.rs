pub(super) fn message_is_tool_result(message: &serde_json::Value) -> bool {
    matches!(
        message.get("role").and_then(serde_json::Value::as_str),
        Some("tool" | "tool_result")
    ) || message
        .get("content")
        .is_some_and(content_contains_tool_result)
}

fn content_contains_tool_result(content: &serde_json::Value) -> bool {
    match content {
        serde_json::Value::String(text) => {
            text.starts_with("[result of ")
                && text.contains("\n[end of ")
                && text.trim_end().ends_with(" result]")
        }
        serde_json::Value::Array(parts) => parts.iter().any(content_contains_tool_result),
        serde_json::Value::Object(map) => {
            map.get("type").and_then(serde_json::Value::as_str) == Some("tool_result")
                || map.contains_key("toolResult")
                || map.get("text").is_some_and(content_contains_tool_result)
                || map.get("content").is_some_and(content_contains_tool_result)
        }
        _ => false,
    }
}
