//! Shared payload fixture for the Gemini provider tests.
//!
//! Both endpoint families are exercised against the same neutral
//! [`LlmRequestPayload`], which is the point: a difference between a
//! `generateContent` body and an Interactions body has to come from the
//! builders, never from two subtly different fixtures.

use crate::llm::api::{LlmRequestPayload, ThinkingConfig};

/// A minimal single-user-turn Gemini payload.
pub(super) fn gemini_payload(model: &str, thinking: ThinkingConfig) -> LlmRequestPayload {
    LlmRequestPayload {
        provider: "gemini".to_string(),
        model: model.to_string(),
        region: None,
        api_key: String::new(),
        api_mode: crate::llm::api::LlmApiMode::ChatCompletions,
        messages: vec![serde_json::json!({
            "role": "user",
            "content": "hello",
        })],
        system: None,
        max_tokens: 64,
        temperature: None,
        top_p: None,
        top_k: None,
        logprobs: false,
        top_logprobs: None,
        stop: None,
        seed: None,
        frequency_penalty: None,
        presence_penalty: None,
        fast: false,
        output_format: crate::llm::api::OutputFormat::Text,
        output_schema: None,
        schema_stream_abort: false,
        thinking,
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
        session_id: None,
        reminder_lifecycle: Vec::new(),
        cli_llm_mock_scope: None,
        mock_scope: None,
    }
}
