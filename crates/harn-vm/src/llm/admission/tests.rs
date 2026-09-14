use super::*;
use crate::llm::api::options::base_opts;
use crate::llm::cost::LlmBudgetEnvelope;

fn opts(ceiling: f64) -> LlmCallOptions {
    let mut opts = base_opts("openai");
    opts.model = "gpt-5.6-luna".into();
    opts.provider_overrides = None;
    opts.budget = Some(LlmBudgetEnvelope {
        admission: Some(AdmissionMode::Conservative),
        total_budget_usd: Some(ceiling),
        ..Default::default()
    });
    opts
}

fn result(input: i64, output: i64) -> LlmResult {
    serde_json::from_value(serde_json::json!({
        "text": "ok", "tool_calls": [], "input_tokens": input, "output_tokens": output,
        "cache_read_tokens": 0, "cache_write_tokens": 0, "model": "gpt-5.6-luna",
        "provider": "openai", "thinking": null, "thinking_summary": null,
        "stop_reason": "stop", "blocks": [], "logprobs": [],
        "telemetry": {"server_prompt_tokens": input, "server_output_tokens": output}
    }))
    .unwrap()
}

#[test]
fn reservation_retains_uncertain_attempt_and_cannot_be_disabled_or_widened() {
    swap_scope(AdmissionScope::default());
    let opts = opts(0.6);
    let request = LlmRequestPayload::from(&opts);
    let first = reserve(&opts, &request).unwrap().unwrap();
    assert!(
        reserve(&opts, &request).is_err(),
        "in-flight reservation is shared"
    );
    drop(first);
    let mut widened = opts.clone();
    widened.budget.as_mut().unwrap().total_budget_usd = Some(10.0);
    assert!(reserve(&widened, &request).is_err());
    widened.budget = None;
    assert!(
        reserve(&widened, &request).is_err(),
        "omission cannot escape a latched scope"
    );
    let scope = SCOPE.with(|slot| slot.borrow().clone());
    let ledger = scope.0.lock().unwrap();
    assert_eq!(ledger.in_flight, Decimal::ZERO);
    assert!(ledger.uncertain > Decimal::ZERO);
    assert_eq!(ledger.ceiling, Some(money(0.6).unwrap()));
}

#[test]
fn observed_short_cached_output_never_reduces_the_next_bound() {
    swap_scope(AdmissionScope::default());
    let opts = opts(0.6);
    let request = LlmRequestPayload::from(&opts);
    let first = reserve(&opts, &request).unwrap().unwrap();
    let original = first.bound.total();
    let mut response = result(1000, 1);
    response.cache_read_tokens = 999;
    first.settle(&response).unwrap();
    let next = reserve(&opts, &request).unwrap().unwrap();
    assert_eq!(next.bound.total(), original);
    assert!(reserve(&opts, &request).is_err());
}

#[test]
fn unknown_pricing_media_and_premium_requests_fail_closed() {
    for kind in ["unknown", "media", "premium", "hosted_tool"] {
        swap_scope(AdmissionScope::default());
        let mut opts = opts(10.0);
        match kind {
            "unknown" => opts.model = "gpt-5.6-luna-unserved".into(),
            "media" => {
                opts.messages = vec![
                    serde_json::json!({"role":"user", "content":[{"type":"input_image","image_url":"https://invalid.example/image"}]}),
                ]
            }
            "premium" => opts.fast = true,
            _ => opts.provider_tools = vec![serde_json::json!({"type":"web_search"})],
        }
        assert!(
            reserve(&opts, &LlmRequestPayload::from(&opts)).is_err(),
            "{kind}"
        );
    }
}

#[test]
fn missing_usage_retains_reservation_and_late_activation_is_refused() {
    swap_scope(AdmissionScope::default());
    let opts = opts(0.6);
    let request = LlmRequestPayload::from(&opts);
    let reservation = reserve(&opts, &request).unwrap().unwrap();
    let mut response = result(1, 1);
    response.telemetry.server_output_tokens = None;
    reservation.settle(&response).unwrap();
    assert!(reserve(&opts, &request).is_err());
    swap_scope(AdmissionScope::default());
    let mut unbounded = opts.clone();
    unbounded.budget = None;
    assert!(reserve(&unbounded, &request).unwrap().is_none());
    assert!(reserve(&opts, &request).is_err());
}

#[test]
fn spawned_scopes_share_reservations_even_when_captured_before_activation() {
    swap_scope(AdmissionScope::default());
    let a = crate::orchestration::AmbientExecutionScope::capture_inherited();
    let b = crate::orchestration::AmbientExecutionScope::capture_inherited();
    // The actual poll wrapper swaps each child onto the same ledger. Hold one
    // reservation across a yield and prove the other child cannot reuse it.
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
            let (done_tx, done_rx) = tokio::sync::oneshot::channel();
            let first = crate::orchestration::scope_ambient(a, async {
                let opts = opts(0.6);
                let reservation = reserve(&opts, &LlmRequestPayload::from(&opts))
                    .unwrap()
                    .unwrap();
                ready_tx.send(()).unwrap();
                done_rx.await.unwrap();
                drop(reservation);
            });
            let second = crate::orchestration::scope_ambient(b, async {
                ready_rx.await.unwrap();
                let opts = opts(0.6);
                assert!(reserve(&opts, &LlmRequestPayload::from(&opts)).is_err());
                done_tx.send(()).unwrap();
            });
            tokio::join!(first, second);
        });
}

#[test]
fn nested_budget_guards_do_not_rearm_the_conservative_allowance() {
    swap_scope(AdmissionScope::default());
    let opts = opts(0.6);
    let request = LlmRequestPayload::from(&opts);
    drop(reserve(&opts, &request).unwrap().unwrap());
    {
        let _nested = crate::llm::cost::install_llm_cost_budget(100.0);
        assert!(reserve(&opts, &request).is_err());
        crate::llm::cost::set_llm_cost_budget(None);
        assert!(reserve(&opts, &request).is_err());
    }
    assert!(reserve(&opts, &request).is_err());
}

#[test]
fn provider_contract_violation_is_accounted_and_stops_further_attempts() {
    swap_scope(AdmissionScope::default());
    let opts = opts(10.0);
    let request = LlmRequestPayload::from(&opts);
    let reservation = reserve(&opts, &request).unwrap().unwrap();
    assert!(reservation.settle(&result(2_000_000, 100)).is_err());
    assert!(reserve(&opts, &request).is_err());
    let scope = SCOPE.with(|slot| slot.borrow().clone());
    let ledger = scope.0.lock().unwrap();
    assert!(ledger.settled_upper > Decimal::ZERO);
    assert_eq!(ledger.in_flight, Decimal::ZERO);
}

fn top_level_scope() -> crate::orchestration::AmbientExecutionScope {
    crate::orchestration::AmbientExecutionScope::capture_for_top_level_execution(
        crate::observability::execution_scope::mint_execution_scope(),
        crate::llm::mock::LlmMockContext::default(),
        crate::stdlib::agents::agents_workers::fresh_worker_registry(),
        crate::stdlib::agents_daemon::fresh_daemon_registry(),
        crate::triggers::registry::runtime::fresh_trigger_registry(),
        crate::agent_sessions::fresh_session_runtime(),
        crate::tracing::fresh_tracing_runtime(),
        crate::llm::agent_session_host::fresh_agent_host_session_runtime(),
    )
}

#[test]
fn independent_top_level_scopes_are_fresh_but_nested_entries_share_the_ceiling() {
    swap_scope(AdmissionScope::default());
    let actor = top_level_scope();
    let grader = top_level_scope();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            crate::orchestration::scope_ambient(actor, async {
                let opts = opts(0.6);
                let request = LlmRequestPayload::from(&opts);
                drop(reserve(&opts, &request).unwrap().unwrap());
                let nested = top_level_scope();
                crate::orchestration::scope_ambient(nested, async {
                    assert!(reserve(&opts, &request).is_err());
                })
                .await;
            })
            .await;
            crate::orchestration::scope_ambient(grader, async {
                let opts = opts(0.6);
                let request = LlmRequestPayload::from(&opts);
                assert!(reserve(&opts, &request).is_ok());
            })
            .await;
        });
}

#[test]
fn an_unexpected_premium_response_stops_the_scope_and_retains_uncertainty() {
    swap_scope(AdmissionScope::default());
    let opts = opts(10.0);
    let request = LlmRequestPayload::from(&opts);
    let reservation = reserve(&opts, &request).unwrap().unwrap();
    let mut response = result(1, 1);
    response.served_fast = true;
    assert!(reservation.settle(&response).is_err());
    assert!(reserve(&opts, &request).is_err());
    let scope = SCOPE.with(|slot| slot.borrow().clone());
    let ledger = scope.0.lock().unwrap();
    assert!(ledger.uncertain > Decimal::ZERO);
    assert_eq!(ledger.in_flight, Decimal::ZERO);
}

#[test]
fn preflight_refusal_latches_ceiling_without_reserving_an_attempt() {
    swap_scope(AdmissionScope::default());
    let mut opts = opts(0.01);
    opts.budget.as_mut().unwrap().max_cost_usd = Some(0.0);
    let request = LlmRequestPayload::from(&opts);
    assert!(reserve(&opts, &request).is_err());
    let scope = SCOPE.with(|slot| slot.borrow().clone());
    {
        let ledger = scope.0.lock().unwrap();
        assert_eq!(ledger.ceiling, Some(money(0.01).unwrap()));
        assert_eq!(ledger.in_flight, Decimal::ZERO);
        assert_eq!(ledger.uncertain, Decimal::ZERO);
        assert!(!ledger.prior_unreserved_attempt);
    }
    opts.budget = None;
    assert!(
        reserve(&opts, &request).is_err(),
        "omitting budget cannot escape the refused call's ceiling"
    );
}

#[test]
fn a_provider_ignoring_the_output_limit_stops_future_admission() {
    swap_scope(AdmissionScope::default());
    let mut opts = opts(0.6);
    opts.max_tokens = 16;
    let request = LlmRequestPayload::from(&opts);
    let reservation = reserve(&opts, &request).unwrap().unwrap();
    assert!(
        reservation.settle(&result(0, 17)).is_err(),
        "an output-limit violation matters even below the aggregate monetary bound"
    );
    assert!(reserve(&opts, &request).is_err());
}
