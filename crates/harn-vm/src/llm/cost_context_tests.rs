use super::cost_context::{
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

#[test]
fn deferred_tools_leave_the_resident_segment_without_changing_the_total() {
    // #7768: a single tool segment made "this tool is deferred, so it costs
    // nothing yet" and "the measurement never fired" the same number. The
    // split is only worth anything if a deferred tool actually moves, so both
    // directions are asserted against the same two-tool request.
    let resident = serde_json::json!({
        "name": "read_file",
        "description": "Read a slice of a file",
        "input_schema": {"type": "object"},
    });
    let deferred = serde_json::json!({
        "name": "run_migration",
        "description": "Run a database migration",
        "input_schema": {"type": "object"},
        "defer_loading": true,
    });

    let mut both_resident = crate::llm::api::options::base_opts("anthropic");
    both_resident.native_tools = Some(vec![resident.clone(), {
        let mut undeferred = deferred.clone();
        undeferred
            .as_object_mut()
            .expect("tool object")
            .remove("defer_loading");
        undeferred
    }]);

    let mut one_deferred = crate::llm::api::options::base_opts("anthropic");
    one_deferred.native_tools = Some(vec![resident, deferred]);

    let all = project_llm_call_context_breakdown(&both_resident);
    let split = project_llm_call_context_breakdown(&one_deferred);

    // The measured zero, and the non-null read that makes it mean something.
    assert_eq!(segment_tokens(&all, "deferred_tool_schemas"), Some(0));
    assert!(
        segment_tokens(&split, "deferred_tool_schemas").unwrap_or(0) > 0,
        "deferring a tool must put tokens in the deferred segment"
    );
    assert!(
        segment_tokens(&split, "native_tool_schemas").unwrap_or(0)
            < segment_tokens(&all, "native_tool_schemas").unwrap_or(0),
        "the resident segment must shrink by what the deferred segment gained"
    );

    // Counts: every tool is still sent, only residency changed.
    assert_eq!(split.native_tool_count, 2);
    assert_eq!(split.resident_tool_count, 1);
    assert_eq!(all.native_tool_count, all.resident_tool_count);

    // Splitting a segment must not change any budget taken from it: the two
    // parts still add up to what one combined segment reported, which is what
    // `input_tokens` and every ceiling downstream are computed from.
    let combined: i64 = one_deferred
        .native_tools
        .as_ref()
        .expect("native tools set")
        .iter()
        .map(|tool| estimate_json_tokens(&serde_json::json!(tool.to_string()), &one_deferred.model))
        .sum();
    assert_eq!(
        segment_tokens(&split, "native_tool_schemas").unwrap_or(0)
            + segment_tokens(&split, "deferred_tool_schemas").unwrap_or(0),
        combined,
    );
}
