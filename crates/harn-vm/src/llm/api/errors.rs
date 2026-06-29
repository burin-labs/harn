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

    pub(crate) fn parse(value: &str) -> Option<Self> {
        match value {
            "transient" => Some(Self::Transient),
            "terminal" => Some(Self::Terminal),
            _ => None,
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

/// Extract the `Retry-After` header for threading into
/// [`classify_provider_http_error`]. Read it before consuming the response
/// body — `Response::text()` takes the response by value.
pub(crate) fn retry_after_header(headers: &reqwest::header::HeaderMap) -> Option<String> {
    headers
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .map(str::to_string)
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
    if let Some(message) = provider_error_message(error).or_else(|| provider_error_message(&json)) {
        let message = truncate_chars(message, MAX_PROVIDER_ERROR_BODY_CHARS.saturating_sub(256));
        let mut details = Vec::new();
        collect_error_details(error, &mut details);
        if !std::ptr::eq(error, &json) {
            collect_error_details(&json, &mut details);
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

fn provider_error_message(value: &serde_json::Value) -> Option<&str> {
    match value {
        serde_json::Value::String(message) => non_empty_str(message),
        serde_json::Value::Object(object) => ["message", "detail", "error_description"]
            .into_iter()
            .find_map(|key| object.get(key).and_then(provider_error_message)),
        _ => None,
    }
}

fn collect_error_details(value: &serde_json::Value, details: &mut Vec<String>) {
    let Some(object) = value.as_object() else {
        return;
    };
    for key in [
        "type",
        "code",
        "status",
        "http_code",
        "request_id",
        "requestId",
    ] {
        if let Some(value) = object.get(key).and_then(provider_error_detail_value) {
            push_unique_detail(details, key, &value);
        }
    }
    if let Some(metadata) = object.get("metadata") {
        collect_error_details(metadata, details);
        if let Some(previous) = metadata
            .get("previous_errors")
            .and_then(serde_json::Value::as_array)
        {
            if let Some(summary) = previous_errors_summary(previous) {
                push_unique_detail(details, "previous_errors", &summary);
            }
        }
    }
}

fn provider_error_detail_value(value: &serde_json::Value) -> Option<String> {
    match value {
        serde_json::Value::String(text) => non_empty_str(text).map(str::to_string),
        serde_json::Value::Number(number) => Some(number.to_string()),
        serde_json::Value::Bool(value) => Some(value.to_string()),
        _ => None,
    }
}

fn previous_errors_summary(errors: &[serde_json::Value]) -> Option<String> {
    let mut parts = Vec::new();
    for error in errors.iter().rev().take(3).rev() {
        let provider = error
            .get("provider_name")
            .or_else(|| error.get("provider"))
            .and_then(provider_error_detail_value);
        let message = error
            .get("error")
            .and_then(provider_error_message)
            .or_else(|| provider_error_message(error));
        if let Some(message) = message {
            let message = truncate_chars(message, 180);
            if let Some(provider) = provider {
                parts.push(format!("{provider}: {message}"));
            } else {
                parts.push(message);
            }
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join(" | "))
    }
}

fn push_unique_detail(details: &mut Vec<String>, key: &str, value: &str) {
    if value.is_empty() {
        return;
    }
    let detail = format!("{key}: {value}");
    if !details.iter().any(|existing| existing == &detail) {
        details.push(detail);
    }
}

fn non_empty_str(value: &str) -> Option<&str> {
    let trimmed = value.trim();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed)
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

/// Provider-agnostic detection of a "the assembled prompt is bigger than the
/// model's context window" error.
///
/// This is the single point that decides whether the agent loop is allowed to
/// recover (emergency-compact + retry) instead of treating the turn as a
/// terminal failure, so it must catch the condition no matter which provider's
/// 400/413/429 phrasing arrives. Every provider funnels its FULL raw error body
/// through here, so we match on substrings of the whole body rather than any
/// single parsed field; adding a new provider's phrasing is a one-line edit.
///
/// Known provider phrasings covered (see the table in the conformance tests):
/// - OpenAI / OpenRouter / Fireworks / Azure / Nvidia / SambaNova / DeepInfra
///   (OpenAI-compatible): `context_length_exceeded`, "maximum context length".
/// - Anthropic: "prompt is too long: N tokens > M maximum".
/// - vLLM: "this model's maximum context length is …".
/// - Ollama: "model context exceeded".
/// - Google / Gemini: "input token count (N) exceeds the maximum number of
///   tokens allowed (M)" / "the input token count … exceeds …".
/// - Cerebras: "please reduce the length of the messages or completion".
/// - Moonshot / Kimi: "exceeded model token limit" / "max tokens per request".
/// - Together: "input validation error: `inputs` tokens + `max_new_tokens` …".
/// - Groq: "request too large" / "reduce the length …" (TPM-style 413/429 — see
///   the `throttle` veto below so a genuine rate-limit is not stolen).
fn is_context_overflow(lower: &str) -> bool {
    // Unambiguous signatures — the body explicitly names the context window or a
    // canonical OpenAI-style code, so no co-occurrence gate is needed.
    let explicit = lower.contains("maximum context length")
        || lower.contains("context length")
        || lower.contains("model context exceeded")
        || lower.contains("context exceeded")
        || lower.contains("context_length_exceeded")
        || lower.contains("context_overflow")
        || lower.contains("context window")
        || lower.contains("prompt is too long")
        || lower.contains("input is too long")
        || lower.contains("input too long")
        || lower.contains("prompt_tokens_exceeded")
        || lower.contains("this model's maximum context")
        || lower.contains("exceeds the maximum")
        || (lower.contains("context") && lower.contains("exceed"))
        || (lower.contains("max_tokens") && lower.contains("exceed"));
    if explicit {
        return true;
    }

    // Token-shaped signatures that DON'T name "context" explicitly. These are
    // genuinely about prompt size on several providers, but a couple of the
    // phrasings ("request too large", "reduce the length …") are also emitted
    // by some providers for tokens-per-minute rate limits. To avoid stealing a
    // real rate-limit (whose correct reaction is back-off, not compaction), only
    // treat them as overflow when the body also talks about tokens/length AND
    // does NOT look like a per-minute / quota throttle.
    let throttle = lower.contains("per minute")
        || lower.contains("per-minute")
        || lower.contains("per day")
        || lower.contains("requests per")
        || lower.contains("tokens per minute")
        || lower.contains("tpm")
        || lower.contains("rpm")
        || lower.contains("quota")
        || lower.contains("retry-after")
        || lower.contains("retry after")
        || lower.contains("rate_limit")
        || lower.contains("rate limit")
        || lower.contains("insufficient_quota");
    if throttle {
        return false;
    }

    let mentions_tokens =
        lower.contains("token") || lower.contains("length") || lower.contains("messages");
    if !mentions_tokens {
        return false;
    }

    // Provider-specific size phrasings, only after the throttle veto above.
    lower.contains("token limit")          // Moonshot / Kimi: "exceeded model token limit"
        || lower.contains("token count")    // Gemini: "input token count … exceeds …"
        || lower.contains("too many tokens")
        || lower.contains("request too large") // Groq (non-throttle 413)
        || lower.contains("too large for")
        || lower.contains("input validation error") // Together
        || lower.contains("reduce the length")       // Cerebras
        || lower.contains("reduce the number of tokens")
        || lower.contains("please reduce")
        || (lower.contains("token") && lower.contains("exceed"))
        || (lower.contains("token") && lower.contains("limit") && lower.contains("exceed"))
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
        // MiniMax returns HTTP 500 with a provider-specific numeric code for
        // account/model-plan mismatches; retries cannot change the route.
        || lower.contains("token plan not support model")
        || lower.contains("(2061)")
        // OpenRouter's HTTP-400 wording for an unknown model ID
        // ("<id> is not a valid model ID"). Mirror the `not_found` mapping in
        // `value::error::classify_error_message` so the reason taxonomy agrees
        // across both classifiers and matches Cerebras's 404 path.
        || lower.contains("is not a valid model id")
        || lower.contains("invalid model id")
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
        let info = classify_provider_http_error(
            "openrouter",
            reqwest::StatusCode::BAD_GATEWAY,
            None,
            r#"{
                "error": {
                    "message": "No endpoints could satisfy the request",
                    "code": 502,
                    "metadata": {
                        "request_id": "or_req_456",
                        "previous_errors": [
                            {
                                "provider_name": "Cerebras",
                                "error": {"message": "tools is incompatible with response_format"}
                            },
                            {
                                "provider_name": "Groq",
                                "message": "Request too large"
                            }
                        ]
                    }
                }
            }"#,
        );

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
        let info = classify_provider_http_error(
            "openrouter",
            reqwest::StatusCode::BAD_REQUEST,
            None,
            body,
        );
        assert_eq!(info.kind, LlmErrorKind::Terminal);
        assert_eq!(info.reason, LlmErrorReason::ModelUnavailable);
        assert!(
            info.message.contains("[model_unavailable]"),
            "msg was: {}",
            info.message
        );
    }
}
