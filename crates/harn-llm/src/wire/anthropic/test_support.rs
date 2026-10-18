//! Shared request-payload fixture for the Anthropic provider tests.
//! One owner so `anthropic.rs` and its sibling test modules do not each
//! carry a drifting copy.

use crate::request::LlmRequestPayload;

pub fn base_payload() -> LlmRequestPayload {
    LlmRequestPayload {
        provider: "anthropic".to_string(),
        model: "claude-sonnet-4-6".to_string(),
        messages: vec![serde_json::json!({"role": "user", "content": "hello"})],
        system: Some("system prompt".to_string()),
        max_tokens: 64,
        native_tools: Some(vec![serde_json::json!({
            "name": "read_file",
            "description": "Read a file",
            "input_schema": {"type": "object"},
        })]),
        stream: true,
        ..Default::default()
    }
}
