use super::*;

#[test]
fn budget_category_recovers_every_dimension() {
    // Structured guards (mcp/pg call counts, LLM preflight) carry the
    // dimension on the `limit` field.
    let structured = |limit: &str| {
        harn_vm::VmError::Thrown(harn_vm::VmValue::dict(std::collections::BTreeMap::from([
            (
                "category".to_string(),
                harn_vm::VmValue::String(arcstr::ArcStr::from("budget_exceeded")),
            ),
            (
                "limit".to_string(),
                harn_vm::VmValue::String(arcstr::ArcStr::from(limit)),
            ),
        ])))
    };
    assert_eq!(
        budget_category_from_error(&structured("mcp_calls")).as_deref(),
        Some("mcp_calls"),
    );
    assert_eq!(
        budget_category_from_error(&structured("pg_queries")).as_deref(),
        Some("pg_queries"),
    );
    assert_eq!(
        budget_category_from_error(&structured("PRIVATE-CUSTOMER-DIAGNOSTIC-123456")),
        None,
        "unrecognized caller data must not become a public budget dimension",
    );

    // LLM cost/token mid-call exhaustion raises the categorised
    // variant; the message disambiguates cost from tokens so the
    // per-class telemetry is accurate.
    let categorized = |message: &str| harn_vm::VmError::CategorizedError {
        message: message.to_string(),
        category: harn_vm::ErrorCategory::BudgetExceeded,
    };
    assert_eq!(
        budget_category_from_error(&categorized("LLM budget exceeded: spent $0.01 of $0.00"))
            .as_deref(),
        Some("llm_cost_usd"),
    );
    assert_eq!(
        budget_category_from_error(&categorized(
            "LLM token budget exceeded: spent 11 of 10 tokens"
        ))
        .as_deref(),
        Some("llm_tokens"),
    );
}

#[test]
fn dispatch_runtime_summary_does_not_render_an_undeclared_throw() {
    let error = classify_vm_error(harn_vm::VmError::Thrown(harn_vm::VmValue::dict(
        std::collections::BTreeMap::from([(
            "message".to_string(),
            harn_vm::VmValue::String(arcstr::ArcStr::from("PRIVATE-CUSTOMER-DIAGNOSTIC-123456")),
        )]),
    )));
    let message = error.message();
    assert_eq!(message, "tool threw an undeclared value");
    assert!(!message.contains("PRIVATE-CUSTOMER-DIAGNOSTIC"));
}
