use super::parse_llm_response;
use crate::llm::capabilities::WireDialect;

#[test]
fn anthropic_parser_records_tool_search_tool_result_as_event() {
    let response = serde_json::json!({
        "content": [
            {
                "type": "tool_search_tool_result",
                "tool_use_id": "srvtoolu_01",
                "content": {
                    "type": "tool_search_tool_search_result",
                    "tool_references": [
                        {"type": "tool_reference", "tool_name": "get_weather"}
                    ]
                }
            },
            {"type": "text", "text": "ok"}
        ],
        "usage": {"input_tokens": 3, "output_tokens": 1}
    });
    let result = parse_llm_response(
        &response,
        "anthropic",
        "claude-opus-4-7",
        WireDialect::Anthropic,
        false,
    )
    .expect("parser succeeds");

    let result_block = result
        .blocks
        .iter()
        .find(|b| b.get("type").and_then(|v| v.as_str()) == Some("tool_search_result"))
        .expect("tool_search_result block present");
    let refs = result_block["tool_references"]
        .as_array()
        .expect("tool_references array");
    assert_eq!(refs.len(), 1);
    assert_eq!(
        refs[0]["tool_name"].as_str(),
        Some("get_weather"),
        "reference name preserved"
    );
}

#[test]
fn anthropic_parser_preserves_signed_reasoning_blocks_for_replay() {
    let response = serde_json::json!({
        "content": [
            {
                "type": "thinking",
                "thinking": "Check the tool arguments.",
                "signature": "signed-thinking"
            },
            {"type": "redacted_thinking", "data": "opaque-reasoning"},
            {"type": "text", "text": "Done."}
        ],
        "usage": {"input_tokens": 5, "output_tokens": 4},
        "stop_reason": "end_turn"
    });

    let result = parse_llm_response(
        &response,
        "anthropic",
        "claude-opus-4-7",
        WireDialect::Anthropic,
        false,
    )
    .expect("parser succeeds");

    assert_eq!(
        result.thinking.as_deref(),
        Some("Check the tool arguments.")
    );
    assert!(result.blocks.contains(&serde_json::json!({
        "type": "thinking",
        "thinking": "Check the tool arguments.",
        "signature": "signed-thinking"
    })));
    assert!(result.blocks.contains(&serde_json::json!({
        "type": "redacted_thinking",
        "data": "opaque-reasoning"
    })));
}
