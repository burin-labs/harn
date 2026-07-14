use super::parse_llm_response;

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
        false,
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
        false,
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
