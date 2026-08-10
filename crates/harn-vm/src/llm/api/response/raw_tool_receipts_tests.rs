use super::parse_llm_response;
use crate::llm::capabilities::WireDialect;

#[test]
fn openai_parser_preserves_harmony_wrapper_before_normalizing() {
    let response = serde_json::json!({
        "choices": [{
            "message": {
                "content": "",
                "tool_calls": [{
                    "id": "chatcmpl-tool-1",
                    "type": "function",
                    "function": {
                        "name": "tool",
                        "arguments": "{\"name\":\"look\",\"args\":{\"intent\":\"read\",\"file\":\"src/lib.rs\"}}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 20}
    });

    let result = parse_llm_response(
        &response,
        "fireworks",
        "accounts/fireworks/models/gpt-oss-120b",
        WireDialect::OpenAiCompat,
        false,
    )
    .expect("parser succeeds");

    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0]["name"], "look");
    assert_eq!(result.tool_calls[0]["arguments"]["intent"], "read");
    assert_eq!(result.tool_calls[0]["arguments"]["file"], "src/lib.rs");
    assert_eq!(result.raw_tool_calls.len(), 1);
    assert_eq!(result.raw_tool_calls[0]["function"]["name"], "tool");
    assert_eq!(
        result.raw_tool_calls[0]["function"]["arguments"],
        "{\"name\":\"look\",\"args\":{\"intent\":\"read\",\"file\":\"src/lib.rs\"}}"
    );
}

#[test]
fn openai_parser_recovers_look_from_harmony_recipient_to_wrapper() {
    // Fireworks Harmony demux can lift the recipient token `to` into
    // function.name while leaving clean look args and no real tool name
    // (burin-code#4809 Face 2).
    let response = serde_json::json!({
        "choices": [{
            "message": {
                "content": "",
                "tool_calls": [{
                    "id": "call_to_wrapper",
                    "type": "function",
                    "function": {
                        "name": "to",
                        "arguments": "{\"file\": \"Sources/App.swift\", \"intent\": \"read\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 12}
    });

    let result = parse_llm_response(
        &response,
        "fireworks",
        "gpt-oss-120b",
        WireDialect::OpenAiCompat,
        false,
    )
    .expect("parser succeeds");

    assert_eq!(result.stop_reason.as_deref(), Some("tool_calls"));
    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0]["name"], "look");
    assert_eq!(
        result.tool_calls[0]["arguments"]["file"],
        "Sources/App.swift"
    );
    assert_eq!(result.tool_calls[0]["arguments"]["intent"], "read");
    assert_eq!(result.raw_tool_calls.len(), 1);
    assert_eq!(result.raw_tool_calls[0]["function"]["name"], "to");
}

#[test]
fn openai_parser_preserves_channel_suffix_before_stripping() {
    let response = serde_json::json!({
        "choices": [{
            "message": {
                "content": "",
                "tool_calls": [{
                    "id": "chatcmpl-tool-1",
                    "type": "function",
                    "function": {
                        "name": "run<|channel|>commentary",
                        "arguments": "{\"command\":\"cargo test\"}"
                    }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 20}
    });

    let result = parse_llm_response(
        &response,
        "fireworks",
        "accounts/fireworks/models/gpt-oss-120b",
        WireDialect::OpenAiCompat,
        false,
    )
    .expect("parser succeeds");

    assert_eq!(result.tool_calls.len(), 1);
    assert_eq!(result.tool_calls[0]["name"], "run");
    assert_eq!(result.tool_calls[0]["arguments"]["command"], "cargo test");
    assert_eq!(result.raw_tool_calls.len(), 1);
    assert_eq!(
        result.raw_tool_calls[0]["function"]["name"],
        "run<|channel|>commentary"
    );
}
