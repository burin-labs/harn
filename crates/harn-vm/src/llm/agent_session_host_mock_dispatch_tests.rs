//! Coverage for the mock/fixture native tool-call path (harn#4798).
//!
//! Split out of `agent_session_host_tests.rs` so new coverage lands in a fresh
//! file instead of growing that grandfathered source past its exact
//! source-file-length baseline.

use super::{assistant_message_from_llm_result, vm_to_json};

#[test]
fn mock_style_native_tool_calls_reach_assistant_envelope() {
    // Reproduces the CLI-mock / fixture-provider case: a native-format result
    // carrying populated `tool_calls` from a provider the capabilities table
    // does not know as native-tools ("fixture"). This must still surface the
    // tool call to the assistant envelope, or a downstream native-tool mock
    // test sees zero tool-call events and stops with end_turn.
    use super::super::api::{vm_build_llm_result, LlmResult, ProviderTelemetry};
    let result = LlmResult {
        served_fast: false,
        text: String::new(),
        tool_calls: vec![serde_json::json!({
            "id": "mock_call_1",
            "type": "tool_call",
            "name": "ask_user",
            "arguments": {"question": "Which output format should I use?"}
        })],
        raw_tool_calls: Vec::new(),
        input_tokens: 5,
        output_tokens: 5,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cache_supported: true,
        model: "fixture-fast-2026".to_string(),
        provider: "fixture".to_string(),
        thinking: None,
        thinking_summary: None,
        stop_reason: Some("tool_use".to_string()),
        blocks: Vec::new(),
        logprobs: Vec::new(),
        telemetry: ProviderTelemetry::default(),
    };
    let vm_result = vm_build_llm_result(&result, None, None, None);
    let message = vm_to_json(&assistant_message_from_llm_result(&vm_result));
    assert!(
        message["tool_calls"]
            .as_array()
            .is_some_and(|calls| !calls.is_empty()),
        "mock native tool_calls must reach the assistant envelope: {message}"
    );
}
