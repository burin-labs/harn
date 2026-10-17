use crate::request::LlmRequestPayload;
use harn_provider_catalog::capabilities::ReasoningRoundTripPolicy;
use serde_json::{json, Value};

/// Canonical message keys exclude durable reasoning and foreign tool metadata.
const MESSAGE_KEYS: &[&str] = &["role", "content", "cache_control"];

pub(super) fn encode_messages(
    opts: &LlmRequestPayload,
    reasoning_round_trip: ReasoningRoundTripPolicy,
) -> Vec<Value> {
    let mut messages: Vec<Value> = opts
        .messages
        .iter()
        .cloned()
        .map(|mut message| {
            crate::reasoning_history::restore_anthropic_continuation(
                &mut message,
                reasoning_round_trip,
            );
            message
        })
        .map(translate_tool_role_message)
        .map(translate_assistant_tool_calls)
        .filter_map(|mut message| {
            if let Some(object) = message.as_object_mut() {
                if let Some(content) = object.get("content").cloned() {
                    let content = crate::content::anthropic_content_for_request(
                        &content,
                        reasoning_round_trip,
                    );
                    let content = match content {
                        Value::Array(blocks) => Value::Array(
                            blocks
                                .into_iter()
                                .filter(|block| {
                                    !(block.get("type").and_then(Value::as_str) == Some("text")
                                        && block
                                            .get("text")
                                            .and_then(Value::as_str)
                                            .is_some_and(|text| text.trim().is_empty()))
                                })
                                .collect(),
                        ),
                        other => other,
                    };
                    object.insert("content".to_owned(), content);
                }
                object.retain(|key, _| MESSAGE_KEYS.contains(&key.as_str()));
            }
            let empty = match message.get("content") {
                Some(Value::String(text)) => text.trim().is_empty(),
                Some(Value::Array(blocks)) => blocks.is_empty(),
                _ => false,
            };
            (!empty).then_some(message)
        })
        .collect();
    super::tool_history::normalize_tool_call_ids(&mut messages);
    super::tool_history::echo_toolset_names(&mut messages);
    // Prefill admission follows caller overrides at the native egress seam.
    if let Some(prefill) = &opts.prefill {
        messages.push(json!({"role":"assistant","content":prefill}));
    }
    messages
}

/// Project foreign top-level tool results before their pairing keys are stripped.
pub(super) fn translate_tool_role_message(message: Value) -> Value {
    if !matches!(
        message.get("role").and_then(Value::as_str),
        Some("tool" | "tool_result")
    ) {
        return message;
    }
    let id = message
        .get("tool_call_id")
        .or_else(|| message.get("tool_use_id"))
        .or_else(|| message.get("call_id"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut result = json!({"type":"tool_result", "tool_use_id":id,
        "content":message.get("content").cloned().unwrap_or_else(|| json!(""))});
    for key in ["is_error", "toolset_name"] {
        if let Some(value) = message.get(key) {
            result[key] = value.clone();
        }
    }
    json!({"role":"user", "content":[result]})
}

/// Preserve assistant text and render foreign calls with the result's paired id.
fn translate_assistant_tool_calls(message: Value) -> Value {
    if message.get("role").and_then(Value::as_str) != Some("assistant") {
        return message;
    }
    let Some(calls) = message
        .get("tool_calls")
        .and_then(Value::as_array)
        .filter(|calls| !calls.is_empty())
    else {
        return message;
    };
    let mut blocks = match message.get("content") {
        Some(Value::String(text)) if !text.is_empty() => vec![json!({"type":"text","text":text})],
        Some(Value::Array(blocks)) => blocks.clone(),
        _ => Vec::new(),
    };
    for call in calls {
        let function = call.get("function");
        let name = function
            .and_then(|f| f.get("name"))
            .and_then(Value::as_str)
            .or_else(|| call.get("name").and_then(Value::as_str))
            .unwrap_or_default();
        let input = match function
            .and_then(|f| f.get("arguments"))
            .or_else(|| call.get("arguments"))
        {
            Some(Value::String(raw)) => {
                serde_json::from_str::<Value>(raw).unwrap_or_else(|_| json!({}))
            }
            Some(value) if value.is_object() => value.clone(),
            _ => json!({}),
        };
        let mut block = json!({"type":"tool_use", "id":call.get("id").and_then(Value::as_str).unwrap_or_default(),
            "name":name, "input":input});
        if let Some(namespace) = call.get("toolset_name") {
            block["toolset_name"] = namespace.clone();
        }
        blocks.push(block);
    }
    let mut message = message;
    if let Some(object) = message.as_object_mut() {
        object.remove("tool_calls");
        object.insert("content".to_owned(), Value::Array(blocks));
    }
    message
}
