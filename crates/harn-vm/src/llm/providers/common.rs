use std::rc::Rc;

use crate::llm::api::{DeltaSender, LlmResult};
use crate::value::{VmError, VmValue};

pub(super) fn vm_err(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::String(Rc::from(message.into())))
}

pub(super) fn maybe_emit_delta(delta_tx: Option<DeltaSender>, text: &str) {
    if let Some(tx) = delta_tx {
        if !text.is_empty() {
            let _ = tx.send(text.to_string());
        }
    }
}

pub(super) fn request_text_content(message: &serde_json::Value) -> String {
    let content = &message["content"];
    if let Some(text) = content.as_str() {
        return text.to_string();
    }
    let Some(parts) = content.as_array() else {
        return String::new();
    };
    let mut text = String::new();
    for part in parts {
        if let Some(value) = part.get("text").and_then(|value| value.as_str()) {
            text.push_str(value);
        } else if part.get("type").and_then(|value| value.as_str()) == Some("text") {
            if let Some(value) = part.get("text").and_then(|value| value.as_str()) {
                text.push_str(value);
            }
        }
    }
    text
}

pub(super) fn percent_encode_path_segment(input: &str) -> String {
    let mut out = String::with_capacity(input.len());
    for byte in input.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(byte as char)
            }
            _ => out.push_str(&format!("%{byte:02X}")),
        }
    }
    out
}

pub(super) fn empty_result(provider: &str, model: &str) -> LlmResult {
    LlmResult {
        text: String::new(),
        tool_calls: Vec::new(),
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        model: model.to_string(),
        provider: provider.to_string(),
        thinking: None,
        thinking_summary: None,
        stop_reason: None,
        blocks: Vec::new(),
    }
}

pub(super) fn apply_provider_overrides(
    body: &mut serde_json::Value,
    overrides: Option<&serde_json::Value>,
) {
    let Some(obj) = overrides.and_then(|value| value.as_object()) else {
        return;
    };
    for (key, value) in obj {
        body[key] = value.clone();
    }
}
