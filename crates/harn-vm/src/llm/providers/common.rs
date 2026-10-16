use crate::llm::api::{DeltaSender, LlmResult};
use crate::llm::providers::schema_compat::{
    sanitize_schema_for_provider, SchemaCompatProfile, SchemaSurface,
};
use crate::value::{VmError, VmValue};

pub(super) fn vm_err(message: impl Into<String>) -> VmError {
    VmError::Thrown(VmValue::string(message.into()))
}

pub(super) fn maybe_emit_delta(delta_tx: Option<DeltaSender>, text: &str) {
    if let Some(tx) = delta_tx {
        if !text.is_empty() {
            let _ = tx.send(text.to_string());
        }
    }
}

/// Canonical assistant-visible text block. `visibility` must stay `"public"`
/// so downstream redaction keeps the text in the user-facing transcript.
pub(super) fn output_text_block(text: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "output_text",
        "text": text,
        "visibility": "public",
    })
}

/// Canonical reasoning/thinking block. `visibility` must stay `"private"` so
/// the chain-of-thought is never surfaced as user-facing output.
pub(super) fn reasoning_block(text: &str) -> serde_json::Value {
    serde_json::json!({
        "type": "reasoning",
        "text": text,
        "visibility": "private",
    })
}

/// Canonical tool-call block. `visibility` must stay `"internal"` so the call
/// is routed to tool execution rather than rendered as assistant output.
/// Provider-specific metadata (e.g. `thought_signature`, `part_index`) is
/// layered onto the returned value by the caller.
pub(super) fn tool_call_block(
    id: impl Into<serde_json::Value>,
    name: impl Into<serde_json::Value>,
    arguments: impl Into<serde_json::Value>,
) -> serde_json::Value {
    serde_json::json!({
        "type": "tool_call",
        "id": id.into(),
        "name": name.into(),
        "arguments": arguments.into(),
        "visibility": "internal",
    })
}

pub(super) use harn_llm::wire::request_text_content;

pub(super) fn empty_result(provider: &str, model: &str) -> LlmResult {
    LlmResult {
        attempts: Default::default(),
        text_projection: None,
        served_fast: false,
        text: String::new(),
        tool_calls: Vec::new(),
        raw_tool_calls: Vec::new(),
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cache_supported: true,
        model: model.to_string(),
        provider: provider.to_string(),
        thinking: None,
        thinking_summary: None,
        stop_reason: None,
        blocks: Vec::new(),
        logprobs: Vec::new(),
        telemetry: Box::default(),
    }
}

pub(super) fn apply_provider_overrides(
    body: &mut serde_json::Value,
    overrides: Option<&serde_json::Value>,
) {
    crate::llm::provider::apply_provider_wire_overrides(body, overrides);
}

pub(super) fn google_function_declaration_tools(
    provider: &str,
    model: &str,
    tools: Option<&[serde_json::Value]>,
) -> Option<serde_json::Value> {
    let declarations: Vec<serde_json::Value> = tools
        .unwrap_or_default()
        .iter()
        .filter_map(|tool| google_function_declaration(provider, model, tool))
        .collect();
    (!declarations.is_empty())
        .then(|| serde_json::json!([{ "functionDeclarations": declarations }]))
}

fn google_function_declaration(
    provider: &str,
    model: &str,
    tool: &serde_json::Value,
) -> Option<serde_json::Value> {
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
        declaration["parameters"] = sanitize_schema_for_provider(
            provider,
            model,
            SchemaCompatProfile::Google,
            SchemaSurface::ToolParameters,
            parameters,
        );
    }
    Some(declaration)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn provider_overrides_do_not_emit_harn_control_fields() {
        let mut body = serde_json::json!({"model": "example"});
        let overrides = serde_json::json!({
            "force_native_tool_search": true,
            "custom_wire_field": "kept",
        });

        apply_provider_overrides(&mut body, Some(&overrides));

        assert!(body.get("force_native_tool_search").is_none());
        assert_eq!(body["custom_wire_field"], "kept");
    }
}
