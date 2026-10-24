use super::*;
use crate::llm::api::options::base_opts;
use crate::llm::cost::LlmBudgetEnvelope;

mod direct_providers;

/// [`super::reserve`] with the attempt's token projection taken from `opts`,
/// as an observed attempt does.
fn reserve(
    opts: &LlmCallOptions,
    request: &LlmRequestPayload,
) -> Result<Option<AttemptReservation>, VmError> {
    super::reserve(
        opts,
        request,
        crate::llm::cost_context::LlmCallTokenProjection::for_options(opts),
    )
}

#[test]
fn concurrent_native_calls_share_one_reservation_and_unknown_usage_keeps_it() {
    let scope = AdmissionScope::default();
    let release = Arc::new(std::sync::Barrier::new(3));
    let (sent, received) = std::sync::mpsc::channel();
    crate::runtime_stack::scope(|threads| {
        for _ in 0..2 {
            let scope = scope.clone();
            let release = release.clone();
            let sent = sent.clone();
            threads.spawn(move || {
                swap_scope(scope);
                let hold = reserve_decision(0.6, Some(1.0), Some(1.0));
                sent.send(hold.is_ok()).unwrap();
                // Both calls have attempted admission before either can
                // finish transport or release its hold.
                release.wait();
                if let Ok(hold) = hold {
                    hold.settle(None).unwrap();
                }
            });
        }
        let admitted =
            usize::from(received.recv().unwrap()) + usize::from(received.recv().unwrap());
        let receipt = scope.receipt().unwrap();
        release.wait();
        assert_eq!(
            admitted, 1,
            "only one physical dispatch may follow admission"
        );
        assert_eq!(receipt.in_flight_usd, money(0.6).unwrap());
        assert_eq!(receipt.denied_attempts, 1);
    });
    let previous = swap_scope(scope.clone());
    assert_eq!(scope.receipt().unwrap().uncertain_usd, money(0.6).unwrap());
    assert!(reserve_decision(0.5, Some(1.0), Some(1.0)).is_err());
    let affordable = reserve_decision(0.4, Some(1.0), Some(1.0)).unwrap();
    affordable.settle(Some(0.1)).unwrap();
    assert_eq!(
        scope.receipt().unwrap().settled_upper_usd,
        money(0.1).unwrap()
    );
    swap_scope(previous);
}

#[test]
fn native_downstream_retry_retains_cost_and_closes_shared_admission() {
    let scope = AdmissionScope::default();
    let previous = swap_scope(scope.clone());
    reserve_decision(0.1, Some(1.0), Some(1.0))
        .unwrap()
        .retain_contract_violation()
        .unwrap();
    let receipt = scope.receipt().unwrap();
    assert_eq!(receipt.uncertain_usd, money(0.1).unwrap());
    assert!(reserve_decision(0.1, Some(1.0), Some(1.0)).is_err());
    let options = opts(1.0);
    assert!(reserve(&options, &LlmRequestPayload::from(&options)).is_err());
    swap_scope(previous);
}

#[test]
fn native_optional_caps_inherit_existing_authority_without_inventing_one() {
    let scope = AdmissionScope::default();
    let previous = swap_scope(scope);
    assert!(reserve_decision(0.1, None, None).is_err());
    assert_eq!(
        remaining_allowance(),
        None,
        "absence is not a measured zero"
    );
    reserve_decision(0.1, Some(0.1), Some(1.0))
        .unwrap()
        .settle(Some(0.1))
        .unwrap();
    let inherited = reserve_decision(0.3, None, None).unwrap();
    assert!((remaining_allowance().unwrap() - 0.6).abs() < 1e-12);
    assert!(
        reserve_decision(0.7, None, None).is_err(),
        "omission cannot bypass in-flight holds"
    );
    inherited.settle(None).unwrap();
    assert!(
        reserve_decision(0.61, None, None).is_err(),
        "unknown usage remains reserved"
    );
    assert!(
        reserve_decision(0.2, Some(0.1), None).is_err(),
        "an explicit per-call cap tightens inheritance"
    );
    swap_scope(previous);
}

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

#[test]
fn native_tool_history_with_harn_transcript_metadata_keeps_conservative_admission() {
    swap_scope(AdmissionScope::default());
    let mut options = opts(10.0);
    options.messages = vec![
        serde_json::json!({
            "role": "assistant",
            "content": null,
            "tool_calls": [{"id":"call_1","type":"function","function":{"name":"inspect","arguments":"{}"}}],
            "_harn": {"tool_calls": [{"id":"call_1","name":"inspect"}]}
        }),
        serde_json::json!({
            "role": "tool",
            "tool_call_id": "call_1",
            "content": "inspected",
            "_harn": {"result_source": "host_tool"}
        }),
    ];
    let request = LlmRequestPayload::from(&options);

    assert!(options
        .messages
        .iter()
        .any(|message| message.get("_harn").is_some()));
    assert!(request
        .messages
        .iter()
        .all(|message| message.get("_harn").is_none()));
    assert!(reserve(&options, &request).is_ok());
}

#[test]
fn conservative_admission_accepts_only_boolean_tool_result_error_flags() {
    for (message, accepted) in [
        (
            serde_json::json!({
                "role": "tool_result",
                "name": "inspect",
                "tool_call_id": "call_1",
                "content": "failed",
                "is_error": true,
            }),
            true,
        ),
        (
            serde_json::json!({"role": "user", "content": "failed", "is_error": true}),
            true,
        ),
        (
            serde_json::json!({
                "role": "tool_result",
                "name": "inspect",
                "tool_call_id": "call_1",
                "content": "failed",
                "is_error": "true",
            }),
            false,
        ),
        (
            serde_json::json!({"role": "assistant", "content": "failed", "is_error": true}),
            false,
        ),
        (
            serde_json::json!({
                "role": "tool_result",
                "name": "inspect",
                "tool_call_id": "call_1",
                "content": "failed",
                "unknown": true,
                "is_error": true,
            }),
            false,
        ),
        (
            serde_json::json!({
                "role": "assistant",
                "content": [{"type": "image", "source": {"type": "base64", "data": "opaque"}}],
            }),
            false,
        ),
        (
            serde_json::json!({
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "call_1",
                    "name": "inspect",
                    "input": [],
                }],
            }),
            false,
        ),
        (
            serde_json::json!({
                "role": "assistant",
                "content": [{
                    "type": "tool_use",
                    "id": "call_1",
                    "name": "inspect",
                    "input": {},
                    "extra": true,
                }],
            }),
            false,
        ),
    ] {
        swap_scope(AdmissionScope::default());
        let mut options = opts(10.0);
        options.provider = "anthropic".into();
        options.model = "claude-opus-4-7".into();
        options.native_tools = Some(vec![serde_json::json!({
            "name": "inspect",
            "input_schema": {"type": "object"},
        })]);
        options.messages = if message["role"] == "tool_result"
            && message["tool_call_id"]
                .as_str()
                .is_some_and(|id| !id.is_empty())
        {
            vec![
                serde_json::json!({
                    "role": "assistant",
                    "content": [{
                        "type": "tool_use",
                        "id": "call_1",
                        "name": "inspect",
                        "input": {},
                    }],
                }),
                message.clone(),
            ]
        } else {
            vec![message.clone()]
        };
        let request = LlmRequestPayload::from(&options);
        match reserve(&options, &request) {
            Ok(_) => assert!(accepted, "unexpected admission for {message}"),
            Err(error) => {
                assert!(!accepted, "unexpected refusal for {message}");
                assert_eq!(
                    crate::llm::admission::denial_reason(&error).as_deref(),
                    Some("unsupported_billing_shape"),
                    "malformed history lacked a typed refusal for {message}"
                );
            }
        }
    }
    assert!(!super::bound::supported_tool_result_error_flag(
        &serde_json::json!({
            "role": "tool_result",
            "name": "inspect",
            "tool_call_id": "",
            "content": "failed",
            "is_error": true,
        })
    ));
}

#[test]
fn anthropic_signed_tool_history_and_private_reasoning_fit_the_full_context_bound() {
    swap_scope(AdmissionScope::default());
    let mut options = opts(10.0);
    options.provider = "anthropic".into();
    options.model = "claude-opus-4-7".into();
    options.native_tools = Some(vec![serde_json::json!({
        "name": "inspect",
        "input_schema": {"type": "object"},
    })]);
    let signed_thinking = serde_json::json!({
        "type": "thinking",
        "thinking": "I should inspect the module.",
        "signature": "opaque-signed-continuation",
    });
    let redacted_thinking = serde_json::json!({
        "type": "redacted_thinking",
        "data": "opaque-redacted-continuation",
    });
    let assistant = crate::llm::tools::build_assistant_response_message(
        "",
        &[signed_thinking.clone(), redacted_thinking.clone()],
        &[serde_json::json!({
            "id": "call_1",
            "name": "inspect",
            "arguments": {},
        })],
        Some("private reasoning stays in durable history"),
    );
    options.messages = vec![
        assistant,
        serde_json::json!({
            "role": "tool",
            "tool_call_id": "call_1",
            "content": "inspected",
            "_harn": {"result_source": "host_tool"},
        }),
    ];

    let request = LlmRequestPayload::from(&options);
    assert!(request.messages[0].get("provider_continuation").is_some());
    assert!(request.messages[0].get("reasoning").is_some());
    assert!(reserve(&options, &request).is_ok());

    let body = crate::llm::providers::AnthropicProvider::build_request_body(&request);
    let messages = body["messages"].as_array().expect("Anthropic messages");
    assert_eq!(
        messages.len(),
        2,
        "assistant call and tool result both survive"
    );
    assert_eq!(messages[0]["role"], "assistant");
    assert_eq!(messages[0]["content"][0], signed_thinking);
    assert_eq!(messages[0]["content"][1], redacted_thinking);
    assert_eq!(messages[0]["content"][2]["type"], "tool_use");
    assert!(messages[0].get("reasoning").is_none());
    assert!(messages[0].get("provider_continuation").is_none());
    assert_eq!(messages[1]["role"], "user");
    assert_eq!(messages[1]["content"][0]["type"], "tool_result");
}

#[test]
fn conservative_admission_rejects_unknown_or_multimodal_anthropic_continuation() {
    for continuation in [
        serde_json::json!({
            "anthropic": {"content_blocks": [{
                "type": "thinking", "thinking": "continue", "signature": "signed",
            }]},
            "openai": {"reasoning": "not supported"},
        }),
        serde_json::json!({
            "anthropic": {"content_blocks": [{
                "type": "thinking", "thinking": "continue", "signature": "signed", "extra": true,
            }]},
        }),
        serde_json::json!({
            "anthropic": {"content_blocks": [{
                "type": "tool_use", "id": "call_1", "name": "inspect", "input": {},
            }]},
        }),
        serde_json::json!({
            "anthropic": {"content_blocks": [{
                "type": "thinking", "thinking": "continue", "signature": "",
            }]},
        }),
    ] {
        swap_scope(AdmissionScope::default());
        let mut options = opts(10.0);
        options.provider = "anthropic".into();
        options.model = "claude-opus-4-7".into();
        options.native_tools = Some(vec![serde_json::json!({
            "name": "inspect",
            "input_schema": {"type": "object"},
        })]);
        options.messages = vec![serde_json::json!({
            "role": "assistant",
            "content": "",
            "provider_continuation": continuation,
        })];
        let request = LlmRequestPayload::from(&options);
        let denial = match reserve(&options, &request) {
            Err(error) => error,
            Ok(_) => panic!("unsupported continuation admitted: {continuation}"),
        };
        assert_eq!(
            crate::llm::admission::denial_reason(&denial).as_deref(),
            Some("unsupported_billing_shape"),
            "unsupported stored continuation was admitted: {continuation}"
        );
    }
    swap_scope(AdmissionScope::default());
    let mut options = opts(10.0);
    options.provider = "openai".into();
    options.messages = vec![serde_json::json!({
        "role": "assistant",
        "content": "",
        "provider_continuation": {
            "anthropic": {"content_blocks": [{
                "type": "thinking", "thinking": "continue", "signature": "signed",
            }]},
        },
    })];
    assert!(reserve(&options, &LlmRequestPayload::from(&options)).is_err());
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
    let mut widened = opts;
    widened.budget.as_mut().unwrap().total_budget_usd = Some(10.0);
    assert!(reserve(&widened, &request).is_err());
    widened.budget = None;
    assert!(
        reserve(&widened, &request).is_err(),
        "omission cannot escape a latched scope"
    );
    let scope = SCOPE.with(|slot| slot.borrow().clone());
    let ledger = scope.ledger.lock().unwrap();
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
    for kind in [
        "unknown",
        "media",
        "premium",
        "reasoning_mode",
        "hosted_tool",
    ] {
        swap_scope(AdmissionScope::default());
        let mut opts = opts(10.0);
        match kind {
            "unknown" => opts.model = "gpt-5.6-luna-unserved".into(),
            "media" => {
                opts.messages = vec![
                    serde_json::json!({"role":"user", "content":[{"type":"input_image","image_url":"https://invalid.example/image"}]}),
                ];
            }
            "premium" => opts.fast = true,
            "reasoning_mode" => {
                opts.model = "gpt-5.6-sol".into();
                opts.reasoning_mode = Some("pro".into());
            }
            _ => opts.provider_tools = vec![serde_json::json!({"type":"web_search"})],
        }
        let denied = reserve(&opts, &LlmRequestPayload::from(&opts))
            .err()
            .expect(kind);
        if kind == "reasoning_mode" {
            assert!(
                denied.to_string().contains("unsupported_billing_shape"),
                "{denied}"
            );
        }
    }
}

#[test]
fn explicit_standard_reasoning_mode_remains_supported() {
    swap_scope(AdmissionScope::default());
    let mut options = opts(0.6);
    options.reasoning_mode = Some(crate::llm::reasoning_modes::STANDARD_MODE_ID.into());
    assert!(reserve(&options, &LlmRequestPayload::from(&options)).is_ok());
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
    let ledger = scope.ledger.lock().unwrap();
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
    let ledger = scope.ledger.lock().unwrap();
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
        let ledger = scope.ledger.lock().unwrap();
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

#[test]
fn partial_usage_cannot_hide_a_known_output_limit_violation() {
    swap_scope(AdmissionScope::default());
    let mut opts = opts(10.0);
    opts.max_tokens = 16;
    let request = LlmRequestPayload::from(&opts);
    let reservation = reserve(&opts, &request).unwrap().unwrap();
    let mut response = result(0, 17);
    response.telemetry.server_prompt_tokens = None;
    assert!(reservation.settle(&response).is_err());
    assert!(reserve(&opts, &request).is_err());
    let scope = SCOPE.with(|slot| slot.borrow().clone());
    let ledger = scope.ledger.lock().unwrap();
    assert!(ledger.uncertain > Decimal::ZERO);
    assert_eq!(ledger.in_flight, Decimal::ZERO);
}

#[test]
fn an_unadmitted_response_model_retains_uncertainty_and_stops_the_scope() {
    swap_scope(AdmissionScope::default());
    let options = opts(10.0);
    let request = LlmRequestPayload::from(&options);
    let reservation = reserve(&options, &request).unwrap().unwrap();
    let mut response = result(1, 1);
    response.model = "unrequested-model".to_string();
    assert!(reservation.settle(&response).is_err());
    assert!(reserve(&options, &request).is_err());
    let snapshot = SCOPE.with(|slot| slot.borrow().receipt().unwrap());
    assert!(snapshot.contract_broken);
    assert!(snapshot.uncertain_usd > Decimal::ZERO);
    assert_eq!(snapshot.in_flight_usd, Decimal::ZERO);
}

#[test]
fn host_allowance_survives_multiple_vm_entries_and_host_spans() {
    swap_scope(AdmissionScope::default());
    let budget = ConservativeLlmBudget::new(0.6).unwrap();
    let independent = ConservativeLlmBudget::new(0.6).unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            budget
                .scope(async {
                    crate::llm::cost::reset_cost_state();
                    crate::orchestration::scope_ambient(top_level_scope(), async {
                        let mut options = opts(0.6);
                        options.budget = None;
                        let request = LlmRequestPayload::from(&options);
                        drop(reserve(&options, &request).unwrap().unwrap());
                    })
                    .await;
                    crate::orchestration::scope_ambient(top_level_scope(), async {
                        let options = opts(10.0);
                        assert!(reserve(&options, &LlmRequestPayload::from(&options)).is_err());
                    })
                    .await;
                })
                .await
                .unwrap();
            budget
                .scope(async {
                    let options = opts(10.0);
                    assert!(reserve(&options, &LlmRequestPayload::from(&options)).is_err());
                })
                .await
                .unwrap();
            independent
                .scope(async {
                    let options = opts(0.6);
                    assert!(reserve(&options, &LlmRequestPayload::from(&options)).is_ok());
                })
                .await
                .unwrap();
        });
}

#[test]
fn host_allowances_are_poll_scoped_and_nested_handles_cannot_replace_them() {
    swap_scope(AdmissionScope::default());
    let left = ConservativeLlmBudget::new(0.6).unwrap();
    let right = ConservativeLlmBudget::new(0.0).unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let a = left.scope(async {
                tokio::task::yield_now().await;
                assert!(right.scope(async {}).await.is_err());
                let nested = ConservativeLlmBudget::new(10.0).unwrap();
                nested
                    .scope(async {
                        let options = opts(0.6);
                        assert!(reserve(&options, &LlmRequestPayload::from(&options)).is_ok());
                    })
                    .await
                    .unwrap();
            });
            let b = right.scope(async {
                tokio::task::yield_now().await;
                let options = opts(10.0);
                assert!(reserve(&options, &LlmRequestPayload::from(&options)).is_err());
            });
            let (a, b) = tokio::join!(a, b);
            a.unwrap();
            b.unwrap();
        });
    for invalid in [-1.0, f64::NAN, f64::INFINITY] {
        assert!(ConservativeLlmBudget::new(invalid).is_err());
    }
}

#[test]
fn host_allowance_preconstructed_future_cannot_replace_a_parent() {
    swap_scope(AdmissionScope::default());
    let parent = ConservativeLlmBudget::new(0.0).unwrap();
    let independent = ConservativeLlmBudget::new(0.6).unwrap();
    let entered = std::cell::Cell::new(false);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let child = independent.scope(async {
                entered.set(true);
            });
            parent
                .scope(async {
                    assert!(child.await.is_err());
                })
                .await
                .unwrap();
        });
    assert!(
        !entered.get(),
        "a preconstructed future replaced the parent allowance"
    );
}

#[test]
fn host_allowance_suspended_future_cannot_replace_a_parent() {
    use std::future::Future as _;
    swap_scope(AdmissionScope::default());
    let parent = ConservativeLlmBudget::new(0.0).unwrap();
    let independent = ConservativeLlmBudget::new(0.6).unwrap();
    let resumed = std::cell::Cell::new(false);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            let child = independent.scope(async {
                tokio::task::yield_now().await;
                resumed.set(true);
            });
            let mut child = std::pin::pin!(child);
            let mut context = std::task::Context::from_waker(std::task::Waker::noop());
            assert!(child.as_mut().poll(&mut context).is_pending());
            parent
                .scope(async {
                    assert!(child.await.is_err());
                })
                .await
                .unwrap();
        });
    assert!(
        !resumed.get(),
        "a suspended future replaced the parent allowance"
    );
}

#[test]
fn host_session_budget_inherits_machine_scope_without_replacing_it() {
    swap_scope(AdmissionScope::default());
    let temp = tempfile::tempdir().unwrap();
    let machine = MachineSpendQuota::open(
        temp.path().join("spend.sqlite"),
        "person",
        MachineSpendPolicy {
            daily_limit_microusd: Some(1_000_000),
            monthly_limit_microusd: Some(1_000_000),
            lifetime_limit_microusd: None,
        },
    )
    .unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            machine
                .scope(async {
                    let session = ConservativeLlmBudget::new(0.6).unwrap();
                    session
                        .scope(async {
                            let options = opts(0.6);
                            let request = LlmRequestPayload::from(&options);
                            let reservation = reserve(&options, &request).unwrap().unwrap();
                            assert!(machine_receipt().unwrap().is_some());
                            drop(reservation);
                            tokio::task::yield_now().await;
                            assert!(machine_receipt().unwrap().is_some());
                        })
                        .await
                        .unwrap();
                })
                .await
                .unwrap();
        });
    assert!(machine.receipt().unwrap().reserved_microusd > 0);
    assert!(machine_receipt().unwrap().is_none());
}

#[test]
fn registered_self_hosted_route_is_known_zero_under_machine_quota() {
    swap_scope(AdmissionScope::default());
    let temp = tempfile::tempdir().unwrap();
    let machine = MachineSpendQuota::open(
        temp.path().join("spend.sqlite"),
        "person",
        MachineSpendPolicy {
            daily_limit_microusd: Some(0),
            monthly_limit_microusd: Some(0),
            lifetime_limit_microusd: None,
        },
    )
    .unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            machine
                .scope(async {
                    let mut options = base_opts("ollama");
                    options.provider = "ollama".into();
                    options.model = "any-locally-served-model".into();
                    let request = LlmRequestPayload::from(&options);
                    assert!(reserve(&options, &request).unwrap().is_none());
                })
                .await
                .unwrap();
        });
    assert_eq!(machine.receipt().unwrap().reserved_microusd, 0);
}

#[test]
fn machine_quota_refuses_late_activation_after_provider_admission() {
    swap_scope(AdmissionScope::default());
    let options = opts(0.6);
    let request = LlmRequestPayload::from(&options);
    drop(reserve(&options, &request).unwrap().unwrap());
    let temp = tempfile::tempdir().unwrap();
    let machine = MachineSpendQuota::open(
        temp.path().join("spend.sqlite"),
        "person",
        MachineSpendPolicy {
            daily_limit_microusd: Some(1_000_000),
            monthly_limit_microusd: Some(1_000_000),
            lifetime_limit_microusd: None,
        },
    )
    .unwrap();
    let outcome = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(machine.scope(async {}));
    assert!(outcome.is_err(), "prior attempt escaped machine accounting");
    swap_scope(AdmissionScope::default());
}

#[test]
fn native_decisions_draw_on_and_are_refused_by_the_machine_quota() {
    swap_scope(AdmissionScope::default());
    let temp = tempfile::tempdir().unwrap();
    let machine = MachineSpendQuota::open(
        temp.path().join("spend.sqlite"),
        "person",
        MachineSpendPolicy {
            daily_limit_microusd: Some(500_000),
            monthly_limit_microusd: Some(500_000),
            lifetime_limit_microusd: None,
        },
    )
    .unwrap();
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap()
        .block_on(async {
            machine
                .scope(async {
                    // The execution ceiling allows 0.6, but the durable
                    // machine allowance of 0.5 refuses it before transport.
                    assert!(reserve_decision(0.6, Some(1.0), Some(1.0)).is_err());
                    let hold = reserve_decision(0.4, Some(1.0), Some(1.0)).unwrap();
                    assert_eq!(machine.receipt().unwrap().reserved_microusd, 400_000);
                    // Complete usage releases only the proven-unused amount.
                    hold.settle(Some(0.1)).unwrap();
                    let receipt = machine.receipt().unwrap();
                    assert_eq!(receipt.reserved_microusd, 100_000);
                    assert_eq!(receipt.actual_known_microusd, 100_000);
                    // A downstream contract violation closes the durable scope.
                    reserve_decision(0.1, Some(1.0), Some(1.0))
                        .unwrap()
                        .retain_contract_violation()
                        .unwrap();
                })
                .await
                .unwrap();
        });
    assert!(machine.receipt().unwrap().contract_broken);
    swap_scope(AdmissionScope::default());
}
