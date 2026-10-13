//! Context-token estimation for one LLM request.
//!
//! Owns "how big is this call" — the per-segment token breakdown a request
//! projects before it is sent. [`super::cost`] owns "what does it cost" and
//! prices these counts; keeping the two apart means a pricing change cannot
//! silently redefine what a segment counts.
//!
//! Because the projection is request-side, it measures what Harn puts on the
//! wire, not what the model ends up reading. Provider-side deferral is where
//! that distinction bites: a deferred tool is sent but not resident, so it
//! gets its own segment and its own count instead of being folded into the
//! resident total, where it would make a real saving read as zero (#7768).

#[derive(Clone, Debug, PartialEq, serde::Serialize)]
pub(crate) struct LlmContextTokenBreakdown {
    pub schema: &'static str,
    pub segments: Vec<LlmContextTokenSegment>,
    pub input_tokens: i64,
    pub output_budget_tokens: i64,
    pub context_tokens: i64,
    pub message_count: usize,
    /// Every tool in the request array, deferred ones included. This is what
    /// Harn sends.
    pub native_tool_count: usize,
    /// The subset whose schema is resident in the model's context on this
    /// call. Without it a reader has the deferred token split but no count to
    /// pair it with, and `native_tool_count` alone reads as "nothing was
    /// deferred" (#7768).
    pub resident_tool_count: usize,
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

/// Token estimate for one transcript message. Each call re-sends the whole
/// transcript, so estimates are memoized by message content and model rather
/// than re-tokenized on every turn.
fn message_tokens(
    message: &serde_json::Value,
    key: super::content_hash::ContentFingerprint,
    model: &str,
) -> i64 {
    MESSAGE_TOKEN_MEMO.with(|memo| {
        let mut memo = memo.borrow_mut();
        if memo.model != model {
            memo.entries.clear();
            memo.model = model.to_string();
        }
        if let Some(tokens) = memo.entries.get(&key) {
            return tokens;
        }
        let tokens = estimate_json_tokens(message, model);
        memo.entries.insert(key, tokens, 1);
        tokens
    })
}

struct MessageTokenMemo {
    model: String,
    entries: super::content_hash::ContentMemo<i64>,
}

thread_local! {
    static MESSAGE_TOKEN_MEMO: std::cell::RefCell<MessageTokenMemo> =
        std::cell::RefCell::new(MessageTokenMemo {
            model: String::new(),
            entries: super::content_hash::ContentMemo::new(MESSAGE_TOKEN_MEMO_ENTRIES),
        });
    #[cfg(test)]
    static CONTEXT_BREAKDOWN_PROJECTIONS: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}

const MESSAGE_TOKEN_MEMO_ENTRIES: usize = 65_536;

fn estimate_text_tokens_for_model(text: &str, model: &str) -> i64 {
    super::token_count::estimate_text_tokens(text, Some(model)).tokens
}

/// How many tokens one provider attempt sends and may receive.
///
/// Cost admission, the route rate limiter, and the rate governor all charge
/// this one projection. An observed attempt computes its
/// [`LlmContextTokenBreakdown`] once and hands every consumer the projection
/// taken from it, because each projection walks the whole transcript.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct LlmCallTokenProjection {
    pub input_tokens: i64,
    pub output_budget_tokens: i64,
}

impl LlmCallTokenProjection {
    pub(crate) fn for_options(opts: &super::api::LlmCallOptions) -> Self {
        project_llm_call_context_breakdown(opts).token_projection()
    }

    /// Gross input plus output budget, the quantity token-rate limits charge.
    pub(crate) fn gross_tokens(self) -> u64 {
        self.input_tokens.max(0) as u64 + self.output_budget_tokens.max(0) as u64
    }
}

impl LlmContextTokenBreakdown {
    pub(crate) fn token_projection(&self) -> LlmCallTokenProjection {
        LlmCallTokenProjection {
            input_tokens: self.input_tokens,
            output_budget_tokens: self.output_budget_tokens,
        }
    }
}

pub(crate) fn project_llm_call_context_breakdown(
    opts: &super::api::LlmCallOptions,
) -> LlmContextTokenBreakdown {
    #[cfg(test)]
    CONTEXT_BREAKDOWN_PROJECTIONS.with(|count| count.set(count.get() + 1));
    let system_tokens = opts
        .system
        .as_deref()
        .map(|system| estimate_text_tokens_for_model(system, &opts.model))
        .unwrap_or(0);
    let mut user_message_tokens = 0;
    let mut assistant_message_tokens = 0;
    let mut tool_result_tokens = 0;
    let mut other_message_tokens = 0;
    let keys = super::content_hash::content_fingerprints(&opts.messages);
    for (message, key) in opts.messages.iter().zip(keys) {
        let tokens = message_tokens(message, key, &opts.model);
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
    let output_schema_tokens = opts.wire_output_schema().map_or(0, |(schema, _)| {
        estimate_text_tokens_for_model(&crate::canonical_json::to_string(&schema), &opts.model)
    });
    let projected_input_tokens = system_tokens
        .saturating_add(user_message_tokens)
        .saturating_add(assistant_message_tokens)
        .saturating_add(tool_result_tokens)
        .saturating_add(other_message_tokens)
        .saturating_add(tool_tokens)
        .saturating_add(provider_tool_tokens)
        .saturating_add(output_schema_tokens);
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
            id: "output_schema",
            label: "Structured output schema",
            tokens: output_schema_tokens,
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
        resident_tool_count: resident_tools.len(),
        native_tool_count: opts
            .native_tools
            .as_ref()
            .map(|tools| tools.len())
            .unwrap_or(0),
        provider_tool_count: opts.provider_tools.len(),
    }
}

pub(crate) fn project_llm_call_context_tokens(opts: &super::api::LlmCallOptions) -> u64 {
    LlmCallTokenProjection::for_options(opts).gross_tokens()
}

/// Context-token breakdowns projected on this thread.
#[cfg(test)]
pub(crate) fn context_breakdown_projections() -> usize {
    CONTEXT_BREAKDOWN_PROJECTIONS.with(std::cell::Cell::get)
}
