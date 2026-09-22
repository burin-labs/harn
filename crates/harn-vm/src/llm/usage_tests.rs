//! Unit tests for `usage.rs`, split out to keep that file under the
//! repository's source-length cap.

use serde_json::json;

use super::{
    extract_probe_usage, summarize_usage_cost_certainty, LlmUsage, ProviderUsageReceipt,
    ToolProbeUsage, UsageAccountingStatus,
};
use crate::llm::api::{LlmResult, ProviderAttempts, ProviderTelemetry};
use crate::value::VmValue;

fn accounted_result() -> LlmResult {
    LlmResult {
        text: "ok".to_string(),
        tool_calls: Vec::new(),
        text_projection: None,
        raw_tool_calls: Vec::new(),
        input_tokens: 1_000,
        output_tokens: 100,
        cache_read_tokens: 800,
        cache_write_tokens: 25,
        cache_supported: true,
        model: "claude-sonnet-4-20250514".to_string(),
        provider: "anthropic".to_string(),
        thinking: None,
        thinking_summary: None,
        stop_reason: Some("end_turn".to_string()),
        served_fast: false,
        blocks: Vec::new(),
        logprobs: Vec::new(),
        telemetry: ProviderTelemetry {
            cache_accounting_declared: Some(true),
            ..ProviderTelemetry::default()
        },
        attempts: ProviderAttempts {
            total: 3,
            rate_limited: 1,
            empty_completion: 1,
            other: 0,
            completed_retry_usage: vec![super::LlmUsage::from_provider_receipt(
                "anthropic",
                "claude-sonnet-4-20250514",
                &ProviderUsageReceipt::new(Some(250), Some(10), None, false)
                    .with_cache(0, 0, None, false),
            )],
        },
    }
}

#[test]
fn reported_hosted_search_is_billed_or_explicitly_unpriced() {
    let _guard = crate::llm::env_guard();
    let mut response = accounted_result();
    response.model = "claude-sonnet-4-5-20250929".into();
    let plain = LlmUsage::from_result(&response).cost_usd.unwrap();
    response.telemetry = ProviderTelemetry::from_anthropic_usage(
        &json!({
            "input_tokens": 1000, "output_tokens": 100,
            "server_tool_use": {"web_search_requests": 1}
        }),
        Some("search-receipt"),
    );
    let searched = LlmUsage::from_result(&response);
    assert!((searched.cost_usd.unwrap() - plain - 0.01).abs() < 1e-12);
    assert_eq!(
        searched.billing.as_ref().unwrap().hosted_tool_calls["web_search"],
        1
    );
    assert!(searched
        .pricing
        .as_ref()
        .unwrap()
        .hosted_tool_unpriced
        .is_empty());

    // An unknown hosted tool cannot look fully priced.
    response
        .telemetry
        .billing
        .as_mut()
        .unwrap()
        .hosted_tool_calls = std::collections::BTreeMap::from([("unknown_hosted_tool".into(), 1)]);
    let unpriced = LlmUsage::from_result(&response);
    assert_eq!(unpriced.accounting_status, UsageAccountingStatus::Partial);
    assert_eq!(unpriced.unpriced_calls, 1);
    assert_eq!(unpriced.projected_cost_usd(), None);
    let aggregate = LlmUsage::aggregate(&[unpriced.clone()]);
    assert_eq!(aggregate.cost_usd, unpriced.cost_usd);
    assert_eq!(aggregate.accounting_status, UsageAccountingStatus::Partial);
    assert_eq!(
        unpriced.pricing.as_ref().unwrap().hosted_tool_unpriced,
        ["unknown_hosted_tool"]
    );

    // An authoritative provider total is retained without a second surcharge.
    response.telemetry.provider_cost_usd = Some(0.123);
    let authoritative = LlmUsage::from_result(&response);
    assert_eq!(authoritative.cost_usd, Some(0.123));
    assert_eq!(authoritative.unpriced_calls, 0);
}

#[test]
fn receipt_retains_request_instant_and_audio_counts_without_inventing_rates() {
    let _guard = crate::llm::env_guard();
    let wire = json!({
        "input_tokens": 1000, "output_tokens": 100,
        "input_tokens_details": {"audio_tokens": 200, "cached_tokens": 0},
        "output_tokens_details": {"audio_tokens": 50}
    });
    let mut receipt = ProviderUsageReceipt::from_openai_usage_tokens(&wire).unwrap();
    receipt.started_at_ms = Some(1_790_000_000_000);
    let roundtrip = ProviderUsageReceipt::from_vm_value(&receipt.to_vm_value()).unwrap();
    assert_eq!(roundtrip, receipt);
    let usage = LlmUsage::from_provider_receipt("openai", "gpt-5.6-luna", &roundtrip);
    assert_eq!(
        usage.pricing.as_ref().unwrap().settled_at_ms,
        1_790_000_000_000
    );
    assert_eq!(
        usage.billing.as_ref().unwrap().audio_input_tokens,
        Some(200)
    );
    assert_eq!(
        usage.pricing.as_ref().unwrap().modality_unpriced,
        ["audio_input", "audio_output"]
    );
    assert_eq!(usage.projected_cost_usd(), None);
}

/// A locally served call that reports no token usage at all, which is what
/// a streaming llama.cpp server sends. Its cost is still known, because the
/// route bills nothing; only its token counts are missing.
fn self_hosted_result_without_usage() -> LlmResult {
    LlmResult {
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cache_supported: false,
        model: "some-locally-served-model".to_string(),
        provider: "llamacpp".to_string(),
        telemetry: ProviderTelemetry {
            cache_accounting_declared: Some(false),
            ..ProviderTelemetry::default()
        },
        attempts: ProviderAttempts::default(),
        ..accounted_result()
    }
}

#[test]
fn self_hosted_call_without_reported_usage_is_priced_but_still_usage_unknown() {
    let usage = self_hosted_result_without_usage().usage();

    // The half that was wrong: an unpriced call spends a whole USD ceiling,
    // so this is what ended budgeted local runs after one model call.
    assert_eq!(usage.cost_usd, Some(0.0));
    assert_eq!(usage.unpriced_calls, 0);

    // The half that must stay honest: nothing here tells us how many
    // tokens the call used, and the ledger should not pretend otherwise.
    assert_eq!(usage.usage_unknown_calls, 1);
    assert_eq!(usage.accounting_status, UsageAccountingStatus::Unknown);
}

#[test]
fn live_tool_probe_preserves_missing_usage_as_unknown() {
    let result = LlmResult {
        provider: "together".to_string(),
        model: "Qwen/Qwen3.6-Plus".to_string(),
        input_tokens: 0,
        output_tokens: 0,
        telemetry: ProviderTelemetry::default(),
        attempts: ProviderAttempts::default(),
        ..accounted_result()
    };

    let usage = ToolProbeUsage::from_llm_result(&result);
    let report = serde_json::to_value(&usage).expect("serialize probe usage");

    assert_eq!(usage.input_tokens, Some(0));
    assert_eq!(usage.output_tokens, Some(0));
    assert_eq!(usage.cost_usd, None);
    assert_eq!(usage.accounting_status, UsageAccountingStatus::Unknown);
    assert_eq!(report["accounting_status"], "unknown");
    assert!(report.get("cost_usd").is_none());

    let reported_zero = ToolProbeUsage::from_llm_result(&LlmResult {
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        telemetry: ProviderTelemetry {
            server_prompt_tokens: Some(0),
            server_output_tokens: Some(0),
            ..ProviderTelemetry::default()
        },
        ..accounted_result()
    });
    assert_eq!(
        reported_zero.accounting_status,
        UsageAccountingStatus::Reported
    );
    assert_eq!(reported_zero.cost_usd, Some(0.0));
}

#[test]
fn paid_call_without_reported_usage_stays_unpriced() {
    let result = LlmResult {
        provider: "anthropic".to_string(),
        ..self_hosted_result_without_usage()
    };
    let usage = result.usage();

    assert_eq!(
        usage.cost_usd, None,
        "a paid route with no usage counts and no provider cost cannot be priced"
    );
    assert_eq!(usage.unpriced_calls, 1);
}

#[test]
fn partial_provider_error_receipt_stays_explicitly_unknown() {
    let receipt = ProviderUsageReceipt::new(Some(9), None, Some(0.25), false).with_cache(
        3,
        2,
        Some(true),
        true,
    );
    let VmValue::Dict(fields) = receipt.to_vm_value() else {
        panic!("receipt must lower to a dictionary");
    };
    assert_eq!(
        fields.get("input_tokens").and_then(VmValue::as_int),
        Some(9)
    );
    assert!(matches!(fields.get("output_tokens"), Some(VmValue::Nil)));
    assert_eq!(
        fields.get("cache_read_tokens").and_then(VmValue::as_int),
        Some(3)
    );

    let usage = LlmUsage::from_provider_receipt("anthropic", "claude-sonnet-4-20250514", &receipt);

    assert_eq!(usage.input_tokens, 9);
    assert_eq!(usage.output_tokens, 0);
    assert_eq!(usage.cost_usd, Some(0.25));
    assert_eq!(usage.known_cost_usd, 0.25);
    assert_eq!(usage.cache_read_tokens, 3);
    assert_eq!(usage.cache_write_tokens, 2);
    assert!(usage.cache_hit);
    assert_eq!(usage.unpriced_calls, 0);
    assert_eq!(usage.usage_unknown_calls, 1);
    assert_eq!(usage.accounting_status, UsageAccountingStatus::Unknown);
}

#[test]
fn cost_certainty_fold_preserves_known_floor_and_unknown_counts() {
    let priced = accounted_result().usage();
    let mut unpriced = priced.clone();
    unpriced.cost_usd = None;
    unpriced.accounting_status = UsageAccountingStatus::Unknown;
    unpriced.known_cost_usd = 0.0;
    unpriced.unpriced_calls = 1;
    unpriced.usage_unknown_calls = 1;

    let summary = summarize_usage_cost_certainty([&priced, &unpriced]);

    assert_eq!(
        summary.known_cost_usd,
        priced.cost_usd.expect("priced call")
    );
    assert_eq!(summary.unpriced_calls, 1);
    assert_eq!(summary.usage_unknown_calls, 1);
}

/// harn#7912 falsifier. A call that recovered from a discarded attempt
/// must report the cost it measured.
///
/// Live shape: a zero-token empty completion was retried, the retry
/// answered and reported full usage, and the envelope still came back
/// `accounting_status: "unknown"` with a null `cost_usd` beside a
/// populated `known_cost_usd`. A real measurement was reported as no
/// measurement, and both cost consumers spend a whole ceiling on that.
#[test]
fn a_recovered_retry_reports_partial_accounting_instead_of_a_black_out() {
    let priced = LlmUsage::from_result(&accounted_result());
    let discarded = LlmUsage::from_result(&LlmResult {
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        attempts: ProviderAttempts::default(),
        ..accounted_result()
    });
    assert_eq!(
        discarded.unpriced_calls, 1,
        "the discarded attempt reported nothing usable, so it stays unpriced"
    );

    let call = LlmUsage::aggregate(&[priced.clone(), discarded]);

    assert_eq!(
        call.cost_usd,
        Some(priced.known_cost_usd),
        "the priced attempt measured this; a discarded sibling must not null it"
    );
    assert_eq!(call.accounting_status, UsageAccountingStatus::Partial);
    assert_eq!(
        call.unpriced_calls, 1,
        "the discarded attempt stays visible"
    );
    assert_eq!(call.provider_call_count, Some(2));
    assert_eq!(
        call.unpriced_reason(),
        Some(super::UnpricedReason::UsageUnreported),
        "the route is priced; what is missing is the attempt's token counts"
    );
}

/// The falsifier control the ruling names: a recovered retry must not
/// consume the whole ceiling. The projection is what a governor spends
/// against, so it has to be a number near the measured cost rather than a
/// refusal.
#[test]
fn a_recovered_retry_projects_close_to_what_it_measured() {
    let priced = LlmUsage::from_result(&accounted_result());
    let discarded = LlmUsage::from_result(&LlmResult {
        input_tokens: 0,
        output_tokens: 0,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        attempts: ProviderAttempts::default(),
        ..accounted_result()
    });

    let call = LlmUsage::aggregate(&[priced.clone(), discarded]);

    let projected = call
        .projected_cost_usd()
        .expect("a priced route bounds its own unreported attempt");
    assert!(
        projected >= priced.known_cost_usd,
        "the projection is a worst case, so it never undercuts what was measured: \
         projected {projected}, measured {}",
        priced.known_cost_usd
    );
    assert!(
        projected <= priced.known_cost_usd * 2.0,
        "an attempt that reported no tokens adds no worst case at this price \
         table, so the projection must not blow up: projected {projected}, \
         measured {}",
        priced.known_cost_usd
    );
}

/// The control that keeps the fix from passing by pricing everything at
/// zero. An attempt that never answered has no token count and no price
/// table entry to bound it, so the projection refuses and every ceiling
/// consumer keeps failing closed.
#[test]
fn an_unprojectable_attempt_still_refuses_the_projection() {
    let priced = LlmUsage::from_result(&accounted_result());

    let call = LlmUsage::aggregate(&[priced.clone(), LlmUsage::unknown_attempt()]);

    assert_eq!(
        call.projected_cost_usd(),
        None,
        "nothing bounds an attempt that produced no response"
    );
    assert_eq!(
        call.unpriced_reason(),
        Some(super::UnpricedReason::NoResponse)
    );
    assert_eq!(
        call.cost_usd,
        Some(priced.known_cost_usd),
        "the priced sibling is still a measurement"
    );
    assert_eq!(call.accounting_status, UsageAccountingStatus::Partial);
}

/// The second control the ruling names: a call that priced nothing at all
/// still blacks out. Only a mixed ledger is partial.
#[test]
fn a_call_that_priced_nothing_still_blacks_out() {
    let call = LlmUsage::aggregate(&[LlmUsage::unknown_attempt(), LlmUsage::unknown_attempt()]);

    assert_eq!(call.cost_usd, None);
    assert_eq!(call.projected_cost_usd(), None);
    assert_eq!(call.accounting_status, UsageAccountingStatus::Unknown);
    assert_eq!(call.unpriced_calls, 2);
}

/// A severed stream and a request that never answered are both unmeasured,
/// but only one of them consumed provider supply. Collapsing them would make
/// a mid-stream abort untriageable in the ledger, which is the same flattening
/// harn#7324 was filed about on the error envelope.
#[test]
fn a_severed_stream_reads_apart_from_a_request_that_never_answered() {
    let priced = LlmUsage::from_result(&accounted_result());

    let severed = LlmUsage::aggregate(&[priced.clone(), LlmUsage::stream_aborted_attempt()]);
    let silent = LlmUsage::aggregate(&[priced.clone(), LlmUsage::unknown_attempt()]);

    assert_eq!(
        severed.unpriced_reason(),
        Some(super::UnpricedReason::StreamAborted)
    );
    assert_eq!(
        silent.unpriced_reason(),
        Some(super::UnpricedReason::NoResponse)
    );
    assert_eq!(
        severed.projected_cost_usd(),
        None,
        "partial output was billed and nothing bounds it, so the ceiling fails closed"
    );
    assert_eq!(
        severed.cost_usd,
        Some(priced.known_cost_usd),
        "the priced sibling stays a measurement"
    );
    assert_eq!(severed.provider_call_count, Some(2));
    assert_eq!(severed.usage_unknown_calls, 1);
}

#[test]
fn terminal_unknown_ledger_counts_every_physical_attempt() {
    let usage = LlmUsage::unknown_attempts(3);

    assert_eq!(usage.provider_call_count, Some(3));
    assert_eq!(usage.unpriced_calls, 3);
    assert_eq!(usage.usage_unknown_calls, 3);
    assert_eq!(usage.cost_usd, None);
    assert_eq!(usage.accounting_status, UsageAccountingStatus::Unknown);
}

#[test]
fn terminal_ledger_preserves_completed_receipts_before_unknown_attempts() {
    let mut completed = LlmUsage::known_zero_attempt();
    completed.cost_usd = Some(0.25);
    completed.known_cost_usd = 0.25;

    let usage = LlmUsage::aggregate_with_unknown_attempts(&[completed], 2);

    assert_eq!(usage.known_cost_usd, 0.25);
    // harn#7912: the completed receipt measured 0.25, and two attempts
    // that never answered do not unmeasure it. What they do is refuse the
    // projection, which is what a ceiling consumer fails closed on.
    assert_eq!(usage.cost_usd, Some(0.25));
    assert_eq!(usage.projected_cost_usd(), None);
    assert_eq!(usage.provider_call_count, Some(3));
    assert_eq!(usage.unpriced_calls, 2);
    assert_eq!(usage.usage_unknown_calls, 2);
    assert_eq!(usage.accounting_status, UsageAccountingStatus::Partial);
}

#[test]
fn legacy_ledger_reconstructs_one_call_without_losing_known_cost() {
    let mut usage = LlmUsage::known_zero_attempt();
    usage.cost_usd = Some(0.25);
    usage.known_cost_usd = 0.0;
    // A ledger recorded before the field existed carries no count at all.
    usage.provider_call_count = None;

    let summary = summarize_usage_cost_certainty([&usage]);

    assert_eq!(summary.known_cost_usd, 0.25);
    assert_eq!(summary.provider_call_count, 1);
    assert_eq!(summary.unpriced_calls, 0);
    assert_eq!(summary.usage_unknown_calls, 0);
}

/// The falsifier for burin-labs/harn#8529: a measured zero must survive the
/// fold as zero. Reading an integer zero as the legacy marker minted every
/// pre-dispatch refusal into one unpriced call.
#[test]
fn measured_zero_folds_as_zero_not_as_a_legacy_call() {
    let usage = LlmUsage::no_provider_request();
    assert_eq!(usage.provider_call_count, Some(0));

    let summary = summarize_usage_cost_certainty([&usage]);

    assert_eq!(summary.provider_call_count, 0);
    assert_eq!(summary.unpriced_calls, 0);
    assert_eq!(summary.usage_unknown_calls, 0);
    assert_eq!(summary.known_cost_usd, 0.0);
    assert_eq!(summary.projected_cost_usd(), Some(0.0));
    assert!(!summary.unprojectable);

    // Composed with a real request, the zero adds nothing and hides nothing.
    let real = LlmUsage::unknown_attempt();
    let composed = summarize_usage_cost_certainty([&usage, &real]);
    assert_eq!(composed.provider_call_count, 1);
    assert_eq!(composed.unpriced_calls, 1);
}

/// A legacy ledger and a measured zero must serialize differently, or a
/// round trip would collapse one into the other.
#[test]
fn absent_count_and_measured_zero_serialize_distinctly() {
    let mut legacy = LlmUsage::known_zero_attempt();
    legacy.provider_call_count = None;
    let measured = LlmUsage::no_provider_request();

    let legacy_json = serde_json::to_value(&legacy).unwrap();
    let measured_json = serde_json::to_value(&measured).unwrap();
    assert!(legacy_json.get("provider_call_count").is_none());
    assert_eq!(measured_json["provider_call_count"], 0);

    let legacy_dict = legacy.to_vm_dict(&ProviderAttempts::default());
    let measured_dict = measured.to_vm_dict(&ProviderAttempts::default());
    assert!(legacy_dict.get("provider_call_count").is_none());
    assert!(matches!(
        measured_dict.get("provider_call_count"),
        Some(VmValue::Int(0))
    ));

    let back: LlmUsage = serde_json::from_value(legacy_json).unwrap();
    assert_eq!(back.provider_call_count, None);
    let back: LlmUsage = serde_json::from_value(measured_json).unwrap();
    assert_eq!(back.provider_call_count, Some(0));
}

#[test]
fn one_ledger_projects_matching_vm_event_and_trace_accounting() {
    let mut result = accounted_result();
    result.telemetry.server_total_tokens = Some(1_100);
    result.attempts = ProviderAttempts::default();
    let usage = result.usage();
    let tool_usage = ToolProbeUsage::from_llm_result(&result);
    let vm_usage =
        crate::llm::vm_value_to_json(&VmValue::Dict(usage.to_vm_dict(&result.attempts).into()));
    let mut event = json!({});
    usage.project_onto_event(&mut event);
    let trace = usage
        .metadata_pairs(&result.provider, &result.model)
        .into_iter()
        .collect::<std::collections::BTreeMap<_, _>>();

    for field in [
        "input_tokens",
        "output_tokens",
        "reported_total_tokens",
        "cost_usd",
        "cache_read_tokens",
        "cache_write_tokens",
        "cache_hit_ratio",
        "cache_savings_usd",
        "served_fast",
    ] {
        assert_eq!(
            vm_usage.get(field),
            event.get(field),
            "{field} drifted between canonical projections"
        );
    }
    assert_eq!(
        trace[crate::tracing::meta::INPUT_TOKENS],
        event["input_tokens"]
    );
    assert_eq!(
        trace[crate::tracing::meta::OUTPUT_TOKENS],
        event["output_tokens"]
    );
    assert_eq!(
        trace[crate::tracing::meta::REPORTED_TOTAL_TOKENS],
        event["reported_total_tokens"]
    );
    assert_eq!(
        tool_usage.reported_total_tokens, usage.reported_total_tokens,
        "tool probes must retain the same measured whole-call total"
    );
    assert_eq!(trace[crate::tracing::meta::COST_USD], event["cost_usd"]);
    assert_eq!(vm_usage["provider_attempts"]["retries"], json!(0));
}

#[test]
fn missing_stream_usage_stays_unknown_instead_of_becoming_free() {
    let mut result = accounted_result();
    result.provider = "fireworks".to_string();
    result.model = "accounts/fireworks/models/minimax-m3".to_string();
    result.input_tokens = 0;
    result.output_tokens = 0;
    result.telemetry = ProviderTelemetry::from_openai_response(
        &serde_json::json!({"usage": {}}),
        Some("chatcmpl-without-usage"),
    );

    let usage = result.usage();
    let vm_usage =
        crate::llm::vm_value_to_json(&VmValue::Dict(usage.to_vm_dict(&result.attempts).into()));

    // The stream attempt reported no usage, so it stays unpriced and
    // visible. It is not turned into a free zero, which is what this test
    // has always guarded.
    assert_eq!(usage.unpriced_calls, 1);
    assert_eq!(usage.usage_unknown_calls, 1);

    // harn#7912: `accounted_result` carries one completed retry receipt
    // that WAS priced. Reporting the call as unmeasured because a sibling
    // attempt was unpriced is the black-out this issue named, so the
    // ledger is now partial and carries the priced portion.
    assert_eq!(vm_usage["accounting_status"], "partial");
    assert_eq!(usage.cost_usd, Some(usage.known_cost_usd));
    assert!(
        usage.known_cost_usd > 0.0,
        "the priced retry receipt is a real measurement"
    );
}

#[test]
fn pre_accounting_status_record_replays_as_unknown() {
    let mut recorded = serde_json::to_value(accounted_result().usage()).expect("serialize");
    recorded
        .as_object_mut()
        .expect("usage object")
        .remove("accounting_status");

    let replayed: super::LlmUsage = serde_json::from_value(recorded).expect("old recording");

    assert_eq!(
        replayed.accounting_status,
        super::UsageAccountingStatus::Unknown
    );
}

#[test]
fn public_usage_projections_do_not_recompute_accounting() {
    let projection_sources = [
        (
            "transcript",
            include_str!("agent_observe/transcript_observability.rs"),
        ),
        (
            "structured envelope",
            include_str!("structured_envelope.rs"),
        ),
        ("trace", include_str!("trace.rs")),
        ("agent result", include_str!("agent_config.rs")),
    ];
    for (name, source) in projection_sources {
        for forbidden in [
            "priced_cost_usd(",
            "cache_hit_ratio(",
            "cache_savings_usd_for_provider(",
            "struct LlmCallUsage",
        ] {
            assert!(
                !source.contains(forbidden),
                "{name} rebuilt canonical usage via {forbidden}"
            );
        }
    }
}

#[test]
fn extracts_openai_responses_usage() {
    let response = json!({
        "usage": {
            "input_tokens": 11,
            "output_tokens": 7
        }
    });

    let usage = extract_probe_usage("unknown", "unknown", &response).expect("usage");

    assert_eq!(usage.input_tokens, Some(11));
    assert_eq!(usage.output_tokens, Some(7));
    assert_eq!(usage.cost_usd, None);
}

#[test]
fn extracts_gemini_usage_metadata_with_thoughts() {
    let response = json!({
        "usageMetadata": {
            "promptTokenCount": 3,
            "candidatesTokenCount": 4,
            "thoughtsTokenCount": 9
        }
    });

    let usage = extract_probe_usage("gemini", "gemini-2.5-pro", &response).expect("usage");

    assert_eq!(usage.input_tokens, Some(3));
    assert_eq!(usage.output_tokens, Some(13));
}

#[test]
fn extracts_vertex_usage_metadata_from_message_wrapper() {
    let response = json!({
        "message": {
            "usageMetadata": {
                "promptTokenCount": 5,
                "candidatesTokenCount": 8
            }
        }
    });

    let usage = extract_probe_usage("vertex", "gemini-2.5-flash", &response).expect("usage");

    assert_eq!(usage.input_tokens, Some(5));
    assert_eq!(usage.output_tokens, Some(8));
}

#[test]
fn extracts_bedrock_usage_tokens() {
    let response = json!({
        "usage": {
            "inputTokens": 17,
            "outputTokens": 23
        }
    });

    let usage = extract_probe_usage("bedrock", "claude-sonnet-5", &response).expect("usage");

    assert_eq!(usage.input_tokens, Some(17));
    assert_eq!(usage.output_tokens, Some(23));
}

#[test]
fn saved_anthropic_probes_normalize_input_and_preserve_cache_pricing() {
    for fresh in [40, 6000] {
        let expected = (fresh as f64 + 500.0 + 125.0 + 40.0) / 1_000_000.0;
        for raw in [
            json!({"input_tokens": fresh, "output_tokens": 8, "cache_read_input_tokens": 5000, "cache_creation_input_tokens": 100}),
            json!({"input_tokens": fresh + 5100, "output_tokens": 8, "cache_read_tokens": 5000, "cache_write_tokens": 100}),
            json!({"prompt_tokens": fresh + 5100, "completion_tokens": 8, "prompt_tokens_details": {"cached_tokens": 5000, "cache_write_tokens": 100}}),
        ] {
            let usage = extract_probe_usage(
                "anthropic",
                "claude-haiku-4-5-20251001",
                &json!({"usage": raw}),
            )
            .unwrap();
            assert_eq!(usage.input_tokens, Some(fresh + 5100));
            assert_eq!(usage.output_tokens, Some(8));
            assert!((usage.cost_usd.unwrap() - expected).abs() < 1e-10);
        }
    }
}

#[test]
fn saved_bedrock_and_gemini_cache_usage_have_comparable_totals() {
    for raw in [
        json!({"usage": {"inputTokens": 40, "outputTokens": 8, "cacheReadInputTokens": 5000, "cacheWriteInputTokens": 0}}),
        json!({"usageMetadata": {"promptTokenCount": 5040, "candidatesTokenCount": 8, "cachedContentTokenCount": 5000}}),
    ] {
        let usage = extract_probe_usage("unknown", "unknown", &raw).unwrap();
        assert_eq!(usage.input_tokens, Some(5040));
        assert_eq!(usage.output_tokens, Some(8));
    }
}

#[test]
fn saved_anthropic_stream_merges_partial_cumulative_usage() {
    let response = json!({"frames": [
        {"type": "message_start", "message": {"usage": {
            "input_tokens": 40, "output_tokens": 0, "cache_read_input_tokens": 5000, "cache_creation_input_tokens": 100,
        }}},
        {"type": "message_delta", "usage": {"output_tokens": 8, "cache_creation_input_tokens": 200}},
        {"type": "message_stop"},
    ]});
    let usage = extract_probe_usage("anthropic", "claude-haiku-4-5-20251001", &response).unwrap();
    assert_eq!(usage.input_tokens, Some(5240));
    assert_eq!(usage.output_tokens, Some(8));
    assert!((usage.cost_usd.unwrap() - 0.00083).abs() < 1e-10);
}

#[test]
fn saved_probe_missing_and_invalid_usage_cannot_become_a_priced_zero() {
    assert!(extract_probe_usage("anthropic", "claude-haiku-4-5-20251001", &json!({})).is_none());
    for raw in [
        json!({"output_tokens": 8, "cache_read_input_tokens": 5000}),
        json!({"input_tokens": -1, "output_tokens": 8, "cache_read_input_tokens": 5000}),
        json!({"input_tokens": 40, "output_tokens": 8, "cache_read_tokens": 5000}),
        json!({"input_tokens": 5040, "output_tokens": 8, "cache_read_tokens": 5000, "cache_supported": false}),
    ] {
        let usage = extract_probe_usage(
            "anthropic",
            "claude-haiku-4-5-20251001",
            &json!({"usage": raw}),
        )
        .unwrap();
        assert_eq!(usage.input_tokens, None);
        assert_eq!(usage.cost_usd, None);
        assert_eq!(usage.accounting_status, UsageAccountingStatus::Unknown);
    }
    let zero = extract_probe_usage(
        "anthropic",
        "claude-haiku-4-5-20251001",
        &json!({"usage": {"input_tokens": 0, "output_tokens": 0}}),
    )
    .unwrap();
    assert_eq!(zero.input_tokens, Some(0));
    assert_eq!(zero.cost_usd, Some(0.0));
    assert_eq!(zero.accounting_status, UsageAccountingStatus::Reported);
}

#[test]
fn uses_final_stream_usage_without_double_counting_prior_frames() {
    let response = json!({
        "frames": [
            {
                "usage": {
                    "prompt_tokens": 1,
                    "completion_tokens": 1
                }
            },
            {
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 2
                }
            }
        ]
    });

    let usage = extract_probe_usage("unknown", "unknown", &response).expect("usage");

    assert_eq!(usage.input_tokens, Some(10));
    assert_eq!(usage.output_tokens, Some(2));
}

#[test]
fn root_usage_dominates_copied_stream_frames() {
    let response = json!({
        "usage": {
            "prompt_tokens": 10,
            "completion_tokens": 2
        },
        "frames": [
            {
                "usage": {
                    "prompt_tokens": 10,
                    "completion_tokens": 2
                }
            }
        ]
    });

    let usage = extract_probe_usage("unknown", "unknown", &response).expect("usage");

    assert_eq!(usage.input_tokens, Some(10));
    assert_eq!(usage.output_tokens, Some(2));
}

fn usage_with_declaration(declared: Option<bool>) -> LlmUsage {
    let mut result = accounted_result();
    result.telemetry.cache_accounting_declared = declared;
    result.attempts = ProviderAttempts::default();
    LlmUsage::from_result(&result)
}

#[test]
fn cache_visibility_projects_three_states() {
    let declared_true = usage_with_declaration(Some(true));
    let mut fields = serde_json::Map::new();
    declared_true.project_onto_fields(&mut fields);
    assert_eq!(fields["cache_visibility"], serde_json::Value::Null);

    let declared_false = usage_with_declaration(Some(false));
    assert_eq!(declared_false.cache_hit_ratio, None);
    let mut fields = serde_json::Map::new();
    declared_false.project_onto_fields(&mut fields);
    assert_eq!(fields["cache_visibility"], json!("unsupported"));

    // The load-bearing state: an undeclared route's zeros carry no
    // information, and must not read as either audited or unsupported.
    let undeclared = usage_with_declaration(None);
    assert_eq!(undeclared.cache_hit_ratio, None);
    let mut fields = serde_json::Map::new();
    undeclared.project_onto_fields(&mut fields);
    assert_eq!(fields["cache_hit_ratio"], serde_json::Value::Null);
    assert_eq!(fields["cache_visibility"], json!("undeclared"));
}

#[test]
fn one_undeclared_call_poisons_the_aggregate_to_undeclared() {
    let declared = usage_with_declaration(Some(true));
    let undeclared = usage_with_declaration(None);

    let all_declared = LlmUsage::aggregate(&[declared.clone(), declared.clone()]);
    assert_eq!(all_declared.cache_accounting_declared, Some(true));

    let poisoned = LlmUsage::aggregate(&[declared, undeclared]);
    assert_eq!(poisoned.cache_accounting_declared, None);
    assert_eq!(poisoned.cache_hit_ratio, None);
    let mut fields = serde_json::Map::new();
    poisoned.project_onto_fields(&mut fields);
    assert_eq!(fields["cache_visibility"], json!("undeclared"));
}

#[test]
fn unknown_attempts_stay_neutral_for_the_accounting_declaration() {
    let declared = usage_with_declaration(Some(true));
    let usage = LlmUsage::aggregate_with_unknown_attempts(&[declared], 1);
    assert_eq!(usage.cache_accounting_declared, Some(true));
}
