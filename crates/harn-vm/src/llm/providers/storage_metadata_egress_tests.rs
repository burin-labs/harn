use serde_json::{json, Value};

use super::anthropic_test_support::base_payload;
use super::{
    AnthropicProvider, BedrockProvider, GeminiInteractions, GeminiProvider, OllamaProvider,
    OpenAiCompatibleProvider, OpenAiResponsesProvider,
};

fn assert_no_storage_metadata(value: &Value, path: &str) {
    match value {
        Value::Object(object) => {
            assert!(
                !object.contains_key("_harn"),
                "storage-only _harn metadata leaked at {path}: {value}"
            );
            assert!(
                !object.contains_key("provider_continuation"),
                "provider continuation leaked at {path}: {value}"
            );
            for (key, child) in object {
                assert_no_storage_metadata(child, &format!("{path}.{key}"));
            }
        }
        Value::Array(items) => {
            for (index, child) in items.iter().enumerate() {
                assert_no_storage_metadata(child, &format!("{path}[{index}]"));
            }
        }
        _ => {}
    }
}

#[test]
fn storage_only_message_facts_never_cross_provider_egress() {
    let mut request = base_payload();
    request.messages = vec![
        json!({
            "role": "assistant",
            "content": "checking",
            "provider_continuation": {
                "anthropic": {
                    "content_blocks": [{
                        "type": "thinking",
                        "thinking": "private",
                        "signature": "signed"
                    }]
                }
            },
            "_harn": {
                "kind": "assistant",
                "tool_calls": [{"id": "call_1", "name": "verify", "arguments": {}}]
            }
        }),
        json!({
            "role": "tool_result",
            "tool_use_id": "call_1",
            "tool_call_id": "call_1",
            "name": "verify",
            "content": "passed",
            "is_error": false,
            "_harn": {
                "kind": "tool_result",
                "tool_call_id": "call_1",
                "tool_name": "verify",
                "outcome": "ok",
                "data": {"passed": 11, "failed": 0}
            }
        }),
    ];

    let bodies = [
        ("anthropic", AnthropicProvider::build_request_body(&request)),
        (
            "openai_compatible",
            OpenAiCompatibleProvider::build_request_body(&request),
        ),
        (
            "openai_responses",
            OpenAiResponsesProvider::build_request_body(&request),
        ),
        ("ollama", OllamaProvider::build_request_body(&request)),
        ("bedrock", BedrockProvider::build_request_body(&request)),
        ("gemini", GeminiProvider::build_request_body(&request)),
        (
            "gemini_interactions",
            GeminiInteractions::build_request_body(&request),
        ),
    ];

    for (provider, body) in bodies {
        assert_no_storage_metadata(&body, provider);
    }
}
