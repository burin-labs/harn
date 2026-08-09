//! Canonical request-side prompt-cache marker lowering.
//!
//! Provider adapters supply the marker payload (for example Anthropic's TTL),
//! while the capability matrix owns whether and where it is placed. Explicit
//! caller-authored markers always win.

use crate::llm::capabilities::{CacheBreakpointStyle, Capabilities};

pub(crate) fn apply_prompt_cache_breakpoint(
    body: &mut serde_json::Value,
    cache_requested: bool,
    caps: &Capabilities,
    marker: serde_json::Value,
) {
    if !cache_requested || !caps.prompt_caching || body_contains_cache_control(body) {
        return;
    }
    match caps.cache_breakpoint_style {
        CacheBreakpointStyle::TopLevel => body["cache_control"] = marker,
        CacheBreakpointStyle::LastBlock => {
            insert_last_message_cache_control(body, &marker);
        }
        CacheBreakpointStyle::None => {}
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

fn insert_last_message_cache_control(
    body: &mut serde_json::Value,
    marker: &serde_json::Value,
) -> bool {
    let Some(messages) = body
        .get_mut("messages")
        .and_then(serde_json::Value::as_array_mut)
    else {
        return false;
    };
    messages
        .iter_mut()
        .rev()
        .any(|message| insert_message_cache_control(message, marker))
}

fn insert_message_cache_control(
    message: &mut serde_json::Value,
    marker: &serde_json::Value,
) -> bool {
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
                "cache_control": marker,
            }]);
            true
        }
        serde_json::Value::Array(blocks) => blocks.iter_mut().rev().any(|block| {
            let Some(object) = block.as_object_mut() else {
                return false;
            };
            object
                .entry("cache_control".to_string())
                .or_insert_with(|| marker.clone());
            true
        }),
        serde_json::Value::Object(object) => {
            object
                .entry("cache_control".to_string())
                .or_insert_with(|| marker.clone());
            true
        }
        _ => false,
    }
}
