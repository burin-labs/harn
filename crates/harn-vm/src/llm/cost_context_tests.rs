use super::cost::{
    estimate_json_tokens, project_llm_call_context_breakdown, project_llm_call_tokens,
    LlmContextTokenBreakdown,
};

fn segment_tokens(breakdown: &LlmContextTokenBreakdown, id: &'static str) -> Option<i64> {
    breakdown
        .segments
        .iter()
        .find(|segment| segment.id == id)
        .map(|segment| segment.tokens)
}

#[test]
fn context_breakdown_reports_request_segments_and_matches_projection() {
    let mut opts = crate::llm::api::options::base_opts("openai");
    opts.system = Some("System policy".to_string());
    opts.messages = vec![
        serde_json::json!({"role": "user", "content": "fix the bug"}),
        serde_json::json!({"role": "assistant", "content": "I will inspect"}),
        serde_json::json!({"role": "tool", "content": "test failed"}),
        serde_json::json!({"role": "developer", "content": "keep it small"}),
    ];
    opts.native_tools = Some(vec![serde_json::json!({
        "type": "function",
        "function": {"name": "read_file", "parameters": {"type": "object"}}
    })]);
    opts.provider_tools = vec![serde_json::json!({
        "type": "web_search_preview",
        "search_context_size": "low"
    })];
    opts.max_tokens = 128;

    let breakdown = project_llm_call_context_breakdown(&opts);
    let (input_tokens, output_tokens) = project_llm_call_tokens(&opts);

    assert_eq!(breakdown.schema, "harn.llm.context_token_breakdown.v1");
    assert_eq!(breakdown.message_count, 4);
    assert_eq!(breakdown.native_tool_count, 1);
    assert_eq!(breakdown.provider_tool_count, 1);
    assert_eq!(breakdown.input_tokens, input_tokens);
    assert_eq!(breakdown.output_budget_tokens, output_tokens);
    assert_eq!(
        breakdown.context_tokens,
        input_tokens.saturating_add(output_tokens)
    );
    assert!(segment_tokens(&breakdown, "system_prompt").unwrap_or(0) > 0);
    assert!(segment_tokens(&breakdown, "user_messages").unwrap_or(0) > 0);
    assert!(segment_tokens(&breakdown, "assistant_messages").unwrap_or(0) > 0);
    assert!(segment_tokens(&breakdown, "tool_results").unwrap_or(0) > 0);
    assert!(segment_tokens(&breakdown, "other_messages").unwrap_or(0) > 0);
    assert!(segment_tokens(&breakdown, "native_tool_schemas").unwrap_or(0) > 0);
    assert!(segment_tokens(&breakdown, "provider_tools").unwrap_or(0) > 0);
    assert_eq!(segment_tokens(&breakdown, "output_budget"), Some(128));
}

#[test]
fn context_breakdown_counts_user_role_tool_result_content_as_tool_results() {
    let mut opts = crate::llm::api::options::base_opts("openai");
    opts.messages = vec![
        serde_json::json!({
            "role": "user",
            "content": "ordinary user request that mentions [result of run] without a closing envelope",
        }),
        serde_json::json!({
            "role": "user",
            "content": "[result of run]\nCommand failed\n[end of run result]\n",
        }),
        serde_json::json!({
            "role": "user",
            "content": [
                {
                    "type": "tool_result",
                    "tool_use_id": "toolu_001",
                    "content": "anthropic-shaped result",
                }
            ],
        }),
        serde_json::json!({
            "role": "user",
            "content": [
                {
                    "toolResult": {
                        "toolUseId": "toolu_002",
                        "content": [{"text": "bedrock-shaped result"}],
                    }
                }
            ],
        }),
        serde_json::json!({
            "role": "assistant",
            "content": "next step",
        }),
    ];
    let ordinary_user_tokens = estimate_json_tokens(&opts.messages[0], &opts.model);
    let tool_result_tokens: i64 = opts.messages[1..4]
        .iter()
        .map(|message| estimate_json_tokens(message, &opts.model))
        .sum();

    let breakdown = project_llm_call_context_breakdown(&opts);

    assert_eq!(breakdown.message_count, 5);
    assert_eq!(
        segment_tokens(&breakdown, "user_messages"),
        Some(ordinary_user_tokens)
    );
    assert!(segment_tokens(&breakdown, "assistant_messages").unwrap_or(0) > 0);
    assert_eq!(
        segment_tokens(&breakdown, "tool_results"),
        Some(tool_result_tokens)
    );
    assert_eq!(segment_tokens(&breakdown, "other_messages"), Some(0));
}
