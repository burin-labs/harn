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

fn assert_mixed_tool_boundary_moves_system_after_client_result(blocks: Vec<serde_json::Value>) {
    let expected_blocks = blocks.clone();
    let opts = LlmCallOptions {
        provider: "anthropic".to_string(),
        model: "claude-opus-4-8".to_string(),
        messages: vec![
            serde_json::json!({"role": "user", "content": "search and read"}),
            serde_json::json!({"role": "assistant", "content": blocks}),
            serde_json::json!({"role": "system", "content": "budget is now $0.25"}),
            serde_json::json!({"role": "user", "content": [{
                "type": "tool_result", "tool_use_id": "toolu_01", "content": "contents"
            }]}),
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
            {"role": "user", "content": "search and read"},
            {"role": "assistant", "content": expected_blocks},
            {"role": "user", "content": [{
                "type": "tool_result", "tool_use_id": "toolu_01", "content": "contents"
            }]},
            {"role": "system", "content": "budget is now $0.25"},
            {"role": "assistant", "content": "continuing"},
        ])
    );
}

#[test]
fn mixed_client_then_server_tool_use_defers_native_system_until_after_result() {
    assert_mixed_tool_boundary_moves_system_after_client_result(vec![
        serde_json::json!({
            "type": "tool_use", "id": "toolu_01", "name": "read_file", "input": {"path": "README.md"}
        }),
        serde_json::json!({
            "type": "server_tool_use", "id": "srvtoolu_01", "name": "web_search", "input": {"query": "Harn"}
        }),
    ]);
}

#[test]
fn mixed_server_then_client_tool_use_defers_native_system_until_after_result() {
    assert_mixed_tool_boundary_moves_system_after_client_result(vec![
        serde_json::json!({
            "type": "server_tool_use", "id": "srvtoolu_01", "name": "web_search", "input": {"query": "Harn"}
        }),
        serde_json::json!({
            "type": "tool_use", "id": "toolu_01", "name": "read_file", "input": {"path": "README.md"}
        }),
    ]);
}

#[test]
fn mixed_tool_fold_keeps_result_only_and_moves_reminder_after_continuation() {
    let blocks = vec![
        serde_json::json!({
            "type": "tool_use", "id": "toolu_01", "name": "read_file", "input": {"path": "README.md"}
        }),
        serde_json::json!({
            "type": "server_tool_use", "id": "srvtoolu_01", "name": "web_search", "input": {"query": "Harn"}
        }),
    ];
    let expected_blocks = blocks.clone();
    let opts = LlmCallOptions {
        provider: "anthropic".to_string(),
        model: "claude-fable-5".to_string(),
        messages: vec![
            serde_json::json!({"role": "user", "content": "search and read"}),
            serde_json::json!({"role": "assistant", "content": blocks}),
            serde_json::json!({"role": "system", "content": "budget is now $0.25"}),
            serde_json::json!({"role": "user", "content": [{
                "type": "tool_result", "tool_use_id": "toolu_01", "content": "contents"
            }]}),
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
            {"role": "user", "content": "search and read"},
            {"role": "assistant", "content": expected_blocks},
            {"role": "user", "content": [{
                "type": "tool_result", "tool_use_id": "toolu_01", "content": "contents"
            }]},
            {"role": "assistant", "content": "continuing"},
            {"role": "user", "content": "<system-reminder>\nbudget is now $0.25\n</system-reminder>"},
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
