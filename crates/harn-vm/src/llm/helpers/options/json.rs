pub(crate) fn extract_json(text: &str) -> String {
    crate::stdlib::json::extract_json_from_text(text)
}

/// Resolve the wall-clock timeout from `timeout_ms` (the canonical spelling,
/// matching the broader option surface — `with_timeout`, HTTP, command exec).
/// Rounded UP to the nearest whole second — the underlying HTTP transports
/// all consume `Duration::from_secs(u64)`. Sub-second budgets must be
/// enforced at the caller (e.g. the wall-clock post-check in
/// `std/llm/tool_binder`).
pub(crate) fn resolve_timeout_secs(timeout_ms: Option<i64>) -> Option<u64> {
    timeout_ms.map(|ms| {
        if ms <= 0 {
            0
        } else {
            (ms as u64).div_ceil(1000)
        }
    })
}

pub(crate) fn expects_structured_output(opts: &crate::llm::api::LlmCallOptions) -> bool {
    opts.output_format.is_structured() || opts.output_schema.is_some()
}
