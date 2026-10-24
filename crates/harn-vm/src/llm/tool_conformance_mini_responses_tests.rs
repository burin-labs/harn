use super::*;
use crate::llm::tool_conformance::ToolConformanceRequestValidationStatus;

#[test]
fn mini_probe_requests_use_responses_function_tools_and_input() {
    for mode in [ToolProbeMode::NonStreaming, ToolProbeMode::Streaming] {
        for case in [
            ToolProbeCase::SingleToolCall,
            ToolProbeCase::LargeStringArgument,
        ] {
            let expected_value = case.expected_value("mini-response-marker");
            let payload = probe_request_payload(
                "openai",
                "gpt-5.4-mini",
                mode,
                case,
                ToolProbeRequestProfile::CatalogDefault,
                &expected_value,
            )
            .expect("mini probe payload");
            assert_eq!(payload.api_mode, LlmApiMode::Responses);
            let body = provider_compatible_probe_request_body(&payload);
            assert_eq!(body["tools"][0]["type"], "function");
            assert_eq!(body["tools"][0]["name"], TOOL_PROBE_TOOL_NAME);
            assert_eq!(
                body["tools"][0]["parameters"]["properties"]["value"]["type"],
                "string"
            );
            assert_eq!(
                body["tool_choice"],
                json!({"type": "function", "name": TOOL_PROBE_TOOL_NAME})
            );
            let prompt = body["input"][0]["content"]
                .as_str()
                .expect("Responses prompt content");
            assert!(prompt.contains("mini-response-marker"));
            if case == ToolProbeCase::LargeStringArgument {
                assert!(prompt.contains("heredoc=<<EOF"));
            }
            assert!(body.get("messages").is_none());
            let validation = validate_probe_request_body(
                "openai",
                "gpt-5.4-mini",
                case,
                ToolProbeRequestProfile::CatalogDefault,
                &body,
            );
            assert_eq!(validation.dialect, "openai_responses");
            assert_eq!(
                validation.status,
                ToolConformanceRequestValidationStatus::Pass
            );
        }
    }
}

#[test]
fn mini_probe_followup_preserves_responses_call_and_output_identity() {
    let body = probe_request_body(
        "openai",
        "gpt-5.4-mini",
        ToolProbeMode::NonStreaming,
        ToolProbeCase::ToolResultFollowup,
        ToolProbeRequestProfile::CatalogDefault,
        "mini-followup-marker",
    )
    .expect("mini Responses follow-up");
    assert_eq!(body["tools"][0]["name"], TOOL_PROBE_TOOL_NAME);
    assert!(body.get("tool_choice").is_none());
    assert_eq!(body["input"].as_array().expect("Responses items").len(), 3);
    assert_eq!(body["input"][1]["type"], "function_call");
    assert_eq!(body["input"][1]["call_id"], "call_harn_tool_probe_1");
    assert_eq!(body["input"][1]["name"], TOOL_PROBE_TOOL_NAME);
    assert_eq!(body["input"][2]["type"], "function_call_output");
    assert_eq!(body["input"][2]["call_id"], "call_harn_tool_probe_1");
    let output: Value = serde_json::from_str(
        body["input"][2]["output"]
            .as_str()
            .expect("actual tool output"),
    )
    .expect("tool output JSON");
    assert_eq!(output["value"], "mini-followup-marker");
    let validation = validate_probe_request_body(
        "openai",
        "gpt-5.4-mini",
        ToolProbeCase::ToolResultFollowup,
        ToolProbeRequestProfile::CatalogDefault,
        &body,
    );
    assert_eq!(validation.dialect, "openai_responses");
    assert_eq!(
        validation.status,
        ToolConformanceRequestValidationStatus::Pass
    );
}
