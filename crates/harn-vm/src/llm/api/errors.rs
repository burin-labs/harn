//! HTTP error classification for LLM provider responses. Shared by both
//! streaming and non-streaming transports so the classification never
//! drifts between them.

/// Build a tagged, provider-prefixed error message from a non-2xx HTTP
/// response so downstream agent loops can react (e.g. trigger compaction on
/// `context_overflow`, back off on `rate_limited`, surface everything else as
/// `http_error`).
pub(crate) fn classify_http_error(
    provider: &str,
    status: reqwest::StatusCode,
    retry_after: Option<&str>,
    body: &str,
) -> String {
    // Patterns cover vLLM, OpenAI, Anthropic, and most OpenAI-compatibles.
    let body_lower = body.to_lowercase();
    let is_context_overflow = body_lower.contains("maximum context length")
        || body_lower.contains("context length")
        || body_lower.contains("context_length_exceeded")
        || body_lower.contains("prompt is too long")
        || body_lower.contains("prompt_tokens_exceeded")
        || body_lower.contains("this model's maximum context")
        || body_lower.contains("exceeds the maximum")
        || (body_lower.contains("max_tokens") && body_lower.contains("exceed"));
    let tag = if is_context_overflow {
        "context_overflow"
    } else if status.as_u16() == 429 {
        "rate_limited"
    } else {
        "http_error"
    };
    let mut msg = format!("{provider} HTTP {status} [{tag}]: {body}");
    if let Some(ra) = retry_after {
        msg.push_str(&format!(" (retry-after: {ra})"));
    }
    msg
}

#[cfg(test)]
mod tests {
    use super::classify_http_error;

    #[test]
    fn classify_tags_vllm_prompt_too_long_as_context_overflow() {
        let msg = classify_http_error(
            "local",
            reqwest::StatusCode::BAD_REQUEST,
            None,
            r#"{"object":"error","message":"This model's maximum context length is 8192 tokens. However, your prompt is too long (10234 tokens)."}"#,
        );
        assert!(msg.contains("[context_overflow]"), "msg was: {msg}");
        assert!(msg.starts_with("local HTTP 400 Bad Request"));
        assert!(!msg.contains("(retry-after"));
    }

    #[test]
    fn classify_tags_openai_context_length_exceeded_as_context_overflow() {
        let msg = classify_http_error(
            "openai",
            reqwest::StatusCode::BAD_REQUEST,
            None,
            r#"{"error":{"code":"context_length_exceeded","message":"maximum context length"}}"#,
        );
        assert!(msg.contains("[context_overflow]"), "msg was: {msg}");
    }

    #[test]
    fn classify_tags_429_with_retry_after_as_rate_limited() {
        let msg = classify_http_error(
            "anthropic",
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            Some("12"),
            r#"{"error":{"type":"rate_limit_error","message":"quota exceeded"}}"#,
        );
        assert!(msg.contains("[rate_limited]"), "msg was: {msg}");
        assert!(msg.ends_with("(retry-after: 12)"), "msg was: {msg}");
    }

    #[test]
    fn classify_tags_opaque_500_as_http_error() {
        let msg = classify_http_error(
            "local",
            reqwest::StatusCode::INTERNAL_SERVER_ERROR,
            None,
            "upstream exploded",
        );
        assert!(msg.contains("[http_error]"), "msg was: {msg}");
        assert!(msg.contains("upstream exploded"));
    }

    #[test]
    fn classify_429_with_context_body_still_prefers_context_overflow() {
        // Some OpenAI-compat servers return 429 for context overflow;
        // classify by body because caller reaction differs (compact vs back off).
        let msg = classify_http_error(
            "local",
            reqwest::StatusCode::TOO_MANY_REQUESTS,
            Some("1"),
            "prompt is too long",
        );
        assert!(msg.contains("[context_overflow]"), "msg was: {msg}");
    }
}
