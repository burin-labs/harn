//! Shared message normalization and native diagnostic publication.

pub(crate) use harn_llm::wire::openai_normalize::*;

fn should_debug_message_shapes() -> bool {
    std::env::var("HARN_DEBUG_MESSAGE_SHAPES")
        .map(|value| value != "0" && !value.eq_ignore_ascii_case("false"))
        .unwrap_or(false)
}

pub(crate) fn debug_log_message_shapes(label: &str, messages: &[serde_json::Value]) {
    if !should_debug_message_shapes() {
        return;
    }
    let summary = messages
        .iter()
        .enumerate()
        .map(|(idx, message)| {
            let role = message
                .get("role")
                .and_then(|value| value.as_str())
                .unwrap_or("?");
            let content_kind = match message.get("content") {
                Some(serde_json::Value::String(_)) => "string",
                Some(serde_json::Value::Null) => "null",
                Some(serde_json::Value::Array(_)) => "array",
                Some(serde_json::Value::Object(_)) => "object",
                Some(_) => "other",
                None => "missing",
            };
            let has_tool_call_id = message.get("tool_call_id").is_some();
            let tool_calls = message
                .get("tool_calls")
                .and_then(|value| value.as_array())
                .map(|calls| calls.len())
                .unwrap_or(0);
            let has_reasoning = message
                .get("reasoning")
                .map(|value| !value.is_null())
                .unwrap_or(false);
            format!(
                "#{idx}:{role}:content={content_kind}:tool_call_id={has_tool_call_id}:tool_calls={tool_calls}:reasoning={has_reasoning}"
            )
        })
        .collect::<Vec<_>>()
        .join(" | ");
    crate::events::log_info("llm.message_shape", &format!("{label}: {summary}"));
}

#[cfg(test)]
mod tests {
    use super::{
        extract_openai_delta_field_str, extract_openai_message_field_as_text,
        normalize_openai_message_text, normalize_openai_style_messages,
    };

    #[test]
    fn normalize_openai_message_text_uses_reasoning_when_content_missing() {
        let message = serde_json::json!({
            "reasoning": "hello from reasoning"
        });
        let (visible, thinking) = normalize_openai_message_text(&message, Some("stop"), true);
        assert_eq!(visible, "hello from reasoning");
        assert_eq!(thinking, "hello from reasoning");
    }

    #[test]
    fn normalize_openai_message_text_merges_reasoning_and_inline_think_blocks() {
        let message = serde_json::json!({
            "content": "<think>inline reasoning</think>visible answer",
            "reasoning": "separate reasoning"
        });
        let (visible, thinking) = normalize_openai_message_text(&message, Some("stop"), true);
        assert_eq!(visible, "visible answer");
        assert_eq!(thinking, "separate reasoning\ninline reasoning");
    }

    #[test]
    fn normalize_openai_message_text_does_not_promote_reasoning_when_truncated() {
        // A reasoning model cut off mid-thought (finish_reason == "length")
        // with empty content must NOT have its partial reasoning trace
        // promoted into `.text` — that garbage would surface as the answer.
        let message = serde_json::json!({
            "content": "",
            "reasoning": "Let me think step by step about the problem. First I need to"
        });
        let (visible, thinking) = normalize_openai_message_text(&message, Some("length"), true);
        assert_eq!(visible, "", "truncated reasoning leaked into visible text");
        assert_eq!(
            thinking,
            "Let me think step by step about the problem. First I need to"
        );
    }

    #[test]
    fn normalize_openai_message_text_promotes_reasoning_on_clean_stop() {
        // The same reasoning-only message on a clean finish (finish_reason
        // != "length") still promotes the trace, preserving prior behaviour
        // for models that legitimately answer inside the reasoning channel.
        let message = serde_json::json!({
            "content": "",
            "reasoning": "the answer is 42"
        });
        let (visible, thinking) = normalize_openai_message_text(&message, Some("stop"), true);
        assert_eq!(visible, "the answer is 42");
        assert_eq!(thinking, "the answer is 42");
    }

    #[test]
    fn normalize_openai_message_text_does_not_promote_reasoning_when_tool_call_present() {
        // gpt-oss / harmony models route their analysis channel into
        // `reasoning_content` and emit a tool call with empty content. The
        // reasoning is intermediate chain-of-thought, NOT a final answer — the
        // tool call is the action. Promoting it into `.text` would leak the
        // model's private CoT into the user-facing message and into the
        // transcript the eval grader mines, contaminating the meter stick.
        let message = serde_json::json!({
            "content": "",
            "reasoning_content": "We need to write unit tests for the parser. First inspect parser.rs.",
            "tool_calls": [{
                "id": "call_1",
                "type": "function",
                "function": {"name": "look", "arguments": "{\"path\":\"parser.rs\"}"}
            }]
        });
        let (visible, thinking) = normalize_openai_message_text(&message, Some("tool_calls"), true);
        assert_eq!(
            visible, "",
            "reasoning leaked into visible text on a tool-call turn"
        );
        assert_eq!(
            thinking,
            "We need to write unit tests for the parser. First inspect parser.rs."
        );
    }

    #[test]
    fn normalize_openai_message_text_does_not_promote_reasoning_when_tools_are_offered() {
        // Some OpenAI-compatible routes put failed text-tool attempts in a
        // provider reasoning field without emitting a native `tool_calls`
        // array. When the caller offered tools, that reasoning is not a final
        // answer and must stay private.
        let message = serde_json::json!({
            "content": "",
            "reasoning_content": "<tool_call>\nrun({ command: \"echo should-not-run\" })\n</tool_call>"
        });
        let (visible, thinking) = normalize_openai_message_text(&message, Some("stop"), false);
        assert_eq!(
            visible, "",
            "tools-offered reasoning leaked into visible text"
        );
        assert_eq!(
            thinking,
            "<tool_call>\nrun({ command: \"echo should-not-run\" })\n</tool_call>"
        );
    }

    #[test]
    fn normalize_openai_message_text_keeps_structured_reasoning_private() {
        // Some OpenAI-style providers use structured content blocks for
        // hidden reasoning. Those blocks must not be flattened into visible
        // text, because downstream text-tool parsing only sees visible text.
        let message = serde_json::json!({
            "content": [
                {
                    "type": "reasoning",
                    "text": "<tool_call>\nrun({ command: \"echo should-not-run\" })\n</tool_call>"
                },
                { "type": "output_text", "text": "Done." }
            ]
        });
        let (visible, thinking) = normalize_openai_message_text(&message, Some("stop"), false);
        assert_eq!(visible, "Done.");
        assert_eq!(
            thinking,
            "<tool_call>\nrun({ command: \"echo should-not-run\" })\n</tool_call>"
        );
    }

    #[test]
    fn normalize_openai_style_messages_does_not_stringify_structured_reasoning() {
        // When a string-only provider receives conversation history, private
        // reasoning blocks should not be coerced into the visible `content`
        // string sent back to the model.
        let messages = vec![serde_json::json!({
            "role": "assistant",
            "content": [
                { "type": "reasoning", "text": "private plan" },
                { "type": "output_text", "text": "public answer" }
            ]
        })];

        let normalized = normalize_openai_style_messages(messages, true);

        assert_eq!(
            normalized[0]
                .get("content")
                .and_then(|value| value.as_str()),
            Some("public answer")
        );
    }

    #[test]
    fn normalize_openai_message_text_uses_minimax_reasoning_details() {
        let message = serde_json::json!({
            "content": "visible answer",
            "reasoning_details": "minimax private trace"
        });
        let (visible, thinking) = normalize_openai_message_text(&message, Some("stop"), true);
        assert_eq!(visible, "visible answer");
        assert_eq!(thinking, "minimax private trace");
    }

    #[test]
    fn normalize_openai_message_text_can_keep_reasoning_private_without_content() {
        let message = serde_json::json!({
            "content": "",
            "reasoning": "private trace ending with OK"
        });

        let (visible, thinking) = normalize_openai_message_text(&message, Some("stop"), false);

        assert_eq!(visible, "");
        assert_eq!(thinking, "private trace ending with OK");
    }

    #[test]
    fn extract_openai_delta_field_str_returns_raw_chunk_with_inter_token_whitespace() {
        // Ollama's qwen3.6 streaming delivers reasoning as token-sized
        // fragments — leading/trailing whitespace must survive so the
        // accumulated text reads "Here's a thinking process" not
        // "Here'sathinking" or "Here\n's\na\nthinking\nprocess".
        for chunk in [r#""Here""#, r#""'s""#, r#"" a""#, r#"" thinking""#] {
            let delta: serde_json::Value =
                serde_json::from_str(&format!(r#"{{"reasoning":{chunk}}}"#)).unwrap();
            let raw = extract_openai_delta_field_str(&delta, &["reasoning", "reasoning_content"]);
            assert_eq!(raw, chunk.trim_matches('"'));
        }
    }

    #[test]
    fn extract_openai_delta_field_str_prefers_first_present_field() {
        let delta = serde_json::json!({
            "reasoning_content": "from-content",
            "reasoning": "from-bare",
        });
        let raw = extract_openai_delta_field_str(&delta, &["reasoning", "reasoning_content"]);
        assert_eq!(raw, "from-bare");
    }

    #[test]
    fn extract_openai_delta_field_str_skips_empty_fields() {
        let delta = serde_json::json!({
            "reasoning": "",
            "reasoning_content": " token-with-leading-space",
        });
        let raw = extract_openai_delta_field_str(&delta, &["reasoning", "reasoning_content"]);
        assert_eq!(raw, " token-with-leading-space");
    }

    #[test]
    fn extract_openai_delta_field_str_returns_empty_for_missing_fields() {
        let delta = serde_json::json!({"content": "anything"});
        let raw = extract_openai_delta_field_str(&delta, &["reasoning", "reasoning_content"]);
        assert!(raw.is_empty());
    }

    #[test]
    fn extract_openai_message_field_still_paragraph_joins_for_non_streaming_blocks() {
        // The non-streaming response normalizer keeps paragraph-style
        // joining: each `field_names` entry is a complete block, not a
        // streaming delta.
        let message = serde_json::json!({
            "reasoning": "  block one  ",
            "reasoning_content": "block two",
        });
        let combined =
            extract_openai_message_field_as_text(&message, &["reasoning", "reasoning_content"]);
        assert_eq!(combined, "block one\nblock two");
    }
}
