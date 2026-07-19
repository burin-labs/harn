//! Native tool-call surfacing tests, split out of `agent_session_host_tests.rs`
//! so this coverage lands in a fresh file instead of growing that existing
//! source past its exact source-file-length baseline (harn#4798). Declared as a
//! child of the `tests` module, so `super::super` still reaches the private
//! `agent_session_host` helpers these tests exercise.

use serde_json::json;

use super::super::{assistant_message_from_llm_result, vm_to_json};

#[test]
fn native_tool_calls_replay_with_openai_wire_shape() {
    let result = crate::stdlib::json_to_vm_value(&json!({
        "provider": "local",
        "text": "",
        "native_tool_calls": [{
            "id": "call_001",
            "name": "release_run",
            "arguments": {"command": "git status --short"}
        }],
    }));
    let message = vm_to_json(&assistant_message_from_llm_result(&result));

    assert_eq!(message["role"], "assistant");
    assert_eq!(message["tool_calls"][0]["id"], "call_001");
    assert_eq!(message["tool_calls"][0]["type"], "function");
    assert_eq!(message["tool_calls"][0]["function"]["name"], "release_run");
    assert_eq!(
        message["tool_calls"][0]["function"]["arguments"],
        r#"{"command":"git status --short"}"#
    );
}

#[test]
fn mock_style_native_tool_calls_reach_assistant_envelope() {
    // Reproduces the CLI-mock / fixture-provider case: a native-format result
    // carrying populated `tool_calls` from a provider the capabilities table
    // does not know as native-tools ("fixture"). This must still surface the
    // tool call to the assistant envelope, or a downstream native-tool mock
    // test sees zero tool-call events and stops with end_turn.
    use super::super::super::api::{vm_build_llm_result, LlmResult, ProviderTelemetry};
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
