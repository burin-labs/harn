fn defang_directive_sentinels(text: &str) -> String {
    text.replace("<context-directives", "&lt;context-directives")
        .replace("</context-directives", "&lt;/context-directives")
        .replace("<directive", "&lt;directive")
        .replace("</directive", "&lt;/directive")
}

fn defang_tool_result_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::String(text) => {
            *text = defang_directive_sentinels(text);
        }
        serde_json::Value::Array(values) => {
            for value in values {
                defang_tool_result_value(value);
            }
        }
        serde_json::Value::Object(object) => {
            let entries = std::mem::take(object);
            for (key, mut value) in entries {
                defang_tool_result_value(&mut value);
                let mut safe_key = defang_directive_sentinels(&key);
                while object.contains_key(&safe_key) {
                    safe_key = format!("&amp;{safe_key}");
                }
                object.insert(safe_key, value);
            }
        }
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => {}
    }
}

/// Neutralize directive-shaped text only inside tool results at the one
/// provider-independent egress boundary. Durable history keeps the original
/// bytes for audit and replay; every provider dialect receives the same
/// defanged projection.
pub(super) fn defang_tool_result_directives(messages: &mut [serde_json::Value]) {
    for message in messages {
        let role_is_tool_result = matches!(
            message.get("role").and_then(serde_json::Value::as_str),
            Some("tool" | "tool_result")
        );
        if role_is_tool_result {
            if let Some(content) = message.get_mut("content") {
                defang_tool_result_value(content);
            }
            continue;
        }
        let Some(blocks) = message
            .get_mut("content")
            .and_then(serde_json::Value::as_array_mut)
        else {
            continue;
        };
        for block in blocks {
            if block.get("type").and_then(serde_json::Value::as_str) == Some("tool_result") {
                if let Some(content) = block.get_mut("content") {
                    defang_tool_result_value(content);
                }
            }
        }
    }
}
