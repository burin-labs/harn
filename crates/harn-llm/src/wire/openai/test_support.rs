//! Shared request fixtures for the OpenAI-compatible provider tests.

use crate::request::LlmRequestPayload;
use serde_json::json;

pub fn base_request_payload() -> LlmRequestPayload {
    LlmRequestPayload {
        provider: "openrouter".to_string(),
        model: "google/gemini-2.5-pro".to_string(),
        messages: vec![json!({"role": "user", "content": "hello"})],
        max_tokens: 64,
        temperature: Some(0.0),
        ..Default::default()
    }
}

pub fn cache_control_count(value: &serde_json::Value) -> usize {
    match value {
        serde_json::Value::Object(object) => {
            usize::from(object.contains_key("cache_control"))
                + object.values().map(cache_control_count).sum::<usize>()
        }
        serde_json::Value::Array(values) => values.iter().map(cache_control_count).sum(),
        _ => 0,
    }
}

pub fn part_is_image(part: &serde_json::Value) -> bool {
    matches!(
        part.get("type").and_then(|value| value.as_str()),
        Some("image_url") | Some("image")
    )
}
