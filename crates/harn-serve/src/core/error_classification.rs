use crate::DispatchError;

/// Translate a VM-level error into the dispatcher's typed error.
///
/// Three signals get hoisted out of `Generic` so adapters can render
/// each correctly:
///
/// * `ErrorCategory::Cancelled` — caller-initiated cancel (HTTP 499).
/// * `ErrorCategory::BudgetExceeded` — a `@budget(...)` ceiling fired
///   (HTTP 429, `code = "budget_exceeded"`).
/// * everything else → `Execution` (HTTP 500).
pub(super) fn classify_vm_error(error: harn_vm::VmError) -> DispatchError {
    let category = harn_vm::error_to_category(&error);
    let message = error.to_string();
    match category {
        harn_vm::ErrorCategory::Cancelled => DispatchError::Cancelled(message),
        harn_vm::ErrorCategory::BudgetExceeded => DispatchError::BudgetExceeded {
            category: budget_category_from_error(&error)
                .unwrap_or_else(|| "llm_cost_usd".to_string()),
            message,
        },
        _ => DispatchError::Execution(message),
    }
}

/// Best-effort attempt to recover the specific budget dimension that
/// fired (one of `llm_cost_usd`, `llm_tokens`, `mcp_calls`,
/// `pg_queries`) from a `VmError` so per-class rejection telemetry stays
/// accurate. The structured form (`VmError::Thrown(Dict)` — the
/// preflight LLM check and the mcp/pg call-count guards) carries it as
/// the `limit` field. The LLM cost/token guards raise the categorised
/// mid-call variant instead, where we disambiguate on the message.
pub(super) fn budget_category_from_error(error: &harn_vm::VmError) -> Option<String> {
    match error {
        harn_vm::VmError::Thrown(harn_vm::VmValue::Dict(d)) => d
            .get("limit")
            .map(|value| value.display())
            .filter(|s| !s.is_empty()),
        harn_vm::VmError::CategorizedError { message, .. } if message.contains("LLM") => {
            if message.contains("token") {
                Some("llm_tokens".to_string())
            } else {
                Some("llm_cost_usd".to_string())
            }
        }
        _ => None,
    }
}
