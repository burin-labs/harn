use crate::llm::usage::ProviderUsageReceipt;
use crate::value::{VmError, VmValue};

use super::{
    parse_openai_responses_response, test_support::parse_llm_response, ProviderResponseEnvelope,
};

fn assert_provider_response_envelope(
    error: &VmError,
    response_id: Option<&str>,
    stop_reason: Option<&str>,
    block_types: &[&str],
    input_tokens: i64,
    output_tokens: i64,
) {
    let response = ProviderResponseEnvelope::from_error(error)
        .expect("empty generation must retain its typed provider response");
    assert_eq!(response.response_id(), response_id);
    assert_eq!(response.stop_reason(), stop_reason);
    assert_eq!(response.content_block_count(), block_types.len());
    assert_eq!(
        response.content_block_types(),
        block_types
            .iter()
            .map(|value| (*value).to_string())
            .collect::<Vec<_>>()
    );
    let receipt = ProviderUsageReceipt::from_error(error)
        .expect("empty generation must retain the compatibility usage receipt");
    let VmValue::Dict(fields) = receipt.to_vm_value() else {
        panic!("receipt must lower to a dictionary");
    };
    assert_eq!(
        fields.get("input_tokens").and_then(VmValue::as_int),
        Some(input_tokens)
    );
    assert_eq!(
        fields.get("output_tokens").and_then(VmValue::as_int),
        Some(output_tokens)
    );
}

#[test]
fn anthropic_empty_completion_keeps_provider_response_envelope() {
    let response = serde_json::json!({
        "id": "msg-empty",
        "content": [{"type": "policy_outcome", "detail": ""}],
        "stop_reason": "refusal",
        "usage": {"input_tokens": 11, "output_tokens": 7}
    });

    let error = parse_llm_response(&response, "anthropic", "claude-opus-4-7", true, false)
        .expect_err("empty Anthropic completion must be rejected");

    assert_provider_response_envelope(
        &error,
        Some("msg-empty"),
        Some("refusal"),
        &["policy_outcome"],
        11,
        7,
    );
}

#[test]
fn openai_responses_empty_completion_keeps_provider_response_envelope() {
    let response = serde_json::json!({
        "id": "resp-empty",
        "status": "completed",
        "output": [{
            "type": "message",
            "content": [{"type": "policy_outcome", "detail": ""}]
        }],
        "usage": {"input_tokens": 19, "output_tokens": 3}
    });

    let error = parse_openai_responses_response(&response, "openai", "gpt-5.4-preview")
        .expect_err("empty Responses API completion must be rejected");

    assert_provider_response_envelope(
        &error,
        Some("resp-empty"),
        Some("completed"),
        &["policy_outcome"],
        19,
        3,
    );
}

#[test]
fn openai_chat_empty_completion_keeps_provider_response_envelope() {
    let response = serde_json::json!({
        "id": "chatcmpl-empty",
        "choices": [{
            "message": {"content": ""},
            "finish_reason": "stop"
        }],
        "usage": {"prompt_tokens": 1, "completion_tokens": 0}
    });

    let error = parse_llm_response(&response, "openai", "gpt-5.4-preview", false, false)
        .expect_err("empty provider message must be rejected");

    let VmError::Thrown(VmValue::Dict(fields)) = &error else {
        panic!("empty generation must be structured: {error:?}");
    };
    assert!(fields
        .get("code")
        .is_some_and(|value| value.display() == "empty_generation"));
    assert!(matches!(fields.get("output_tokens"), Some(VmValue::Int(0))));
    assert_provider_response_envelope(
        &error,
        Some("chatcmpl-empty"),
        Some("stop"),
        &["text"],
        1,
        0,
    );
    assert!(error.to_string().contains("delivered no content"));
}
