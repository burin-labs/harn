use serde_json::{json, Value};

use super::{probe_tool_registry, ToolProbeCase, ToolProbeMode, TOOL_PROBE_TOOL_NAME};
use crate::llm::api::{LlmApiMode, LlmRequestPayload, OutputFormat};
use crate::llm::capabilities::WireDialect;
use crate::llm_config;

pub(super) fn probe_request_body(
    provider: &str,
    model: &str,
    mode: ToolProbeMode,
    probe_case: ToolProbeCase,
    marker: &str,
) -> Result<Value, String> {
    let payload = probe_request_payload(provider, model, mode, probe_case, marker)?;
    Ok(provider_compatible_probe_request_body(&payload))
}

pub(super) fn probe_request_payload(
    provider: &str,
    model: &str,
    mode: ToolProbeMode,
    probe_case: ToolProbeCase,
    marker: &str,
) -> Result<LlmRequestPayload, String> {
    let model_defaults = llm_config::model_params_for_route(provider, model);
    let default_float =
        |key: &str| -> Option<f64> { model_defaults.get(key).and_then(toml::Value::as_float) };
    let default_int =
        |key: &str| -> Option<i64> { model_defaults.get(key).and_then(toml::Value::as_integer) };
    let prompt = probe_prompt(probe_case, marker);
    let native_tools =
        crate::llm::tools::vm_tools_to_native(&probe_tool_registry(), provider, model)
            .expect("tool probe registry is static and should convert to native tools");
    let tool_choice = if crate::llm::provider::provider_uses_ollama_messages(provider, model) {
        None
    } else {
        Some(json!({
            "type": "function",
            "function": {"name": TOOL_PROBE_TOOL_NAME}
        }))
    };
    let caps = crate::llm::capabilities::lookup(provider, model);
    let thinking = crate::llm::helpers::resolve_catalog_thinking_config(
        &model_defaults,
        provider,
        model,
        &caps,
        true,
    )
    .map_err(|error| error.to_string())?;
    Ok(LlmRequestPayload {
        provider: provider.to_string(),
        model: model.to_string(),
        region: None,
        api_key: String::new(),
        api_mode: LlmApiMode::ChatCompletions,
        messages: vec![json!({"role": "user", "content": prompt})],
        system: None,
        max_tokens: default_int("max_tokens").unwrap_or(256),
        temperature: Some(default_float("temperature").unwrap_or(0.0)),
        top_p: default_float("top_p"),
        top_k: default_int("top_k"),
        logprobs: false,
        top_logprobs: None,
        stop: None,
        seed: None,
        frequency_penalty: None,
        presence_penalty: None,
        fast: false,
        output_format: OutputFormat::Text,
        response_format: None,
        json_schema: None,
        output_schema: None,
        schema_stream_abort: false,
        thinking,
        anthropic_beta_features: Vec::new(),
        vision: false,
        native_tools: Some(native_tools),
        provider_tools: Vec::new(),
        tool_choice,
        cache: false,
        prompt_cache_ttl: None,
        timeout: None,
        stream: mode == ToolProbeMode::Streaming,
        provider_overrides: None,
        previous_response_id: None,
        store: None,
        background: None,
        truncation: None,
        compact: None,
        include: None,
        max_tool_calls: None,
        prefill: None,
        session_id: None,
        reminder_lifecycle: Vec::new(),
        cli_llm_mock_scope: None,
    })
}

fn probe_prompt(probe_case: ToolProbeCase, marker: &str) -> String {
    match probe_case {
        ToolProbeCase::SingleToolCall => format!(
            "Call the {TOOL_PROBE_TOOL_NAME} tool exactly once with value {marker:?}. Do not answer in prose."
        ),
        ToolProbeCase::LargeStringArgument => format!(
            "Call the {TOOL_PROBE_TOOL_NAME} tool exactly once. The value argument must exactly equal this string, preserving newlines and escapes: {marker:?}. Do not answer in prose."
        ),
    }
}

fn provider_compatible_probe_request_body(payload: &LlmRequestPayload) -> Value {
    match payload.provider.as_str() {
        "azure_openai" => {
            return crate::llm::providers::AzureOpenAiProvider::build_request_body(payload);
        }
        "bedrock" => return crate::llm::providers::BedrockProvider::build_request_body(payload),
        "vertex" => return crate::llm::providers::VertexProvider::build_request_body(payload),
        _ => {}
    }

    match crate::llm::capabilities::lookup(&payload.provider, &payload.model).message_wire_format {
        WireDialect::Anthropic => {
            crate::llm::providers::AnthropicProvider::build_request_body(payload)
        }
        WireDialect::Gemini => crate::llm::providers::GeminiProvider::build_request_body(payload),
        WireDialect::Ollama => crate::llm::providers::OllamaProvider::build_request_body(payload),
        WireDialect::OpenAiCompat => {
            crate::llm::providers::OpenAiCompatibleProvider::build_request_body(payload, false)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::api::{ReasoningEffort, ThinkingConfig};

    #[test]
    fn gpt_oss_payload_and_body_inherit_logical_generation_defaults() {
        let _guard = crate::llm::env_guard();
        llm_config::clear_user_overrides();
        crate::agent_sessions::reset_session_store();
        let session_id = crate::agent_sessions::open_or_create(Some(
            "tool-probe-catalog-reasoning-default".to_string(),
        ));
        crate::agent_sessions::set_pinned_reasoning_policy(&session_id, Some("off".to_string()))
            .expect("pin ambient reasoning policy");
        let session_guard = crate::agent_sessions::enter_current_session(session_id);
        let payload = probe_request_payload(
            "fireworks",
            "accounts/fireworks/models/gpt-oss-120b",
            ToolProbeMode::NonStreaming,
            super::super::ToolProbeCase::SingleToolCall,
            super::super::DEFAULT_TOOL_PROBE_MARKER,
        )
        .expect("GPT-OSS probe payload");
        drop(session_guard);
        crate::agent_sessions::reset_session_store();

        assert_eq!(payload.temperature, Some(1.0));
        assert_eq!(payload.top_p, Some(1.0));
        assert_eq!(
            payload.thinking,
            ThinkingConfig::Effort {
                level: ReasoningEffort::High
            }
        );
        let body = provider_compatible_probe_request_body(&payload);
        assert_eq!(body["temperature"], 1.0);
        assert_eq!(body["top_p"], 1.0);
        assert_eq!(body["reasoning_effort"], "high");
    }
}
