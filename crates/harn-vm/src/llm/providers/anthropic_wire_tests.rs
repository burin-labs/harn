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
                "content": [
                    {
                        "type": "text",
                        "text": "first <>&",
                        "cache_control": {"type": "ephemeral"}
                    },
                    {"type": "text", "text": "second"}
                ]
            },
            {"role": "assistant", "content": "A1"},
        ])
    );
}

#[test]
fn exact_server_tool_use_boundary_keeps_native_system_message() {
    let server_tool_use = serde_json::json!({
        "type": "server_tool_use",
        "id": "srvtoolu_01",
        "name": "web_search",
        "input": {"query": "Harn"}
    });
    let opts = LlmCallOptions {
        provider: "anthropic".to_string(),
        model: "claude-opus-4-8".to_string(),
        messages: vec![
            serde_json::json!({"role": "user", "content": "search"}),
            serde_json::json!({"role": "assistant", "content": [server_tool_use]}),
            serde_json::json!({"role": "system", "content": "budget is now $0.25"}),
            serde_json::json!({"role": "assistant", "content": "continuing"}),
        ],
        max_tokens: 64,
        ..LlmCallOptions::default()
    };

    let payload = LlmRequestPayload::from(&opts);
    let body = AnthropicProvider::build_request_body(&payload);

    assert_eq!(
        body["messages"],
        serde_json::json!([
            {"role": "user", "content": "search"},
            {"role": "assistant", "content": [{
                "type": "server_tool_use",
                "id": "srvtoolu_01",
                "name": "web_search",
                "input": {"query": "Harn"}
            }]},
            {"role": "system", "content": "budget is now $0.25"},
            {"role": "assistant", "content": "continuing"},
        ])
    );
}

#[test]
fn unsupported_fable_route_folds_system_message_on_exact_wire() {
    let opts = LlmCallOptions {
        provider: "anthropic".to_string(),
        model: "claude-fable-5".to_string(),
        messages: vec![
            serde_json::json!({"role": "user", "content": "U1"}),
            serde_json::json!({"role": "system", "content": "operator constraint"}),
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
            {
                "role": "user",
                "content": "U1\n\n<system-reminder>\noperator constraint\n</system-reminder>"
            },
            {"role": "assistant", "content": "A1"},
        ])
    );
}
