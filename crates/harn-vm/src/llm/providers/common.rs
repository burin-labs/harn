use std::rc::Rc;

use crate::llm::api::{DeltaSender, LlmResult, ProviderTelemetry};
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
        logprobs: Vec::new(),
        telemetry: ProviderTelemetry::default(),
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

pub(super) fn google_function_declaration_tools(
    tools: Option<&[serde_json::Value]>,
) -> Option<serde_json::Value> {
    let declarations: Vec<serde_json::Value> = tools
        .unwrap_or_default()
        .iter()
        .filter_map(google_function_declaration)
        .collect();
    (!declarations.is_empty())
        .then(|| serde_json::json!([{ "functionDeclarations": declarations }]))
}

fn google_function_declaration(tool: &serde_json::Value) -> Option<serde_json::Value> {
    let function = tool.get("function").unwrap_or(tool);
    let name = function
        .get("name")
        .and_then(serde_json::Value::as_str)
        .filter(|name| !name.is_empty())?;
    let mut declaration = serde_json::json!({ "name": name });
    if let Some(description) = function.get("description") {
        declaration["description"] = description.clone();
    }
    if let Some(parameters) = function
        .get("parameters")
        .or_else(|| function.get("input_schema"))
    {
        declaration["parameters"] = parameters.clone();
    }
    Some(declaration)
}

/// Parse a `(major, minor)` generation pair from the tail of a model ID after
/// the family needle. Accepts both dotted forms (`4.7`, `5.4-preview`) and
/// dashed forms (`4-7`, `5-4-preview`, `4-20251001`).
///
/// Trailing chunks that look like date stamps (≥ 1000) are not treated as a
/// minor version — `claude-haiku-4-5-20251001` parses as `(4, 5)`,
/// `claude-opus-4-20251001` parses as `(4, 0)`. If no integer major can be
/// parsed at the start of `tail`, returns `None`.
pub(super) fn parse_major_minor_tail(tail: &str) -> Option<(u32, u32)> {
    if let Some((major, rest)) = tail.split_once('.') {
        if let Ok(major) = major.parse::<u32>() {
            let minor_str: String = rest.chars().take_while(|c| c.is_ascii_digit()).collect();
            if let Ok(minor) = minor_str.parse::<u32>() {
                return Some((major, minor));
            }
        }
    }
    let mut parts = tail.split('-');
    let major = parts.next()?.parse::<u32>().ok()?;
    let Some(minor_chunk) = parts.next() else {
        return Some((major, 0));
    };
    let Ok(minor) = minor_chunk.parse::<u32>() else {
        return Some((major, 0));
    };
    Some((major, if minor >= 1000 { 0 } else { minor }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_major_minor_tail_dotted() {
        assert_eq!(parse_major_minor_tail("4.7"), Some((4, 7)));
        assert_eq!(parse_major_minor_tail("5.4-preview"), Some((5, 4)));
        assert_eq!(parse_major_minor_tail("5.4.1-turbo"), Some((5, 4)));
    }

    #[test]
    fn parse_major_minor_tail_dashed() {
        assert_eq!(parse_major_minor_tail("4-7"), Some((4, 7)));
        assert_eq!(parse_major_minor_tail("5-4-preview"), Some((5, 4)));
        assert_eq!(parse_major_minor_tail("5"), Some((5, 0)));
    }

    #[test]
    fn parse_major_minor_tail_date_chunk_is_zero_minor() {
        assert_eq!(parse_major_minor_tail("4-20251001"), Some((4, 0)));
        assert_eq!(parse_major_minor_tail("5-20260115"), Some((5, 0)));
    }

    #[test]
    fn parse_major_minor_tail_rejects_non_numeric_major() {
        assert!(parse_major_minor_tail("preview").is_none());
        assert!(parse_major_minor_tail("").is_none());
    }
}
