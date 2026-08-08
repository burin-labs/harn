//! Prompt-cache breakpoint placement.
//!
//! A route either takes a top-level `cache_control` marker or one on the last
//! content block; the capability row says which. A breakpoint the caller
//! already placed anywhere in the body wins, so an explicit prefix marker is
//! never doubled.

pub(super) fn apply_prompt_cache_breakpoint(
    body: &mut serde_json::Value,
    cache_requested: bool,
    caps: &crate::llm::capabilities::Capabilities,
) {
    if !cache_requested || !caps.prompt_caching || body_contains_cache_control(body) {
        return;
    }
    match caps.cache_breakpoint_style.as_str() {
        "top_level" => {
            body["cache_control"] = serde_json::json!({"type": "ephemeral"});
        }
        "last_block" => {
            insert_last_message_cache_control(body);
        }
        _ => {}
    }
}

fn body_contains_cache_control(value: &serde_json::Value) -> bool {
    match value {
        serde_json::Value::Object(object) => {
            object.contains_key("cache_control") || object.values().any(body_contains_cache_control)
        }
        serde_json::Value::Array(values) => values.iter().any(body_contains_cache_control),
        _ => false,
    }
}

fn insert_last_message_cache_control(body: &mut serde_json::Value) -> bool {
    let Some(messages) = body
        .get_mut("messages")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return false;
    };
    for message in messages.iter_mut().rev() {
        if insert_message_cache_control(message) {
            return true;
        }
    }
    false
}

fn insert_message_cache_control(message: &mut serde_json::Value) -> bool {
    let Some(content) = message
        .as_object_mut()
        .and_then(|object| object.get_mut("content"))
    else {
        return false;
    };
    match content {
        serde_json::Value::String(text) => {
            if text.is_empty() {
                return false;
            }
            let text = text.clone();
            *content = serde_json::json!([{
                "type": "text",
                "text": text,
                "cache_control": {"type": "ephemeral"},
            }]);
            true
        }
        serde_json::Value::Array(blocks) => {
            for block in blocks.iter_mut().rev() {
                let Some(object) = block.as_object_mut() else {
                    continue;
                };
                if object.contains_key("cache_control") {
                    return true;
                }
                object.insert(
                    "cache_control".to_string(),
                    serde_json::json!({"type": "ephemeral"}),
                );
                return true;
            }
            false
        }
        serde_json::Value::Object(object) => {
            object
                .entry("cache_control".to_string())
                .or_insert_with(|| serde_json::json!({"type": "ephemeral"}));
            true
        }
        _ => false,
    }
}
