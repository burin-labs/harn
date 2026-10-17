//! Shared payload fixture for the Gemini provider tests.
//!
//! Both endpoint families are exercised against the same neutral
//! [`LlmRequestPayload`], which is the point: a difference between a
//! `generateContent` body and an Interactions body has to come from the
//! builders, never from two subtly different fixtures.

use crate::request::{LlmRequestPayload, ThinkingConfig};

/// A minimal single-user-turn Gemini payload.
pub fn gemini_payload(model: &str, thinking: ThinkingConfig) -> LlmRequestPayload {
    LlmRequestPayload {
        provider: "gemini".to_string(),
        model: model.to_string(),
        messages: vec![serde_json::json!({
            "role": "user",
            "content": "hello",
        })],
        max_tokens: 64,
        thinking,
        ..Default::default()
    }
}
