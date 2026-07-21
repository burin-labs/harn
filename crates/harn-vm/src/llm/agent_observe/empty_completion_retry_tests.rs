//! Zero-token empty-completion retry coverage. A provider stall can end
//! with an empty HTTP 200 (observed live on OpenRouter: 133s hang,
//! `output_tokens=0`), which is not an error at the wire level —
//! `observed_llm_call` must treat it as a transient hiccup and retry.
//! Driven through `FakeLlmProvider` (an empty scripted stream produces
//! exactly the zero-token empty shape) so the full retry loop runs
//! without network I/O. Simulators intentionally preserve their scripted
//! terminal empty result; live routes convert it to typed exhaustion.

use super::*;
use crate::llm::fake::{
    fake_llm_captured_calls, install_fake_llm_script, FakeLlmEvent, FakeLlmScript, FakeLlmTurn,
    FakeStopReason,
};
use crate::llm::trace::{peek_agent_trace, reset_agent_trace_state, AgentTraceEvent};

fn fake_opts() -> crate::llm::api::LlmCallOptions {
    let mut opts = crate::llm::api::options::base_opts("fake");
    opts.model = "fake-stream".to_string();
    opts.native_tools = None;
    opts.tools = None;
    opts.tool_choice = None;
    opts.provider_overrides = None;
    opts
}

fn current_thread_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
}

fn provider_call_errors(transcript_dir: &std::path::Path) -> Vec<serde_json::Value> {
    std::fs::read_to_string(transcript_dir.join("llm_transcript.jsonl"))
        .expect("provider error transcript")
        .lines()
        .map(|line| serde_json::from_str(line).expect("valid transcript JSON"))
        .filter(|event: &serde_json::Value| event["type"] == "provider_call_error")
        .collect()
}

fn empty_turn() -> FakeLlmTurn {
    FakeLlmTurn::stream(vec![FakeLlmEvent::Done(FakeStopReason::EndTurn)])
}

/// The live cheap-model failure: the turn narrates an intended tool call in
/// its text but finishes with a provider error (`stop_reason == "error"`)
/// and emits ZERO tool calls. Non-empty text means the zero-token predicate
/// misses it, yet the loop has no action to run.
fn errored_actionless_turn() -> FakeLlmTurn {
    FakeLlmTurn::stream(vec![
        FakeLlmEvent::Token("We need to make edit to create tests/foo_test.cpp".into()),
        FakeLlmEvent::Done(FakeStopReason::Custom("error".into())),
    ])
}

/// The *thrown* billed-noncommittal failure. The response/transport parsers
/// raise `billed_noncommittal_completion_error` when a clean-finish turn
/// bills output but commits no tool call or visible text (the action went
/// only to the reasoning channel). The fake provider can't drive the real parser,
/// so we re-create the exact wire-error message under a *non-transient*
/// `Generic` category — proving the retry comes from the empty-completion
/// budget, not from `is_retryable_llm_error` (which returns false here).
fn billed_noncommittal_turn() -> FakeLlmTurn {
    FakeLlmTurn::Error(crate::llm::fake::FakeLlmError::new(
        crate::value::ErrorCategory::Generic,
        "provider openrouter model qwen/qwen3.6-35b-a3b returned billed output \
             (completion_tokens=342) with no dispatchable tool call or answer \
             (upstream contract violation): the model finished cleanly but committed \
             neither a tool call nor visible text.",
    ))
}

#[test]
fn empty_completion_retries_then_succeeds_on_second_attempt() {
    current_thread_runtime().block_on(async {
        reset_agent_trace_state();
        let transcript_dir = tempfile::tempdir().expect("transcript tempdir");
        push_llm_transcript_dir(transcript_dir.path().to_str().expect("utf8 tempdir"));
        let _guard = install_fake_llm_script(FakeLlmScript::new().push(empty_turn()).push(
            FakeLlmTurn::stream(vec![
                FakeLlmEvent::Token("recovered".into()),
                FakeLlmEvent::Done(FakeStopReason::EndTurn),
            ]),
        ));
        let result = observed_llm_call(&fake_opts(), None, None, None, false, false, None, None)
            .await
            .expect("empty completion retry should recover");
        pop_llm_transcript_dir();
        assert_eq!(result.text, "recovered");

        let retries: Vec<(usize, String, String, String)> = peek_agent_trace()
            .iter()
            .filter_map(|event| match event {
                AgentTraceEvent::EmptyCompletionRetry {
                    attempt,
                    provider,
                    model,
                    reason,
                    ..
                } => Some((*attempt, provider.clone(), model.clone(), reason.clone())),
                _ => None,
            })
            .collect();
        assert_eq!(
            retries,
            vec![(
                1,
                "fake".to_string(),
                "fake-stream".to_string(),
                "empty_generation".to_string(),
            )],
            "retry receipt must identify the exact route and failure class"
        );
        let transcript =
            std::fs::read_to_string(transcript_dir.path().join("llm_transcript.jsonl"))
                .expect("retry receipt");
        let receipts: Vec<serde_json::Value> = transcript
            .lines()
            .map(|line| serde_json::from_str(line).expect("valid receipt JSON"))
            .filter(|event: &serde_json::Value| event["type"] == "empty_completion_retry")
            .collect();
        assert_eq!(receipts.len(), 1);
        assert_eq!(receipts[0]["schema"], "harn.llm.empty_completion_retry.v1");
        assert_eq!(receipts[0]["provider"], "fake");
        assert_eq!(receipts[0]["model"], "fake-stream");
        assert_eq!(receipts[0]["reason"], "empty_generation");
        assert!(receipts[0]["duration_ms"].is_u64());
        reset_agent_trace_state();
        // _guard drop asserts both scripted turns were consumed.
    });
}

#[test]
fn errored_actionless_completion_retries_then_succeeds() {
    // An errored turn that narrated intent but emitted no tool call must be
    // RETRIED (not advanced on); the next good turn proceeds.
    current_thread_runtime().block_on(async {
        reset_agent_trace_state();
        let _guard =
            install_fake_llm_script(FakeLlmScript::new().push(errored_actionless_turn()).push(
                FakeLlmTurn::stream(vec![
                    FakeLlmEvent::Token("recovered".into()),
                    FakeLlmEvent::Done(FakeStopReason::EndTurn),
                ]),
            ));
        let result = observed_llm_call(&fake_opts(), None, None, None, false, false, None, None)
            .await
            .expect("errored-actionless retry should recover");
        assert_eq!(result.text, "recovered");

        let retries: Vec<usize> = peek_agent_trace()
            .iter()
            .filter_map(|event| match event {
                AgentTraceEvent::EmptyCompletionRetry { attempt, .. } => Some(*attempt),
                _ => None,
            })
            .collect();
        assert_eq!(retries, vec![1], "expected exactly one retry trace event");
        reset_agent_trace_state();
        // _guard drop asserts both scripted turns were consumed.
    });
}

#[test]
fn errored_actionless_completion_returns_unchanged_after_budget_exhausted() {
    // Retries stay bounded: once the budget is spent, the errored turn is
    // returned unchanged (callers see today's shape, not a novel error).
    current_thread_runtime().block_on(async {
        reset_agent_trace_state();
        let _guard = install_fake_llm_script(
            FakeLlmScript::new()
                .push(errored_actionless_turn())
                .push(errored_actionless_turn()),
        );
        let result = observed_llm_call(&fake_opts(), None, None, None, false, false, None, None)
            .await
            .expect("exhausted retries must return Ok, not a new error");
        assert!(result.tool_calls.is_empty());
        assert_eq!(result.stop_reason.as_deref(), Some("error"));

        let retries = peek_agent_trace()
            .iter()
            .filter(|event| matches!(event, AgentTraceEvent::EmptyCompletionRetry { .. }))
            .count();
        assert_eq!(retries, 1, "exactly one retry before the budget is spent");
        reset_agent_trace_state();
    });
}

#[test]
fn empty_completion_returns_result_unchanged_after_budget_exhausted() {
    current_thread_runtime().block_on(async {
        reset_agent_trace_state();
        let _guard =
            install_fake_llm_script(FakeLlmScript::new().push(empty_turn()).push(empty_turn()));
        let result = observed_llm_call(&fake_opts(), None, None, None, false, false, None, None)
            .await
            .expect("exhausted empty-completion retries must return Ok, not a new error");
        assert!(result.text.is_empty());
        assert!(result.tool_calls.is_empty());
        assert_eq!(result.output_tokens, 0);
        reset_agent_trace_state();
    });
}

#[test]
fn billed_noncommittal_completion_retries_then_succeeds() {
    // The KEY missing case: a *thrown* billed-noncommittal turn (clean
    // finish, billed output, tool call only on the reasoning channel) must
    // be RETRIED within the empty-completion budget — not hard-break the
    // loop as a silent `provider_error` — and the next good turn proceeds.
    current_thread_runtime().block_on(async {
        reset_agent_trace_state();
        let _guard =
            install_fake_llm_script(FakeLlmScript::new().push(billed_noncommittal_turn()).push(
                FakeLlmTurn::stream(vec![
                    FakeLlmEvent::Token("recovered".into()),
                    FakeLlmEvent::Done(FakeStopReason::EndTurn),
                ]),
            ));
        let result = observed_llm_call(&fake_opts(), None, None, None, false, false, None, None)
            .await
            .expect("billed-noncommittal retry should recover");
        assert_eq!(result.text, "recovered");

        let retries: Vec<usize> = peek_agent_trace()
            .iter()
            .filter_map(|event| match event {
                AgentTraceEvent::EmptyCompletionRetry { attempt, .. } => Some(*attempt),
                _ => None,
            })
            .collect();
        assert_eq!(
            retries,
            vec![1],
            "expected exactly one EmptyCompletionRetry trace event"
        );
        reset_agent_trace_state();
        // _guard drop asserts both scripted turns were consumed.
    });
}

#[test]
fn billed_noncommittal_completion_surfaces_contract_violation_after_budget_exhausted() {
    // Bounded retry on a chronically-broken upstream: once the budget is
    // spent the LOUD thrown error is surfaced unchanged (NOT a silent
    // advance), and it still names the `upstream contract violation` so the
    // a host eval layer can classify it as infra, not capability.
    current_thread_runtime().block_on(async {
        reset_agent_trace_state();
        let transcript_dir = tempfile::tempdir().expect("transcript tempdir");
        push_llm_transcript_dir(transcript_dir.path().to_str().expect("utf8 tempdir"));
        let _guard = install_fake_llm_script(
            FakeLlmScript::new()
                .push(billed_noncommittal_turn())
                .push(billed_noncommittal_turn()),
        );
        let err = observed_llm_call(&fake_opts(), None, None, None, false, false, None, None)
            .await
            .expect_err("exhausted billed-noncommittal retries must surface the loud error");
        pop_llm_transcript_dir();
        let message = err.to_string();
        assert!(
            message.contains("upstream contract violation"),
            "exhausted-path error must stay tagged as a provider contract violation: {message}"
        );
        assert!(
            message.contains("completion_tokens="),
            "exhausted-path error must keep the billed-output signature: {message}"
        );

        let retries = peek_agent_trace()
            .iter()
            .filter(|event| matches!(event, AgentTraceEvent::EmptyCompletionRetry { .. }))
            .count();
        assert_eq!(retries, 1, "exactly one retry before the budget is spent");
        let errors = provider_call_errors(transcript_dir.path());
        assert_eq!(errors.len(), 2);
        assert_eq!(errors[0]["attempt"], 1);
        assert_eq!(errors[0]["status"], "retrying");
        assert_eq!(errors[0]["retryable"], true);
        assert_eq!(errors[1]["attempt"], 2);
        assert_eq!(errors[1]["status"], "retries_exhausted");
        assert_eq!(errors[1]["retryable"], false);
        reset_agent_trace_state();
    });
}

#[test]
fn rate_limit_429_fails_fast() {
    // Transient errors have NO in-call retry budget: a 429 (even with a
    // Retry-After hint) surfaces immediately. Retry policy is composed via
    // `with_retry` in `std/llm/handlers`. Only one turn is scripted, so a
    // hidden retry would panic the guard on drop.
    current_thread_runtime().block_on(async {
        let _guard = install_fake_llm_script(
            FakeLlmScript::new().push(FakeLlmTurn::Error(
                crate::llm::fake::FakeLlmError::new(
                    crate::value::ErrorCategory::RateLimit,
                    "429 too many requests",
                )
                .with_retry_after_ms(10),
            )),
        );
        let err = observed_llm_call(&fake_opts(), None, None, None, false, false, None, None)
            .await
            .expect_err("429 must surface immediately (no in-call transient retry)");
        assert!(
            is_retryable_llm_error(&err),
            "the surfaced 429 should classify as retryable"
        );
    });
}

fn native_opts() -> crate::llm::api::LlmCallOptions {
    let mut opts = fake_opts();
    opts.native_tools = Some(vec![serde_json::json!({"name": "edit"})]);
    opts.tools = Some(crate::value::VmValue::Nil);
    opts
}

#[test]
fn native_tool_channel_failure_degrades_to_text_and_recovers() {
    // The runtime tool_format fallback: a native-pinned tool call whose
    // server-side parser 500/EOFs degrades to the text channel and recovers
    // on the retry. Crucially this needs no transient-retry budget —
    // retrying native would re-feed the broken parser forever; the
    // productive move is to switch channels. The _guard drop asserts the
    // second (text-channel) turn was actually consumed.
    current_thread_runtime().block_on(async {
        let _guard = install_fake_llm_script(
            FakeLlmScript::new()
                .push(FakeLlmTurn::Error(crate::llm::fake::FakeLlmError::new(
                    crate::value::ErrorCategory::ServerError,
                    "[http_error] ollama 500: tool call parser hit unexpected EOF \
                         while parsing tool_calls",
                )))
                .push(FakeLlmTurn::stream(vec![
                    FakeLlmEvent::Token(
                        "<tool_call>\nedit({ path: \"a.rs\" })\n</tool_call>".into(),
                    ),
                    FakeLlmEvent::Done(FakeStopReason::EndTurn),
                ])),
        );
        let result = observed_llm_call(
            &native_opts(),
            Some("native"),
            None,
            None,
            false,
            false,
            None,
            None,
        )
        .await
        .expect("native tool-channel failure should degrade to text and recover");
        assert!(
            result.text.contains("edit({ path: \"a.rs\" })"),
            "the degraded text-channel turn should be returned"
        );
    });
}

#[test]
fn native_tool_channel_failure_degrade_fires_at_most_once() {
    // The degrade is one-shot per call: if the text-channel retry ALSO
    // fails (a different problem), the loop does not keep re-degrading.
    // With two failing turns scripted, only the FIRST degrade may fire;
    // the second failure must surface as a hard error. Exactly two
    // turns are scripted, so a third call would panic the guard on drop.
    current_thread_runtime().block_on(async {
        let _guard = install_fake_llm_script(
            FakeLlmScript::new()
                .push(FakeLlmTurn::Error(crate::llm::fake::FakeLlmError::new(
                    crate::value::ErrorCategory::ServerError,
                    "[http_error] 500: tool call parser unexpected EOF",
                )))
                .push(FakeLlmTurn::Error(crate::llm::fake::FakeLlmError::new(
                    crate::value::ErrorCategory::ServerError,
                    "[http_error] 500: tool call parser unexpected EOF",
                ))),
        );
        let err = observed_llm_call(
            &native_opts(),
            Some("native"),
            None,
            None,
            false,
            false,
            None,
            None,
        )
        .await
        .expect_err("a second tool-channel failure after degrade must surface");
        assert!(
            err.to_string().to_lowercase().contains("tool call parser"),
            "the surfaced error is the second (post-degrade) failure"
        );
    });
}

#[test]
fn stream_body_failure_degrades_to_non_streaming_and_recovers() {
    // A provider that accepts the request and then fails while reading the
    // streaming body should not keep retrying the identical SSE path. If the
    // route does not require streaming, retry once through the ordinary
    // request/response transport. The fake provider records the forwarded
    // request, so this test verifies the second call actually carries
    // `stream=false`.
    current_thread_runtime().block_on(async {
        let mut opts = fake_opts();
        opts.stream = true;
        let _guard = install_fake_llm_script(
            FakeLlmScript::new()
                .push(FakeLlmTurn::Error(crate::llm::fake::FakeLlmError::new(
                    crate::value::ErrorCategory::TransientNetwork,
                    "llamacpp stream error (mid-stream read): error decoding response body",
                )))
                .push(FakeLlmTurn::stream(vec![
                    FakeLlmEvent::Token("recovered".into()),
                    FakeLlmEvent::Done(FakeStopReason::EndTurn),
                ])),
        );
        let result = observed_llm_call(&opts, None, None, None, false, false, None, None)
            .await
            .expect("stream body failure should degrade transport and recover");
        assert_eq!(result.text, "recovered");

        let calls = fake_llm_captured_calls();
        assert_eq!(calls.len(), 2, "expected one degraded retry");
        assert!(calls[0].stream, "first call should use streaming");
        assert!(
            !calls[1].stream,
            "degraded retry should use non-streaming transport"
        );
    });
}

#[test]
fn stream_transport_degrade_fires_at_most_once() {
    current_thread_runtime().block_on(async {
        let mut opts = fake_opts();
        opts.stream = true;
        let _guard = install_fake_llm_script(
            FakeLlmScript::new()
                .push(FakeLlmTurn::Error(crate::llm::fake::FakeLlmError::new(
                    crate::value::ErrorCategory::TransientNetwork,
                    "llamacpp stream error (mid-stream read): error decoding response body",
                )))
                .push(FakeLlmTurn::Error(crate::llm::fake::FakeLlmError::new(
                    crate::value::ErrorCategory::TransientNetwork,
                    "llamacpp stream error (mid-stream read): error decoding response body",
                ))),
        );
        let err = observed_llm_call(&opts, None, None, None, false, false, None, None)
            .await
            .expect_err("second transport failure after degrade must surface");
        assert!(
            err.to_string().contains("stream error"),
            "surface the post-degrade provider error, got: {err}"
        );

        let calls = fake_llm_captured_calls();
        assert_eq!(calls.len(), 2, "degrade should be one-shot");
        assert!(calls[0].stream);
        assert!(!calls[1].stream);
    });
}

#[test]
fn billed_noncommittal_throw_degrades_to_text_and_recovers() {
    // Mechanism-fitness: the canonical cheap-model vanishing-call signature
    // (billed output, clean finish, zero tool calls — the action stranded in
    // the reasoning channel) on a NATIVE channel must degrade to text and
    // recover, NOT loop re-feeding the broken native channel. Before this
    // generalization the throw routed onto the bounded SAME-CHANNEL empty-
    // completion retry and never switched channels. The _guard drop asserts
    // the second (text-channel) turn was actually consumed.
    current_thread_runtime().block_on(async {
        let _guard = install_fake_llm_script(
            FakeLlmScript::new()
                .push(FakeLlmTurn::Error(crate::llm::fake::FakeLlmError::new(
                    crate::value::ErrorCategory::Generic,
                    "provider deepinfra model openai/gpt-oss-120b returned billed \
                         output (completion_tokens=86) with no dispatchable tool call \
                         or answer (upstream contract violation): the model finished \
                         cleanly but committed neither a tool call nor visible text.",
                )))
                .push(FakeLlmTurn::stream(vec![
                    FakeLlmEvent::Token(
                        "<tool_call>\nedit({ path: \"a.rs\" })\n</tool_call>".into(),
                    ),
                    FakeLlmEvent::Done(FakeStopReason::EndTurn),
                ])),
        );
        let result = observed_llm_call(
            &native_opts(),
            Some("native"),
            None,
            None,
            false,
            false,
            None,
            None,
        )
        .await
        .expect("billed-noncommittal vanish should degrade to text and recover");
        assert!(
            result.text.contains("edit({ path: \"a.rs\" })"),
            "the degraded text-channel turn should be returned"
        );
    });
}

#[test]
fn sambanova_function_call_refusal_degrades_to_text_and_recovers() {
    // Mechanism-fitness: a native function-call protocol refusal (the observed
    // SambaNova HTTP 400 "Model started a function call but did not complete
    // it") is a 4xx, so the #3500 5xx/EOF predicate deliberately misses it and
    // it is NOT a generic-retryable error — yet it is unambiguously a broken
    // native tool channel for the route. It must earn the one-shot degrade to
    // text rather than aborting the run (the sweep showed 6 such calls abort a
    // run today).
    current_thread_runtime().block_on(async {
        let _guard = install_fake_llm_script(
            FakeLlmScript::new()
                .push(FakeLlmTurn::Error(crate::llm::fake::FakeLlmError::new(
                    crate::value::ErrorCategory::Generic,
                    "sambanova HTTP 400 Bad Request [invalid_request]: Model \
                         started a function call but did not complete it.",
                )))
                .push(FakeLlmTurn::stream(vec![
                    FakeLlmEvent::Token(
                        "<tool_call>\nedit({ path: \"a.rs\" })\n</tool_call>".into(),
                    ),
                    FakeLlmEvent::Done(FakeStopReason::EndTurn),
                ])),
        );
        let result = observed_llm_call(
            &native_opts(),
            Some("native"),
            None,
            None,
            false,
            false,
            None,
            None,
        )
        .await
        .expect("native function-call refusal should degrade to text and recover");
        assert!(
            result.text.contains("edit({ path: \"a.rs\" })"),
            "the degraded text-channel turn should be returned"
        );
    });
}

#[test]
fn builtin_empty_retry_budget_excludes_mock_only() {
    assert_eq!(empty_completion_retry_budget("openrouter"), 1);
    assert_eq!(empty_completion_retry_budget("fake"), 1);
    assert_eq!(empty_completion_retry_budget("mock"), 0);
}

fn empty_result() -> crate::llm::api::LlmResult {
    crate::llm::api::LlmResult {
        text: String::new(),
        tool_calls: Vec::new(),
        raw_tool_calls: Vec::new(),
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cache_supported: true,
        model: "test-model".to_string(),
        provider: "openrouter".to_string(),
        thinking: None,
        thinking_summary: None,
        stop_reason: Some("stop".to_string()),
        served_fast: false,
        blocks: Vec::new(),
        logprobs: Vec::new(),
        telemetry: crate::llm::api::ProviderTelemetry::default(),
    }
}

#[test]
fn terminal_empty_completion_is_typed_and_failover_eligible() {
    let mut opts = fake_opts();
    opts.provider = "openrouter".to_string();
    let result = empty_result();

    let err = terminal_unproductive_completion_failover_error(&opts, &result, false, 2, None)
        .expect("live-provider exhausted empty should become failover-eligible");
    assert_eq!(
        crate::value::error_to_category(&err),
        crate::value::ErrorCategory::CircuitOpen
    );
    let VmError::Thrown(VmValue::Dict(fields)) = &err else {
        panic!("expected structured provider exhaustion, got {err:?}");
    };
    assert_eq!(
        fields.get("code").map(VmValue::display).as_deref(),
        Some("provider_exhausted")
    );
    assert_eq!(
        fields.get("reason").map(VmValue::display).as_deref(),
        Some("empty_generation")
    );
    let message = fields
        .get("message")
        .map(VmValue::display)
        .unwrap_or_default();
    assert!(message.contains("completion_tokens=0"));
    assert!(message.contains("delivered no content"));
    let Some(VmValue::List(attempts)) = fields.get("attempts") else {
        panic!("expected typed attempt chain");
    };
    assert_eq!(attempts.len(), 1);
    let attempt = attempts[0].as_dict().expect("attempt object");
    assert_eq!(
        attempt.get("provider").map(VmValue::display).as_deref(),
        Some("openrouter")
    );
    assert_eq!(
        attempt.get("attempt_count").and_then(VmValue::as_int),
        Some(2)
    );
    assert!(
        attempt.get("duration_ms").is_none(),
        "the pure classifier has no transport timer to invent"
    );

    opts.provider = "fake".to_string();
    assert!(
        terminal_unproductive_completion_failover_error(&opts, &result, false, 2, None).is_none()
    );
}

#[test]
fn live_terminal_empty_path_quarantines_the_route() {
    let _guard = crate::llm::env_guard();
    let mut opts = fake_opts();
    opts.provider = "empty-quarantine-live-path".to_string();
    opts.model = "empty-quarantine-model".to_string();
    let result = empty_result();

    for _ in 0..crate::llm::rate_limit::UNPRODUCTIVE_COMPLETION_BREAKER_THRESHOLD {
        terminal_unproductive_completion_failure(&opts, &result, false, 2, 17)
            .expect("terminal empty must be provider exhaustion");
    }

    let error = crate::llm::rate_limit::check_network_breaker_for_llm_call(&opts)
        .expect_err("the production terminal-empty path must quarantine its route");
    assert_eq!(
        crate::value::error_to_category(&error),
        crate::value::ErrorCategory::CircuitOpen
    );
}

#[test]
fn terminal_errored_actionless_completion_is_failover_eligible() {
    let mut opts = fake_opts();
    opts.provider = "openrouter".to_string();
    let mut result = empty_result();
    result.stop_reason = Some("error".to_string());
    result.text = "I need to edit tests/foo_test.cpp".to_string();
    result.output_tokens = 17;

    assert!(
        terminal_unproductive_completion_failover_error(&opts, &result, false, 2, None).is_none(),
        "non-empty actionless completions retain the existing throttle gate"
    );
    let message = terminal_unproductive_completion_failover_error(&opts, &result, true, 2, None)
        .expect("throttled-provider actionless error should fail over")
        .to_string();
    assert!(message.contains("circuit_open"));
    assert!(message.contains("completion_tokens=17"));
    assert!(message.contains("no dispatchable tool call"));
    assert!(
        !message.contains("delivered no content, thinking"),
        "non-empty text should not be mislabeled as a zero-content completion"
    );
}

#[test]
fn empty_unproductive_completion_predicate_edges() {
    assert!(is_empty_unproductive_completion(&empty_result()));

    // Token-cap truncation is deterministic — not a retryable hiccup.
    let mut truncated = empty_result();
    truncated.stop_reason = Some("length".to_string());
    assert!(!is_empty_unproductive_completion(&truncated));
    let mut truncated_upper = empty_result();
    truncated_upper.stop_reason = Some("MAX_TOKENS".to_string());
    assert!(!is_empty_unproductive_completion(&truncated_upper));

    // Real visible content, thinking, a tool call, or a server-side
    // tool-search block all disqualify — the loop has something to act on.
    let mut with_text = empty_result();
    with_text.text = "hi".to_string();
    assert!(!is_empty_unproductive_completion(&with_text));
    let mut with_thinking = empty_result();
    with_thinking.thinking = Some("hmm".to_string());
    assert!(!is_empty_unproductive_completion(&with_thinking));
    let mut with_tool_call = empty_result();
    with_tool_call.tool_calls = vec![serde_json::json!({"id": "t1", "name": "look"})];
    assert!(!is_empty_unproductive_completion(&with_tool_call));
    let mut with_tool_search = empty_result();
    with_tool_search.blocks = vec![serde_json::json!({"type": "tool_search_query"})];
    assert!(!is_empty_unproductive_completion(&with_tool_search));

    // harn#4744: billed tokens with no usable content — a whitespace-only or
    // echoed-stop-sequence completion — is unproductive, not a real serve.
    // The old `output_tokens == 0` gate let these slip through and book as
    // served with no retry.
    let mut billed_empty = empty_result();
    billed_empty.output_tokens = 3;
    assert!(is_empty_unproductive_completion(&billed_empty));
    let mut whitespace_only = empty_result();
    whitespace_only.text = "  \n\t".to_string();
    whitespace_only.output_tokens = 5;
    assert!(is_empty_unproductive_completion(&whitespace_only));
}

#[test]
fn errored_actionless_completion_predicate_edges() {
    // The live failure shape: stop_reason=error, the model narrated an
    // intended tool call in its text but emitted ZERO tool calls. The
    // zero-token predicate misses it (non-empty text), but it IS retryable.
    let mut narrated = empty_result();
    narrated.stop_reason = Some("error".to_string());
    narrated.text = "We need to make edit to create tests/foo_test.cpp".to_string();
    narrated.output_tokens = 42;
    assert!(is_errored_actionless_completion(&narrated));
    assert!(!is_empty_unproductive_completion(&narrated));
    assert!(is_retryable_unproductive_completion(&narrated));

    // Case-insensitive on the stop reason.
    let mut upper = narrated.clone();
    upper.stop_reason = Some("ERROR".to_string());
    assert!(is_errored_actionless_completion(&upper));

    // A clean finish is not the errored-actionless case (it may still be a
    // genuine answer or a billed-noncommittal parse error — not ours).
    let mut clean = narrated.clone();
    clean.stop_reason = Some("stop".to_string());
    assert!(!is_errored_actionless_completion(&clean));

    // An errored turn that STILL dispatched a tool call has real work — the
    // loop must not treat it as actionless.
    let mut errored_with_call = narrated.clone();
    errored_with_call.tool_calls = vec![serde_json::json!({"id": "t1", "name": "edit"})];
    assert!(!is_errored_actionless_completion(&errored_with_call));

    // An errored turn whose only activity was a server-side tool search is
    // not actionless either (no dispatchable call, but the search counts).
    let mut errored_with_search = narrated;
    errored_with_search.blocks = vec![serde_json::json!({"type": "tool_search_query"})];
    assert!(errored_with_search.tool_calls.is_empty());
    assert!(!is_errored_actionless_completion(&errored_with_search));

    // The zero-token empty completion still flows through the unified
    // predicate.
    assert!(is_retryable_unproductive_completion(&empty_result()));
}
