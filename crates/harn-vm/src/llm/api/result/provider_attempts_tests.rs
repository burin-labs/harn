use super::*;

fn result_with(attempts: ProviderAttempts) -> LlmResult {
    LlmResult {
        attempts,
        text: "ok".to_string(),
        tool_calls: Vec::new(),
        text_projection: None,
        raw_tool_calls: Vec::new(),
        input_tokens: 10,
        output_tokens: 1,
        cache_read_tokens: 0,
        cache_write_tokens: 0,
        cache_supported: true,
        model: "gpt-5.6-luna".to_string(),
        provider: "openai".to_string(),
        thinking: None,
        thinking_summary: None,
        stop_reason: None,
        served_fast: false,
        blocks: Vec::new(),
        logprobs: Vec::new(),
        telemetry: Box::default(),
    }
}

/// `usage` is documented as the single owner of all accounting for a call,
/// so retry pressure has to arrive there rather than in a second location a
/// consumer would have to learn about separately. #5847 stayed invisible
/// precisely because every counter a reader could reach counted agent
/// iterations.
#[test]
fn provider_attempts_ride_in_the_usage_block() {
    let usage = build_usage_dict(&result_with(ProviderAttempts {
        total: 4,
        rate_limited: 2,
        empty_completion: 1,
        other: 0,
        completed_retry_usage: Vec::new(),
    }));
    let attempts = usage
        .get("provider_attempts")
        .expect("usage must carry provider attempts");
    let VmValue::Dict(attempts) = attempts else {
        panic!("provider_attempts must be a dict, got {attempts:?}");
    };
    assert_eq!(attempts.get("total").and_then(VmValue::as_int), Some(4));
    assert_eq!(
        attempts.get("retries").and_then(VmValue::as_int),
        Some(3),
        "three requests failed before the fourth succeeded"
    );
    assert_eq!(
        attempts.get("rate_limited").and_then(VmValue::as_int),
        Some(2)
    );
    assert_eq!(
        attempts.get("empty_completion").and_then(VmValue::as_int),
        Some(1)
    );
}

/// The overwhelming majority of calls succeed first try. Serializing a
/// zero-valued object onto every one of them would grow a persisted
/// transcript for no information.
#[test]
fn a_clean_single_request_is_omitted_from_the_wire_form() {
    let clean = ProviderAttempts {
        total: 1,
        ..ProviderAttempts::default()
    };
    assert!(clean.is_single_clean_call());
    assert_eq!(clean.retries(), 0);

    let json = serde_json::to_value(result_with(clean)).expect("serialize");
    assert!(
        json.get("attempts").is_none(),
        "a clean call must not carry an attempts object: {json:?}"
    );

    // A call that retried is information, and must survive the round trip.
    let retried = ProviderAttempts {
        total: 2,
        rate_limited: 1,
        ..ProviderAttempts::default()
    };
    assert!(!retried.is_single_clean_call());
    let json = serde_json::to_value(result_with(retried.clone())).expect("serialize");
    assert_eq!(
        json.pointer("/attempts/rate_limited")
            .and_then(|v| v.as_u64()),
        Some(1)
    );
    let round_tripped: LlmResult = serde_json::from_value(json).expect("deserialize");
    assert_eq!(round_tripped.attempts, retried);

    let completed = ProviderAttempts {
        total: 2,
        empty_completion: 1,
        completed_retry_usage: vec![crate::llm::usage::LlmUsage::known_zero_attempt()],
        ..ProviderAttempts::default()
    };
    let json = serde_json::to_value(result_with(completed.clone())).expect("serialize usage");
    let round_tripped: LlmResult = serde_json::from_value(json).expect("deserialize usage");
    assert_eq!(
        round_tripped.attempts, completed,
        "retry usage must survive replay"
    );
}

/// A recording made before this field existed deserializes with no
/// attempts, which must read as "unknown" rather than crashing the load.
#[test]
fn a_recording_without_attempts_still_loads() {
    let mut json =
        serde_json::to_value(result_with(ProviderAttempts::default())).expect("serialize");
    json.as_object_mut().expect("object").remove("attempts");
    let loaded: LlmResult = serde_json::from_value(json).expect("deserialize");
    assert_eq!(loaded.attempts, ProviderAttempts::default());
    assert_eq!(loaded.attempts.retries(), 0);
}
