//! One typed boundary for provider completions that consumed tokens without a
//! dispatchable result. Complete and streaming parsers share this contract.

use crate::llm::usage::ProviderUsageReceipt;
use crate::value::{VmError, VmValue};

/// Structural signals for a finished LLM turn. They are derived from the
/// parsed response and outbound request, never model-name branching.
pub(crate) struct CompletionContractSignals<'a> {
    pub stop_reason: Option<&'a str>,
    pub output_tokens: i64,
    pub tools_offered: bool,
    pub tool_call_count: usize,
    pub has_tool_search_block: bool,
    pub text: &'a str,
}

/// Detect a clean, billed tool turn with neither a tool call nor committed
/// text. A length stop is always recoverable truncation, not this violation.
pub(crate) fn is_billed_noncommittal_completion(signals: &CompletionContractSignals) -> bool {
    !is_length_stop_reason(signals.stop_reason)
        && signals.output_tokens > 0
        && signals.tools_offered
        && signals.tool_call_count == 0
        && !signals.has_tool_search_block
        && signals.text.trim().is_empty()
}

pub(crate) fn is_length_stop_reason(stop_reason: Option<&str>) -> bool {
    stop_reason.is_some_and(super::super::result::stop_reason_is_length)
}

/// Builds the actionable error for a billed completion that committed neither
/// an answer nor a dispatchable tool call.
pub(crate) fn billed_noncommittal_completion_error(
    provider: &str,
    model: &str,
    usage: ProviderUsageReceipt,
) -> VmError {
    let completion_tokens = usage
        .output_tokens()
        .map_or_else(|| "unknown".to_string(), |tokens| tokens.to_string());
    provider_completion_error(
        provider,
        model,
        "unproductive_completion",
        "unproductive_completion",
        "billed_noncommittal",
        usage,
        format!(
            "provider {provider} model {model} returned billed output \
             (completion_tokens={completion_tokens}) with no dispatchable tool call or answer \
             (upstream contract violation): the model finished cleanly but committed neither a \
             tool call nor visible text. This usually means the route serialized \
             the action only in a private reasoning channel or returned an empty committed \
             message. For OpenRouter aggregate routes, consider provider_route_denylist or \
             provider_order; for first-party routes, prefer a Harn text/json tool format or \
             disable auto reasoning when the capability row documents it.",
        ),
    )
}

/// Builds the typed empty-generation error shared by complete and stream
/// parsers. Dispatch branches on `code`; the diagnostic message may evolve.
pub(crate) fn empty_generation_error(
    provider: &str,
    model: &str,
    usage: ProviderUsageReceipt,
    message: String,
) -> VmError {
    provider_completion_error(
        provider,
        model,
        "empty_generation",
        "empty_generation",
        "empty_generation",
        usage,
        message,
    )
}

fn provider_completion_error(
    provider: &str,
    model: &str,
    code: &str,
    reason: &str,
    completion_kind: &str,
    usage: ProviderUsageReceipt,
    message: String,
) -> VmError {
    let mut fields = crate::value::DictMap::from_iter([
        (
            "category".to_string(),
            VmValue::String(arcstr::ArcStr::from("server_error")),
        ),
        (
            "code".to_string(),
            VmValue::String(arcstr::ArcStr::from(code)),
        ),
        (
            "reason".to_string(),
            VmValue::String(arcstr::ArcStr::from(reason)),
        ),
        (
            "completion_kind".to_string(),
            VmValue::String(arcstr::ArcStr::from(completion_kind)),
        ),
        (
            "provider".to_string(),
            VmValue::String(arcstr::ArcStr::from(provider)),
        ),
        (
            "model".to_string(),
            VmValue::String(arcstr::ArcStr::from(model)),
        ),
        (
            "message".to_string(),
            VmValue::String(arcstr::ArcStr::from(message)),
        ),
        ("provider_usage".to_string(), usage.to_vm_value()),
    ]);
    if let Some(output_tokens) = usage.output_tokens() {
        fields.insert(
            crate::value::intern_key("output_tokens"),
            VmValue::Int(output_tokens),
        );
    }
    VmError::Thrown(VmValue::dict(fields))
}
