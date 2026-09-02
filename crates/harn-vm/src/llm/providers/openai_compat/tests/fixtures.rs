//! Shared request fixtures for the OpenAI-compatible provider tests.

use crate::llm::api::{LlmRequestPayload, ThinkingConfig};
use serde_json::json;

pub(super) fn base_request_payload() -> LlmRequestPayload {
    LlmRequestPayload {
        data_controls: crate::llm_config::DataPosture::Default,
        provider: "openrouter".to_string(),
        model: "google/gemini-2.5-pro".to_string(),
        region: None,
        api_key: String::new(),
        api_mode: crate::llm::api::LlmApiMode::ChatCompletions,
        session_id: None,
        messages: vec![json!({"role": "user", "content": "hello"})],
        system: None,
        max_tokens: 64,
        temperature: Some(0.0),
        top_p: None,
        top_k: None,
        logprobs: None,
        logit_bias: Vec::new(),
        min_p: None,
        repetition_penalty: None,
        prediction: None,
        verbosity: None,
        mirostat: None,
        stop: None,
        seed: None,
        frequency_penalty: None,
        presence_penalty: None,
        parallel_tool_calls: None,
        provider_contract_probe: None,
        fast: false,
        reasoning_mode: None,
        output_format: crate::llm::api::OutputFormat::Text,
        output_schema: None,
        schema_stream_abort: false,
        thinking: ThinkingConfig::Disabled,
        anthropic_beta_features: Vec::new(),
        vision: false,
        native_tools: None,
        provider_tools: Vec::new(),
        tool_choice: None,
        cache: false,
        prompt_cache_ttl: None,
        timeout: None,
        idle_timeout: None,
        stream: false,
        provider_overrides: None,
        previous_response_id: None,
        store: None,
        background: None,
        truncation: None,
        compact: None,
        include: None,
        max_tool_calls: None,
        prefill: None,
        reminder_lifecycle: Vec::new(),
        cli_llm_mock_scope: None,
        mock_scope: None,
        done_sentinel: None,
        done_sentinel_form: None,
    }
}

pub(super) fn cache_control_count(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(object) => {
            usize::from(object.contains_key("cache_control"))
                + object.values().map(cache_control_count).sum::<usize>()
        }
        serde_json::Value::Array(values) => values.iter().map(cache_control_count).sum(),
        _ => 0,
    }
}

pub(super) fn part_is_image(part: &serde_json::Value) -> bool {
    matches!(
        part.get("type").and_then(|value| value.as_str()),
        Some("image_url") | Some("image")
    )
}
