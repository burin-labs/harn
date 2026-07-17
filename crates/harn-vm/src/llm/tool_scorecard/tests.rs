use super::*;
use crate::llm::tool_conformance::{
    ToolCallingConformanceSummary, ToolProbeMode, ToolProbeStatus, ToolProbeUsage,
};

#[test]
fn scorecard_ranks_successful_native_route_first() {
    let pass = complete_success_reports(
        "anthropic",
        "claude",
        ToolProbeClassification::StructuredNativeToolCall,
    );
    let fail = report(
        "fireworks",
        "gpt-oss",
        vec![case(ToolProbeClassification::EmptySilent, false)],
    );

    let scorecard = scorecard_from_tool_reports(pass.into_iter().chain([fail]).collect());

    assert_eq!(scorecard.schema_version, TOOL_SCORECARD_SCHEMA_VERSION);
    assert_eq!(scorecard.route_count, 2);
    assert_eq!(scorecard.summary.pass, 1);
    assert_eq!(scorecard.summary.warn, 0);
    assert_eq!(scorecard.summary.fail, 1);
    assert_eq!(scorecard.routes[0].provider, "anthropic");
    assert_eq!(scorecard.routes[0].status, "pass");
    assert_eq!(scorecard.routes[0].evidence_status, "complete");
    assert_eq!(scorecard.routes[0].probe_evidence_status, "complete");
    assert_eq!(scorecard.routes[0].request_evidence_status, "complete");
    assert_eq!(scorecard.routes[0].recommended_tool_mode, "native");
    assert_eq!(
        scorecard.routes[1].issues,
        vec![
            "tool_calling_disabled",
            "incomplete_required_probe_evidence",
            "empty_or_actionless_completion"
        ]
    );
}

#[test]
fn scorecard_reports_catalog_drift_without_failing_route() {
    let scorecard = scorecard_from_tool_reports(complete_success_reports(
        "anthropic",
        "claude-sonnet-4-6",
        ToolProbeClassification::ParseableHarnTextToolCall,
    ));

    let route = &scorecard.routes[0];
    assert_eq!(route.status, "warn");
    assert_eq!(route.recommended_tool_mode, "text");
    assert!(route.catalog_claim.is_some());
    assert!(route
        .catalog_mismatches
        .iter()
        .any(|mismatch| mismatch.code == "preferred_tool_format_disagrees"));
    assert!(route.suggested_catalog_updates.iter().any(|update| {
        update.field == "tool_support.preferred_format"
            && update.operation == "set"
            && update.value.as_deref() == Some("json")
    }));
}

#[test]
fn scorecard_does_not_complete_mode_both_plan_with_non_streaming_only_reports() {
    let scorecard = scorecard_from_tool_reports(non_streaming_success_reports(
        "anthropic",
        "claude-sonnet-5",
        ToolProbeClassification::StructuredNativeToolCall,
    ));

    let route = &scorecard.routes[0];
    assert_eq!(route.status, "warn");
    assert_eq!(route.probe_evidence_status, "partial");
    assert_eq!(route.request_evidence_status, "partial");
    assert_eq!(route.quality_score, 100);
    assert!(route
        .missing_required_probe_evidence
        .iter()
        .any(|evidence| evidence.case_id == "single_tool_call" && evidence.mode == "streaming"));
    assert!(route.missing_required_cases.contains(&"single_tool_call"));
    assert!(route.missing_required_cases.contains(&"parameter_edges"));
    assert!(route.issues.contains(&"incomplete_required_probe_evidence"));
    assert!(route
        .issues
        .contains(&"incomplete_required_request_evidence"));
}

#[test]
fn scorecard_matches_catalog_claims_by_wire_model() {
    let scorecard = scorecard_from_tool_reports(complete_success_reports(
        "nvidia",
        "openai/gpt-oss-120b",
        ToolProbeClassification::StructuredNativeToolCall,
    ));

    let route = &scorecard.routes[0];
    assert_eq!(route.provider, "nvidia");
    assert_eq!(route.model, "openai/gpt-oss-120b");
    assert!(route.catalog_claim.is_some());
    assert!(route
        .catalog_mismatches
        .iter()
        .all(|mismatch| mismatch.code != "route_missing_from_catalog"));
}

#[test]
fn scorecard_splits_evidence_by_transport_mode() {
    let scorecard = scorecard_from_tool_reports(vec![report(
        "deepinfra",
        "openai/gpt-oss-120b",
        vec![
            case_with_mode(
                ToolProbeMode::NonStreaming,
                ToolProbeClassification::EmptySilent,
                false,
            ),
            case_with_mode(
                ToolProbeMode::Streaming,
                ToolProbeClassification::StructuredNativeToolCall,
                true,
            ),
        ],
    )]);

    let route = &scorecard.routes[0];
    assert_eq!(route.quality_score, 50);
    assert_eq!(route.status, "warn");
    assert_eq!(route.recommended_tool_mode, "native");
    assert_eq!(route.mode_evidence.len(), 2);
    assert_eq!(route.mode_evidence[0].mode, "non_streaming");
    assert_eq!(route.mode_evidence[0].status, "fail");
    assert_eq!(
        route.mode_evidence[0].issues,
        vec!["tool_calling_disabled", "empty_or_actionless_completion"]
    );
    assert_eq!(route.mode_evidence[1].mode, "streaming");
    assert_eq!(route.mode_evidence[1].status, "pass");
    assert_eq!(route.mode_evidence[1].recommended_tool_mode, "native");
}

#[test]
fn scorecard_aggregates_saved_probe_telemetry_without_guessing_missing_usage() {
    let mut fast = case_with_mode(
        ToolProbeMode::NonStreaming,
        ToolProbeClassification::StructuredNativeToolCall,
        true,
    );
    fast.elapsed_ms = Some(10);
    fast.usage = Some(ToolProbeUsage {
        input_tokens: Some(100),
        output_tokens: Some(20),
        cost_usd: Some(0.0012),
    });
    let mut throttled = case_with_mode(
        ToolProbeMode::Streaming,
        ToolProbeClassification::HttpError,
        false,
    );
    throttled.http_status = Some(429);
    throttled.elapsed_ms = Some(30);
    throttled.usage = Some(ToolProbeUsage {
        input_tokens: Some(40),
        output_tokens: None,
        cost_usd: None,
    });

    let scorecard =
        scorecard_from_tool_reports(vec![report("anthropic", "claude", vec![fast, throttled])]);

    let route = &scorecard.routes[0];
    assert_eq!(route.observed_latency_case_count, 2);
    assert_eq!(route.latency_p50_ms, Some(30));
    assert_eq!(route.latency_p95_ms, Some(30));
    assert_eq!(route.rate_limited_cases, 1);
    assert!(route.issues.contains(&"provider_rate_limited"));
    assert_eq!(route.observed_usage_case_count, 2);
    assert_eq!(route.input_tokens, Some(140));
    assert_eq!(route.output_tokens, Some(20));
    assert_eq!(route.observed_cost_case_count, 1);
    assert_eq!(route.cost_usd, Some(0.0012));
    let streaming = route
        .mode_evidence
        .iter()
        .find(|mode| mode.mode == "streaming")
        .expect("streaming mode evidence");
    assert_eq!(streaming.observed_latency_case_count, 1);
    assert_eq!(streaming.latency_p50_ms, Some(30));
    assert_eq!(streaming.rate_limited_cases, 1);
    assert_eq!(streaming.observed_usage_case_count, 1);
    assert_eq!(streaming.input_tokens, Some(40));
    assert_eq!(streaming.output_tokens, None);
    assert_eq!(streaming.observed_cost_case_count, 0);
    assert_eq!(streaming.cost_usd, None);
}

#[test]
fn scorecard_marks_successful_prose_followup_as_partial_evidence() {
    let scorecard = scorecard_from_tool_reports(vec![report_with_probe_case(
        "anthropic",
        "claude-sonnet-5",
        ToolProbeCase::ToolResultFollowup,
        vec![case(ToolProbeClassification::ProseOnlyNonTool, true)],
    )]);

    let route = &scorecard.routes[0];
    assert_eq!(route.status, "warn");
    assert_eq!(route.evidence_status, "partial");
    assert_eq!(route.probe_evidence_status, "partial");
    assert_eq!(route.request_evidence_status, "partial");
    assert_eq!(route.quality_score, 100);
    assert_eq!(route.successful_cases, 1);
    assert_eq!(route.actionless_cases, 0);
    assert_eq!(route.observed_probe_cases, vec!["tool_result_followup"]);
    assert!(route.missing_required_cases.contains(&"single_tool_call"));
    assert!(route.issues.contains(&"incomplete_required_probe_evidence"));
    assert_eq!(
        route.classification_counts.get("prose_only_non_tool"),
        Some(&1)
    );
}

#[test]
fn scorecard_does_not_pass_without_required_baseline_probe_evidence() {
    let scorecard = scorecard_from_tool_reports(vec![report_with_probe_case(
        "anthropic",
        "claude",
        ToolProbeCase::ToolResultFollowup,
        vec![case(ToolProbeClassification::ProseOnlyNonTool, true)],
    )]);

    let route = &scorecard.routes[0];
    assert_eq!(route.status, "warn");
    assert_eq!(route.evidence_status, "partial");
    assert_eq!(route.quality_score, 100);
    assert_eq!(route.observed_probe_cases, vec!["tool_result_followup"]);
    assert_eq!(
        route.missing_required_cases,
        vec![
            "done_sentinel",
            "large_string_argument",
            "no_tool_answer_or_refusal",
            "single_tool_call",
            "tool_result_followup",
            "unavailable_tool_repair",
        ]
    );
}

#[test]
fn scorecard_does_not_suggest_catalog_disable_without_positive_evidence() {
    let scorecard = scorecard_from_tool_reports(vec![report(
        "anthropic",
        "claude-sonnet-4-6",
        vec![case(ToolProbeClassification::HttpError, false)],
    )]);

    let route = &scorecard.routes[0];
    assert_eq!(route.status, "fail");
    assert_eq!(route.recommended_tool_mode, "disabled");
    assert!(route.catalog_mismatches.is_empty());
    assert!(route.suggested_catalog_updates.is_empty());
}

#[test]
fn catalog_drift_treats_missing_preferred_format_as_no_preference() {
    let (mismatches, updates) = catalog_drift(&Some(catalog_claim(None, true, false)), "native");

    assert!(mismatches.is_empty());
    assert!(updates.is_empty());
}

#[test]
fn catalog_drift_treats_json_preferred_format_as_text_channel_match() {
    let (mismatches, updates) =
        catalog_drift(&Some(catalog_claim(Some("json"), false, true)), "text");

    assert!(mismatches.is_empty());
    assert!(updates.is_empty());
}

#[test]
fn catalog_drift_suggests_safe_text_channel_default_for_native_mismatch() {
    let (mismatches, updates) =
        catalog_drift(&Some(catalog_claim(Some("native"), true, true)), "text");

    assert_eq!(mismatches[0].code, "preferred_tool_format_disagrees");
    assert_eq!(updates[0].field, "tool_support.preferred_format");
    assert_eq!(updates[0].value.as_deref(), Some("json"));
}

#[test]
fn scorecard_plan_filters_catalog_routes_and_names_required_cases() {
    let plan = tool_scorecard_plan_from_catalog(&[String::from("anthropic:claude-sonnet-5")], true)
        .expect("plan from catalog");

    assert_eq!(plan.schema_version, TOOL_SCORECARD_PLAN_SCHEMA_VERSION);
    assert_eq!(plan.kind, "plan");
    assert_eq!(plan.route_count, 1);
    assert!(plan.unscorecardable_provider_count > 0);
    assert_eq!(plan.routes[0].provider, "anthropic");
    assert_eq!(plan.routes[0].model, "claude-sonnet-5");
    assert!(plan.catalog.hash_blake3.starts_with("blake3:"));
    let case_ids = plan.routes[0]
        .cases
        .iter()
        .map(|case| case.id)
        .collect::<Vec<_>>();
    assert!(case_ids.contains(&"single_tool_call"));
    assert!(case_ids.contains(&"large_string_argument"));
    assert!(case_ids.contains(&"tool_result_followup"));
    assert!(case_ids.contains(&"signed_thinking_tool_result_followup"));
    assert!(case_ids.contains(&"done_sentinel"));
    assert_eq!(plan.case_count, plan.routes[0].cases.len());
    assert!(plan.required_case_count >= 7);
    let unscorecardable_by_provider = plan
        .unscorecardable_providers
        .iter()
        .map(|provider| (provider.provider.as_str(), provider))
        .collect::<std::collections::BTreeMap<_, _>>();
    let vllm = unscorecardable_by_provider
        .get("vllm")
        .expect("vLLM provider state should be explicit");
    assert_eq!(vllm.reason, "requires_runtime_model");
    assert_eq!(vllm.model_count, 0);
    assert!(vllm.local_runtime);
    assert!(!vllm.auth_required);
    assert!(vllm.credential_env_names.is_empty());
    let bedrock = unscorecardable_by_provider
        .get("bedrock")
        .expect("Bedrock provider state should be explicit");
    assert_eq!(bedrock.reason, "catalog_provider_has_no_models");
    assert_eq!(bedrock.model_count, 0);
    assert!(!bedrock.local_runtime);
    assert!(bedrock.auth_required);
    assert!(bedrock.credential_env_names.is_empty());
    let single_tool_case = plan.routes[0]
        .cases
        .iter()
        .find(|case| case.id == "single_tool_call")
        .expect("single tool case");
    assert_eq!(single_tool_case.execution.status, "executable");
    assert_eq!(single_tool_case.execution.runner, "provider_tool_probe");
    assert_eq!(
        single_tool_case.execution.command.as_ref().unwrap(),
        &vec![
            "harn".to_string(),
            "provider".to_string(),
            "tool-probe".to_string(),
            "anthropic".to_string(),
            "--model".to_string(),
            "claude-sonnet-5".to_string(),
            "--mode".to_string(),
            "both".to_string(),
            "--case".to_string(),
            "single_tool_call".to_string(),
            "--repeat".to_string(),
            "1".to_string(),
            "--timeout-secs".to_string(),
            "120".to_string(),
            "--json".to_string(),
        ]
    );
    let large_string_case = plan.routes[0]
        .cases
        .iter()
        .find(|case| case.id == "large_string_argument")
        .expect("large string case");
    assert_eq!(large_string_case.execution.status, "executable");
    assert_eq!(large_string_case.execution.runner, "provider_tool_probe");
    assert_eq!(
        large_string_case.execution.command.as_ref().unwrap(),
        &vec![
            "harn".to_string(),
            "provider".to_string(),
            "tool-probe".to_string(),
            "anthropic".to_string(),
            "--model".to_string(),
            "claude-sonnet-5".to_string(),
            "--mode".to_string(),
            "both".to_string(),
            "--case".to_string(),
            "large_string_argument".to_string(),
            "--repeat".to_string(),
            "1".to_string(),
            "--timeout-secs".to_string(),
            "120".to_string(),
            "--json".to_string(),
        ]
    );
    let parallel_case = plan.routes[0]
        .cases
        .iter()
        .find(|case| case.id == "parallel_tool_calls")
        .expect("parallel case");
    assert_eq!(parallel_case.execution.status, "executable");
    assert_eq!(parallel_case.execution.runner, "provider_tool_probe");
    assert_eq!(
        parallel_case.execution.command.as_ref().unwrap(),
        &vec![
            "harn".to_string(),
            "provider".to_string(),
            "tool-probe".to_string(),
            "anthropic".to_string(),
            "--model".to_string(),
            "claude-sonnet-5".to_string(),
            "--mode".to_string(),
            "both".to_string(),
            "--case".to_string(),
            "parallel_tool_calls".to_string(),
            "--repeat".to_string(),
            "1".to_string(),
            "--timeout-secs".to_string(),
            "120".to_string(),
            "--json".to_string(),
        ]
    );
    let done_case = plan.routes[0]
        .cases
        .iter()
        .find(|case| case.id == "done_sentinel")
        .expect("done sentinel case");
    assert_eq!(done_case.execution.status, "executable");
    assert_eq!(done_case.execution.runner, "provider_tool_probe");
    assert_eq!(
        done_case.execution.command.as_ref().unwrap(),
        &vec![
            "harn".to_string(),
            "provider".to_string(),
            "tool-probe".to_string(),
            "anthropic".to_string(),
            "--model".to_string(),
            "claude-sonnet-5".to_string(),
            "--mode".to_string(),
            "both".to_string(),
            "--case".to_string(),
            "done_sentinel".to_string(),
            "--repeat".to_string(),
            "1".to_string(),
            "--timeout-secs".to_string(),
            "120".to_string(),
            "--json".to_string(),
        ]
    );
    let parameter_edges_case = plan.routes[0]
        .cases
        .iter()
        .find(|case| case.id == "parameter_edges")
        .expect("parameter edges case");
    assert_eq!(parameter_edges_case.execution.status, "executable");
    assert_eq!(
        parameter_edges_case.execution.runner,
        "provider_tool_probe_request"
    );
    assert_eq!(
        parameter_edges_case.execution.command.as_ref().unwrap(),
        &vec![
            "harn".to_string(),
            "provider".to_string(),
            "tool-probe".to_string(),
            "anthropic".to_string(),
            "--model".to_string(),
            "claude-sonnet-5".to_string(),
            "--mode".to_string(),
            "both".to_string(),
            "--case".to_string(),
            "single_tool_call".to_string(),
            "--request-profile".to_string(),
            "parameter_edges".to_string(),
            "--dry-run-request".to_string(),
            "--json".to_string(),
        ]
    );
    let followup_case = plan.routes[0]
        .cases
        .iter()
        .find(|case| case.id == "tool_result_followup")
        .expect("follow-up case");
    assert_eq!(followup_case.execution.status, "executable");
    assert_eq!(followup_case.execution.runner, "provider_tool_probe");
    assert_eq!(
        followup_case.execution.command.as_ref().unwrap(),
        &vec![
            "harn".to_string(),
            "provider".to_string(),
            "tool-probe".to_string(),
            "anthropic".to_string(),
            "--model".to_string(),
            "claude-sonnet-5".to_string(),
            "--mode".to_string(),
            "both".to_string(),
            "--case".to_string(),
            "tool_result_followup".to_string(),
            "--repeat".to_string(),
            "1".to_string(),
            "--timeout-secs".to_string(),
            "120".to_string(),
            "--json".to_string(),
        ]
    );
    let signed_thinking_case = plan.routes[0]
        .cases
        .iter()
        .find(|case| case.id == "signed_thinking_tool_result_followup")
        .expect("signed thinking follow-up case");
    assert_eq!(signed_thinking_case.execution.status, "executable");
    assert_eq!(
        signed_thinking_case.execution.runner,
        "provider_tool_probe_request"
    );
    assert_eq!(
        signed_thinking_case.execution.command.as_ref().unwrap(),
        &vec![
            "harn".to_string(),
            "provider".to_string(),
            "tool-probe".to_string(),
            "anthropic".to_string(),
            "--model".to_string(),
            "claude-sonnet-5".to_string(),
            "--mode".to_string(),
            "both".to_string(),
            "--case".to_string(),
            "signed_thinking_tool_result_followup".to_string(),
            "--dry-run-request".to_string(),
            "--json".to_string(),
        ]
    );
}

#[test]
fn scorecard_plan_does_not_require_tool_cases_for_no_tool_routes() {
    let plan = tool_scorecard_plan_from_catalog(&[String::from("groq:groq/compound")], false)
        .expect("plan from catalog");
    let cases = &plan.routes[0].cases;

    for case_id in [
        "single_tool_call",
        "large_string_argument",
        "tool_result_followup",
        "signed_thinking_tool_result_followup",
    ] {
        let case = cases
            .iter()
            .find(|case| case.id == case_id)
            .expect("case exists");
        assert_eq!(case.requirement, "not_applicable", "{case_id}");
        assert_eq!(
            case.requirement_reason, "route_declares_no_tool_surface",
            "{case_id}"
        );
        assert_eq!(case.execution.status, "not_applicable", "{case_id}");
        assert_eq!(case.execution.runner, "none", "{case_id}");
        assert_eq!(
            case.execution.reason, "route_declares_no_tool_surface",
            "{case_id}"
        );
        assert!(case.execution.command.is_none(), "{case_id}");
    }

    let parallel = cases
        .iter()
        .find(|case| case.id == "parallel_tool_calls")
        .expect("parallel case exists");
    assert_eq!(parallel.requirement, "not_applicable");
    assert_eq!(
        parallel.requirement_reason,
        "route_does_not_claim_parallel_tool_calls"
    );
    assert_eq!(parallel.execution.status, "not_applicable");
    assert_eq!(
        parallel.execution.reason,
        "route_does_not_claim_parallel_tool_calls"
    );

    let parameter_edges = cases
        .iter()
        .find(|case| case.id == "parameter_edges")
        .expect("parameter edge case exists");
    assert_eq!(parameter_edges.execution.status, "executable");
    assert_eq!(
        parameter_edges.execution.command.as_ref().unwrap(),
        &vec![
            "harn".to_string(),
            "provider".to_string(),
            "tool-probe".to_string(),
            "groq".to_string(),
            "--model".to_string(),
            "groq/compound".to_string(),
            "--mode".to_string(),
            "both".to_string(),
            "--case".to_string(),
            "no_tool_answer_or_refusal".to_string(),
            "--request-profile".to_string(),
            "parameter_edges".to_string(),
            "--dry-run-request".to_string(),
            "--json".to_string(),
        ]
    );
}

#[test]
fn scorecard_plan_does_not_treat_text_routes_as_native_parallel_evidence() {
    let plan =
        tool_scorecard_plan_from_catalog(&[String::from("deepinfra:openai/gpt-oss-120b")], false)
            .expect("plan from catalog");
    assert!(!plan.routes[0].catalog_claim.native_tools);
    assert!(plan.routes[0].catalog_claim.text_tools);

    let parallel = plan.routes[0]
        .cases
        .iter()
        .find(|case| case.id == "parallel_tool_calls")
        .expect("parallel case exists");
    assert_eq!(parallel.requirement, "not_applicable");
    assert_eq!(parallel.execution.status, "not_applicable");
    assert!(parallel.execution.command.is_none());

    let single = plan.routes[0]
        .cases
        .iter()
        .find(|case| case.id == "single_tool_call")
        .expect("single tool case exists");
    assert_eq!(single.requirement, "required");
    assert_eq!(single.execution.status, "executable");
}

#[test]
fn scorecard_plan_rejects_unknown_route_filters() {
    let err = tool_scorecard_plan_from_catalog(&[String::from("missing:nope")], false)
        .expect_err("unknown route should fail");

    assert!(err.contains("missing:nope"), "{err}");
}

fn report(provider: &str, model: &str, cases: Vec<ToolConformanceCase>) -> ToolConformanceReport {
    report_with_probe_case(
        provider,
        model,
        crate::llm::tool_conformance::ToolProbeCase::SingleToolCall,
        cases,
    )
}

fn report_with_probe_case(
    provider: &str,
    model: &str,
    probe_case: ToolProbeCase,
    cases: Vec<ToolConformanceCase>,
) -> ToolConformanceReport {
    ToolConformanceReport {
        schema_version: 1,
        provider: provider.to_string(),
        model: model.to_string(),
        base_url: None,
        probe_case,
        tool_name: "echo_marker".to_string(),
        marker: "marker".to_string(),
        expected_value: "marker".to_string(),
        cases,
        tool_calling: ToolCallingConformanceSummary {
            native: ToolProbeStatus::Unknown,
            text: ToolProbeStatus::Unknown,
            streaming_native: ToolProbeStatus::Unknown,
            fallback_mode: ToolProbeFallbackMode::Disabled,
            failure_reason: None,
        },
    }
}

fn complete_success_reports(
    provider: &str,
    model: &str,
    tool_call_classification: ToolProbeClassification,
) -> Vec<ToolConformanceReport> {
    success_reports(provider, model, tool_call_classification, None)
}

fn non_streaming_success_reports(
    provider: &str,
    model: &str,
    tool_call_classification: ToolProbeClassification,
) -> Vec<ToolConformanceReport> {
    success_reports(
        provider,
        model,
        tool_call_classification,
        Some(ToolProbeMode::NonStreaming),
    )
}

fn success_reports(
    provider: &str,
    model: &str,
    tool_call_classification: ToolProbeClassification,
    only_mode: Option<ToolProbeMode>,
) -> Vec<ToolConformanceReport> {
    let claims = catalog_claims_by_route();
    let claim = claims.get(&(provider.to_string(), model.to_string()));
    let mut cases_by_probe =
        BTreeMap::<&'static str, (ToolProbeCase, Vec<ToolConformanceCase>)>::new();
    for evidence in required_scorecard_probe_evidence(provider, model, claim) {
        let mode = mode_from_scorecard_evidence(evidence.mode);
        if only_mode.is_some_and(|only_mode| only_mode != mode) {
            continue;
        }
        let probe_case = probe_case_from_id(evidence.case_id);
        cases_by_probe
            .entry(evidence.case_id)
            .or_insert_with(|| (probe_case, Vec::new()))
            .1
            .push(successful_case_for_probe_case(
                probe_case,
                mode,
                tool_call_classification.clone(),
            ));
    }
    cases_by_probe
        .into_iter()
        .map(|(_, (probe_case, cases))| report_with_probe_case(provider, model, probe_case, cases))
        .collect()
}

fn probe_case_from_id(case_id: &str) -> ToolProbeCase {
    match case_id {
        "single_tool_call" => ToolProbeCase::SingleToolCall,
        "parallel_tool_calls" => ToolProbeCase::ParallelToolCalls,
        "large_string_argument" => ToolProbeCase::LargeStringArgument,
        "tool_result_followup" => ToolProbeCase::ToolResultFollowup,
        "signed_thinking_tool_result_followup" => ToolProbeCase::SignedThinkingToolResultFollowup,
        "no_tool_answer_or_refusal" => ToolProbeCase::NoToolAnswerOrRefusal,
        "unavailable_tool_repair" => ToolProbeCase::UnavailableToolRepair,
        "done_sentinel" => ToolProbeCase::DoneSentinel,
        other => panic!("unsupported scorecard probe case id {other}"),
    }
}

fn successful_case_for_probe_case(
    probe_case: ToolProbeCase,
    mode: ToolProbeMode,
    tool_call_classification: ToolProbeClassification,
) -> ToolConformanceCase {
    let classification = match probe_case {
        ToolProbeCase::SingleToolCall
        | ToolProbeCase::ParallelToolCalls
        | ToolProbeCase::LargeStringArgument => tool_call_classification,
        ToolProbeCase::ToolResultFollowup | ToolProbeCase::SignedThinkingToolResultFollowup => {
            ToolProbeClassification::ProseOnlyNonTool
        }
        ToolProbeCase::NoToolAnswerOrRefusal => ToolProbeClassification::DirectAnswerNoTool,
        ToolProbeCase::UnavailableToolRepair => ToolProbeClassification::UnavailableToolRepair,
        ToolProbeCase::DoneSentinel => ToolProbeClassification::DoneSentinel,
    };
    case_with_mode(mode, classification, true)
}

fn mode_from_scorecard_evidence(mode: &str) -> ToolProbeMode {
    match mode {
        "non_streaming" => ToolProbeMode::NonStreaming,
        "streaming" => ToolProbeMode::Streaming,
        other => panic!("unsupported scorecard mode {other}"),
    }
}

fn case(classification: ToolProbeClassification, ok: bool) -> ToolConformanceCase {
    case_with_mode(ToolProbeMode::NonStreaming, classification, ok)
}

fn case_with_mode(
    mode: ToolProbeMode,
    classification: ToolProbeClassification,
    ok: bool,
) -> ToolConformanceCase {
    ToolConformanceCase {
        mode,
        ok,
        classification,
        fallback_mode: ToolProbeFallbackMode::Native,
        failure_reason: None,
        http_status: None,
        elapsed_ms: Some(1),
        native_tool_call_count: usize::from(ok),
        text_tool_call_count: 0,
        usage: None,
        parser_errors: Vec::new(),
        protocol_violations: Vec::new(),
        content_sample: None,
    }
}

fn catalog_claim(
    preferred_tool_format: Option<&str>,
    native_tools: bool,
    text_tools: bool,
) -> ToolScorecardCatalogClaim {
    ToolScorecardCatalogClaim {
        preferred_tool_format: preferred_tool_format.map(str::to_string),
        tool_mode_parity: None,
        native_tools,
        text_tools,
        text_tool_wire_format_supported: text_tools,
        max_tools: None,
        supports_parallel_tool_calls: false,
        server_parser: "unknown".to_string(),
        tool_search: Vec::new(),
        batch_api: false,
        batch_wire_format: None,
        batch_input_mode: None,
        batch_discount_percent: None,
        provider_rate_limits: false,
        model_rate_limits: false,
        provider_rpm: None,
        pricing: false,
        provider_latency_p50_ms: None,
    }
}
