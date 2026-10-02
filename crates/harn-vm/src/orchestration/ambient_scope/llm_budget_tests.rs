use crate::call_budget::{charge_mcp_call, install_mcp_call_budget, mcp_calls_spent};
use crate::llm::cost::*;
use crate::orchestration::ambient_scope::{scope_ambient, AmbientExecutionScope};
use crate::value::{ErrorCategory, VmError};
use std::future::Future;
use std::task::{Context, Poll};

fn completed_call() -> crate::llm::api::LlmResult {
    serde_json::from_value(serde_json::json!({
        "text": "ok",
        "tool_calls": [],
        "input_tokens": 3,
        "output_tokens": 2,
        "cache_read_tokens": 1,
        "cache_write_tokens": 0,
        "cache_supported": true,
        "model": "scope-test-unpriced-model",
        "provider": "mock",
        "blocks": [],
        "logprobs": [],
    }))
    .unwrap()
}

fn assert_uncapped() {
    assert_eq!(peek_llm_cost_budget(), None);
    assert_eq!(peek_llm_token_budget(), None);
    assert_eq!(peek_total_cost(), 0.0);
    assert_eq!(peek_total_tokens(), 0);
    assert_eq!(
        peek_observed_session_usage(),
        ObservedSessionUsage::default()
    );
    assert_eq!(mcp_calls_spent(), None);
}

#[tokio::test(flavor = "current_thread")]
async fn interleaved_spans_keep_ceilings_and_accounting_until_their_own_completion() {
    reset_cost_state();
    async fn turn(max: f64, tokens: u64, spent: f64, calls: u64) {
        let _cost = install_llm_cost_budget_seeded(Some(max), spent);
        let _tokens = install_llm_token_budget(tokens);
        let _mcp = install_mcp_call_budget(calls);
        for _ in 0..calls {
            charge_mcp_call().unwrap();
        }
        accumulate_llm_usage("test", 0, 0, 0.125).unwrap();
        for _ in 0..calls {
            record_llm_usage(&completed_call()).unwrap();
        }
        tokio::task::yield_now().await;
        // MCP is the direction control: its existing captured owner must
        // survive the exact same poll order as the newly captured LLM owners.
        assert_eq!(mcp_calls_spent(), Some(calls));
        assert!(charge_mcp_call().is_err(), "each MCP ceiling must fire");
        assert_eq!(peek_llm_cost_budget(), Some(max));
        assert_eq!(peek_llm_token_budget(), Some(tokens));
        assert_eq!(peek_total_cost(), spent + 0.125);
        assert_eq!(peek_total_tokens(), 5 * calls);
        assert_eq!(peek_observed_session_usage().calls, calls);
        assert_eq!(
            peek_observed_session_usage().cache_read_tokens,
            calls as i64
        );
    }

    let mut first = Box::pin(scope_ambient(
        AmbientExecutionScope::default(),
        turn(1.0, 10, 0.25, 1),
    ));
    let mut second = Box::pin(scope_ambient(
        AmbientExecutionScope::default(),
        turn(2.0, 20, 0.5, 2),
    ));
    let waker = futures::task::noop_waker();
    let mut cx = Context::from_waker(&waker);
    assert!(first.as_mut().poll(&mut cx).is_pending());
    assert_eq!(mcp_calls_spent(), None);
    assert!(second.as_mut().poll(&mut cx).is_pending());
    assert_eq!(mcp_calls_spent(), None);
    // Resume the first while the second guard is still held across its await.
    // Without LLM capture, MCP still reads its own ledger but cost reads 2.0.
    assert_eq!(first.as_mut().poll(&mut cx), Poll::Ready(()));
    assert_uncapped();
    assert_eq!(second.as_mut().poll(&mut cx), Poll::Ready(()));
    assert_uncapped();
}

#[tokio::test(flavor = "current_thread")]
async fn fanout_shares_spend_observations_and_rearmed_ceilings() {
    reset_cost_state();
    let _cost = install_llm_cost_budget_seeded(Some(1.0), 0.25);
    let _tokens = install_llm_token_budget(10);
    // Both child kinds belong to the same accounting tree, even on another
    // executor thread. A fresh install inside either child remains independent.
    let inline = AmbientExecutionScope::capture_for_inline_subtask();
    let worker = AmbientExecutionScope::capture_inherited();
    std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap()
            .block_on(scope_ambient(inline, async {
                accumulate_llm_usage("test", 0, 0, 0.125).unwrap();
                record_llm_usage(&completed_call()).unwrap();
                set_llm_cost_budget(Some(0.375));
                set_llm_token_budget(Some(5));
            }));
    })
    .join()
    .unwrap();
    scope_ambient(worker, async {
        assert_eq!(peek_total_cost(), 0.375);
        assert_eq!(peek_total_tokens(), 5);
        assert_eq!(peek_observed_session_usage().calls, 1);
        assert_eq!(peek_llm_cost_budget(), Some(0.375));
        assert_eq!(peek_llm_token_budget(), Some(5));
        // Charge beyond both limits. The completed response must enter both
        // ledgers even though token exhaustion takes precedence in the error.
        assert!(matches!(
            accumulate_llm_usage("test", 1, 0, 0.125),
            Err(VmError::CategorizedError {
                category: ErrorCategory::BudgetExceeded,
                ..
            })
        ));
    })
    .await;
    assert_eq!(peek_total_cost(), 0.5);
    assert_eq!(peek_total_tokens(), 6);
    set_llm_token_budget(None);
    assert!(matches!(
        accumulate_llm_usage("test", 0, 0, 0.125),
        Err(VmError::CategorizedError {
            category: ErrorCategory::BudgetExceeded,
            ..
        })
    ));
    assert_eq!(peek_total_cost(), 0.625);
}

#[tokio::test(flavor = "current_thread")]
async fn cancelling_a_suspended_span_leaves_the_callers_budget_owner_intact() {
    reset_cost_state();
    let _cost = install_llm_cost_budget_seeded(Some(3.0), 0.5);
    let _tokens = install_llm_token_budget(30);
    accumulate_llm_usage("test", 5, 0, 0.0).unwrap();
    let mut suspended = Box::pin(scope_ambient(AmbientExecutionScope::default(), async {
        let _cost = install_llm_cost_budget(1.0);
        let _tokens = install_llm_token_budget(10);
        std::future::pending::<()>().await;
    }));
    let waker = futures::task::noop_waker();
    assert!(suspended
        .as_mut()
        .poll(&mut Context::from_waker(&waker))
        .is_pending());
    drop(suspended);
    assert_eq!(peek_llm_cost_budget(), Some(3.0));
    assert_eq!(peek_llm_token_budget(), Some(30));
    assert_eq!(peek_total_cost(), 0.5);
    assert_eq!(peek_total_tokens(), 5);
}

#[tokio::test(flavor = "current_thread")]
async fn uncapped_fanout_still_has_one_accounting_owner() {
    reset_cost_state();
    let inline = AmbientExecutionScope::capture_for_inline_subtask();
    let worker = AmbientExecutionScope::capture_inherited();
    scope_ambient(inline, async {
        record_llm_usage(&completed_call()).unwrap();
        accumulate_llm_usage("test", 0, 0, 0.125).unwrap();
    })
    .await;
    scope_ambient(worker, async {
        assert_eq!(peek_total_tokens(), 5);
        assert_eq!(peek_total_cost(), 0.125);
        record_llm_usage(&completed_call()).unwrap();
    })
    .await;
    assert_eq!(peek_total_tokens(), 10);
    assert_eq!(peek_total_cost(), 0.125);
    assert_eq!(peek_observed_session_usage().calls, 2);
    assert_eq!(peek_llm_cost_budget(), None);
    assert_eq!(peek_llm_token_budget(), None);
    reset_cost_state();
}
