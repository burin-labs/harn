//! Response-side extraction for the tool-conformance probe.
//!
//! Everything here answers "what did the provider actually send back?": native
//! `tool_calls` in each dialect's shape, assistant text with private reasoning
//! blocks filtered out, and the bounded samples that land in a probe report.
//! Split out of `tool_conformance.rs` so the probe module keeps to case
//! orchestration and classification; `tool_conformance_parse.rs` remains the
//! owner of Harn's *text* tool grammar.

use serde_json::{json, Value};

#[derive(Debug)]
pub(super) struct NativeToolCall {
    pub(super) name: String,
    pub(super) arguments: Option<Value>,
}

pub(super) fn extract_native_tool_calls(response: &Value) -> Vec<NativeToolCall> {
    // Streaming aggregation owns this top-level list. Its raw `frames` remain
    // diagnostic evidence and must not become a second semantic call source.
    if let Some(tool_calls) = response.get("tool_calls").and_then(Value::as_array) {
        return tool_calls
            .iter()
            .filter_map(parse_native_tool_call)
            .collect();
    }
    let mut calls = Vec::new();
    visit_native_tool_call_arrays(response, &mut calls);
    calls
}

pub(super) fn visit_native_tool_call_arrays(value: &Value, calls: &mut Vec<NativeToolCall>) {
    match value {
        Value::Object(map) => {
            if let Some(call) = parse_anthropic_tool_use_object(map) {
                calls.push(call);
            }
            if let Some(tool_calls) = map.get("tool_calls").and_then(Value::as_array) {
                for item in tool_calls {
                    if let Some(call) = parse_native_tool_call(item) {
                        calls.push(call);
                    }
                }
            }
            for child in map.values() {
                visit_native_tool_call_arrays(child, calls);
            }
        }
        Value::Array(items) => {
            for item in items {
                visit_native_tool_call_arrays(item, calls);
            }
        }
        _ => {}
    }
}

pub(super) fn parse_anthropic_tool_use_object(
    object: &serde_json::Map<String, Value>,
) -> Option<NativeToolCall> {
    if object.get("type").and_then(Value::as_str) != Some("tool_use") {
        return None;
    }
    let name = object
        .get("name")
        .and_then(Value::as_str)
        .filter(|name| !name.is_empty())?
        .to_string();
    let arguments = object
        .get("input")
        .or_else(|| object.get("arguments"))
        .cloned()
        .unwrap_or_else(|| json!({}));
    Some(NativeToolCall {
        name,
        arguments: Some(arguments),
    })
}

pub(super) fn parse_native_tool_call(item: &Value) -> Option<NativeToolCall> {
    let obj = item.as_object()?;
    let function = obj.get("function").and_then(Value::as_object);
    let name = function
        .and_then(|function| function.get("name"))
        .or_else(|| obj.get("name"))
        .and_then(Value::as_str)?
        .to_string();
    match crate::llm::tools::parse_text_tool_call_from_native_name(&name) {
        crate::llm::tools::NativeToolNameTextCall::Parsed { name, arguments } => {
            return Some(NativeToolCall {
                name,
                arguments: Some(arguments),
            });
        }
        crate::llm::tools::NativeToolNameTextCall::Malformed { name, .. } => {
            return Some(NativeToolCall {
                name,
                arguments: None,
            });
        }
        crate::llm::tools::NativeToolNameTextCall::NotCall => {}
    }
    let raw_args = function
        .and_then(|function| function.get("arguments"))
        .or_else(|| obj.get("arguments"));
    let arguments = match raw_args {
        Some(Value::String(raw)) => serde_json::from_str::<Value>(raw).ok(),
        Some(value @ Value::Object(_)) => Some(value.clone()),
        Some(_) => None,
        None => Some(json!({})),
    };
    Some(NativeToolCall { name, arguments })
}

pub(super) fn extract_content(response: &Value) -> String {
    let mut parts = Vec::new();
    visit_content(response, &mut parts);
    parts
        .into_iter()
        .filter(|part| !part.trim().is_empty())
        .collect::<Vec<_>>()
        .join("\n")
}

pub(super) fn visit_content(value: &Value, parts: &mut Vec<String>) {
    match value {
        Value::Object(map) => {
            if object_is_private_reasoning_content(map) {
                return;
            }
            for key in ["content", "response", "text"] {
                if let Some(text) = map.get(key).and_then(Value::as_str) {
                    parts.push(text.to_string());
                }
            }
            for (key, child) in map {
                if field_is_private_reasoning(key) {
                    continue;
                }
                visit_content(child, parts);
            }
        }
        Value::Array(items) => {
            for item in items {
                visit_content(item, parts);
            }
        }
        _ => {}
    }
}

pub(super) fn object_is_private_reasoning_content(map: &serde_json::Map<String, Value>) -> bool {
    let block_type = map.get("type").and_then(Value::as_str).unwrap_or("");
    if matches!(block_type, "reasoning" | "thinking" | "reasoning_summary") {
        return true;
    }
    matches!(
        map.get("visibility").and_then(Value::as_str),
        Some("private" | "internal")
    )
}

pub(super) fn field_is_private_reasoning(field: &str) -> bool {
    matches!(
        field,
        "analysis"
            | "reasoning"
            | "reasoning_content"
            | "reasoning_details"
            | "reasoning_summary"
            | "thinking"
            | "thinking_summary"
    )
}

pub(super) fn has_raw_model_tool_tag(content: &str) -> bool {
    let lowered = content.to_ascii_lowercase();
    lowered.contains("<tool_call")
        || lowered.contains("<toolcall")
        || lowered.contains("tool_code:")
        || lowered.contains("tool_call:")
        || lowered.contains("call:")
        || lowered.contains("<function")
}

pub(super) fn content_sample(response: &Value) -> Option<String> {
    sample_content(&extract_content(response))
}

pub(super) fn sample_content(content: &str) -> Option<String> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.chars().take(240).collect())
    }
}

pub(super) fn sample_failure(text: &str, fallback: &str) -> String {
    let trimmed = text.trim();
    if trimmed.is_empty() {
        fallback.to_string()
    } else {
        format!(
            "{fallback}: {}",
            trimmed.chars().take(240).collect::<String>()
        )
    }
}

pub(super) fn first_non_empty(value: Option<String>, fallback: &str) -> String {
    value
        .filter(|value| !value.trim().is_empty())
        .unwrap_or_else(|| fallback.to_string())
}
