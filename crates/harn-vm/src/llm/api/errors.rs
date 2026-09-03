//! HTTP error classification for LLM provider responses. Shared by both
//! streaming and non-streaming transports so the classification never
//! drifts between them.

use crate::value::{ErrorCategory, VmError, VmValue};

const MAX_PROVIDER_ERROR_BODY_CHARS: usize = 2048;

/// HTTP 500 bodies that report a deterministic response-shape failure rather
/// than transient provider unavailability. Each entry is a conjunction; the
/// table stays provider-agnostic and can absorb new serving-stack phrasings.
const INVALID_RESPONSE_FINGERPRINTS: &[&[&str]] = &[
    &["does not match the expected", "format"],
    &["peg-native"],
    &["grammar", "parse"],
    &["format", "decode"],
];

/// Coarse retry semantics for provider failures.
///
/// This is the closed vocabulary carried in the `kind` field of the structured
/// error dict `llm_call` throws, and therefore in `kind` on the
/// `harn.acp.prompt_error.v1` envelope. It is exported through the protocol
/// artifacts so hosts branch on the owner's vocabulary instead of inventing
/// sibling strings.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LlmErrorKind {
    Transient,
    Terminal,
}

impl LlmErrorKind {
    /// Every kind, in wire order. Consumed by the protocol-artifact generator.
    pub const ALL: &'static [Self] = &[Self::Transient, Self::Terminal];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::Transient => "transient",
            Self::Terminal => "terminal",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "transient" => Some(Self::Transient),
            "terminal" => Some(Self::Terminal),
            _ => None,
        }
    }
}

/// Canonical reason within the LLM error taxonomy.
///
/// This is the closed vocabulary carried in the `reason` field of the
/// structured error dict `llm_call` throws, and therefore in `reason` on the
/// `harn.acp.prompt_error.v1` envelope. It is exported through the protocol
/// artifacts. `code`, by contrast, is a provider passthrough with no closed
/// set: hosts must treat it as opaque diagnostic text and never branch on it.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum LlmErrorReason {
    RateLimit,
    ServerError,
    NetworkError,
    Timeout,
    AuthFailure,
    ContextOverflow,
    ContentPolicy,
    InvalidRequest,
    InvalidResponse,
    ModelUnavailable,
    EmptyGeneration,
    /// The account cannot pay for the call: it is out of credit, its quota is
    /// exhausted, or it has hit a configured spend ceiling.
    ///
    /// Separate from `RateLimit` because a backoff never clears it. Retrying
    /// spends the rest of the run's wall clock and budget against a condition
    /// that refuses identically every time, and the loop then sees an
    /// exhausted retry budget rather than an account with no credit, so the
    /// terminal record names the wrong cause.
    BillingLimit,
    /// The call consumed its entire output budget and committed nothing. The
    /// same context under the same cap exhausts the same way, so this is a
    /// deterministic budget failure rather than a provider hiccup: recovery is
    /// a larger cap or a smaller request, never a byte-identical replay.
    OutputBudgetExhausted,
    Unknown,
}

impl LlmErrorReason {
    /// Every reason, in wire order. Consumed by the protocol-artifact
    /// generator so a new reason cannot land without a regenerated binding.
    pub const ALL: &'static [Self] = &[
        Self::RateLimit,
        Self::ServerError,
        Self::NetworkError,
        Self::Timeout,
        Self::AuthFailure,
        Self::ContextOverflow,
        Self::ContentPolicy,
        Self::InvalidRequest,
        Self::InvalidResponse,
        Self::ModelUnavailable,
        Self::EmptyGeneration,
        Self::BillingLimit,
        Self::OutputBudgetExhausted,
        Self::Unknown,
    ];

    pub fn as_str(self) -> &'static str {
        match self {
            Self::RateLimit => "rate_limit",
            Self::ServerError => "server_error",
            Self::NetworkError => "network_error",
            Self::Timeout => "timeout",
            Self::AuthFailure => "auth_failure",
            Self::ContextOverflow => "context_overflow",
            Self::ContentPolicy => "content_policy",
            Self::InvalidRequest => "invalid_request",
            Self::InvalidResponse => "invalid_response",
            Self::ModelUnavailable => "model_unavailable",
            Self::EmptyGeneration => "empty_generation",
            Self::BillingLimit => "billing_limit",
            Self::OutputBudgetExhausted => "output_budget_exhausted",
            Self::Unknown => "unknown",
        }
    }

    pub fn parse(value: &str) -> Option<Self> {
        match value {
            "rate_limit" | "rate_limited" => Some(Self::RateLimit),
            "server_error" | "http_error" => Some(Self::ServerError),
            "network_error" => Some(Self::NetworkError),
            "timeout" => Some(Self::Timeout),
            "auth_failure" => Some(Self::AuthFailure),
            "context_overflow" => Some(Self::ContextOverflow),
            "content_policy" => Some(Self::ContentPolicy),
            "invalid_request" => Some(Self::InvalidRequest),
            "invalid_response" => Some(Self::InvalidResponse),
            "model_unavailable" => Some(Self::ModelUnavailable),
            "empty_generation" => Some(Self::EmptyGeneration),
            "billing_limit" => Some(Self::BillingLimit),
            "output_budget_exhausted" => Some(Self::OutputBudgetExhausted),
            "unknown" => Some(Self::Unknown),
            _ => None,
        }
    }

    fn legacy_tag(self) -> &'static str {
        match self {
            Self::RateLimit => "rate_limited",
            Self::ServerError => "http_error",
            other => other.as_str(),
        }
    }

    fn default_kind(self) -> LlmErrorKind {
        match self {
            Self::RateLimit
            | Self::ServerError
            | Self::NetworkError
            | Self::Timeout
            | Self::EmptyGeneration => LlmErrorKind::Transient,
            Self::AuthFailure
            | Self::ContextOverflow
            | Self::ContentPolicy
            | Self::InvalidRequest
            | Self::InvalidResponse
            | Self::ModelUnavailable
            | Self::BillingLimit
            | Self::OutputBudgetExhausted
            | Self::Unknown => LlmErrorKind::Terminal,
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

/// Provider-reported state for a tokens-per-minute quota window.
///
/// This is deliberately sourced only from structured HTTP headers. Provider
/// error prose is useful to a human, but it is not a contract and must not
/// become an orchestration input.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) struct ProviderTokenQuotaSnapshot {
    pub(crate) limit: u64,
    pub(crate) used: u64,
    pub(crate) window_ms: u64,
}

const TOKEN_QUOTA_HEADER_PAIRS: [(&str, &str); 2] = [
    ("x-ratelimit-limit-tokens", "x-ratelimit-remaining-tokens"),
    (
        "x-ratelimit-limit-tokens-minute",
        "x-ratelimit-remaining-tokens-minute",
    ),
];

/// Read a provider TPM snapshot without consulting its error message.
pub(crate) fn provider_token_quota_snapshot(
    headers: &reqwest::header::HeaderMap,
) -> Option<ProviderTokenQuotaSnapshot> {
    TOKEN_QUOTA_HEADER_PAIRS
        .iter()
        .find_map(|(limit_name, remaining_name)| {
            let limit = header_u64(headers, limit_name)?;
            let remaining = header_u64(headers, remaining_name)?.min(limit);
            (limit > 0).then_some(ProviderTokenQuotaSnapshot {
                limit,
                used: limit.saturating_sub(remaining),
                window_ms: 60_000,
            })
        })
}

fn header_u64(headers: &reqwest::header::HeaderMap, name: &str) -> Option<u64> {
    headers.get(name)?.to_str().ok()?.trim().parse::<u64>().ok()
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

/// Parse an RFC 7231 Retry-After field value into a bounded delay.
pub(crate) fn parse_retry_after_value(value: &str) -> Option<u64> {
    const MAX_MS: u64 = 60_000;
    let value = value.trim();
    let numeric_prefix = value
        .chars()
        .take_while(|character| character.is_ascii_digit() || *character == '.')
        .collect::<String>();
    if let Ok(seconds) = numeric_prefix.parse::<f64>() {
        if !seconds.is_finite() || seconds < 0.0 {
            return None;
        }
        return Some(((seconds * 1000.0) as u64).min(MAX_MS));
    }
    let target = httpdate::parse_http_date(value).ok()?;
    Some(
        target
            .duration_since(std::time::SystemTime::now())
            .map(|duration| duration.as_millis() as u64)
            .unwrap_or(0)
            .min(MAX_MS),
    )
}

fn provider_http_error_value(
    classified: LlmErrorInfo,
    status: reqwest::StatusCode,
    retry_after: Option<&str>,
    quota: Option<ProviderTokenQuotaSnapshot>,
) -> VmError {
    use crate::value::VmDictExt;

    let category = category_owned_by_llm_reason(classified.reason)
        .or_else(|| crate::value::error_category_for_http_status(status.as_u16()))
        .unwrap_or(match classified.reason {
            LlmErrorReason::RateLimit => ErrorCategory::RateLimit,
            LlmErrorReason::ServerError => ErrorCategory::ServerError,
            LlmErrorReason::NetworkError => ErrorCategory::TransientNetwork,
            LlmErrorReason::Timeout => ErrorCategory::Timeout,
            LlmErrorReason::AuthFailure => ErrorCategory::Auth,
            LlmErrorReason::ModelUnavailable => ErrorCategory::NotFound,
            _ => ErrorCategory::Generic,
        });
    let mut fields = std::collections::BTreeMap::new();
    fields.put_str("category", category.as_str());
    fields.put_str("kind", classified.kind.as_str());
    fields.put_str("reason", classified.reason.as_str());
    fields.put_str("message", classified.message);
    if let Some(ms) = retry_after.and_then(parse_retry_after_value) {
        fields.insert("retry_after_ms".to_string(), VmValue::Int(ms as i64));
    }
    let quota = quota.and_then(|quota| {
        Some((
            i64::try_from(quota.limit).ok()?,
            i64::try_from(quota.used).ok()?,
            i64::try_from(quota.window_ms).ok()?,
        ))
    });
    if let Some((limit, used, window_ms)) = quota {
        let mut snapshot = std::collections::BTreeMap::new();
        snapshot.put_str("schema", "harn.llm.provider_token_quota.v1");
        snapshot.put_str("resource", "tokens");
        snapshot.insert("limit".to_string(), VmValue::Int(limit));
        snapshot.insert("used".to_string(), VmValue::Int(used));
        snapshot.insert("window_ms".to_string(), VmValue::Int(window_ms));
        fields.insert("provider_quota".to_string(), VmValue::dict(snapshot));
    }
    VmError::Thrown(VmValue::dict(fields))
}

/// Reasons that name an error category independently of the HTTP status.
fn category_owned_by_llm_reason(reason: LlmErrorReason) -> Option<ErrorCategory> {
    match reason {
        LlmErrorReason::InvalidRequest => Some(ErrorCategory::InvalidRequest),
        _ => None,
    }
}

/// Classify a drained non-success response while retaining structured quota
/// headers in the thrown value consumed by the route limiter.
pub(crate) fn provider_http_error(
    dialect: Option<super::DialectContract>,
    provider: &str,
    status: reqwest::StatusCode,
    headers: &reqwest::header::HeaderMap,
    body: &str,
) -> VmError {
    let retry_after = retry_after_header(headers);
    let classified = match dialect {
        Some(dialect) => {
            dialect.classify_http_error(provider, status, retry_after.as_deref(), body)
        }
        None => classify_provider_http_error(provider, status, retry_after.as_deref(), body),
    };
    provider_http_error_value(
        classified,
        status,
        retry_after.as_deref(),
        provider_token_quota_snapshot(headers),
    )
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

/// Classify a mid-stream structured provider error payload (SSE `event: error`
/// or top-level error JSON) through the same sanitizer and taxonomy as HTTP
/// failures. Prefer explicit upstream `kind`/`reason` fields when present so a
/// valid terminal provider error does not fall through to premature EOF.
pub(crate) fn classify_provider_stream_error(provider: &str, body: &str, partial: bool) -> VmError {
    let json = serde_json::from_str::<serde_json::Value>(body).ok();
    let (kind, reason) = match explicit_stream_error_taxonomy(json.as_ref()) {
        Some(taxonomy) => taxonomy,
        // No HTTP status on an in-band SSE error frame; classify from body
        // fingerprints only (neutral status avoids status-forced reasons).
        None => classify_http_status_and_body(reqwest::StatusCode::OK, body),
    };
    let body_summary = sanitize_provider_error_body(body);
    let mut message = format!(
        "{provider} stream error [{}]: {body_summary}",
        reason.legacy_tag()
    );
    if reason == LlmErrorReason::ContextOverflow {
        if let Some(tokens) = extract_token_count_hint(body) {
            message.push_str(&format!(" (offending_tokens: {tokens})"));
        }
    }
    stream_error_thrown(kind, reason, message, partial)
}

fn explicit_stream_error_taxonomy(
    json: Option<&serde_json::Value>,
) -> Option<(LlmErrorKind, LlmErrorReason)> {
    let json = json?;
    let kind = json_taxonomy_str(json, "kind").and_then(LlmErrorKind::parse);
    let reason = json_taxonomy_str(json, "reason").and_then(LlmErrorReason::parse);
    match (kind, reason) {
        (Some(kind), Some(reason)) => Some((kind, reason)),
        (None, Some(reason)) => Some((reason.default_kind(), reason)),
        (Some(kind), None) => Some((kind, LlmErrorReason::Unknown)),
        (None, None) => None,
    }
}

fn json_taxonomy_str<'a>(json: &'a serde_json::Value, key: &str) -> Option<&'a str> {
    json.get(key)
        .and_then(serde_json::Value::as_str)
        .or_else(|| {
            json.get("error")
                .and_then(|error| error.get(key))
                .and_then(serde_json::Value::as_str)
        })
}

fn stream_error_thrown(
    kind: LlmErrorKind,
    reason: LlmErrorReason,
    message: String,
    partial: bool,
) -> VmError {
    use crate::value::VmDictExt;

    let mut fields = std::collections::BTreeMap::new();
    fields.put_str(
        "category",
        category_owned_by_llm_reason(reason)
            .unwrap_or(ErrorCategory::Generic)
            .as_str(),
    );
    fields.put_str("kind", kind.as_str());
    fields.put_str("reason", reason.as_str());
    fields.put_str("message", message);
    fields.put_str("source", "provider_stream");
    fields.put_bool("partial", partial);
    VmError::Thrown(VmValue::dict(fields))
}

/// Consume a non-2xx provider [`Response`] and build the thrown [`VmError`]
/// every provider adapter surfaces for a failed HTTP call. Reads the
/// `Retry-After` header before draining the body, since `Response::text`
/// takes the response by value.
pub(crate) async fn err_for_non_success(provider: &str, response: reqwest::Response) -> VmError {
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.text().await.unwrap_or_default();
    provider_http_error(None, provider, status, &headers, &body)
}

/// Consume and classify a non-success response through the resolved dialect
/// contract. Provider transports use this once request/response semantics have
/// moved behind [`super::DialectContract`].
pub(crate) async fn err_for_non_success_with_dialect(
    dialect: super::DialectContract,
    provider: &str,
    response: reqwest::Response,
) -> VmError {
    let status = response.status();
    let headers = response.headers().clone();
    let body = response.text().await.unwrap_or_default();
    provider_http_error(Some(dialect), provider, status, &headers, &body)
}

fn sanitize_provider_error_body(body: &str) -> String {
    let summary =
        structured_provider_error_summary(body).unwrap_or_else(|| body.trim().to_string());
    let redacted = redact_provider_error_secrets(&summary);
    crate::text::truncate_end(&redacted, MAX_PROVIDER_ERROR_BODY_CHARS)
}

fn structured_provider_error_summary(body: &str) -> Option<String> {
    let json: serde_json::Value = serde_json::from_str(body).ok()?;
    let error = json.get("error").unwrap_or(&json);
    if let Some(message) = provider_error_message(error).or_else(|| provider_error_message(&json)) {
        let message =
            crate::text::truncate_end(message, MAX_PROVIDER_ERROR_BODY_CHARS.saturating_sub(256));
        let mut details = Vec::new();
        collect_error_details(error, &mut details);
        if !std::ptr::eq(std::ptr::from_ref(error), std::ptr::addr_of!(json)) {
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
            let message = crate::text::truncate_end(message, 180);
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
        ErrorCategory::InvalidRequest => (LlmErrorKind::Terminal, LlmErrorReason::InvalidRequest),
        ErrorCategory::NotFound => (LlmErrorKind::Terminal, LlmErrorReason::ModelUnavailable),
        _ => (LlmErrorKind::Terminal, LlmErrorReason::Unknown),
    };

    LlmErrorInfo {
        kind,
        reason,
        message: message.to_string(),
    }
}

/// Structured error codes that name a hard billing or quota stop.
///
/// Matched against the provider's own `error.code` / `error.type` first,
/// because a code is a closed value the provider controls and a lowercased
/// body scan is not. The scan stays as a fallback for providers that send the
/// same condition as prose.
const BILLING_STOP_CODES: &[&str] = &[
    "insufficient_quota",
    "billing_hard_limit_reached",
    "billing_not_active",
    "account_deactivated",
];

/// Message phrasings that name the same condition on providers that do not
/// emit a code for it. Kept separate from the codes above so a reader can see
/// which half of this is a contract and which half is pattern matching.
const BILLING_STOP_PHRASES: &[&str] = &[
    "credit balance is too low",
    "exceeded your current quota",
    "spend limit",
    "spending limit",
    "billing hard limit",
];

/// The provider's own error code and type, when the body is the usual
/// `{"error": {...}}` envelope. Absence is not a billing stop; it just means
/// the caller has to fall back to the phrase scan.
fn provider_error_code_and_type(body: &str) -> (Option<String>, Option<String>) {
    let Ok(json) = serde_json::from_str::<serde_json::Value>(body) else {
        return (None, None);
    };
    let error = json.get("error").unwrap_or(&json);
    let field = |name: &str| {
        error
            .get(name)
            .and_then(serde_json::Value::as_str)
            .map(str::to_ascii_lowercase)
    };
    (field("code"), field("type"))
}

/// Whether this response says the account cannot pay, rather than that it is
/// going too fast.
///
/// `body` is used only for the structured read. A caller that holds only the
/// lowercased copy may pass it for both: the codes are lowercase already, so
/// the structured read still works, and nothing else here is case-sensitive.
pub(crate) fn is_billing_stop(body: &str, body_lower: &str) -> bool {
    let (code, kind) = provider_error_code_and_type(body);
    let names_a_billing_code = |value: &Option<String>| {
        value
            .as_deref()
            .is_some_and(|value| BILLING_STOP_CODES.contains(&value))
    };
    if names_a_billing_code(&code) || names_a_billing_code(&kind) {
        return true;
    }
    BILLING_STOP_CODES
        .iter()
        .chain(BILLING_STOP_PHRASES.iter())
        .any(|marker| body_lower.contains(marker))
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
    // Before the throttle arm, because these arrive as 429s and as bodies that
    // also say "quota". A backoff never clears them.
    if is_billing_stop(body, &body_lower) {
        return (LlmErrorKind::Terminal, LlmErrorReason::BillingLimit);
    }
    if status.as_u16() == 429 || body_lower.contains("rate_limit") {
        return (LlmErrorKind::Transient, LlmErrorReason::RateLimit);
    }
    if matches!(status.as_u16(), 408 | 504 | 522 | 524) || body_lower.contains("timeout") {
        return (LlmErrorKind::Transient, LlmErrorReason::Timeout);
    }
    if is_model_unavailable(&body_lower) || matches!(status.as_u16(), 404 | 410) {
        return (LlmErrorKind::Terminal, LlmErrorReason::ModelUnavailable);
    }
    if is_invalid_response(status, &body_lower) {
        return (LlmErrorKind::Terminal, LlmErrorReason::InvalidResponse);
    }
    if matches!(status.as_u16(), 500 | 502 | 503 | 529)
        || body_lower.contains("internal_server_error")
        || body_lower.contains("server_error")
        || body_lower.contains("upstream_error")
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
    if lower.contains("reason=empty_generation") {
        return Some((LlmErrorKind::Transient, LlmErrorReason::EmptyGeneration));
    }
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
    if lower.contains("[invalid_response]") {
        return Some((LlmErrorKind::Terminal, LlmErrorReason::InvalidResponse));
    }
    if lower.contains("[billing_limit]") || is_billing_stop(msg, &lower) {
        return Some((LlmErrorKind::Terminal, LlmErrorReason::BillingLimit));
    }
    if lower.contains("[rate_limited]") || lower.contains("too many requests") {
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

fn is_invalid_response(status: reqwest::StatusCode, body_lower: &str) -> bool {
    status.as_u16() == 500
        && INVALID_RESPONSE_FINGERPRINTS
            .iter()
            .any(|fingerprint| fingerprint.iter().all(|marker| body_lower.contains(marker)))
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
#[path = "errors_tests.rs"]
mod tests;
