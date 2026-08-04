//! Tool definitions and `tool_choice` on an OpenAI-compatible request.
//!
//! Harn-private annotations (`x-harn-output-schema`, `namespace`,
//! `defer_loading`) are stripped unless the route advertises OpenAI's
//! tool-search wire extensions, parameter schemas are run through the
//! provider's schema-compatibility profile, and `tool_choice` is narrowed to
//! the modes the capability row says the route accepts.

use crate::llm::api::LlmRequestPayload;
use crate::llm::providers::schema_compat::{
    sanitize_schema_for_provider, SchemaCompatProfile, SchemaSurface,
};

pub(super) fn normalize_tool_choice_for_capabilities(
    tool_choice: &serde_json::Value,
    caps: &crate::llm::capabilities::Capabilities,
    has_native_tools: bool,
) -> Option<serde_json::Value> {
    // OpenAI-compatible providers interpret `tool_choice` as a selector over the
    // native `tools` array. Text-tool routes deliberately omit that array, and
    // stricter providers reject any `tool_choice` without tools.
    if !has_native_tools {
        return None;
    }
    let tool_choice = normalize_openai_compat_tool_choice(tool_choice)?;
    if caps.allowed_tool_choice_modes.is_empty() {
        return Some(tool_choice);
    }

    let mode = tool_choice_mode(&tool_choice);
    if mode.as_deref().is_some_and(|mode| {
        caps.allowed_tool_choice_modes
            .iter()
            .any(|allowed| allowed == mode)
    }) {
        return Some(tool_choice);
    }

    if caps
        .allowed_tool_choice_modes
        .iter()
        .any(|mode| mode == "auto")
    {
        return Some(serde_json::Value::String("auto".to_string()));
    }
    if caps
        .allowed_tool_choice_modes
        .iter()
        .any(|mode| mode == "none")
    {
        return Some(serde_json::Value::String("none".to_string()));
    }
    None
}

fn normalize_openai_compat_tool_choice(
    tool_choice: &serde_json::Value,
) -> Option<serde_json::Value> {
    match tool_choice {
        serde_json::Value::String(raw) => {
            let trimmed = raw.trim();
            if trimmed.is_empty() {
                return None;
            }
            let mode = trimmed.to_ascii_lowercase();
            if matches!(mode.as_str(), "auto" | "none" | "any" | "required") {
                return Some(serde_json::Value::String(mode));
            }
            Some(serde_json::json!({
                "type": "function",
                "function": {"name": trimmed},
            }))
        }
        serde_json::Value::Object(_) => Some(tool_choice.clone()),
        serde_json::Value::Null => None,
        _ => Some(tool_choice.clone()),
    }
}

fn tool_choice_mode(tool_choice: &serde_json::Value) -> Option<String> {
    match tool_choice {
        serde_json::Value::String(mode) => Some(mode.to_ascii_lowercase()),
        serde_json::Value::Object(object) => match object.get("type").and_then(|v| v.as_str()) {
            Some("function") | Some("tool") => Some("required".to_string()),
            Some(other) => Some(other.to_ascii_lowercase()),
            None => None,
        },
        _ => None,
    }
}

pub(super) fn provider_request_tools(
    opts: &LlmRequestPayload,
    tools: &[serde_json::Value],
) -> Vec<serde_json::Value> {
    let supports_openai_tool_search_extensions =
        crate::llm::provider::openai_tool_search_wire_extensions_enabled(
            &opts.provider,
            &opts.model,
            tools,
            opts.provider_overrides.as_ref(),
        );

    tools
        .iter()
        .map(|tool| {
            sanitize_openai_tool_for_request(
                &opts.provider,
                &opts.model,
                tool,
                supports_openai_tool_search_extensions,
            )
        })
        .collect()
}

fn sanitize_openai_tool_for_request(
    provider: &str,
    model: &str,
    tool: &serde_json::Value,
    supports_openai_tool_search_extensions: bool,
) -> serde_json::Value {
    let mut tool = tool.clone();
    let Some(object) = tool.as_object_mut() else {
        return tool;
    };

    object.remove("x-harn-output-schema");
    if !supports_openai_tool_search_extensions {
        object.remove("defer_loading");
        object.remove("namespace");
        object.remove("namespaces");
    }

    if let Some(function) = object
        .get_mut("function")
        .and_then(serde_json::Value::as_object_mut)
    {
        function.remove("x-harn-output-schema");
        function.remove("namespace");
        let schema_profile = if function
            .get("strict")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
        {
            SchemaCompatProfile::OpenAiStrict
        } else {
            SchemaCompatProfile::OpenAiLenient
        };
        if let Some(parameters) = function.get("parameters").cloned() {
            function.insert(
                "parameters".to_string(),
                sanitize_schema_for_provider(
                    provider,
                    model,
                    schema_profile,
                    SchemaSurface::ToolParameters,
                    &parameters,
                ),
            );
        }
    }

    tool
}
