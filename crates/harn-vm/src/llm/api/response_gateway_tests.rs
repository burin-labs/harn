use super::response::parse_llm_response;

#[test]
fn openai_parser_preserves_partial_usage_in_telemetry() {
    let response = serde_json::json!({
        "id": "chatcmpl-abc",
        "choices": [{
            "message": {"content": "done"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 314, "completion_tokens": 27}
    });

    let result =
        parse_llm_response(&response, "vllm", "qwen3.6", false, false).expect("parser succeeds");
    assert_eq!(
        result.telemetry.source,
        super::telemetry_source::OPENAI_USAGE
    );
    assert_eq!(result.telemetry.server_prompt_tokens, Some(314));
    assert_eq!(result.telemetry.server_output_tokens, Some(27));
    assert_eq!(result.telemetry.server_prompt_eval_ms, None);
    assert_eq!(result.telemetry.request_id.as_deref(), Some("chatcmpl-abc"));
}

#[test]
fn openai_parser_preserves_gateway_routing_metadata() {
    let response = serde_json::json!({
        "id": "gen_gateway",
        "choices": [{
            "message": {"content": "done"},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 10, "completion_tokens": 2, "cost": 0.00001},
        "provider_metadata": {
            "gateway": {
                "routing": {"resolvedProvider": "openai", "modelAttemptCount": 1},
                "cost": "0.00001"
            }
        }
    });

    let result = parse_llm_response(
        &response,
        "vercel_ai_gateway",
        "openai/gpt-5.4-nano",
        false,
        false,
    )
    .expect("gateway response parses");

    assert_eq!(
        result
            .telemetry
            .provider_metadata
            .as_ref()
            .and_then(|metadata| metadata.pointer("/gateway/routing/resolvedProvider"))
            .and_then(serde_json::Value::as_str),
        Some("openai")
    );
}
