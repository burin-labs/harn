//! Typed construction and classification for deterministic mock-provider errors.

use crate::value::ErrorCategory;

/// Categorized error injected by a mock. When present, the mock
/// short-circuits the provider call and surfaces through the same typed error
/// envelope as a live provider failure.
#[derive(Clone, Debug)]
pub struct MockError {
    pub category: ErrorCategory,
    pub message: String,
    pub status: Option<u16>,
    pub kind: Option<String>,
    pub reason: Option<String>,
    /// Optional retry hint. Provider-envelope mocks put this directly on the
    /// thrown dict; legacy mocks embed it in the message for parser parity.
    pub retry_after_ms: Option<u64>,
}

impl MockError {
    pub(super) fn has_provider_envelope(&self) -> bool {
        self.status.is_some() || self.kind.is_some() || self.reason.is_some()
    }
}

pub(crate) fn build_mock_error(
    category: Option<String>,
    message: Option<String>,
    status: Option<u16>,
    kind: Option<String>,
    reason: Option<String>,
    retry_after_ms: Option<u64>,
) -> Result<MockError, String> {
    if retry_after_ms.is_some_and(|ms| ms > i64::MAX as u64) {
        return Err("error.retry_after_ms must fit in a signed 64-bit integer".to_string());
    }
    let kind = match kind {
        Some(value) if value.trim().is_empty() => None,
        Some(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            if super::api::LlmErrorKind::parse(&normalized).is_none() {
                return Err(format!("unknown error kind `{value}`"));
            }
            Some(normalized)
        }
        None => None,
    };
    let reason = reason.and_then(|value| {
        let trimmed = value.trim();
        (!trimmed.is_empty()).then(|| trimmed.to_string())
    });
    let category_was_provided = category.is_some();
    let category = match category {
        Some(value) if value.trim().is_empty() => {
            return Err("error.category must not be empty".to_string());
        }
        Some(value) => {
            let normalized = value.trim().to_ascii_lowercase();
            let category = ErrorCategory::parse(&normalized);
            if category.as_str() != normalized {
                return Err(format!("unknown error category `{value}`"));
            }
            category
        }
        None => infer_mock_error_category(status, kind.as_deref(), reason.as_deref()),
    };
    if !category_was_provided && kind.is_none() && status.is_none() && reason.is_none() {
        return Err(
            "error.category is required unless error.status, error.kind, or error.reason is set"
                .to_string(),
        );
    }
    Ok(MockError {
        category,
        message: message.unwrap_or_else(|| {
            default_mock_error_message(status, kind.as_deref(), reason.as_deref())
        }),
        status,
        kind,
        reason,
        retry_after_ms,
    })
}

pub(crate) fn validate_mock_error_status(status: i64) -> Result<u16, String> {
    let status = u16::try_from(status)
        .map_err(|_| "error.status must be an HTTP status code".to_string())?;
    reqwest::StatusCode::from_u16(status)
        .map_err(|_| "error.status must be an HTTP status code".to_string())?;
    Ok(status)
}

fn infer_mock_error_category(
    status: Option<u16>,
    kind: Option<&str>,
    reason: Option<&str>,
) -> ErrorCategory {
    if let Some(category) = status.and_then(crate::value::error_category_for_http_status) {
        return category;
    }
    if let Some(reason) = reason {
        match reason {
            "rate_limit" => return ErrorCategory::RateLimit,
            "timeout" => return ErrorCategory::Timeout,
            "network_error" | "transient_network" => return ErrorCategory::TransientNetwork,
            "server_error" | "provider_error" | "provider_5xx" | "upstream_unavailable" => {
                return ErrorCategory::ServerError;
            }
            "auth_failure" => return ErrorCategory::Auth,
            "model_unavailable" => return ErrorCategory::NotFound,
            _ => {}
        }
    }
    if kind == Some("transient") {
        return ErrorCategory::ServerError;
    }
    ErrorCategory::Generic
}

fn default_mock_error_message(
    status: Option<u16>,
    kind: Option<&str>,
    reason: Option<&str>,
) -> String {
    match (status, kind, reason) {
        (Some(status), Some(kind), Some(reason)) => {
            format!("HTTP {status} mock LLM error ({kind}/{reason})")
        }
        (Some(status), _, Some(reason)) => format!("HTTP {status} mock LLM error ({reason})"),
        (Some(status), _, _) => format!("HTTP {status} mock LLM error"),
        (None, Some(kind), Some(reason)) => format!("mock LLM error ({kind}/{reason})"),
        (None, Some(kind), None) => format!("mock LLM error ({kind})"),
        (None, None, Some(reason)) => format!("mock LLM error ({reason})"),
        (None, None, None) => String::new(),
    }
}
