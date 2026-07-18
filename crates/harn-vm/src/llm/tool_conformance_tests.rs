use std::collections::BTreeMap;

use super::request::{probe_request_body, probe_request_payload};
use super::*;

#[test]
fn probe_resolves_catalog_key_to_provider_wire_model() {
    let resolved = llm_config::resolve_model_info("baseten-glm-5.2");
    assert_eq!(resolved_probe_model_id(&resolved.id), "zai-org/GLM-5.2");
}

#[test]
fn probe_payload_applies_provider_qualified_model_defaults() {
    let _guard = crate::llm::env_guard();
    let mut overlay = llm_config::ProvidersConfig::default();
    overlay.model_defaults.insert(
        "probe-provider/wire-model".to_string(),
        BTreeMap::from_iter([
            ("max_tokens".to_string(), toml::Value::Integer(321)),
            ("temperature".to_string(), toml::Value::Float(1.0)),
            ("top_p".to_string(), toml::Value::Float(0.9)),
            ("top_k".to_string(), toml::Value::Integer(40)),
        ]),
    );
    llm_config::set_user_overrides(Some(overlay));

    let payload = probe_request_payload(
        "probe-provider",
        "wire-model",
        ToolProbeMode::NonStreaming,
        ToolProbeCase::SingleToolCall,
        ToolProbeRequestProfile::CatalogDefault,
        DEFAULT_TOOL_PROBE_MARKER,
    )
    .expect("probe payload");

    assert_eq!(payload.max_tokens, 321);
    assert_eq!(payload.temperature, Some(1.0));
    assert_eq!(payload.top_p, Some(0.9));
    assert_eq!(payload.top_k, Some(40));

    llm_config::clear_user_overrides();
}

#[test]
fn request_report_materializes_large_string_case_without_provider_call() {
    let report = tool_conformance_request_report(
        "openai",
        "gpt-5.4-mini",
        None,
        vec![ToolProbeMode::NonStreaming, ToolProbeMode::Streaming],
        ToolProbeCase::LargeStringArgument,
        ToolProbeRequestProfile::CatalogDefault,
        "case-seed",
    )
    .expect("request report");

    assert_eq!(
        report.schema_version,
        TOOL_CONFORMANCE_REQUEST_SCHEMA_VERSION
    );
    assert_eq!(report.probe_case, ToolProbeCase::LargeStringArgument);
    assert_eq!(report.marker, "case-seed");
    assert!(
        report.expected_value.contains("heredoc=<<EOF"),
        "expected value should exercise heredoc-shaped content: {}",
        report.expected_value
    );
    assert_eq!(report.requests.len(), 2);
    assert_eq!(report.requests[0].mode, ToolProbeMode::NonStreaming);
    assert_eq!(report.requests[1].mode, ToolProbeMode::Streaming);
    assert_eq!(
        report.requests[0].validation.status,
        ToolConformanceRequestValidationStatus::Pass
    );
    assert_eq!(report.requests[0].validation.dialect, "openai_compat");
    assert_eq!(
        report.requests[0].request_body["tools"][0]["type"],
        "function"
    );
    assert_eq!(
        report.requests[0].request_body["tool_choice"],
        json!({"type": "function", "function": {"name": TOOL_PROBE_TOOL_NAME}})
    );
    let prompt = report.requests[0].request_body["messages"][0]["content"]
        .as_str()
        .expect("prompt content");
    assert!(
        prompt.contains("heredoc=<<EOF"),
        "prompt should carry the exact large string request: {prompt}"
    );
    assert!(
        report.requests[0].request_body.get("stream").is_none(),
        "OpenAI-compatible request bodies omit the stream flag here; the report mode is the transport contract"
    );
    assert!(
        report.requests[1].request_body.get("stream").is_none(),
        "OpenAI-compatible request bodies omit the stream flag here; the report mode is the transport contract"
    );
}

#[test]
fn request_catalog_audit_validates_every_catalog_route_in_process() {
    let report = tool_conformance_request_catalog_audit(
        ToolProbeCase::catalog_request_audit_cases(),
        ToolProbeRequestProfile::catalog_request_audit_profiles(),
        vec![ToolProbeMode::NonStreaming, ToolProbeMode::Streaming],
    );

    assert_eq!(
        report.schema_version,
        TOOL_CONFORMANCE_REQUEST_AUDIT_SCHEMA_VERSION
    );
    assert!(
        report.catalog_model_count >= 20,
        "catalog unexpectedly tiny: {}",
        report.catalog_model_count
    );
    assert_eq!(
        report.route_count,
        crate::provider_catalog::artifact().routing_routes.len()
    );
    assert_eq!(
        report.probe_cases,
        vec![
            "single_tool_call",
            "parallel_tool_calls",
            "large_string_argument",
            "tool_result_followup",
            "signed_thinking_tool_result_followup",
            "no_tool_answer_or_refusal",
            "unavailable_tool_repair",
            "done_sentinel",
        ]
    );
    assert_eq!(report.request_profiles.len(), 2);
    assert_eq!(report.modes.len(), 2);
    assert_eq!(
        report.request_count,
        report.route_count
            * report.probe_cases.len()
            * report.request_profiles.len()
            * report.modes.len()
    );
    assert_eq!(report.validation_fail_count, 0, "{:#?}", report.failures);
    assert_eq!(
        report.warning_count,
        report
            .warnings
            .iter()
            .map(|row| row.warnings.len())
            .sum::<usize>(),
        "{:#?}",
        report.warnings
    );
    assert!(
        report.warnings.iter().any(|row| row.provider == "anthropic"
            && row.warnings.iter().any(|warning| matches!(
                warning,
                ToolConformanceRequestWarning::SamplingParamsOmitted { .. }
            ))),
        "request audit should surface Anthropic sampling sanitization warnings: {:#?}",
        report.warnings
    );
    assert_eq!(
        report.request_count,
        report.validation_pass_count + report.not_applicable_count
    );
    assert_eq!(
        report.not_applicable_count,
        report.not_applicable.len(),
        "{:#?}",
        report.not_applicable
    );
    assert!(
        report.not_applicable.iter().any(|row| row.probe_case
            == "signed_thinking_tool_result_followup"),
        "default zero-network request audit should include signed-thinking not_applicable rows: {:#?}",
        report.not_applicable
    );
    assert_eq!(report.failures.len(), 0, "{:#?}", report.failures);
    assert!(report.dialect_counts.contains_key("openai_compat"));
    assert!(report.dialect_counts.contains_key("anthropic"));
}

#[test]
fn request_catalog_audit_marks_unsupported_signed_thinking_routes_not_applicable() {
    let report = tool_conformance_request_catalog_audit(
        vec![ToolProbeCase::SignedThinkingToolResultFollowup],
        ToolProbeRequestProfile::catalog_request_audit_profiles(),
        vec![ToolProbeMode::NonStreaming, ToolProbeMode::Streaming],
    );

    assert_eq!(report.validation_fail_count, 0, "{:#?}", report.failures);
    assert!(
        report.validation_pass_count > 0,
        "native signed-thinking routes should validate: {report:#?}"
    );
    assert!(
        report.not_applicable_count > 0,
        "non-native signed-thinking routes should be counted separately: {report:#?}"
    );
    assert_eq!(
        report.request_count,
        report.validation_pass_count + report.not_applicable_count
    );
    assert!(
        report
            .not_applicable
            .iter()
            .any(|row| row.dialect == "openai_compat"
                && row.probe_case == "signed_thinking_tool_result_followup"),
        "expected OpenAI-compatible signed-thinking rows to be not applicable: {report:#?}"
    );
    assert!(
        report.not_applicable.iter().any(|row| row
            .reason
            .contains("route has no signed-thinking tool-history surface")),
        "signed-thinking not_applicable rows should use the shared scorecard capability predicate: {report:#?}"
    );
}

#[test]
fn no_tool_request_case_omits_tool_declarations() {
    let body = probe_request_body(
        "openai",
        "gpt-5.4-mini",
        ToolProbeMode::NonStreaming,
        ToolProbeCase::NoToolAnswerOrRefusal,
        ToolProbeRequestProfile::CatalogDefault,
        "direct_answer:case",
    )
    .expect("no-tool probe body");

    assert!(body.get("tools").is_none(), "{body}");
    assert!(body.get("tool_choice").is_none(), "{body}");
    let prompt = body["messages"][0]["content"].as_str().unwrap();
    assert!(prompt.contains("direct_answer:case"), "{prompt}");

    let validation = validate_probe_request_body(
        "openai",
        "gpt-5.4-mini",
        ToolProbeCase::NoToolAnswerOrRefusal,
        ToolProbeRequestProfile::CatalogDefault,
        &body,
    );
    assert_eq!(
        validation.status,
        ToolConformanceRequestValidationStatus::Pass,
        "{:?}",
        validation.issues
    );
}

#[test]
fn probe_request_body_uses_anthropic_tool_dialect() {
    let body = probe_request_body(
        "anthropic",
        "claude-sonnet-4-6",
        ToolProbeMode::NonStreaming,
        ToolProbeCase::SingleToolCall,
        ToolProbeRequestProfile::CatalogDefault,
        DEFAULT_TOOL_PROBE_MARKER,
    )
    .expect("Anthropic probe body");

    assert_eq!(
        body["tools"][0]["name"], TOOL_PROBE_TOOL_NAME,
        "Anthropic tools use root-level names, not OpenAI function wrappers"
    );
    assert!(
        body["tools"][0].get("function").is_none(),
        "probe must not send OpenAI function-wrapper tools to Anthropic"
    );
    assert_eq!(
        body["tools"][0]["input_schema"]["properties"]["value"]["type"],
        "string"
    );
    assert_eq!(
        body["tool_choice"],
        json!({"type": "tool", "name": TOOL_PROBE_TOOL_NAME}),
        "Anthropic rejects OpenAI-shaped tool_choice objects"
    );

    let validation = validate_probe_request_body(
        "anthropic",
        "claude-sonnet-4-6",
        ToolProbeCase::SingleToolCall,
        ToolProbeRequestProfile::CatalogDefault,
        &body,
    );
    assert_eq!(
        validation.status,
        ToolConformanceRequestValidationStatus::Pass
    );
    assert_eq!(validation.dialect, "anthropic");
}

#[test]
fn probe_request_body_preserves_openai_tool_dialect() {
    let body = probe_request_body(
        "openai",
        "gpt-5.4-mini",
        ToolProbeMode::NonStreaming,
        ToolProbeCase::SingleToolCall,
        ToolProbeRequestProfile::CatalogDefault,
        DEFAULT_TOOL_PROBE_MARKER,
    )
    .expect("OpenAI probe body");

    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["name"], TOOL_PROBE_TOOL_NAME);
    assert_eq!(
        body["tool_choice"],
        json!({"type": "function", "function": {"name": TOOL_PROBE_TOOL_NAME}})
    );

    let validation = validate_probe_request_body(
        "openai",
        "gpt-5.4-mini",
        ToolProbeCase::SingleToolCall,
        ToolProbeRequestProfile::CatalogDefault,
        &body,
    );
    assert_eq!(
        validation.status,
        ToolConformanceRequestValidationStatus::Pass
    );
    assert_eq!(validation.dialect, "openai_compat");
}

#[test]
fn probe_request_body_accepts_openai_compat_allowed_scalar_tool_choice() {
    let body = probe_request_body(
        "openrouter",
        "moonshotai/kimi-k2.7-code",
        ToolProbeMode::NonStreaming,
        ToolProbeCase::SingleToolCall,
        ToolProbeRequestProfile::CatalogDefault,
        DEFAULT_TOOL_PROBE_MARKER,
    )
    .expect("OpenRouter Kimi probe body");

    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["name"], TOOL_PROBE_TOOL_NAME);
    assert_eq!(
        body["tool_choice"], "auto",
        "catalog constrains this route to scalar auto/none tool_choice modes"
    );

    let validation = validate_probe_request_body(
        "openrouter",
        "moonshotai/kimi-k2.7-code",
        ToolProbeCase::SingleToolCall,
        ToolProbeRequestProfile::CatalogDefault,
        &body,
    );
    assert_eq!(
        validation.status,
        ToolConformanceRequestValidationStatus::Pass
    );
    assert_eq!(validation.dialect, "openai_compat");
}

#[test]
fn probe_request_body_uses_llamacpp_scalar_required_tool_choice() {
    let body = probe_request_body(
        "llamacpp",
        "qwen3.6-35b-a3b-ud-q4-k-xl",
        ToolProbeMode::NonStreaming,
        ToolProbeCase::SingleToolCall,
        ToolProbeRequestProfile::CatalogDefault,
        DEFAULT_TOOL_PROBE_MARKER,
    )
    .expect("llama.cpp probe body");

    assert_eq!(body["tools"][0]["function"]["name"], TOOL_PROBE_TOOL_NAME);
    assert_eq!(
        body["tool_choice"], "required",
        "llama.cpp rejects OpenAI named-tool objects but the probe exposes exactly one tool"
    );
    let validation = validate_probe_request_body(
        "llamacpp",
        "qwen3.6-35b-a3b-ud-q4-k-xl",
        ToolProbeCase::SingleToolCall,
        ToolProbeRequestProfile::CatalogDefault,
        &body,
    );
    assert_eq!(
        validation.status,
        ToolConformanceRequestValidationStatus::Pass
    );
}

#[test]
fn probe_request_body_accepts_unrestricted_parameter_edge_tool_choice() {
    let body = probe_request_body(
        "fireworks",
        "accounts/fireworks/models/gpt-oss-120b",
        ToolProbeMode::NonStreaming,
        ToolProbeCase::SingleToolCall,
        ToolProbeRequestProfile::ParameterEdges,
        DEFAULT_TOOL_PROBE_MARKER,
    )
    .expect("Fireworks parameter-edge probe body");

    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["name"], TOOL_PROBE_TOOL_NAME);
    assert_eq!(
        body["tool_choice"], "required",
        "empty allowed_tool_choice_modes means unrestricted OpenAI-compatible scalar modes"
    );
    assert_eq!(body["temperature"], 2.0);
    assert_eq!(body["max_tokens"], 1);

    let validation = validate_probe_request_body(
        "fireworks",
        "accounts/fireworks/models/gpt-oss-120b",
        ToolProbeCase::SingleToolCall,
        ToolProbeRequestProfile::ParameterEdges,
        &body,
    );
    assert_eq!(
        validation.status,
        ToolConformanceRequestValidationStatus::Pass,
        "{:?}",
        validation.issues
    );
    assert_eq!(validation.dialect, "openai_compat");
}

#[test]
fn probe_request_body_maps_gemini_tool_choice_to_tool_config() {
    let body = probe_request_body(
        "gemini",
        "gemini-2.5-pro",
        ToolProbeMode::NonStreaming,
        ToolProbeCase::SingleToolCall,
        ToolProbeRequestProfile::CatalogDefault,
        DEFAULT_TOOL_PROBE_MARKER,
    )
    .expect("Gemini probe body");
    assert_eq!(
        body["tools"][0]["functionDeclarations"][0]["name"],
        TOOL_PROBE_TOOL_NAME
    );
    assert_eq!(body["toolConfig"]["functionCallingConfig"]["mode"], "ANY");
    assert_eq!(
        body["toolConfig"]["functionCallingConfig"]["allowedFunctionNames"],
        json!([TOOL_PROBE_TOOL_NAME])
    );

    let validation = validate_probe_request_body(
        "gemini",
        "gemini-2.5-pro",
        ToolProbeCase::SingleToolCall,
        ToolProbeRequestProfile::CatalogDefault,
        &body,
    );
    assert_eq!(
        validation.status,
        ToolConformanceRequestValidationStatus::Pass
    );
    assert_eq!(validation.dialect, "gemini");
}

#[test]
fn probe_request_body_validates_ollama_tool_dialect_without_tool_choice() {
    let body = probe_request_body(
        "ollama",
        "gemma4:26b",
        ToolProbeMode::NonStreaming,
        ToolProbeCase::SingleToolCall,
        ToolProbeRequestProfile::CatalogDefault,
        DEFAULT_TOOL_PROBE_MARKER,
    )
    .expect("Ollama probe body");

    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["name"], TOOL_PROBE_TOOL_NAME);
    assert!(body.get("tool_choice").is_none());

    let validation = validate_probe_request_body(
        "ollama",
        "gemma4:26b",
        ToolProbeCase::SingleToolCall,
        ToolProbeRequestProfile::CatalogDefault,
        &body,
    );
    assert_eq!(
        validation.status,
        ToolConformanceRequestValidationStatus::Pass
    );
    assert_eq!(validation.dialect, "ollama");
}

#[test]
fn request_validation_reports_provider_dialect_mismatches() {
    let body = json!({
        "messages": [{"role": "user", "content": "call the tool"}],
        "tools": [{"name": TOOL_PROBE_TOOL_NAME, "input_schema": {"type": "object"}}],
        "toolConfig": {"functionCallingConfig": {"mode": "ANY"}}
    });

    let validation = validate_probe_request_body(
        "openai",
        "gpt-5.4-mini",
        ToolProbeCase::SingleToolCall,
        ToolProbeRequestProfile::CatalogDefault,
        &body,
    );
    assert_eq!(
        validation.status,
        ToolConformanceRequestValidationStatus::Fail
    );
    assert_eq!(validation.dialect, "openai_compat");
    assert!(
        validation
            .issues
            .iter()
            .any(|issue| issue.contains("tool type")),
        "{:?}",
        validation.issues
    );
    assert!(
        validation
            .issues
            .iter()
            .any(|issue| issue.contains("must not include /toolConfig")),
        "{:?}",
        validation.issues
    );
}

#[test]
fn tool_conformance_report_accepts_compact_empty_diagnostics() {
    let report: ToolConformanceReport = serde_json::from_str(
        concat!(
            r#"{"schema_version":1,"provider":"groq","model":"openai/gpt-oss-120b","#,
            r#""tool_name":"echo_marker","marker":"harn_tool_probe_marker","cases":["#,
            r#"{"mode":"non_streaming","ok":true,"classification":"structured_native_tool_call","#,
            r#""fallback_mode":"native","native_tool_call_count":1,"text_tool_call_count":0}],"#,
            r#""tool_calling":{"native":"pass","text":"unknown","streaming_native":"unknown","fallback_mode":"native"}}"#,
        ),
    )
    .expect("compact v1 reports should deserialize");

    assert!(report.cases[0].parser_errors.is_empty());
    assert!(report.cases[0].protocol_violations.is_empty());
    assert!(report.cases[0].usage.is_none());

    let serialized = serde_json::to_value(&report).expect("serialize report");
    assert!(
        serialized["cases"][0].get("parser_errors").is_none(),
        "empty parser diagnostics stay compact on write"
    );
    assert!(
        serialized["cases"][0].get("protocol_violations").is_none(),
        "empty protocol diagnostics stay compact on write"
    );
}

#[test]
fn tool_probe_fixture_preserves_reported_usage_and_priced_cost() {
    let response = native_tool_call_fixture_with_usage(serde_json::json!({
        "prompt_tokens": 1200,
        "completion_tokens": 75,
    }));
    let report = classify_tool_conformance_fixture(
        "openai",
        "gpt-4.1",
        ToolProbeMode::NonStreaming,
        DEFAULT_TOOL_PROBE_MARKER,
        &response,
    );

    let usage = report.cases[0]
        .usage
        .as_ref()
        .expect("reported usage should be preserved");
    assert_eq!(usage.input_tokens, Some(1200));
    assert_eq!(usage.output_tokens, Some(75));
    assert!(
        usage.cost_usd.unwrap_or(0.0) > 0.0,
        "priced catalog route should carry non-zero cost: {usage:?}"
    );
}

#[test]
fn tool_probe_fixture_preserves_usage_without_fabricating_unpriced_cost() {
    let response = native_tool_call_fixture_with_usage(serde_json::json!({
        "input_tokens": 9,
        "output_tokens": 3,
    }));
    let report = classify_tool_conformance_fixture(
        "ghost-provider",
        "ghost-model",
        ToolProbeMode::NonStreaming,
        DEFAULT_TOOL_PROBE_MARKER,
        &response,
    );

    let usage = report.cases[0]
        .usage
        .as_ref()
        .expect("reported usage should be preserved");
    assert_eq!(usage.input_tokens, Some(9));
    assert_eq!(usage.output_tokens, Some(3));
    assert_eq!(usage.cost_usd, None);
}

#[test]
fn tool_probe_fixture_preserves_gemini_usage_metadata_with_thoughts() {
    let response = native_tool_call_fixture_with_usage_metadata(serde_json::json!({
        "promptTokenCount": 12,
        "candidatesTokenCount": 5,
        "thoughtsTokenCount": 7,
    }));
    let report = classify_tool_conformance_fixture(
        "gemini",
        "gemini-2.5-pro",
        ToolProbeMode::NonStreaming,
        DEFAULT_TOOL_PROBE_MARKER,
        &response,
    );

    let usage = report.cases[0]
        .usage
        .as_ref()
        .expect("reported Gemini usage should be preserved");
    assert_eq!(usage.input_tokens, Some(12));
    assert_eq!(usage.output_tokens, Some(12));
}

fn native_tool_call_fixture_with_usage(usage: serde_json::Value) -> String {
    let mut fixture = native_tool_call_fixture_base();
    fixture["usage"] = usage;
    fixture.to_string()
}

fn native_tool_call_fixture_with_usage_metadata(usage_metadata: serde_json::Value) -> String {
    let mut fixture = native_tool_call_fixture_base();
    fixture["usageMetadata"] = usage_metadata;
    fixture.to_string()
}

fn native_tool_call_fixture_base() -> serde_json::Value {
    serde_json::json!({
        "choices": [{
            "message": {
                "tool_calls": [{
                    "id": "call_1",
                    "type": "function",
                    "function": {
                        "name": "echo_marker",
                        "arguments": serde_json::json!({
                            "value": DEFAULT_TOOL_PROBE_MARKER,
                        })
                        .to_string(),
                    },
                }],
            },
        }],
    })
}

#[test]
fn classify_openai_native_tool_call_as_pass() {
    let report = classify_tool_conformance_fixture(
        "local",
        "model",
        ToolProbeMode::NonStreaming,
        DEFAULT_TOOL_PROBE_MARKER,
        r#"{"choices":[{"message":{"tool_calls":[{"id":"call_1","type":"function","function":{"name":"echo_marker","arguments":"{\"value\":\"harn_tool_probe_marker\"}"}}]}}]}"#,
    );
    assert_eq!(report.tool_calling.native, ToolProbeStatus::Pass);
    assert_eq!(
        report.tool_calling.fallback_mode,
        ToolProbeFallbackMode::Native
    );
    assert_eq!(
        report.cases[0].classification,
        ToolProbeClassification::StructuredNativeToolCall
    );
}

#[test]
fn classify_anthropic_tool_use_as_native_pass() {
    let report = classify_tool_conformance_fixture(
        "anthropic",
        "claude-sonnet-4-6",
        ToolProbeMode::NonStreaming,
        DEFAULT_TOOL_PROBE_MARKER,
        r#"{"content":[{"type":"tool_use","id":"toolu_1","name":"echo_marker","input":{"value":"harn_tool_probe_marker"}}],"stop_reason":"tool_use"}"#,
    );

    assert_eq!(report.tool_calling.native, ToolProbeStatus::Pass);
    assert_eq!(
        report.tool_calling.fallback_mode,
        ToolProbeFallbackMode::Native
    );
    assert_eq!(
        report.cases[0].classification,
        ToolProbeClassification::StructuredNativeToolCall
    );
}

#[test]
fn classify_parallel_native_tool_calls_as_pass() {
    let raw = serde_json::json!({
        "choices": [{
            "message": {
                "tool_calls": [
                    {
                        "id": "call_1",
                        "type": "function",
                        "function": {
                            "name": "echo_marker",
                            "arguments": serde_json::json!({
                                "value": "harn_tool_probe_marker:first"
                            }).to_string(),
                        },
                    },
                    {
                        "id": "call_2",
                        "type": "function",
                        "function": {
                            "name": "echo_marker",
                            "arguments": serde_json::json!({
                                "value": "harn_tool_probe_marker:second"
                            }).to_string(),
                        },
                    },
                ],
            },
        }],
    })
    .to_string();
    let report = classify_tool_conformance_fixture_for_case(
        "local",
        "model",
        ToolProbeMode::NonStreaming,
        ToolProbeCase::ParallelToolCalls,
        DEFAULT_TOOL_PROBE_MARKER,
        &raw,
    );

    assert_eq!(report.tool_calling.native, ToolProbeStatus::Pass);
    assert_eq!(
        report.tool_calling.fallback_mode,
        ToolProbeFallbackMode::Native
    );
    assert_eq!(report.cases[0].native_tool_call_count, 2);
    assert_eq!(
        report.cases[0].classification,
        ToolProbeClassification::StructuredNativeToolCall
    );
}

#[test]
fn classify_parallel_text_tool_calls_as_fallback_pass() {
    let first = serde_json::json!({
        "name": "echo_marker",
        "arguments": {"value": "harn_tool_probe_marker:first"},
    });
    let second = serde_json::json!({
        "name": "echo_marker",
        "arguments": {"value": "harn_tool_probe_marker:second"},
    });
    let content = [
        "<tool_call>",
        &first.to_string(),
        "</tool_call>\n<tool_call>",
        &second.to_string(),
        "</tool_call>",
    ]
    .concat();
    let raw = serde_json::json!({
        "content": content,
    })
    .to_string();
    let report = classify_tool_conformance_fixture_for_case(
        "local",
        "model",
        ToolProbeMode::NonStreaming,
        ToolProbeCase::ParallelToolCalls,
        DEFAULT_TOOL_PROBE_MARKER,
        &raw,
    );

    assert_eq!(report.tool_calling.native, ToolProbeStatus::Fail);
    assert_eq!(report.tool_calling.text, ToolProbeStatus::Pass);
    assert_eq!(
        report.tool_calling.fallback_mode,
        ToolProbeFallbackMode::Text
    );
    assert_eq!(report.cases[0].text_tool_call_count, 2);
    assert_eq!(
        report.cases[0].classification,
        ToolProbeClassification::ParseableHarnTextToolCall
    );
}

#[test]
fn classify_single_native_tool_call_rejects_duplicate_calls() {
    let raw = serde_json::json!({
        "choices": [{
            "message": {
                "tool_calls": [
                    {
                        "type": "function",
                        "function": {
                            "name": "echo_marker",
                            "arguments": serde_json::json!({
                                "value": DEFAULT_TOOL_PROBE_MARKER
                            }).to_string(),
                        },
                    },
                    {
                        "type": "function",
                        "function": {
                            "name": "echo_marker",
                            "arguments": serde_json::json!({
                                "value": DEFAULT_TOOL_PROBE_MARKER
                            }).to_string(),
                        },
                    },
                ],
            },
        }],
    })
    .to_string();
    let report = classify_tool_conformance_fixture_for_case(
        "local",
        "model",
        ToolProbeMode::NonStreaming,
        ToolProbeCase::SingleToolCall,
        DEFAULT_TOOL_PROBE_MARKER,
        &raw,
    );

    assert!(!report.cases[0].ok, "{:#?}", report.cases[0]);
    assert_eq!(report.cases[0].native_tool_call_count, 2);
    assert_eq!(
        report.cases[0].failure_reason.as_deref(),
        Some("expected_1_native_tool_calls_got_2")
    );
}

#[test]
fn classify_single_text_tool_call_rejects_duplicate_calls() {
    let call = serde_json::json!({
        "name": "echo_marker",
        "arguments": {"value": DEFAULT_TOOL_PROBE_MARKER},
    });
    let content = [
        "<tool_call>",
        &call.to_string(),
        "</tool_call>\n<tool_call>",
        &call.to_string(),
        "</tool_call>",
    ]
    .concat();
    let raw = serde_json::json!({"content": content}).to_string();
    let report = classify_tool_conformance_fixture_for_case(
        "local",
        "model",
        ToolProbeMode::NonStreaming,
        ToolProbeCase::SingleToolCall,
        DEFAULT_TOOL_PROBE_MARKER,
        &raw,
    );

    assert!(!report.cases[0].ok, "{:#?}", report.cases[0]);
    assert_eq!(report.cases[0].text_tool_call_count, 2);
    assert_eq!(
        report.cases[0].failure_reason.as_deref(),
        Some("expected_1_text_tool_calls_got_2")
    );
}

#[test]
fn classify_parallel_tool_calls_requires_both_values() {
    let report = classify_tool_conformance_fixture_for_case(
        "local",
        "model",
        ToolProbeMode::NonStreaming,
        ToolProbeCase::ParallelToolCalls,
        DEFAULT_TOOL_PROBE_MARKER,
        r#"{"choices":[{"message":{"tool_calls":[{"id":"call_1","type":"function","function":{"name":"echo_marker","arguments":"{\"value\":\"harn_tool_probe_marker:first\"}"}}]}}]}"#,
    );

    assert_eq!(report.tool_calling.native, ToolProbeStatus::Fail);
    assert_eq!(
        report.cases[0].classification,
        ToolProbeClassification::ProseOnlyNonTool
    );
    assert_eq!(
        report.cases[0].failure_reason.as_deref(),
        Some("no_executable_tool_call")
    );
}

#[test]
fn classify_native_tool_call_with_text_call_in_name_as_pass() {
    let report = classify_tool_conformance_fixture(
        "zai",
        "glm-5",
        ToolProbeMode::NonStreaming,
        DEFAULT_TOOL_PROBE_MARKER,
        r#"{"choices":[{"message":{"tool_calls":[{"id":"call_1","type":"function","function":{"name":"echo_marker({ value: \"harn_tool_probe_marker\" })</arg_value>","arguments":"{}"}}]}}]}"#,
    );

    assert_eq!(report.tool_calling.native, ToolProbeStatus::Pass);
    assert_eq!(
        report.tool_calling.fallback_mode,
        ToolProbeFallbackMode::Native
    );
    assert_eq!(
        report.cases[0].classification,
        ToolProbeClassification::StructuredNativeToolCall
    );
}

#[test]
fn classify_partial_text_call_in_native_name_as_malformed() {
    let report = classify_tool_conformance_fixture(
        "zai",
        "glm-5",
        ToolProbeMode::NonStreaming,
        DEFAULT_TOOL_PROBE_MARKER,
        r#"{"choices":[{"message":{"tool_calls":[{"id":"call_1","type":"function","function":{"name":"echo_marker({ value: <<EOF","arguments":"{"}}]}}]}"#,
    );

    assert_eq!(report.tool_calling.native, ToolProbeStatus::Fail);
    assert_eq!(
        report.cases[0].classification,
        ToolProbeClassification::MalformedJsonArguments
    );
}

#[test]
fn classify_gemma_raw_json_tool_call_content_as_text_fallback() {
    let report = classify_tool_conformance_fixture(
        "ollama",
        "gemma4:26b",
        ToolProbeMode::NonStreaming,
        DEFAULT_TOOL_PROBE_MARKER,
        r#"{"message":{"content":"<tool_call>{\"name\":\"echo_marker\",\"arguments\":{\"value\":\"harn_tool_probe_marker\"}}</tool_call>"}}"#,
    );
    assert_eq!(report.tool_calling.native, ToolProbeStatus::Fail);
    assert_eq!(report.tool_calling.text, ToolProbeStatus::Pass);
    assert_eq!(
        report.tool_calling.fallback_mode,
        ToolProbeFallbackMode::Text
    );
    assert_eq!(
        report.cases[0].classification,
        ToolProbeClassification::ParseableHarnTextToolCall
    );
}

#[test]
fn classify_qwen_call_colon_marker_as_text_fallback() {
    let report = classify_tool_conformance_fixture(
        "llamacpp",
        "qwen",
        ToolProbeMode::NonStreaming,
        DEFAULT_TOOL_PROBE_MARKER,
        r#"{"content":"call:echo_marker{ value: \"harn_tool_probe_marker\" }"}"#,
    );
    assert_eq!(report.tool_calling.text, ToolProbeStatus::Pass);
    assert_eq!(
        report.tool_calling.fallback_mode,
        ToolProbeFallbackMode::Text
    );
}

#[test]
fn structured_reasoning_tool_text_does_not_satisfy_probe() {
    let report = classify_tool_conformance_fixture(
        "local",
        "model",
        ToolProbeMode::NonStreaming,
        DEFAULT_TOOL_PROBE_MARKER,
        r#"{"choices":[{"message":{"content":[{"type":"reasoning","text":"<tool_call>{\"name\":\"echo_marker\",\"arguments\":{\"value\":\"harn_tool_probe_marker\"}}</tool_call>"}]}}]}"#,
    );

    assert_ne!(report.tool_calling.text, ToolProbeStatus::Pass);
    assert_eq!(
        report.tool_calling.fallback_mode,
        ToolProbeFallbackMode::Disabled
    );
    assert!(!report_satisfies_required_probe(&report, "tool_probe"));
}

#[test]
fn reasoning_field_tool_text_does_not_satisfy_probe() {
    let report = classify_tool_conformance_fixture(
        "local",
        "model",
        ToolProbeMode::NonStreaming,
        DEFAULT_TOOL_PROBE_MARKER,
        r#"{"choices":[{"message":{"content":"","reasoning":{"content":"call:echo_marker{ value: \"harn_tool_probe_marker\" }"}}}]}"#,
    );

    assert_ne!(report.tool_calling.text, ToolProbeStatus::Pass);
    assert_eq!(
        report.tool_calling.fallback_mode,
        ToolProbeFallbackMode::Disabled
    );
    assert!(!report_satisfies_required_probe(&report, "tool_probe"));
}

#[test]
fn classify_prose_only_as_disabled() {
    let report = classify_tool_conformance_fixture(
        "ollama",
        "gemma4:26b",
        ToolProbeMode::NonStreaming,
        DEFAULT_TOOL_PROBE_MARKER,
        r#"{"message":{"content":"The comment has been added. I will now verify it."}}"#,
    );
    assert_eq!(
        report.tool_calling.fallback_mode,
        ToolProbeFallbackMode::Disabled
    );
    assert_eq!(
        report.cases[0].classification,
        ToolProbeClassification::ProseOnlyNonTool
    );
    assert_eq!(
        report.cases[0].failure_reason.as_deref(),
        Some("no_executable_tool_call")
    );
}

#[test]
fn prose_only_response_does_not_satisfy_required_tool_probe() {
    let report = classify_tool_conformance_fixture(
        "anthropic",
        "claude-sonnet-4-6",
        ToolProbeMode::NonStreaming,
        DEFAULT_TOOL_PROBE_MARKER,
        r#"{"content":[{"type":"text","text":"I can call echo_marker with that value."}],"stop_reason":"end_turn"}"#,
    );

    assert_eq!(
        report.cases[0].classification,
        ToolProbeClassification::ProseOnlyNonTool
    );
    assert!(!report_satisfies_required_probe(&report, "tool_probe"));
    assert_eq!(
        report.tool_calling.fallback_mode,
        ToolProbeFallbackMode::Disabled
    );
}

#[test]
fn no_tool_fixture_passes_without_tool_calls() {
    let report = classify_tool_conformance_fixture_for_case(
        "openai",
        "gpt-5.4-mini",
        ToolProbeMode::NonStreaming,
        ToolProbeCase::NoToolAnswerOrRefusal,
        "case",
        r#"{"choices":[{"message":{"content":"direct_answer:case"}}]}"#,
    );

    assert!(report.cases[0].ok, "{:?}", report.cases[0]);
    assert_eq!(
        report.cases[0].classification,
        ToolProbeClassification::DirectAnswerNoTool
    );
}

#[test]
fn done_sentinel_fixture_passes_only_as_text() {
    let report = classify_tool_conformance_fixture_for_case(
        "anthropic",
        "claude-sonnet-5",
        ToolProbeMode::NonStreaming,
        ToolProbeCase::DoneSentinel,
        "##DONE##",
        r#"{"content":[{"type":"text","text":"<done>##DONE##</done>"}]}"#,
    );

    assert!(report.cases[0].ok, "{:?}", report.cases[0]);
    assert_eq!(
        report.cases[0].classification,
        ToolProbeClassification::DoneSentinel
    );
}

#[test]
fn tool_result_followup_fixture_passes_without_second_tool_call() {
    let report = classify_tool_conformance_fixture_for_case(
        "openai",
        "gpt-5.4-mini",
        ToolProbeMode::NonStreaming,
        ToolProbeCase::ToolResultFollowup,
        "case",
        r#"{"choices":[{"message":{"content":"tool_result_followup:case"}}]}"#,
    );

    assert!(report.cases[0].ok, "{:?}", report.cases[0]);
    assert_eq!(
        report.cases[0].classification,
        ToolProbeClassification::ProseOnlyNonTool
    );
}

#[test]
fn tool_result_followup_fixture_fails_on_spurious_second_tool_call() {
    let report = classify_tool_conformance_fixture_for_case(
        "openai",
        "gpt-5.4-mini",
        ToolProbeMode::NonStreaming,
        ToolProbeCase::ToolResultFollowup,
        "case",
        r#"{"choices":[{"message":{"tool_calls":[{"type":"function","function":{"name":"echo_marker","arguments":"{\"value\":\"case\"}"}}]}}]}"#,
    );

    assert!(!report.cases[0].ok, "{:?}", report.cases[0]);
    assert_eq!(
        report.cases[0].classification,
        ToolProbeClassification::RawModelToolTag
    );
}

#[test]
fn unavailable_tool_fixture_fails_on_spurious_tool_call() {
    let report = classify_tool_conformance_fixture_for_case(
        "local",
        "model",
        ToolProbeMode::NonStreaming,
        ToolProbeCase::UnavailableToolRepair,
        "case",
        r#"{"choices":[{"message":{"tool_calls":[{"type":"function","function":{"name":"echo_marker","arguments":"{\"value\":\"case\"}"}}]}}]}"#,
    );

    assert!(!report.cases[0].ok, "{:?}", report.cases[0]);
    assert_eq!(
        report.cases[0].classification,
        ToolProbeClassification::RawModelToolTag
    );
    assert_eq!(
        report.cases[0].failure_reason.as_deref(),
        Some("unexpected_tool_call")
    );
}

#[test]
fn aggregates_openai_streaming_tool_call_deltas() {
    let raw = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"echo_marker\",\"arguments\":\"{\\\"value\\\":\"}}]}}]}\n\
                   data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"harn_tool_probe_marker\\\"}\"}}]}}]}\n\
                   data: [DONE]\n";
    let response = aggregate_stream_text(raw, "local");
    assert_eq!(response["frames"].as_array().map(Vec::len), Some(2));
    let case = classify_tool_probe_response(
        ToolProbeMode::Streaming,
        &response,
        ToolProbeCase::SingleToolCall,
        DEFAULT_TOOL_PROBE_MARKER,
        None,
        None,
        None,
    );
    assert!(case.ok, "{case:?}");
    assert_eq!(
        case.classification,
        ToolProbeClassification::StructuredNativeToolCall
    );
    assert_eq!(case.native_tool_call_count, 1);
}

#[test]
fn aggregates_anthropic_streaming_tool_use_deltas() {
    let raw = "event: message_start\n\
                   data: {\"type\":\"message_start\",\"message\":{\"id\":\"msg_1\",\"type\":\"message\",\"role\":\"assistant\",\"content\":[]}}\n\
                   event: content_block_start\n\
                   data: {\"type\":\"content_block_start\",\"index\":0,\"content_block\":{\"type\":\"tool_use\",\"id\":\"toolu_1\",\"name\":\"echo_marker\",\"input\":{}}}\n\
                   event: content_block_delta\n\
                   data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"{\\\"value\\\":\"}}\n\
                   event: content_block_delta\n\
                   data: {\"type\":\"content_block_delta\",\"index\":0,\"delta\":{\"type\":\"input_json_delta\",\"partial_json\":\"\\\"harn_tool_probe_marker\\\"}\"}}\n\
                   event: message_delta\n\
                   data: {\"type\":\"message_delta\",\"delta\":{\"stop_reason\":\"tool_use\"}}\n\
                   event: message_stop\n\
                   data: {\"type\":\"message_stop\"}\n";
    let response = aggregate_stream_text(raw, "anthropic");
    let case = classify_tool_probe_response(
        ToolProbeMode::Streaming,
        &response,
        ToolProbeCase::SingleToolCall,
        DEFAULT_TOOL_PROBE_MARKER,
        None,
        None,
        None,
    );
    assert!(case.ok, "{case:?}");
    assert_eq!(
        case.classification,
        ToolProbeClassification::StructuredNativeToolCall
    );
    assert_eq!(case.native_tool_call_count, 1);
}

#[test]
fn report_satisfies_tool_probe_when_text_fallback_passes() {
    let report = classify_tool_conformance_fixture(
        "llamacpp",
        "qwen",
        ToolProbeMode::NonStreaming,
        DEFAULT_TOOL_PROBE_MARKER,
        r#"{"content":"echo_marker({ value: \"harn_tool_probe_marker\" })"}"#,
    );
    assert!(report_satisfies_required_probe(&report, "tool_probe"));
    assert!(!report_satisfies_required_probe(
        &report,
        "native_tool_probe"
    ));
}

#[test]
fn summary_requires_every_repeated_native_case_to_pass() {
    let summary = summarize_cases(&[
        probe_case(
            ToolProbeMode::NonStreaming,
            true,
            ToolProbeClassification::StructuredNativeToolCall,
        ),
        probe_case(
            ToolProbeMode::NonStreaming,
            false,
            ToolProbeClassification::ProseOnlyNonTool,
        ),
    ]);
    assert_eq!(summary.native, ToolProbeStatus::Fail);
    assert_eq!(summary.fallback_mode, ToolProbeFallbackMode::Disabled);
}

#[test]
fn summary_requires_every_repeated_text_case_to_pass() {
    let summary = summarize_cases(&[
        probe_case(
            ToolProbeMode::NonStreaming,
            true,
            ToolProbeClassification::ParseableHarnTextToolCall,
        ),
        probe_case(
            ToolProbeMode::NonStreaming,
            false,
            ToolProbeClassification::MalformedJsonArguments,
        ),
    ]);
    assert_eq!(summary.native, ToolProbeStatus::Fail);
    assert_eq!(summary.text, ToolProbeStatus::Fail);
    assert_eq!(summary.fallback_mode, ToolProbeFallbackMode::Disabled);
}

#[test]
fn summary_preserves_nonstreaming_text_fallback_when_streaming_fails() {
    let summary = summarize_cases(&[
        probe_case(
            ToolProbeMode::NonStreaming,
            true,
            ToolProbeClassification::ParseableHarnTextToolCall,
        ),
        probe_case(
            ToolProbeMode::Streaming,
            false,
            ToolProbeClassification::ProseOnlyNonTool,
        ),
    ]);
    assert_eq!(summary.native, ToolProbeStatus::Fail);
    assert_eq!(summary.streaming_native, ToolProbeStatus::Fail);
    assert_eq!(summary.text, ToolProbeStatus::Pass);
    assert_eq!(summary.fallback_mode, ToolProbeFallbackMode::Text);
}

fn probe_case(
    mode: ToolProbeMode,
    ok: bool,
    classification: ToolProbeClassification,
) -> ToolConformanceCase {
    let native_tool_call_count =
        usize::from(classification == ToolProbeClassification::StructuredNativeToolCall);
    let text_tool_call_count =
        usize::from(classification == ToolProbeClassification::ParseableHarnTextToolCall);
    ToolConformanceCase {
        mode,
        ok,
        classification,
        fallback_mode: ToolProbeFallbackMode::Disabled,
        failure_reason: None,
        http_status: None,
        elapsed_ms: None,
        native_tool_call_count,
        text_tool_call_count,
        usage: None,
        parser_errors: Vec::new(),
        protocol_violations: Vec::new(),
        content_sample: None,
    }
}
