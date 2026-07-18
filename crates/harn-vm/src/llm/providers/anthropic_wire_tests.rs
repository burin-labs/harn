use super::AnthropicProvider;
use crate::llm::api::{LlmCallOptions, LlmRequestPayload};

#[test]
fn mid_conversation_system_section_reaches_exact_anthropic_wire_json() {
    let opts = LlmCallOptions {
        provider: "anthropic".to_string(),
        model: "claude-opus-4-8".to_string(),
        messages: vec![
            serde_json::json!({"role": "user", "content": "U1 <>&"}),
            serde_json::json!({
                "role": "system",
                "content": [{
                    "type": "text",
                    "text": "first <>&",
                    "cache_control": {"type": "ephemeral"}
                }]
            }),
            serde_json::json!({
                "role": "developer",
                "content": [{"type": "text", "text": "second"}]
            }),
            serde_json::json!({"role": "assistant", "content": "A1"}),
        ],
        max_tokens: 64,
        ..LlmCallOptions::default()
    };

    let payload = LlmRequestPayload::from(&opts);
    let body = AnthropicProvider::build_request_body(&payload);

    assert_eq!(
        body["messages"],
        serde_json::json!([
            {"role": "user", "content": "U1 <>&"},
            {
                "role": "system",
                "content": [{
                    "type": "text",
                    "text": "first <>&",
                    "cache_control": {"type": "ephemeral"}
                }]
            },
            {
                "role": "system",
                "content": [{"type": "text", "text": "second"}]
            },
            {"role": "assistant", "content": "A1"},
        ])
    );
}
