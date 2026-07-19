use super::*;
use crate::value::VmValue;

#[test]
fn response_event_and_returned_usage_share_priced_cost() {
    let _guard = crate::llm::env_guard();
    crate::llm_config::clear_user_overrides();

    let priced = crate::llm::api::LlmResult {
        text: "priced result".to_string(),
        tool_calls: Vec::new(),
        raw_tool_calls: Vec::new(),
        input_tokens: 1_000,
        output_tokens: 1_000,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cache_supported: true,
        model: "claude-sonnet-4-20250514".to_string(),
        provider: "anthropic".to_string(),
        thinking: None,
        thinking_summary: None,
        stop_reason: Some("stop".to_string()),
        served_fast: false,
        blocks: Vec::new(),
        logprobs: Vec::new(),
        telemetry: crate::llm::api::ProviderTelemetry::default(),
    };
    let mut unpriced = priced.clone();
    unpriced.provider = "nonexistent_provider".to_string();
    unpriced.model = "ghost-model".to_string();

    let dir = tempfile::tempdir().expect("tempdir");
    push_llm_transcript_dir(dir.path().to_str().expect("utf8"));
    dump_llm_response(0, "call-priced", &priced, 1, None, None);
    dump_llm_response(1, "call-unpriced", &unpriced, 1, None, None);
    pop_llm_transcript_dir();

    let transcript =
        std::fs::read_to_string(dir.path().join("llm_transcript.jsonl")).expect("read transcript");
    let events = transcript
        .lines()
        .map(|line| serde_json::from_str::<serde_json::Value>(line).expect("parse event"))
        .filter(|event| event["type"] == "provider_call_response")
        .collect::<Vec<_>>();
    assert_eq!(events.len(), 2);

    let vm_usage_cost = |result: &crate::llm::api::LlmResult| {
        let vm_result = crate::llm::api::vm_build_llm_result(result, None, None, None);
        let result_dict = vm_result.as_dict().expect("result dict");
        let Some(VmValue::Dict(usage)) = result_dict.get("usage") else {
            panic!("missing usage dict: {result_dict:?}");
        };
        let usage = crate::llm::vm_value_to_json(&VmValue::Dict(usage.clone()));
        usage
            .as_object()
            .and_then(|usage| usage.get("cost_usd"))
            .cloned()
            .expect("cost_usd")
    };
    let expected_cost = priced.priced_cost_usd().expect("catalog-priced result");
    assert_eq!(events[0]["cost_usd"].as_f64(), Some(expected_cost));
    assert_eq!(vm_usage_cost(&priced), serde_json::json!(expected_cost));
    assert_eq!(events[1]["cost_usd"], serde_json::Value::Null);
    assert_eq!(vm_usage_cost(&unpriced), serde_json::Value::Null);
}
