use super::AnthropicProvider;
use crate::llm::api::{LlmCallOptions, LlmRequestPayload};

/// Producer-only session facts must survive recording but never leak into the
/// provider request. This also retains the byte-perfect screenshot regression
/// across record -> transcript -> provider egress.
#[test]
fn live_computer_screenshot_reaches_anthropic_body_without_producer_data() {
    use base64::Engine;

    let bytes: Vec<u8> = (0..300_000u32)
        .map(|i| (i.wrapping_mul(2654435761) >> 16) as u8)
        .collect();
    let src_b64 = base64::engine::general_purpose::STANDARD.encode(&bytes);

    crate::llm::agent_session_host::reset_agent_session_host_state();
    let session_id = crate::agent_sessions::open_or_create(Some("cu-live-diag".to_string()));
    crate::llm::agent_session_host::seed_host_session_provider_model(
        &session_id,
        "anthropic",
        "claude-opus-4-8",
    );
    crate::agent_sessions::inject_message(
        &session_id,
        crate::stdlib::json_to_vm_value(&serde_json::json!({
            "role": "user", "content": "take a screenshot"
        })),
    )
    .expect("user message");
    crate::agent_sessions::inject_message(
        &session_id,
        crate::stdlib::json_to_vm_value(&serde_json::json!({
            "role": "assistant",
            "content": [{
                "type": "tool_use",
                "id": "tc1",
                "name": "computer",
                "input": {"action": "screenshot"}
            }],
        })),
    )
    .expect("assistant tool call");
    let dispatch = crate::stdlib::json_to_vm_value(&serde_json::json!([{
        "tool_name": "computer",
        "tool_call_id": "tc1",
        "ok": true,
        "observation": "Captured screenshot 1024x768.",
        "data": {"producer_only_status": "succeeded"},
        "result": {
            "ok": true,
            "text": "Captured screenshot 1024x768.",
            "screenshot": {
                "base64": src_b64,
                "media_type": "image/png",
                "width": 1024,
                "height": 768,
                "scale_factor": 2.0,
            },
        },
    }]));
    crate::llm::agent_session_host::record_tool_results_for_test(&session_id, dispatch);

    let transcript = crate::agent_sessions::transcript(&session_id).expect("transcript");
    let message_vms: Vec<crate::value::VmValue> =
        match transcript.as_dict().and_then(|dict| dict.get("messages")) {
            Some(crate::value::VmValue::List(list)) => list.iter().cloned().collect(),
            _ => Vec::new(),
        };
    let messages = crate::llm::helpers::vm_messages_to_json(&message_vms).expect("messages json");
    let opts = LlmCallOptions {
        provider: "anthropic".to_string(),
        model: "claude-opus-4-8".to_string(),
        messages,
        ..Default::default()
    };
    let body = AnthropicProvider::build_request_body(&LlmRequestPayload::from(&opts));
    assert!(!body.to_string().contains("producer_only_status"));

    fn find_image_data(value: &serde_json::Value) -> Option<String> {
        match value {
            serde_json::Value::Object(map) => {
                if map.get("type").and_then(|kind| kind.as_str()) == Some("image") {
                    return map
                        .get("source")
                        .and_then(|source| source.get("data"))
                        .and_then(|data| data.as_str())
                        .map(str::to_string);
                }
                map.values().find_map(find_image_data)
            }
            serde_json::Value::Array(items) => items.iter().find_map(find_image_data),
            _ => None,
        }
    }

    let out_b64 = find_image_data(&body).expect("Anthropic image block");
    let out_bytes = base64::engine::general_purpose::STANDARD
        .decode(out_b64)
        .expect("valid base64");
    assert_eq!(out_bytes, bytes);
}

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
