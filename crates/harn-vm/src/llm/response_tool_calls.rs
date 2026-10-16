//! Native result adapter for the canonical response-event call projection.

pub(crate) use harn_llm::response_tool_calls::project_onto_response;
pub use harn_llm::response_tool_calls::{resolve, ResolveError, NATIVE_TOOL_CALLS_REF};

/// Build the observability-only merged view without mutating request history.
pub(crate) fn merged_from_result(result: &crate::llm::api::LlmResult) -> Vec<serde_json::Value> {
    if !result.tool_calls.is_empty() {
        return result.tool_calls.clone();
    }
    result
        .text_projection
        .as_deref()
        .map(|projection| projection.merged_tool_calls(result))
        .unwrap_or_default()
}
