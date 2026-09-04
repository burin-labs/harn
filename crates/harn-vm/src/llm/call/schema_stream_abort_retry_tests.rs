//! Integration coverage for treating early streaming schema aborts as
//! ordinary schema retry failures. The tests drive the retry loop through
//! the in-process `FakeLlmProvider` so we can script:
//!
//! 1. an attempt that emits schema-violating tokens mid-stream
//!    (triggers the abort, fires a `SchemaStreamAborted` event, and
//!    consumes one retry budget slot), and
//! 2. a follow-up attempt that emits a conforming JSON document
//!    (the loop accepts it as the final answer).
//!
//! The corrective `SchemaRetry` event surfaces the abort path /
//! reason verbatim, so callers see why the retry happened rather
//! than a generic stream failure.

use super::*;
use crate::llm::fake::{
    install_fake_llm_script, FakeLlmEvent, FakeLlmScript, FakeLlmTurn, FakeStopReason,
};
use crate::llm::trace::{peek_agent_trace, reset_agent_trace_state, AgentTraceEvent};

fn options_with_retries(retries: i64) -> crate::value::DictMap {
    let mut opts = crate::value::DictMap::new();
    opts.insert(
        crate::value::intern_key("schema_retries"),
        VmValue::Int(retries),
    );
    opts
}

fn schema() -> serde_json::Value {
    serde_json::json!({
        "type": "object",
        "required": ["age"],
        "properties": {"age": {"type": "integer"}}
    })
}

fn fake_opts_with_schema() -> api::LlmCallOptions {
    let mut opts = api::options::base_opts("fake");
    opts.model = "fake-stream".to_string();
    opts.output_schema = Some(schema());
    opts.output_format = api::OutputFormat::JsonSchema {
        schema: schema(),
        strict: true,
    };
    opts.output_validation = Some("error".to_string());
    opts.schema_stream_abort = true;
    opts.native_tools = None;
    opts.tools = None;
    opts.tool_choice = None;
    opts.provider_overrides = None;
    opts
}

fn fake_routing_policy() -> Arc<routing::RoutingPolicyConfig> {
    routing::clear_policy_registry();
    let chain = VmValue::List(std::sync::Arc::new(vec![VmValue::Dict(
        std::sync::Arc::new(crate::value::DictMap::from_iter([
            (
                crate::value::intern_key("provider"),
                VmValue::String(arcstr::ArcStr::from("fake")),
            ),
            (
                crate::value::intern_key("model"),
                VmValue::String(arcstr::ArcStr::from("fake-stream")),
            ),
        ])),
    )]));
    let tagged = routing::build_routing_policy(&crate::value::DictMap::from_iter([(
        crate::value::intern_key("chain"),
        chain,
    )]))
    .expect("routing policy validates");
    let options = crate::value::DictMap::from_iter([(crate::value::intern_key("routing"), tagged)]);
    routing::extract_routing_policy(Some(&options))
        .expect("routing policy extracts")
        .expect("routing policy present")
}

fn drain_deltas(rx: &mut tokio::sync::mpsc::UnboundedReceiver<String>) -> Vec<String> {
    let mut deltas = Vec::new();
    while let Ok(delta) = rx.try_recv() {
        deltas.push(delta);
    }
    deltas
}

// Regression: a truncated structured-output response must be reported as a
// token-limit hit regardless of provider stop_reason spelling. Gemini /
// Vertex pass `MAX_TOKENS` (uppercase) through unnormalized; the previous
// case-sensitive `matches!(.., "length" | "max_tokens")` missed it and
// mislabeled the failure as "did not contain parseable JSON".
#[test]
fn structured_output_truncation_detected_case_insensitively() {
    let opts = api::options::base_opts("fake");
    for spelling in ["max_tokens", "MAX_TOKENS", "length", "LENGTH"] {
        let dict = VmValue::dict(crate::value::DictMap::from_iter([(
            crate::value::intern_key("stop_reason"),
            VmValue::String(arcstr::ArcStr::from(spelling)),
        )]));
        let errors = structured_output_errors(&dict, &opts);
        assert!(
            errors.iter().any(|e| e.contains("hit the token limit")),
            "spelling {spelling:?} should be flagged as truncation, got: {errors:?}"
        );
    }
    // A non-truncation stop_reason must NOT add the token-limit error.
    let dict = VmValue::dict(crate::value::DictMap::from_iter([(
        crate::value::intern_key("stop_reason"),
        VmValue::String(arcstr::ArcStr::from("stop")),
    )]));
    let errors = structured_output_errors(&dict, &opts);
    assert!(
        !errors.iter().any(|e| e.contains("hit the token limit")),
        "non-truncation stop must not add token-limit error, got: {errors:?}"
    );
}

#[test]
fn in_flight_llm_guard_snapshots_and_clears() {
    clear_in_flight_llm_calls();
    let mut opts = fake_opts_with_schema();
    opts.messages = vec![serde_json::json!({"role": "assistant", "content": "thinking"})];

    let guard = InFlightLlmCallGuard::enter(&opts);
    let calls = snapshot_in_flight_llm_calls();
    assert_eq!(calls.len(), 1);
    assert_eq!(calls[0]["model"], "fake-stream");
    assert_eq!(calls[0]["role"], "assistant");
    assert!(
        calls[0]["age_ms"].as_i64().unwrap_or(-1) >= 0,
        "age must be a non-negative duration"
    );

    drop(guard);
    assert!(snapshot_in_flight_llm_calls().is_empty());
}

#[test]
fn mid_stream_abort_consumes_one_retry_then_recovers() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        reset_agent_trace_state();

        // Turn 1: stream a partial doc that violates `age: int`.
        // Turn 2: stream a valid doc.
        let _script_guard = install_fake_llm_script(
            FakeLlmScript::new()
                .push(FakeLlmTurn::stream(vec![
                    FakeLlmEvent::Token("{\"age\": ".into()),
                    FakeLlmEvent::Token("\"twenty".into()),
                    // Done isn't reached — the abort returns Err before
                    // the validator sees this chunk.
                    FakeLlmEvent::Token("\"}".into()),
                    FakeLlmEvent::Done(FakeStopReason::EndTurn),
                ]))
                .push(FakeLlmTurn::stream(vec![
                    FakeLlmEvent::Token("{\"age\": 20}".into()),
                    FakeLlmEvent::Done(FakeStopReason::EndTurn),
                ])),
        );

        let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let opts = fake_opts_with_schema();
        let outcome = execute_schema_retry_loop(
            None,
            opts,
            Some(options_with_retries(2)),
            None,
            Some(delta_tx),
        )
        .await
        .expect("retry loop runs cleanly");

        assert_eq!(outcome.attempts, 2, "expected the recovery to run twice");
        let deltas = drain_deltas(&mut delta_rx);
        assert_eq!(
            deltas,
            vec![
                "{\"age\": ".to_string(),
                "\"twenty".to_string(),
                "{\"age\": 20}".to_string()
            ],
            "stream sink should receive the aborting attempt and the recovery attempt"
        );
        assert!(
            outcome.errors.is_empty(),
            "final attempt must validate cleanly; got {:?}",
            outcome.errors
        );

        // The result envelope carries the validated data on the second
        // turn (post-loop, dict-shaped).
        match &outcome.vm_result {
            VmValue::Dict(d) => {
                let data = d.get("data").cloned().unwrap_or(VmValue::Nil);
                match data {
                    VmValue::Dict(inner) => match inner.get("age") {
                        Some(VmValue::Int(n)) => assert_eq!(*n, 20),
                        other => panic!("expected age=20; got {other:?}"),
                    },
                    other => panic!("expected validated dict; got {other:?}"),
                }
            }
            other => panic!("expected dict result; got {other:?}"),
        }

        // Transcript events: exactly one SchemaStreamAborted, exactly one
        // SchemaRetry whose `errors` includes the abort path.
        let events = peek_agent_trace();
        let aborts: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AgentTraceEvent::SchemaStreamAborted { path, reason, .. } => {
                    Some((path.clone(), reason.clone()))
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(
            aborts.len(),
            1,
            "expected one SchemaStreamAborted; got {events:#?}"
        );
        assert_eq!(aborts[0].0, "$.age");

        let retries: Vec<_> = events
            .iter()
            .filter_map(|e| match e {
                AgentTraceEvent::SchemaRetry { errors, .. } => Some(errors.clone()),
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(retries.len(), 1, "expected one SchemaRetry event");
        assert!(
            retries[0].iter().any(|err| err.contains("$.age")),
            "retry nudge should cite the abort path; got {:?}",
            retries[0]
        );

        reset_agent_trace_state();
    });
}

#[test]
fn routed_call_uses_schema_retry_loop() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        reset_agent_trace_state();

        let _script_guard = install_fake_llm_script(
            FakeLlmScript::new()
                .push(FakeLlmTurn::stream(vec![
                    FakeLlmEvent::Token("{\"age\":\"twenty\"}".into()),
                    FakeLlmEvent::Done(FakeStopReason::EndTurn),
                ]))
                .push(FakeLlmTurn::stream(vec![
                    FakeLlmEvent::Token("{\"age\":20}".into()),
                    FakeLlmEvent::Done(FakeStopReason::EndTurn),
                ])),
        );

        let (delta_tx, mut delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        let mut opts = fake_opts_with_schema();
        opts.routing_policy = Some(fake_routing_policy());
        let result = execute_llm_call(
            None,
            opts,
            Some(options_with_retries(1)),
            None,
            Some(delta_tx),
        )
        .await
        .expect("routed schema retry should recover");

        let dict = result.as_dict().expect("result dict");
        let deltas = drain_deltas(&mut delta_rx);
        assert_eq!(
            deltas,
            vec![
                "{\"age\":\"twenty\"}".to_string(),
                "{\"age\":20}".to_string()
            ],
            "routed calls should stream through the llm_call wrapper path"
        );
        let data = dict.get("data").expect("validated data");
        let data = data.as_dict().expect("validated data dict");
        match data.get("age") {
            Some(VmValue::Int(age)) => assert_eq!(*age, 20),
            other => panic!("expected age=20, got {other:?}"),
        }
        assert!(
            dict.contains_key("routing"),
            "routed result should preserve routing diagnostics"
        );
        let retries = peek_agent_trace()
            .iter()
            .filter(|event| matches!(event, AgentTraceEvent::SchemaRetry { .. }))
            .count();
        assert_eq!(retries, 1);

        reset_agent_trace_state();
    });
}

#[test]
fn opt_out_lets_invalid_stream_run_to_completion() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        reset_agent_trace_state();

        let _script_guard = install_fake_llm_script(FakeLlmScript::streaming(vec![
            FakeLlmEvent::Token("{\"age\":".into()),
            FakeLlmEvent::Token("\"twenty\"}".into()),
            FakeLlmEvent::Done(FakeStopReason::EndTurn),
        ]));

        let mut opts = fake_opts_with_schema();
        opts.schema_stream_abort = false;
        let outcome =
            execute_schema_retry_loop(None, opts, Some(options_with_retries(0)), None, None)
                .await
                .expect("retry loop completes");

        // No mid-stream abort fired; the stream ran to completion and
        // the schema validator caught the failure post-hoc instead.
        let events = peek_agent_trace();
        assert!(
            !events
                .iter()
                .any(|e| matches!(e, AgentTraceEvent::SchemaStreamAborted { .. })),
            "abort must not fire when opted out; got {events:#?}"
        );
        assert!(
            !outcome.errors.is_empty(),
            "post-hoc validation should still flag the malformed response"
        );

        reset_agent_trace_state();
    });
}

/// A call whose every attempt is severed mid-stream still consumed
/// provider supply, and the ledger has to say so.
///
/// Before this was fixed the aborting attempts were dropped from `usages`
/// entirely and the stand-in envelope carried top-level `input_tokens: 0`
/// and `output_tokens: 0` with no `usage` block at all, so a severed call
/// read back as a real measurement of zero. Downstream that priced the
/// trial at nothing and then skipped it as accounting-incomplete. Each
/// assertion below fails on that shape.
#[test]
fn exhausted_stream_aborts_are_unpriced_requests_not_a_measured_zero() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime");
    runtime.block_on(async {
        reset_agent_trace_state();

        // Both attempts violate the schema mid-stream, so the retry
        // budget is spent and the caller sees the abort stand-in.
        let aborting_turn = || {
            FakeLlmTurn::stream(vec![
                FakeLlmEvent::Token("{\"age\": ".into()),
                FakeLlmEvent::Token("\"twenty\"}".into()),
                FakeLlmEvent::Done(FakeStopReason::EndTurn),
            ])
        };
        let _script_guard = install_fake_llm_script(
            FakeLlmScript::new()
                .push(aborting_turn())
                .push(aborting_turn()),
        );

        let outcome = execute_schema_retry_loop(
            None,
            fake_opts_with_schema(),
            Some(options_with_retries(1)),
            None,
            None,
        )
        .await
        .expect("retry loop returns the exhausted outcome");

        assert_eq!(outcome.attempts, 2, "both attempts should have run");
        assert!(
            !outcome.errors.is_empty(),
            "an exhausted abort surfaces as a schema failure"
        );

        assert_eq!(
            outcome.usages.len(),
            2,
            "each severed provider request stays in the ledger; got {:?}",
            outcome.usages
        );
        let ledger = crate::llm::usage::LlmUsage::aggregate(&outcome.usages);
        assert_eq!(
            ledger.accounting_status,
            crate::llm::usage::UsageAccountingStatus::Unknown,
            "no usage frame arrived, so the accounting is unknown, not reported"
        );
        assert_eq!(ledger.provider_call_count, 2);
        assert_eq!(ledger.usage_unknown_calls, 2);
        assert_eq!(ledger.unpriced_calls, 2);
        assert_eq!(
            ledger.unpriced_reason(),
            Some(crate::llm::usage::UnpricedReason::StreamAborted),
            "a severed stream is distinguishable from a request that never answered"
        );
        assert_eq!(
            ledger.cost_usd, None,
            "an unmeasured request must not price as free"
        );
        assert_eq!(
            ledger.projected_cost_usd(),
            None,
            "nothing bounds a severed stream, so a ceiling consumer fails closed"
        );

        let dict = outcome.vm_result.as_dict().expect("stand-in dict");
        assert!(
            !dict.contains_key("input_tokens") && !dict.contains_key("output_tokens"),
            "usage is the single owner of accounting; the stand-in must not \
             duplicate token counts at the top level: {dict:?}"
        );
        let usage = dict
            .get("usage")
            .and_then(|usage| usage.as_dict())
            .expect("stand-in carries the canonical usage envelope");
        assert_eq!(
            usage
                .get("accounting_status")
                .map(|v| v.as_str_cow())
                .as_deref(),
            Some("unknown"),
            "the envelope a host reads must say the accounting is absent"
        );
        assert!(
            matches!(usage.get("cost_usd"), Some(VmValue::Nil)),
            "a severed stream has no measured cost; got {:?}",
            usage.get("cost_usd")
        );

        // The only partial evidence the abort has stays reachable.
        let abort_meta = dict
            .get("schema_stream_aborted")
            .and_then(|meta| meta.as_dict())
            .expect("abort metadata");
        assert!(
            matches!(abort_meta.get("chunks_consumed"), Some(VmValue::Int(n)) if *n > 0),
            "chunks_consumed is the partial evidence: {abort_meta:?}"
        );

        reset_agent_trace_state();
    });
}

// A structured retry after a token-limit truncation must grow the
// output-token budget so a reasoning model (whose analysis channel is
// billed against the same budget but invisible in parsed text) gets room
// to emit complete JSON instead of re-truncating to empty.
#[test]
fn truncation_retry_escalates_max_tokens() {
    let mut opts = api::options::base_opts("fake");
    opts.max_tokens = 640;
    let errors = vec!["response hit the token limit before producing complete JSON".to_string()];
    let grew = escalate_max_tokens_on_truncation(&mut opts, &errors);
    assert!(grew, "truncation marker should escalate the budget");
    assert_eq!(opts.max_tokens, 1280, "640 should double to 1280");
}

// A non-truncation failure (e.g. a schema-validation miss) must NOT touch
// the budget — escalation is reserved for the under-budget root cause.
#[test]
fn non_truncation_failure_leaves_max_tokens_unchanged() {
    let mut opts = api::options::base_opts("fake");
    opts.max_tokens = 640;
    let errors = vec!["data.age: expected integer, got string".to_string()];
    let grew = escalate_max_tokens_on_truncation(&mut opts, &errors);
    assert!(!grew, "non-truncation failure must not escalate");
    assert_eq!(opts.max_tokens, 640);
}

// The escalation is clamped at the retry ceiling so a pathological
// never-converging loop can't request an unbounded completion.
#[test]
fn truncation_retry_clamps_at_ceiling() {
    let mut opts = api::options::base_opts("fake");
    opts.max_tokens = MAX_TOKENS_RETRY_CEILING - 100;
    let errors = vec!["response hit the token limit before producing complete JSON".to_string()];
    let grew = escalate_max_tokens_on_truncation(&mut opts, &errors);
    assert!(grew, "below the ceiling, the budget should still grow");
    assert_eq!(opts.max_tokens, MAX_TOKENS_RETRY_CEILING);

    // Already at the ceiling: no further growth, no wasted retry signal.
    let grew_again = escalate_max_tokens_on_truncation(&mut opts, &errors);
    assert!(
        !grew_again,
        "at the ceiling the budget must not grow further"
    );
    assert_eq!(opts.max_tokens, MAX_TOKENS_RETRY_CEILING);
}
