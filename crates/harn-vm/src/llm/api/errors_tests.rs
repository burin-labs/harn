//! Unit tests for `errors.rs`, split out to keep that file under the
//! repository's source-length cap.

use super::{
    classify_llm_error, classify_provider_http_error, classify_provider_stream_error,
    provider_http_error, provider_token_quota_snapshot, LlmErrorKind, LlmErrorReason,
    ProviderTokenQuotaSnapshot,
};
use crate::value::{ErrorCategory, VmError, VmValue};

/// Exhaustive-match falsifier for the exported vocabularies.
///
/// Adding a variant without extending `ALL` breaks this match at compile
/// time, and the length assertions fail if `ALL` and the match disagree.
/// The protocol-artifact generator reads `ALL`, so a silent omission here
/// would ship a binding that is missing a value Harn actually emits.
#[test]
fn exported_llm_outcome_vocabularies_are_complete_and_round_trip() {
    const fn kind_ordinal(kind: LlmErrorKind) -> usize {
        match kind {
            LlmErrorKind::Transient => 0,
            LlmErrorKind::Terminal => 1,
        }
    }
    const fn reason_ordinal(reason: LlmErrorReason) -> usize {
        match reason {
            LlmErrorReason::RateLimit => 0,
            LlmErrorReason::ServerError => 1,
            LlmErrorReason::NetworkError => 2,
            LlmErrorReason::Timeout => 3,
            LlmErrorReason::AuthFailure => 4,
            LlmErrorReason::ContextOverflow => 5,
            LlmErrorReason::ContentPolicy => 6,
            LlmErrorReason::InvalidRequest => 7,
            LlmErrorReason::InvalidResponse => 8,
            LlmErrorReason::ModelUnavailable => 9,
            LlmErrorReason::EmptyGeneration => 10,
            LlmErrorReason::OutputBudgetExhausted => 11,
            LlmErrorReason::Unknown => 12,
        }
    }

    assert_eq!(LlmErrorKind::ALL.len(), 2);
    for (index, kind) in LlmErrorKind::ALL.iter().enumerate() {
        assert_eq!(kind_ordinal(*kind), index);
        assert_eq!(LlmErrorKind::parse(kind.as_str()), Some(*kind));
    }

    assert_eq!(LlmErrorReason::ALL.len(), 13);
    for (index, reason) in LlmErrorReason::ALL.iter().enumerate() {
        assert_eq!(reason_ordinal(*reason), index);
        assert_eq!(LlmErrorReason::parse(reason.as_str()), Some(*reason));
    }
}

/// Reach proof: the real classifier's connection-failure outcome is a
/// member of the vocabulary the protocol artifacts export.
///
/// The falsifier is a producer that classifies into a string outside
/// `ALL`. That would ship a `reason` no generated binding declares, which
/// is exactly the condition that made hosts invent sibling values.
#[test]
fn network_failure_classifies_into_the_exported_vocabulary() {
    let classified = classify_llm_error(
        ErrorCategory::TransientNetwork,
        "error sending request: connection reset by peer",
    );
    assert_eq!(classified.reason.as_str(), "network_error");
    assert_eq!(classified.kind.as_str(), "transient");
    assert!(LlmErrorReason::ALL.contains(&classified.reason));
    assert!(LlmErrorKind::ALL.contains(&classified.kind));

    // Negative control: a value no producer emits is not in the
    // vocabulary, so `parse` refuses it rather than folding it into a
    // neighbour.
    assert_eq!(LlmErrorReason::parse("provider_connection_failed"), None);
    assert_eq!(LlmErrorKind::parse("provider_degraded"), None);
}

fn thrown_field(error: &VmError, key: &str) -> Option<String> {
    let VmError::Thrown(VmValue::Dict(fields)) = error else {
        return None;
    };
    fields.get(key).map(VmValue::display)
}

#[test]
fn classify_openai_compatible_internal_server_stream_error_as_transient() {
    let error = classify_provider_stream_error(
        "fireworks",
        r#"{"error":{"message":"server had an error while processing your request, please retry again after a brief wait","type":"internal_server_error","code":"internal_server_error"}}"#,
        false,
    );

    assert_eq!(thrown_field(&error, "kind").as_deref(), Some("transient"));
    assert_eq!(
        thrown_field(&error, "reason").as_deref(),
        Some("server_error")
    );
    assert_eq!(
        thrown_field(&error, "source").as_deref(),
        Some("provider_stream")
    );
    assert_eq!(thrown_field(&error, "partial").as_deref(), Some("false"));
}

#[test]
fn classify_empty_generation_reason_tag() {
    let info = classify_llm_error(
        ErrorCategory::CircuitOpen,
        "provider route failed: reason=empty_generation attempt_count=2",
    );
    assert_eq!(info.kind, LlmErrorKind::Transient);
    assert_eq!(info.reason, LlmErrorReason::EmptyGeneration);
}

/// An exhausted output budget is deterministic under its cap: the taxonomy
/// has to say terminal, or every consumer that branches on `kind` treats it
/// as a hiccup worth re-sending the same request for.
#[test]
fn output_budget_exhaustion_is_terminal_not_transient() {
    assert_eq!(
        LlmErrorReason::OutputBudgetExhausted.default_kind(),
        LlmErrorKind::Terminal
    );
    assert_eq!(
        LlmErrorReason::EmptyGeneration.default_kind(),
        LlmErrorKind::Transient,
        "a genuinely empty generation stays a retryable hiccup"
    );
    assert_eq!(
        LlmErrorReason::parse(LlmErrorReason::OutputBudgetExhausted.as_str()),
        Some(LlmErrorReason::OutputBudgetExhausted)
    );
}

#[test]
fn classify_tags_vllm_prompt_too_long_as_context_overflow() {
    let msg = classify_provider_http_error(
        "local",
        reqwest::StatusCode::BAD_REQUEST,
        None,
        r#"{"object":"error","message":"This model's maximum context length is 8192 tokens. However, your prompt is too long (10234 tokens)."}"#,
    )
    .message;
    assert!(msg.contains("[context_overflow]"), "msg was: {msg}");
    assert!(msg.starts_with("local HTTP 400 Bad Request"));
    assert!(!msg.contains("(retry-after"));
}

#[test]
fn classify_tags_openai_context_length_exceeded_as_context_overflow() {
    let info = classify_provider_http_error(
        "openai",
        reqwest::StatusCode::BAD_REQUEST,
        None,
        r#"{"error":{"code":"context_length_exceeded","message":"maximum context length"}}"#,
    );
    let msg = info.message;
    assert_eq!(info.kind, LlmErrorKind::Terminal);
    assert_eq!(info.reason, LlmErrorReason::ContextOverflow);
    assert!(msg.contains("[context_overflow]"), "msg was: {msg}");
}

#[test]
fn classify_tags_429_with_retry_after_as_rate_limited() {
    let msg = classify_provider_http_error(
        "anthropic",
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        Some("12"),
        r#"{"error":{"type":"rate_limit_error","message":"quota exceeded"}}"#,
    )
    .message;
    assert!(msg.contains("[rate_limited]"), "msg was: {msg}");
    assert!(msg.ends_with("(retry-after: 12)"), "msg was: {msg}");
}

#[test]
fn classify_tags_opaque_500_as_http_error() {
    let msg = classify_provider_http_error(
        "local",
        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        None,
        "upstream exploded",
    )
    .message;
    assert!(msg.contains("[http_error]"), "msg was: {msg}");
    assert!(msg.contains("upstream exploded"));
}

#[test]
fn classify_decode_format_500_as_terminal_invalid_response() {
    let info = classify_provider_http_error(
        "llamacpp",
        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        None,
        r#"{"error":{"code":500,"message":"The model produced output that does not match the expected peg-native format","type":"server_error"}}"#,
    );

    assert_eq!(info.kind, LlmErrorKind::Terminal);
    assert_eq!(info.reason, LlmErrorReason::InvalidResponse);
    assert!(
        info.message.contains("[invalid_response]"),
        "msg was: {}",
        info.message
    );

    let round_trip = classify_llm_error(ErrorCategory::ServerError, &info.message);
    assert_eq!(round_trip.kind, LlmErrorKind::Terminal);
    assert_eq!(round_trip.reason, LlmErrorReason::InvalidResponse);
}

#[test]
fn classify_generic_decode_format_500_fingerprints_as_invalid_response() {
    for body in [
        "PEG-NATIVE decoder rejected the response",
        "response grammar parse failed",
        "response format decode failed",
    ] {
        let info = classify_provider_http_error(
            "local",
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            None,
            body,
        );
        assert_eq!(
            info.kind,
            LlmErrorKind::Terminal,
            "expected terminal classification for {body}"
        );
        assert_eq!(
            info.reason,
            LlmErrorReason::InvalidResponse,
            "expected invalid_response for {body}"
        );
    }

    let non_500 = classify_provider_http_error(
        "local",
        reqwest::StatusCode::BAD_GATEWAY,
        None,
        "PEG-NATIVE decoder rejected the response",
    );
    assert_eq!(non_500.kind, LlmErrorKind::Transient);
    assert_eq!(non_500.reason, LlmErrorReason::ServerError);
}

#[test]
fn classify_overloaded_500_as_transient_server_error() {
    let info = classify_provider_http_error(
        "anthropic",
        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        None,
        r#"{"error":{"type":"overloaded_error","message":"Service unavailable"}}"#,
    );

    assert_eq!(info.kind, LlmErrorKind::Transient);
    assert_eq!(info.reason, LlmErrorReason::ServerError);
}

#[test]
fn classify_429_with_context_body_still_prefers_context_overflow() {
    // Some OpenAI-compat servers return 429 for context overflow;
    // classify by body because caller reaction differs (compact vs back off).
    let info = classify_provider_http_error(
        "local",
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        Some("1"),
        "prompt is too long",
    );
    let msg = info.message;
    assert_eq!(info.kind, LlmErrorKind::Terminal);
    assert_eq!(info.reason, LlmErrorReason::ContextOverflow);
    assert!(msg.contains("[context_overflow]"), "msg was: {msg}");
}

#[test]
fn classify_ollama_model_context_exceeded_as_context_overflow() {
    let info = classify_provider_http_error(
        "ollama",
        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        None,
        r#"{"error":"model context exceeded: requested 49152 tokens"}"#,
    );
    assert_eq!(info.kind, LlmErrorKind::Terminal);
    assert_eq!(info.reason, LlmErrorReason::ContextOverflow);
    assert!(info.message.contains("[context_overflow]"));
    assert!(info.message.contains("offending_tokens: 49152"));
}

/// Helper: assert that a provider's HTTP-error body classifies as a
/// recoverable context overflow with the `[context_overflow]` tag stamped.
fn assert_overflow(provider: &str, status: reqwest::StatusCode, body: &str) {
    let info = classify_provider_http_error(provider, status, None, body);
    assert_eq!(
        info.reason,
        LlmErrorReason::ContextOverflow,
        "expected context_overflow for {provider}; body={body}; msg={}",
        info.message
    );
    assert_eq!(info.kind, LlmErrorKind::Terminal);
    assert!(
        info.message.contains("[context_overflow]"),
        "missing tag for {provider}: {}",
        info.message
    );
}

fn assert_not_overflow(provider: &str, status: reqwest::StatusCode, body: &str) {
    let info = classify_provider_http_error(provider, status, None, body);
    assert_ne!(
        info.reason,
        LlmErrorReason::ContextOverflow,
        "unexpectedly classified as context_overflow for {provider}; body={body}; msg={}",
        info.message
    );
}

#[test]
fn classify_gemini_token_count_exceeds_as_context_overflow() {
    // Gemini phrases overflow without the word "context".
    assert_overflow(
        "gemini",
        reqwest::StatusCode::BAD_REQUEST,
        r#"{"error":{"code":400,"message":"The input token count (1052431) exceeds the maximum number of tokens allowed (1048576).","status":"INVALID_ARGUMENT"}}"#,
    );
}

#[test]
fn classify_moonshot_token_limit_as_context_overflow() {
    assert_overflow(
        "moonshot",
        reqwest::StatusCode::BAD_REQUEST,
        r#"{"error":{"type":"invalid_request_error","message":"Your request exceeded model token limit: 262144"}}"#,
    );
}

#[test]
fn classify_together_input_validation_error_as_context_overflow() {
    assert_overflow(
        "together",
        reqwest::StatusCode::BAD_REQUEST,
        r#"{"error":{"message":"Input validation error: `inputs` tokens + `max_new_tokens` must be <= 131073. Given: 198342 `inputs` tokens","type":"invalid_request_error"}}"#,
    );
}

#[test]
fn classify_cerebras_reduce_length_as_context_overflow() {
    assert_overflow(
        "cerebras",
        reqwest::StatusCode::BAD_REQUEST,
        r#"{"message":"Please reduce the length of the messages or completion.","type":"invalid_request_error"}"#,
    );
}

#[test]
fn classify_groq_request_too_large_as_context_overflow() {
    // Groq's non-throttle 413 for a single oversized request.
    assert_overflow(
        "groq",
        reqwest::StatusCode::PAYLOAD_TOO_LARGE,
        r#"{"error":{"message":"Request too large for model with 131072 tokens. Reduce the number of tokens.","type":"invalid_request_error","code":"request_too_large"}}"#,
    );
}

#[test]
fn classify_groq_tpm_rate_limit_is_not_context_overflow() {
    // The SAME "request too large" phrasing, but a per-minute throttle: must
    // NOT be stolen as context_overflow (correct reaction is back-off).
    assert_not_overflow(
        "groq",
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        r#"{"error":{"message":"Rate limit reached: Request too large. Limit 6000 tokens per minute. Please try again.","type":"tokens","code":"rate_limit_exceeded"}}"#,
    );
}

#[test]
fn classify_explicit_overflow_wins_even_with_throttle_words() {
    // An explicit "maximum context length" signature must classify as
    // overflow even if the body coincidentally also mentions a rate limit —
    // the explicit branch returns before the throttle veto.
    assert_overflow(
        "openai",
        reqwest::StatusCode::BAD_REQUEST,
        r#"{"error":{"code":"context_length_exceeded","message":"This model's maximum context length is 8192 tokens. (rate limit note: unrelated)"}}"#,
    );
}

#[test]
fn classify_openai_quota_is_not_context_overflow() {
    // insufficient_quota mentions tokens but is a billing throttle, not overflow.
    assert_not_overflow(
        "openai",
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        r#"{"error":{"code":"insufficient_quota","message":"You exceeded your current quota, please check your plan and billing details."}}"#,
    );
}

#[test]
fn classify_content_policy_as_terminal() {
    let info = classify_provider_http_error(
        "openai",
        reqwest::StatusCode::BAD_REQUEST,
        None,
        r#"{"error":{"code":"content_policy_violation","message":"blocked"}}"#,
    );
    assert_eq!(info.kind, LlmErrorKind::Terminal);
    assert_eq!(info.reason, LlmErrorReason::ContentPolicy);
}

#[test]
fn provider_http_errors_redact_and_truncate_bodies() {
    let body = format!(
        r#"{{"error":{{"message":"Authorization: Bearer sk-secret api_key=abc123 {}","type":"invalid_request_error","code":"bad"}}}}"#,
        "x".repeat(3000)
    );
    let message =
        classify_provider_http_error("openai", reqwest::StatusCode::BAD_REQUEST, None, &body)
            .message;

    assert!(message.contains("type: invalid_request_error"));
    assert!(message.contains("code: bad"));
    assert!(!message.contains("sk-secret"));
    assert!(!message.contains("abc123"));
    assert!(
        message.len() < 2300,
        "message was too long: {}",
        message.len()
    );
}

#[test]
fn provider_http_errors_use_shared_secret_pattern_redaction() {
    let body = concat!(
        r#"{"error":{"message":"jwt=eyJabcd.eyJefgh.signature_pad "#,
        "-----BEGIN OPENSSH PRIVATE KEY-----\nsecret-material\n",
        r#"-----END OPENSSH PRIVATE KEY-----"}}"#
    );
    let message =
        classify_provider_http_error("openai", reqwest::StatusCode::BAD_REQUEST, None, body)
            .message;

    assert!(!message.contains("eyJabcd.eyJefgh.signature_pad"));
    assert!(!message.contains("secret-material"));
    assert!(message.contains("<redacted:jwt:"));
    assert!(message.contains("<redacted:private_key_block:"));
}

#[test]
fn provider_http_errors_surface_numeric_codes_and_request_ids() {
    let info = classify_provider_http_error(
        "minimax",
        reqwest::StatusCode::INTERNAL_SERVER_ERROR,
        None,
        r#"{"type":"error","error":{"message":"token plan not support model","http_code":"500","code":2061,"request_id":"req_123"}}"#,
    );

    assert_eq!(info.kind, LlmErrorKind::Terminal);
    assert_eq!(info.reason, LlmErrorReason::ModelUnavailable);
    assert!(info.message.contains("token plan not support model"));
    assert!(info.message.contains("http_code: 500"));
    assert!(info.message.contains("code: 2061"));
    assert!(info.message.contains("request_id: req_123"));
}

#[test]
fn provider_http_errors_surface_openrouter_previous_errors_tail() {
    let body = concat!(
        r#"{"error":{"message":"No endpoints could satisfy the request","code":502,"metadata":{"#,
        r#""request_id":"or_req_456","previous_errors":["#,
        r#"{"provider_name":"Cerebras","error":{"message":"tools is incompatible with response_format"}},"#,
        r#"{"provider_name":"Groq","message":"Request too large"}]}}}"#,
    );
    let info =
        classify_provider_http_error("openrouter", reqwest::StatusCode::BAD_GATEWAY, None, body);

    assert_eq!(info.kind, LlmErrorKind::Transient);
    assert_eq!(info.reason, LlmErrorReason::ServerError);
    assert!(info
        .message
        .contains("No endpoints could satisfy the request"));
    assert!(info.message.contains("code: 502"));
    assert!(info.message.contains("request_id: or_req_456"));
    assert!(info.message.contains(
        "previous_errors: Cerebras: tools is incompatible with response_format | Groq: Request too large"
    ));
}

#[test]
fn provider_http_errors_accept_top_level_json_string() {
    let info = classify_provider_http_error(
        "nvidia",
        reqwest::StatusCode::NOT_FOUND,
        None,
        r#""404 page not found""#,
    );

    assert_eq!(info.kind, LlmErrorKind::Terminal);
    assert_eq!(info.reason, LlmErrorReason::ModelUnavailable);
    assert!(info.message.contains("404 page not found"));
}

#[test]
fn category_mapping_preserves_transient_semantics() {
    let info = classify_llm_error(ErrorCategory::TransientNetwork, "connection reset");
    assert_eq!(info.kind, LlmErrorKind::Transient);
    assert_eq!(info.reason, LlmErrorReason::NetworkError);
}

#[test]
fn classifies_together_dedicated_only_route_as_model_unavailable() {
    // Together returns HTTP 400 + invalid_request_error for routes
    // listed in `/v1/models` that actually require a dedicated
    // endpoint. The body wording is stable and distinct from a normal
    // missing-model error, but callers' fallback logic only kicks in
    // on `model_unavailable`, so we lift it out of `invalid_request`.
    let body = concat!(
        r#"{"error":{"message":"#,
        r#""Unable to access non-serverless model Qwen/Qwen3-Coder-Next-FP8. "#,
        r#"Please visit https://api.together.ai/models/Qwen/Qwen3-Coder-Next-FP8 "#,
        r#"to create and start a new dedicated endpoint for the model.","#,
        r#""type":"invalid_request_error","code":"model_not_available"}}"#,
    );
    let info =
        classify_provider_http_error("together", reqwest::StatusCode::BAD_REQUEST, None, body);
    assert_eq!(info.kind, LlmErrorKind::Terminal);
    assert_eq!(info.reason, LlmErrorReason::ModelUnavailable);
    assert!(
        info.message.contains("[model_unavailable]"),
        "msg was: {}",
        info.message
    );
}

#[test]
fn classifies_openrouter_invalid_model_id_as_model_unavailable() {
    // OpenRouter returns HTTP 400 with a prose body for an unknown model
    // ID rather than a typed `model_not_found`. Cerebras returns 404 for
    // the same situation; both should land on `model_unavailable` so the
    // reason taxonomy is uniform across providers.
    let body = concat!(
        r#"{"error":{"message":"#,
        r#""qwen/qwen3-coder-bogus is not a valid model ID","#,
        r#""code":400}}"#,
    );
    let info =
        classify_provider_http_error("openrouter", reqwest::StatusCode::BAD_REQUEST, None, body);
    assert_eq!(info.kind, LlmErrorKind::Terminal);
    assert_eq!(info.reason, LlmErrorReason::ModelUnavailable);
    assert!(
        info.message.contains("[model_unavailable]"),
        "msg was: {}",
        info.message
    );
}

#[test]
fn token_quota_snapshot_uses_headers_and_ignores_provider_prose() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-ratelimit-limit-tokens", "200000".parse().unwrap());
    headers.insert("x-ratelimit-remaining-tokens", "16751".parse().unwrap());

    assert_eq!(
        provider_token_quota_snapshot(&headers),
        Some(ProviderTokenQuotaSnapshot {
            limit: 200_000,
            used: 183_249,
            window_ms: 60_000,
        })
    );

    let no_headers = reqwest::header::HeaderMap::new();
    assert_eq!(
        provider_token_quota_snapshot(&no_headers),
        None,
        "numeric Limit/Used prose must never become a quota contract"
    );

    let error = provider_http_error(
        None,
        "openai",
        reqwest::StatusCode::TOO_MANY_REQUESTS,
        &headers,
        r#"{"error":{"type":"tokens","message":"human wording may change"}}"#,
    );
    let VmError::Thrown(VmValue::Dict(error)) = error else {
        panic!("provider error must retain a typed envelope");
    };
    let Some(VmValue::Dict(quota)) = error.get("provider_quota") else {
        panic!("typed provider quota missing");
    };
    assert_eq!(
        quota.get("limit").map(VmValue::display),
        Some("200000".into())
    );
    assert_eq!(
        quota.get("used").map(VmValue::display),
        Some("183249".into())
    );
}

#[test]
fn typed_http_error_preserves_overload_category() {
    let error = provider_http_error(
        None,
        "anthropic",
        reqwest::StatusCode::from_u16(529).unwrap(),
        &reqwest::header::HeaderMap::new(),
        r#"{"type":"error","error":{"type":"overloaded_error"}}"#,
    );

    assert_eq!(
        crate::value::error_to_category(&error),
        ErrorCategory::Overloaded,
        "the typed envelope must retain the status-owned overload signal"
    );
}

#[test]
fn invalid_request_http_envelope_keeps_its_category_with_quota_metadata() {
    let mut headers = reqwest::header::HeaderMap::new();
    headers.insert("x-ratelimit-limit-tokens", "200000".parse().unwrap());
    headers.insert("x-ratelimit-remaining-tokens", "16751".parse().unwrap());
    let error = provider_http_error(
        None,
        "test",
        reqwest::StatusCode::BAD_REQUEST,
        &headers,
        r#"{"error":{"type":"invalid_request_error","message":"unsupported parameter"}}"#,
    );

    assert_eq!(
        crate::value::error_to_category(&error).as_str(),
        "invalid_request",
        "quota metadata must not erase the terminal request-error category"
    );
    assert_eq!(
        classify_llm_error(crate::value::error_to_category(&error), &error.to_string()).reason,
        LlmErrorReason::InvalidRequest,
    );
}
