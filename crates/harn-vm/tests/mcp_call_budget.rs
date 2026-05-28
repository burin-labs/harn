#![recursion_limit = "256"]
//! Proves the `@budget(mcp_calls: N)` ceiling is enforced at the real
//! `mcp_host::call` charge site: once `N` calls have been issued, the
//! `(N + 1)`-th is rejected with a `BudgetExceeded`-categorised error
//! before it can touch a transport — exactly how `harn-serve` caps a
//! runaway `.harn` tool loop. No MCP server is spawned: the charge runs
//! ahead of connection setup, so calls against an unregistered server
//! still consume the budget and the over-limit call rejects deterministically.

use harn_vm::{error_to_category, ErrorCategory, VmError, VmValue};
use serde_json::json;

#[tokio::test]
async fn mcp_call_budget_rejects_call_past_ceiling() {
    harn_vm::reset_thread_local_state();
    let _guard = harn_vm::install_mcp_call_budget(2);

    // Calls 1 and 2 are within budget; they still fail because no server
    // is registered, but the failure is a transport/lookup error — not a
    // budget rejection. The point is that they consume a slot apiece.
    for attempt in 1..=2 {
        let err = harn_vm::mcp_host::call("ghost", "tool", json!({}))
            .await
            .expect_err("unregistered server fails to connect");
        assert_ne!(
            error_to_category(&err),
            ErrorCategory::BudgetExceeded,
            "call {attempt} is within budget and must not be a budget rejection"
        );
    }

    // The third call crosses the ceiling and rejects up front.
    let err = harn_vm::mcp_host::call("ghost", "tool", json!({}))
        .await
        .expect_err("third call exceeds mcp_calls: 2");
    assert_eq!(error_to_category(&err), ErrorCategory::BudgetExceeded);
    match &err {
        VmError::Thrown(VmValue::Dict(d)) => {
            assert_eq!(
                d.get("limit").map(|v| v.display()).as_deref(),
                Some("mcp_calls"),
            );
        }
        other => panic!("expected structured Thrown dict, got {other:?}"),
    }

    harn_vm::reset_thread_local_state();
}

#[tokio::test]
async fn mcp_calls_without_budget_are_uncapped() {
    harn_vm::reset_thread_local_state();
    // No guard installed → the charge is a no-op and every call reaches
    // the transport (failing only because `ghost` is unregistered).
    for _ in 0..5 {
        let err = harn_vm::mcp_host::call("ghost", "tool", json!({}))
            .await
            .expect_err("unregistered server fails to connect");
        assert_ne!(error_to_category(&err), ErrorCategory::BudgetExceeded);
    }
    harn_vm::reset_thread_local_state();
}
