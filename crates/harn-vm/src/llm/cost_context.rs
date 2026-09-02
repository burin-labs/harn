//! Context-token estimation for one LLM request.
//!
//! Owns "how big is this call" — the per-segment token breakdown a request
//! projects before it is sent. [`super::cost`] owns "what does it cost" and
//! prices these counts; keeping the two apart means a pricing change cannot
//! silently redefine what a segment counts.

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub(crate) struct LlmContextTokenBreakdown {
    pub schema: &'static str,
    pub segments: Vec<LlmContextTokenSegment>,
    pub input_tokens: i64,
    pub output_budget_tokens: i64,
    pub context_tokens: i64,
    pub message_count: usize,
    pub native_tool_count: usize,
    pub provider_tool_count: usize,
}

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub(crate) struct LlmContextTokenSegment {
    pub id: &'static str,
    pub label: &'static str,
    pub tokens: i64,
}

pub(super) fn estimate_json_tokens(value: &serde_json::Value, model: &str) -> i64 {
    match value {
        serde_json::Value::Null | serde_json::Value::Bool(_) | serde_json::Value::Number(_) => 1,
        serde_json::Value::String(s) => estimate_text_tokens_for_model(s, model),
        serde_json::Value::Array(items) => items
            .iter()
            .map(|item| estimate_json_tokens(item, model))
            .sum(),
        serde_json::Value::Object(map) => map
            .iter()
            .map(|(key, value)| {
                estimate_text_tokens_for_model(key, model) + estimate_json_tokens(value, model)
            })
            .sum(),
    }
}

fn estimate_text_tokens_for_model(text: &str, model: &str) -> i64 {
    super::token_count::estimate_text_tokens(text, Some(model)).tokens
}

pub(crate) fn project_llm_call_tokens(opts: &super::api::LlmCallOptions) -> (i64, i64) {
    let breakdown = project_llm_call_context_breakdown(opts);
    (breakdown.input_tokens, breakdown.output_budget_tokens)
}

pub(crate) fn project_llm_call_context_breakdown(
    opts: &super::api::LlmCallOptions,
) -> LlmContextTokenBreakdown {
    let system_tokens = opts
        .system
        .as_deref()
        .map(|system| estimate_text_tokens_for_model(system, &opts.model))
        .unwrap_or(0);
    let mut user_message_tokens = 0;
    let mut assistant_message_tokens = 0;
    let mut tool_result_tokens = 0;
    let mut other_message_tokens = 0;
    for message in &opts.messages {
        let tokens = estimate_json_tokens(message, &opts.model);
        if super::context_breakdown::message_is_tool_result(message) {
            tool_result_tokens += tokens;
            continue;
        }
        match message.get("role").and_then(serde_json::Value::as_str) {
            Some("user") => user_message_tokens += tokens,
            Some("assistant") => assistant_message_tokens += tokens,
            _ => other_message_tokens += tokens,
        }
    }
    // Deferred tools ship in the same array as resident ones but stay out of
    // the model's context until a tool-search call surfaces them, so a single
    // total cannot tell "this tool costs nothing yet" from "this measurement
    // did not fire". Splitting the two makes a `defer_loading` change visible
    // instead of reading as zero (#7768). The two partition the array, so
    // every total below — and every budget decision taken from it — is
    // byte-identical to a single combined segment.
    let tool_token_estimate = |tool: &serde_json::Value| {
        estimate_text_tokens_for_model(
            &serde_json::to_string(tool).unwrap_or_default(),
            &opts.model,
        )
    };
    let (resident_tools, deferred_tools): (Vec<_>, Vec<_>) = opts
        .native_tools
        .as_deref()
        .unwrap_or_default()
        .iter()
        .partition(|tool| !crate::llm::tools::native_tool_is_deferred(tool));
    let resident_tool_tokens: i64 = resident_tools
        .iter()
        .copied()
        .map(tool_token_estimate)
        .sum();
    let deferred_tool_tokens: i64 = deferred_tools
        .iter()
        .copied()
        .map(tool_token_estimate)
        .sum();
    let tool_tokens = resident_tool_tokens.saturating_add(deferred_tool_tokens);
    let provider_tool_tokens: i64 = opts
        .provider_tools
        .iter()
        .map(|tool| {
            estimate_text_tokens_for_model(
                &serde_json::to_string(tool).unwrap_or_default(),
                &opts.model,
            )
        })
        .sum();
    let projected_input_tokens = system_tokens
        .saturating_add(user_message_tokens)
        .saturating_add(assistant_message_tokens)
        .saturating_add(tool_result_tokens)
        .saturating_add(other_message_tokens)
        .saturating_add(tool_tokens)
        .saturating_add(provider_tool_tokens);
    let projected_output_tokens = opts.max_tokens.max(0);
    let segments = vec![
        LlmContextTokenSegment {
            id: "system_prompt",
            label: "System prompt",
            tokens: system_tokens,
        },
        LlmContextTokenSegment {
            id: "user_messages",
            label: "User turns",
            tokens: user_message_tokens,
        },
        LlmContextTokenSegment {
            id: "assistant_messages",
            label: "Assistant turns",
            tokens: assistant_message_tokens,
        },
        LlmContextTokenSegment {
            id: "tool_results",
            label: "Tool results",
            tokens: tool_result_tokens,
        },
        LlmContextTokenSegment {
            id: "other_messages",
            label: "Other messages",
            tokens: other_message_tokens,
        },
        LlmContextTokenSegment {
            id: "native_tool_schemas",
            label: "Native tool schemas",
            tokens: resident_tool_tokens,
        },
        LlmContextTokenSegment {
            id: "deferred_tool_schemas",
            label: "Deferred tool schemas",
            tokens: deferred_tool_tokens,
        },
        LlmContextTokenSegment {
            id: "provider_tools",
            label: "Provider-hosted tools",
            tokens: provider_tool_tokens,
        },
        LlmContextTokenSegment {
            id: "output_budget",
            label: "Output budget",
            tokens: projected_output_tokens,
        },
    ];
    LlmContextTokenBreakdown {
        schema: "harn.llm.context_token_breakdown.v1",
        segments,
        input_tokens: projected_input_tokens,
        output_budget_tokens: projected_output_tokens,
        context_tokens: projected_input_tokens.saturating_add(projected_output_tokens),
        message_count: opts.messages.len(),
        native_tool_count: opts
            .native_tools
            .as_ref()
            .map(|tools| tools.len())
            .unwrap_or(0),
        provider_tool_count: opts.provider_tools.len(),
    }
}

pub(crate) fn project_llm_call_context_tokens(opts: &super::api::LlmCallOptions) -> u64 {
    let (input, output) = project_llm_call_tokens(opts);
    input.max(0) as u64 + output.max(0) as u64
}
