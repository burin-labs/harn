pub(super) fn message_is_tool_result(message: &serde_json::Value) -> bool {
    harn_orchestration::tool_messages::is_tool_output_message(message)
        || message
            .get("content")
            .is_some_and(content_contains_tool_result)
}

/// Provider block shapes the shared predicate leaves to the wire adapters.
fn content_contains_tool_result(content: &serde_json::Value) -> bool {
    match content {
        serde_json::Value::String(text) => {
            harn_orchestration::tool_messages::is_framed_tool_observation(text)
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
