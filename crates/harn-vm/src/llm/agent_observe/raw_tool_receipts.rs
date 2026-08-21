//! Provider-native tool-call projection for response transcript events.

pub(super) fn project_onto_event(
    event: &mut serde_json::Value,
    result: &crate::llm::api::LlmResult,
) {
    if result.raw_tool_calls.is_empty() {
        return;
    }
    event["raw_tool_calls"] = serde_json::Value::Array(
        result
            .raw_tool_calls
            .iter()
            .cloned()
            .map(crate::llm::api::RawProviderToolCall::into_value)
            .collect(),
    );
}

#[cfg(test)]
mod tests {
    use super::super::{dump_llm_response, pop_llm_transcript_dir, push_llm_transcript_dir};

    #[test]
    fn response_record_retains_raw_provider_tool_calls() {
        let result = parsed_wrapped_text_tool_call_result();

        let dir = tempfile::tempdir().expect("tempdir");
        push_llm_transcript_dir(dir.path().to_str().expect("utf8"));
        dump_llm_response(0, "call-raw", &result, 42, None);
        pop_llm_transcript_dir();

        let event = response_event(&dir);
        assert_eq!(event["tool_calls"][0]["name"], "look");
        assert_eq!(event["parsed_tool_calls_ref"], "tool_calls");
        assert!(event.get("parsed_tool_calls").is_none());
        assert_eq!(
            crate::llm::response_tool_calls::resolve(&event).unwrap(),
            event["tool_calls"].as_array().map(Vec::as_slice)
        );
        assert_eq!(event["raw_tool_calls"][0]["function"]["name"], "tool_call");
        assert_eq!(
            event["raw_tool_calls"][0]["function"]["arguments"],
            "<tool_call>\nlook({ file: \"Sources/App.swift\", intent: \"read\" })\n</tool_call>"
        );
    }

    #[test]
    fn response_record_omits_absent_raw_provider_tool_calls() {
        let mut result = parsed_wrapped_text_tool_call_result();
        result.raw_tool_calls.clear();

        let dir = tempfile::tempdir().expect("tempdir");
        push_llm_transcript_dir(dir.path().to_str().expect("utf8"));
        dump_llm_response(0, "call-no-raw", &result, 42, None);
        pop_llm_transcript_dir();

        let event = response_event(&dir);
        assert_eq!(event["tool_calls"][0]["name"], "look");
        assert!(
            event.get("raw_tool_calls").is_none(),
            "raw_tool_calls must be omitted when no provider-native receipt exists: {event}"
        );
    }

    fn response_event(dir: &tempfile::TempDir) -> serde_json::Value {
        std::fs::read_to_string(dir.path().join("llm_transcript.jsonl"))
            .expect("read transcript")
            .lines()
            .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("valid jsonl"))
            .find(|event| event["type"].as_str() == Some("provider_call_response"))
            .expect("provider_call_response event")
    }

    fn parsed_wrapped_text_tool_call_result() -> crate::llm::api::LlmResult {
        let response = serde_json::json!({
            "choices": [{
                "message": {
                    "content": "",
                    "tool_calls": [{
                        "id": "call_wrapper_args",
                        "type": "function",
                        "function": {
                            "name": "tool_call",
                            "arguments": "<tool_call>\nlook({ file: \"Sources/App.swift\", intent: \"read\" })\n</tool_call>"
                        }
                    }]
                },
                "finish_reason": "tool_calls"
            }],
            "usage": {"prompt_tokens": 12, "completion_tokens": 5}
        });

        crate::llm::api::parse_llm_response_for_provider(
            &response,
            "fireworks",
            "accounts/fireworks/models/gpt-oss-120b",
            crate::llm::capabilities::WireDialect::OpenAiCompat,
            false,
        )
        .expect("parser succeeds")
    }
}
