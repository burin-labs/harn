use super::VmValue;

#[derive(Debug, Clone)]
pub enum VmError {
    StackUnderflow,
    StackOverflow,
    UndefinedVariable(String),
    UndefinedBuiltin(String),
    ImmutableAssignment(String),
    TypeError(String),
    Runtime(String),
    DivisionByZero,
    Thrown(VmValue),
    /// Thrown with error category for structured error handling.
    CategorizedError {
        message: String,
        category: ErrorCategory,
    },
    Return(VmValue),
    InvalidInstruction(u8),
}

/// Error categories for structured error handling in agent orchestration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorCategory {
    /// Network/connection timeout
    Timeout,
    /// Authentication/authorization failure
    Auth,
    /// Rate limit exceeded (HTTP 429 / quota)
    RateLimit,
    /// Upstream provider is overloaded (HTTP 503 / 529).
    /// Distinct from RateLimit: the client hasn't exceeded a quota — the
    /// provider is shedding load and will recover on its own.
    Overloaded,
    /// Provider-side 5xx error (500, 502) that isn't specifically overload.
    ServerError,
    /// Network-level transient failure (connection reset, DNS hiccup,
    /// partial stream) — retryable but not provider-status-coded.
    TransientNetwork,
    /// LLM output failed schema validation. Retryable via `schema_retries`.
    SchemaValidation,
    /// Tool execution failure
    ToolError,
    /// Tool was rejected by the host (not permitted / not in allowlist)
    ToolRejected,
    /// Operation was cancelled
    Cancelled,
    /// Resource not found
    NotFound,
    /// Circuit breaker is open
    CircuitOpen,
    /// Generic/unclassified error
    Generic,
}

impl ErrorCategory {
    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCategory::Timeout => "timeout",
            ErrorCategory::Auth => "auth",
            ErrorCategory::RateLimit => "rate_limit",
            ErrorCategory::Overloaded => "overloaded",
            ErrorCategory::ServerError => "server_error",
            ErrorCategory::TransientNetwork => "transient_network",
            ErrorCategory::SchemaValidation => "schema_validation",
            ErrorCategory::ToolError => "tool_error",
            ErrorCategory::ToolRejected => "tool_rejected",
            ErrorCategory::Cancelled => "cancelled",
            ErrorCategory::NotFound => "not_found",
            ErrorCategory::CircuitOpen => "circuit_open",
            ErrorCategory::Generic => "generic",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "timeout" => ErrorCategory::Timeout,
            "auth" => ErrorCategory::Auth,
            "rate_limit" => ErrorCategory::RateLimit,
            "overloaded" => ErrorCategory::Overloaded,
            "server_error" => ErrorCategory::ServerError,
            "transient_network" => ErrorCategory::TransientNetwork,
            "schema_validation" => ErrorCategory::SchemaValidation,
            "tool_error" => ErrorCategory::ToolError,
            "tool_rejected" => ErrorCategory::ToolRejected,
            "cancelled" => ErrorCategory::Cancelled,
            "not_found" => ErrorCategory::NotFound,
            "circuit_open" => ErrorCategory::CircuitOpen,
            _ => ErrorCategory::Generic,
        }
    }

    /// Whether an error of this category is worth retrying for a transient
    /// provider-side reason. Agent loops consult this to decide whether to
    /// back off and retry vs surface the error to the user.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            ErrorCategory::Timeout
                | ErrorCategory::RateLimit
                | ErrorCategory::Overloaded
                | ErrorCategory::ServerError
                | ErrorCategory::TransientNetwork
        )
    }
}

/// Create a categorized error conveniently.
pub fn categorized_error(message: impl Into<String>, category: ErrorCategory) -> VmError {
    VmError::CategorizedError {
        message: message.into(),
        category,
    }
}

/// Extract error category from a VmError.
///
/// Classification priority:
/// 1. Explicit CategorizedError variant (set by throw_error or internal code)
/// 2. Thrown dict with a "category" field (user-created structured errors)
/// 3. HTTP status code extraction (standard, unambiguous)
/// 4. Deadline exceeded (VM-internal)
/// 5. Fallback to Generic
pub fn error_to_category(err: &VmError) -> ErrorCategory {
    match err {
        VmError::CategorizedError { category, .. } => category.clone(),
        VmError::Thrown(VmValue::Dict(d)) => d
            .get("category")
            .map(|v| ErrorCategory::parse(&v.display()))
            .unwrap_or(ErrorCategory::Generic),
        VmError::Thrown(VmValue::String(s)) => classify_error_message(s),
        VmError::Runtime(msg) => classify_error_message(msg),
        _ => ErrorCategory::Generic,
    }
}

/// Classify an error message using HTTP status codes and well-known patterns.
/// Prefers unambiguous signals (status codes) over substring heuristics.
pub fn classify_error_message(msg: &str) -> ErrorCategory {
    // 1. HTTP status codes — most reliable signal
    if let Some(cat) = classify_by_http_status(msg) {
        return cat;
    }
    // 2. Well-known error identifiers from major APIs
    //    (Anthropic, OpenAI, and standard HTTP patterns)
    if msg.contains("Deadline exceeded") || msg.contains("context deadline exceeded") {
        return ErrorCategory::Timeout;
    }
    if msg.contains("overloaded_error") {
        // Anthropic overloaded_error surfaces as HTTP 529.
        return ErrorCategory::Overloaded;
    }
    if msg.contains("api_error") {
        // Anthropic catch-all server-side error.
        return ErrorCategory::ServerError;
    }
    if msg.contains("insufficient_quota") || msg.contains("billing_hard_limit_reached") {
        // OpenAI-specific quota error types.
        return ErrorCategory::RateLimit;
    }
    if msg.contains("invalid_api_key") || msg.contains("authentication_error") {
        return ErrorCategory::Auth;
    }
    if msg.contains("not_found_error") || msg.contains("model_not_found") {
        return ErrorCategory::NotFound;
    }
    if msg.contains("circuit_open") {
        return ErrorCategory::CircuitOpen;
    }
    // Network-level transient patterns (pre-HTTP-status, pre-provider-framing).
    let lower = msg.to_lowercase();
    if lower.contains("connection reset")
        || lower.contains("connection refused")
        || lower.contains("connection closed")
        || lower.contains("broken pipe")
        || lower.contains("dns error")
        || lower.contains("stream error")
        || lower.contains("unexpected eof")
    {
        return ErrorCategory::TransientNetwork;
    }
    ErrorCategory::Generic
}

/// Classify errors by HTTP status code if one appears in the message.
/// This is the most reliable classification method since status codes
/// are standardized (RFC 9110) and unambiguous.
fn classify_by_http_status(msg: &str) -> Option<ErrorCategory> {
    // Extract 3-digit HTTP status codes from common patterns:
    // "HTTP 429", "status 429", "429 Too Many", "error: 401"
    for code in extract_http_status_codes(msg) {
        return Some(match code {
            401 | 403 => ErrorCategory::Auth,
            404 | 410 => ErrorCategory::NotFound,
            408 | 504 | 522 | 524 => ErrorCategory::Timeout,
            429 => ErrorCategory::RateLimit,
            503 | 529 => ErrorCategory::Overloaded,
            500 | 502 => ErrorCategory::ServerError,
            _ => continue,
        });
    }
    None
}

/// Extract plausible HTTP status codes from an error message.
fn extract_http_status_codes(msg: &str) -> Vec<u16> {
    let mut codes = Vec::new();
    let bytes = msg.as_bytes();
    for i in 0..bytes.len().saturating_sub(2) {
        // Look for 3-digit sequences in the 100-599 range
        if bytes[i].is_ascii_digit()
            && bytes[i + 1].is_ascii_digit()
            && bytes[i + 2].is_ascii_digit()
        {
            // Ensure it's not part of a longer number
            let before_ok = i == 0 || !bytes[i - 1].is_ascii_digit();
            let after_ok = i + 3 >= bytes.len() || !bytes[i + 3].is_ascii_digit();
            if before_ok && after_ok {
                if let Ok(code) = msg[i..i + 3].parse::<u16>() {
                    if (400..=599).contains(&code) {
                        codes.push(code);
                    }
                }
            }
        }
    }
    codes
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VmError::StackUnderflow => write!(f, "Stack underflow"),
            VmError::StackOverflow => write!(f, "Stack overflow: too many nested calls"),
            VmError::UndefinedVariable(n) => write!(f, "Undefined variable: {n}"),
            VmError::UndefinedBuiltin(n) => write!(f, "Undefined builtin: {n}"),
            VmError::ImmutableAssignment(n) => {
                write!(f, "Cannot assign to immutable binding: {n}")
            }
            VmError::TypeError(msg) => write!(f, "Type error: {msg}"),
            VmError::Runtime(msg) => write!(f, "Runtime error: {msg}"),
            VmError::DivisionByZero => write!(f, "Division by zero"),
            VmError::Thrown(v) => write!(f, "Thrown: {}", v.display()),
            VmError::CategorizedError { message, category } => {
                write!(f, "Error [{}]: {}", category.as_str(), message)
            }
            VmError::Return(_) => write!(f, "Return from function"),
            VmError::InvalidInstruction(op) => write!(f, "Invalid instruction: 0x{op:02x}"),
        }
    }
}

impl std::error::Error for VmError {}
