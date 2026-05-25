//! HTTP error classification for LLM provider responses. Shared by both
//! streaming and non-streaming transports so the classification never
//! drifts between them.

use crate::value::ErrorCategory;

const MAX_PROVIDER_ERROR_BODY_CHARS: usize = 2048;

/// Coarse retry semantics for provider failures.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LlmErrorKind {
    Transient,
    Terminal,
}

impl LlmErrorKind {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Transient => "transient",
            Self::Terminal => "terminal",
        }
    }
}

/// Canonical reason within the LLM error taxonomy.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum LlmErrorReason {
    RateLimit,
    ServerError,
    NetworkError,
    Timeout,
    AuthFailure,
    ContextOverflow,
    ContentPolicy,
    InvalidRequest,
    ModelUnavailable,
    Unknown,
}

impl LlmErrorReason {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::RateLimit => "rate_limit",
            Self::ServerError => "server_error",
            Self::NetworkError => "network_error",
            Self::Timeout => "timeout",
            Self::AuthFailure => "auth_failure",
            Self::ContextOverflow => "context_overflow",
            Self::ContentPolicy => "content_policy",
            Self::InvalidRequest => "invalid_request",
            Self::ModelUnavailable => "model_unavailable",
            Self::Unknown => "unknown",
        }
    }

    fn legacy_tag(self) -> &'static str {
        match self {
            Self::RateLimit => "rate_limited",
            Self::ServerError => "http_error",
            other => other.as_str(),
        }
    }
}

/// Fully classified provider failure.
#[derive(Clone, Debug, Eq, PartialEq)]
pub(crate) struct LlmErrorInfo {
    pub(crate) kind: LlmErrorKind,
    pub(crate) reason: LlmErrorReason,
    pub(crate) message: String,
}

/// Build a tagged, provider-prefixed error message from a non-2xx HTTP
/// response so downstream agent loops can react (e.g. trigger compaction on
/// `context_overflow`, back off on `rate_limited`, surface everything else as
/// `http_error`).
pub(crate) fn classify_provider_http_error(
    provider: &str,
    status: reqwest::StatusCode,
    retry_after: Option<&str>,
    body: &str,
) -> LlmErrorInfo {
    let (kind, reason) = classify_http_status_and_body(status, body);
    let body_summary = sanitize_provider_error_body(body);
    let mut msg = format!(
        "{provider} HTTP {status} [{}]: {body_summary}",
        reason.legacy_tag()
    );
    if reason == LlmErrorReason::ContextOverflow {
        if let Some(tokens) = extract_token_count_hint(body) {
            msg.push_str(&format!(" (offending_tokens: {tokens})"));
        }
    }
    if let Some(ra) = retry_after {
        msg.push_str(&format!(" (retry-after: {ra})"));
    }
    LlmErrorInfo {
        kind,
        reason,
        message: msg,
    }
}

fn sanitize_provider_error_body(body: &str) -> String {
    let summary =
        structured_provider_error_summary(body).unwrap_or_else(|| body.trim().to_string());
    let redacted = redact_provider_error_secrets(&summary);
    truncate_chars(&redacted, MAX_PROVIDER_ERROR_BODY_CHARS)
}

fn structured_provider_error_summary(body: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let error = json.get("error").unwrap_or(&json);
    if let Some(message) = error.get("message").and_then(serde_json::Value::as_str) {
        let message = truncate_chars(message, MAX_PROVIDER_ERROR_BODY_CHARS.saturating_sub(256));
        let mut details = Vec::new();
        for key in ["type", "code", "status"] {
            if let Some(value) = error.get(key).and_then(serde_json::Value::as_str) {
                if !value.is_empty() {
                    details.push(format!("{key}: {value}"));
                }
            }
        }
        if details.is_empty() {
            Some(message)
        } else {
            Some(format!("{message} ({})", details.join(", ")))
        }
    } else {
        error.as_str().map(str::to_string)
    }
}

fn redact_provider_error_secrets(text: &str) -> String {
    use regex::Regex;
    use std::sync::OnceLock;

    static SECRET_FIELD_RE: OnceLock<Regex> = OnceLock::new();
    static BEARER_RE: OnceLock<Regex> = OnceLock::new();
    let secret_field_re = SECRET_FIELD_RE.get_or_init(|| {
        Regex::new(
            r#"(?i)((?:api[_-]?key|access[_-]?token|refresh[_-]?token|id[_-]?token|authorization|secret|password)["']?\s*[:=]\s*["']?)[^"',\s}]+"#,
        )
        .expect("valid secret redaction regex")
    });
    let bearer_re = BEARER_RE.get_or_init(|| {
        Regex::new(r#"(?i)(bearer\s+)[^"',\s}]+"#).expect("valid bearer redaction regex")
    });
    let redacted = bearer_re.replace_all(text, "$1[redacted]");
    let redacted = secret_field_re
        .replace_all(&redacted, "$1[redacted]")
        .into_owned();
    crate::redact::current_policy()
        .redact_string(&redacted)
        .into_owned()
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    if text.chars().count() <= max_chars {
        return text.to_string();
    }
    let mut out = text.chars().take(max_chars).collect::<String>();
    out.push_str("...");
    out
}

pub(crate) fn classify_llm_error(category: ErrorCategory, message: &str) -> LlmErrorInfo {
    if let Some((kind, reason)) = classify_error_message_taxonomy(message) {
        return LlmErrorInfo {
            kind,
            reason,
            message: message.to_string(),
        };
    }

    let (kind, reason) = match category {
        ErrorCategory::RateLimit => (LlmErrorKind::Transient, LlmErrorReason::RateLimit),
        ErrorCategory::Timeout => (LlmErrorKind::Transient, LlmErrorReason::Timeout),
        ErrorCategory::Overloaded | ErrorCategory::ServerError => {
            (LlmErrorKind::Transient, LlmErrorReason::ServerError)
        }
        ErrorCategory::TransientNetwork => (LlmErrorKind::Transient, LlmErrorReason::NetworkError),
        ErrorCategory::Auth => (LlmErrorKind::Terminal, LlmErrorReason::AuthFailure),
        ErrorCategory::NotFound => (LlmErrorKind::Terminal, LlmErrorReason::ModelUnavailable),
        _ => (LlmErrorKind::Terminal, LlmErrorReason::Unknown),
    };

    LlmErrorInfo {
        kind,
        reason,
        message: message.to_string(),
    }
}

fn classify_http_status_and_body(
    status: reqwest::StatusCode,
    body: &str,
) -> (LlmErrorKind, LlmErrorReason) {
    // Patterns cover vLLM, OpenAI, Anthropic, and most OpenAI-compatibles.
    let body_lower = body.to_lowercase();

    if is_context_overflow(&body_lower) {
        return (LlmErrorKind::Terminal, LlmErrorReason::ContextOverflow);
    }
    if is_content_policy(&body_lower) {
        return (LlmErrorKind::Terminal, LlmErrorReason::ContentPolicy);
    }
    if is_auth_failure(&body_lower) || matches!(status.as_u16(), 401 | 403) {
        return (LlmErrorKind::Terminal, LlmErrorReason::AuthFailure);
    }
    if status.as_u16() == 429
        || body_lower.contains("rate_limit")
        || body_lower.contains("insufficient_quota")
        || body_lower.contains("billing_hard_limit_reached")
    {
        return (LlmErrorKind::Transient, LlmErrorReason::RateLimit);
    }
    if matches!(status.as_u16(), 408 | 504 | 522 | 524) || body_lower.contains("timeout") {
        return (LlmErrorKind::Transient, LlmErrorReason::Timeout);
    }
    if is_model_unavailable(&body_lower) || matches!(status.as_u16(), 404 | 410) {
        return (LlmErrorKind::Terminal, LlmErrorReason::ModelUnavailable);
    }
    if matches!(status.as_u16(), 500 | 502 | 503 | 529)
        || body_lower.contains("overloaded_error")
        || body_lower.contains("service unavailable")
        || body_lower.contains("bad gateway")
        || body_lower.contains("api_error")
    {
        return (LlmErrorKind::Transient, LlmErrorReason::ServerError);
    }
    if status.as_u16() == 400
        || body_lower.contains("invalid_request")
        || body_lower.contains("bad request")
    {
        return (LlmErrorKind::Terminal, LlmErrorReason::InvalidRequest);
    }

    (LlmErrorKind::Terminal, LlmErrorReason::Unknown)
}

fn classify_error_message_taxonomy(msg: &str) -> Option<(LlmErrorKind, LlmErrorReason)> {
    let lower = msg.to_lowercase();
    if lower.contains("kind") && lower.contains("transient") {
        if lower.contains("rate_limit") || lower.contains("rate_limited") {
            return Some((LlmErrorKind::Transient, LlmErrorReason::RateLimit));
        }
        if lower.contains("timeout") {
            return Some((LlmErrorKind::Transient, LlmErrorReason::Timeout));
        }
        if lower.contains("network_error") || lower.contains("transient_network") {
            return Some((LlmErrorKind::Transient, LlmErrorReason::NetworkError));
        }
        if lower.contains("server_error") || lower.contains("overloaded") {
            return Some((LlmErrorKind::Transient, LlmErrorReason::ServerError));
        }
    }
    if is_context_overflow(&lower) {
        return Some((LlmErrorKind::Terminal, LlmErrorReason::ContextOverflow));
    }
    if is_content_policy(&lower) {
        return Some((LlmErrorKind::Terminal, LlmErrorReason::ContentPolicy));
    }
    if is_auth_failure(&lower) {
        return Some((LlmErrorKind::Terminal, LlmErrorReason::AuthFailure));
    }
    if is_model_unavailable(&lower) {
        return Some((LlmErrorKind::Terminal, LlmErrorReason::ModelUnavailable));
    }
    if lower.contains("[rate_limited]")
        || lower.contains("too many requests")
        || lower.contains("insufficient_quota")
        || lower.contains("billing_hard_limit_reached")
    {
        return Some((LlmErrorKind::Transient, LlmErrorReason::RateLimit));
    }
    if lower.contains("[http_error]")
        || lower.contains("bad gateway")
        || lower.contains("service unavailable")
        || lower.contains("overloaded")
        || lower.contains("api_error")
    {
        return Some((LlmErrorKind::Transient, LlmErrorReason::ServerError));
    }
    if lower.contains("timed out") || lower.contains("timeout") {
        return Some((LlmErrorKind::Transient, LlmErrorReason::Timeout));
    }
    if lower.contains("connection reset")
        || lower.contains("connection refused")
        || lower.contains("connection closed")
        || lower.contains("broken pipe")
        || lower.contains("dns error")
        || lower.contains("stream error")
        || lower.contains("unexpected eof")
        || lower.contains("eof")
    {
        return Some((LlmErrorKind::Transient, LlmErrorReason::NetworkError));
    }
    if lower.contains("invalid_request")
        || lower.contains("bad request")
        || lower.contains("[invalid_request]")
    {
        return Some((LlmErrorKind::Terminal, LlmErrorReason::InvalidRequest));
    }
    None
}

fn is_context_overflow(lower: &str) -> bool {
    lower.contains("maximum context length")
        || lower.contains("context length")
        || lower.contains("model context exceeded")
        || lower.contains("context exceeded")
        || lower.contains("context_length_exceeded")
        || lower.contains("context_overflow")
        || lower.contains("prompt is too long")
        || lower.contains("prompt_tokens_exceeded")
        || lower.contains("this model's maximum context")
        || lower.contains("exceeds the maximum")
        || (lower.contains("context") && lower.contains("exceed"))
        || (lower.contains("max_tokens") && lower.contains("exceed"))
}

fn extract_token_count_hint(body: &str) -> Option<u64> {
    let mut max_number = None;
    let mut current = String::new();
    for ch in body.chars() {
        if ch.is_ascii_digit() {
            current.push(ch);
            continue;
        }
        if !current.is_empty() {
            if let Ok(parsed) = current.parse::<u64>() {
                max_number = Some(max_number.map_or(parsed, |n: u64| n.max(parsed)));
            }
            current.clear();
        }
    }
    if !current.is_empty() {
        if let Ok(parsed) = current.parse::<u64>() {
            max_number = Some(max_number.map_or(parsed, |n: u64| n.max(parsed)));
        }
    }
    max_number
}

fn is_content_policy(lower: &str) -> bool {
    lower.contains("content_policy")
        || lower.contains("content policy")
        || lower.contains("safety policy")
        || lower.contains("moderation")
        || lower.contains("responsible_ai_policy")
        || lower.contains("blocked by policy")
}

fn is_auth_failure(lower: &str) -> bool {
    lower.contains("invalid_api_key")
        || lower.contains("authentication_error")
        || lower.contains("auth_failure")
        || lower.contains("unauthorized")
        || lower.contains("forbidden")
}

fn is_model_unavailable(lower: &str) -> bool {
    lower.contains("model_not_found")
        || lower.contains("not_found_error")
        || lower.contains("model unavailable")
        || lower.contains("model is unavailable")
        || lower.contains("model not found")
        || lower.contains("model_not_available")
        // Together's wording when a route is listed in `/v1/models` but only
        // available through a dedicated endpoint; treat like a missing model
        // so caller fallback logic routes around it instead of surfacing a
        // generic invalid_request to the agent.
        || lower.contains("non-serverless model")
}

#[cfg(test)]
mod tests {
    use super::{classify_llm_error, classify_provider_http_error, LlmErrorKind, LlmErrorReason};
    use crate::value::ErrorCategory;

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
}
