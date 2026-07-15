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
fn probe_request_body_uses_anthropic_tool_dialect() {
    let body = probe_request_body(
        "anthropic",
        "claude-sonnet-4-6",
        ToolProbeMode::NonStreaming,
        ToolProbeCase::SingleToolCall,
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
}

#[test]
fn probe_request_body_preserves_openai_tool_dialect() {
    let body = probe_request_body(
        "openai",
        "gpt-5.4-mini",
        ToolProbeMode::NonStreaming,
        ToolProbeCase::SingleToolCall,
        DEFAULT_TOOL_PROBE_MARKER,
    )
    .expect("OpenAI probe body");

    assert_eq!(body["tools"][0]["type"], "function");
    assert_eq!(body["tools"][0]["function"]["name"], TOOL_PROBE_TOOL_NAME);
    assert_eq!(
        body["tool_choice"],
        json!({"type": "function", "function": {"name": TOOL_PROBE_TOOL_NAME}})
    );
}

#[test]
fn probe_request_body_maps_gemini_tool_choice_to_tool_config() {
    let body = probe_request_body(
        "gemini",
        "gemini-2.5-pro",
        ToolProbeMode::NonStreaming,
        ToolProbeCase::SingleToolCall,
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
fn aggregates_openai_streaming_tool_call_deltas() {
    let raw = "data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"id\":\"call_1\",\"function\":{\"name\":\"echo_marker\",\"arguments\":\"{\\\"value\\\":\"}}]}}]}\n\
                   data: {\"choices\":[{\"delta\":{\"tool_calls\":[{\"index\":0,\"function\":{\"arguments\":\"\\\"harn_tool_probe_marker\\\"}\"}}]}}]}\n\
                   data: [DONE]\n";
    let response = aggregate_stream_text(raw, "local");
    let case = classify_tool_probe_response(
        ToolProbeMode::Streaming,
        &response,
        DEFAULT_TOOL_PROBE_MARKER,
        None,
        None,
    );
    assert!(case.ok, "{case:?}");
    assert_eq!(
        case.classification,
        ToolProbeClassification::StructuredNativeToolCall
    );
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
        DEFAULT_TOOL_PROBE_MARKER,
        None,
        None,
    );
    assert!(case.ok, "{case:?}");
    assert_eq!(
        case.classification,
        ToolProbeClassification::StructuredNativeToolCall
    );
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
        parser_errors: Vec::new(),
        protocol_violations: Vec::new(),
        content_sample: None,
    }
}
