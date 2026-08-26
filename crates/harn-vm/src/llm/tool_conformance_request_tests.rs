use super::request::probe_request_body;
use super::*;

#[test]
fn streaming_probe_request_includes_transport_and_usage_fields() {
    let body = probe_request_body(
        "together",
        "Qwen/Qwen3.6-Plus",
        ToolProbeMode::Streaming,
        ToolProbeCase::SingleToolCall,
        ToolProbeRequestProfile::CatalogDefault,
        DEFAULT_TOOL_PROBE_MARKER,
    )
    .expect("streaming probe body");

    assert_eq!(body["stream"], true);
    assert!(body.get("stream_options").is_none());

    let reported_usage_body = probe_request_body(
        "openai",
        "gpt-5.4-mini",
        ToolProbeMode::Streaming,
        ToolProbeCase::SingleToolCall,
        ToolProbeRequestProfile::CatalogDefault,
        DEFAULT_TOOL_PROBE_MARKER,
    )
    .expect("streaming probe body with terminal usage");
    assert_eq!(reported_usage_body["stream"], true);
    assert_eq!(
        reported_usage_body["stream_options"],
        serde_json::json!({"include_usage": true})
    );

    let gemini_generate_content_body = probe_request_body(
        "gemini",
        "gemini-2.5-flash",
        ToolProbeMode::Streaming,
        ToolProbeCase::SingleToolCall,
        ToolProbeRequestProfile::CatalogDefault,
        DEFAULT_TOOL_PROBE_MARKER,
    )
    .expect("Gemini GenerateContent streaming probe body");
    assert!(
        gemini_generate_content_body.get("stream").is_none(),
        "GenerateContent selects SSE through its endpoint, not its JSON body"
    );
}
