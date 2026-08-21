use super::parse_llm_response;
use crate::llm::capabilities::{should_use_responses_transport, WireDialect};

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

    let result = parse_llm_response(
        &response,
        "vllm",
        "qwen3.6",
        WireDialect::OpenAiCompat,
        false,
    )
    .expect("parser succeeds");
    assert_eq!(
        result.telemetry.source,
        crate::llm::api::telemetry_source::OPENAI_USAGE
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
        WireDialect::OpenAiCompat,
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

#[test]
fn responses_transport_routing_is_provider_capability_driven() {
    assert!(should_use_responses_transport("openai", "gpt-5.4", true));
    assert!(should_use_responses_transport(
        "vercel_ai_gateway",
        "creator/new-model",
        true,
    ));
    assert!(!should_use_responses_transport(
        "anthropic",
        "claude-sonnet-4.6",
        true,
    ));
    assert!(should_use_responses_transport(
        "openai",
        "gpt-5.3-codex",
        false,
    ));
}

/// The observed shape of a non-streaming llama.cpp reply: `system_fingerprint`
/// and `model` sit at the top level, alongside the `timings` extension it
/// ships next to `usage`. Built with `json!` so no fixture string trips the
/// long-string prose lint.
fn observed_llamacpp_body(with_fingerprint: bool) -> serde_json::Value {
    let mut body = serde_json::json!({
        "choices": [{
            "finish_reason": "length",
            "index": 0,
            "message": {"role": "assistant", "content": "answer"}
        }],
        "model": "qwen3.6-35b-a3b-ud-q4-k-xl",
        "object": "chat.completion",
        "usage": {"completion_tokens": 8, "prompt_tokens": 14},
        "id": "chatcmpl-observed"
    });
    if with_fingerprint {
        body["system_fingerprint"] = serde_json::json!("b9994-14d3ba45f");
    }
    body
}

#[test]
fn openai_parser_records_the_served_build_fingerprint() {
    // `serving_base_url` cannot separate several hosts serving byte-identical
    // artifacts on the same local URL, so the server-reported build is the
    // only discriminator on the wire.
    let result = parse_llm_response(
        &observed_llamacpp_body(true),
        "llamacpp",
        "qwen3.6-35b-a3b-ud-q4-k-xl",
        WireDialect::OpenAiCompat,
        false,
    )
    .expect("parser succeeds");

    assert_eq!(
        result.telemetry.serving_fingerprint.as_deref(),
        Some("b9994-14d3ba45f")
    );

    // The discriminator has to reach the projected record, not just the
    // in-memory envelope, or no run record can ever join on it.
    let value = result
        .telemetry
        .as_vm_dict()
        .expect("telemetry should project");
    let dict = value.as_dict().expect("dict body");
    assert_eq!(
        dict.get("serving_fingerprint")
            .map(crate::value::VmValue::display)
            .as_deref(),
        Some("b9994-14d3ba45f")
    );
}

#[test]
fn a_response_without_a_fingerprint_leaves_it_absent() {
    // Absence control on the identical body. A server reporting no build must
    // not read as one that reported an empty build id: "" would compare equal
    // across two genuinely different servers and re-create the ambiguity this
    // field exists to narrow.
    let result = parse_llm_response(
        &observed_llamacpp_body(false),
        "llamacpp",
        "qwen3.6-35b-a3b-ud-q4-k-xl",
        WireDialect::OpenAiCompat,
        false,
    )
    .expect("parser succeeds");

    assert_eq!(result.telemetry.serving_fingerprint, None);
}
