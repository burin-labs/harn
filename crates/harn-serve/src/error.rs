use std::collections::BTreeSet;
use std::fmt::{Display, Formatter};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DispatchError {
    /// The caller failed authentication (no credential matched any
    /// configured method). Adapters render this as HTTP 401.
    Unauthorized(String),
    /// The caller authenticated successfully but lacks one or more scopes
    /// required by the invoked function. Adapters render this as HTTP 403
    /// with a structured body that reports `required` vs `granted` so the
    /// client can prompt for the right credential.
    Forbidden {
        required: BTreeSet<String>,
        granted: BTreeSet<String>,
    },
    /// One of the route's declared rate-limit buckets (per-route /
    /// per-tenant / per-scope) or the backpressure watermark rejected
    /// this dispatch. Adapters render this as HTTP 429 with a
    /// `Retry-After` header derived from `retry_after_ms`. `scope`
    /// identifies which bucket dimension fired so callers can attribute
    /// the rejection (e.g. "your tenant quota" vs "global route ceiling").
    RateLimited {
        scope: String,
        retry_after_ms: u64,
    },
    /// A `@budget(...)` ceiling declared on the route was exhausted
    /// mid-call (e.g. accumulated LLM cost rose above `llm_cost_usd`).
    /// Adapters render this as HTTP 429 with `code = "budget_exceeded"`.
    BudgetExceeded {
        category: String,
        message: String,
    },
    Validation(String),
    MissingExport(String),
    Cancelled(String),
    Execution(String),
    Io(String),
    Cache(String),
}

impl DispatchError {
    /// Human-readable message describing this dispatch error. The
    /// `Forbidden` variant flattens its scope sets into a stable
    /// `missing required scope(s): a, b` form so existing string-based
    /// log/metric sinks pick up scope context without restructuring.
    pub fn message(&self) -> String {
        match self {
            Self::Unauthorized(message)
            | Self::Validation(message)
            | Self::MissingExport(message)
            | Self::Cancelled(message)
            | Self::Execution(message)
            | Self::Io(message)
            | Self::Cache(message) => message.clone(),
            Self::Forbidden { required, granted } => forbidden_message(required, granted),
            Self::RateLimited {
                scope,
                retry_after_ms,
            } => format!("rate limit exceeded ({scope}); retry after {retry_after_ms} ms"),
            Self::BudgetExceeded { category, message } => {
                format!("budget exceeded ({category}): {message}")
            }
        }
    }
}

/// Render a stable diagnostic for a scope-mismatch decision. Used both
/// by `DispatchError::Forbidden::message()` and by adapter-layer error
/// envelopes (JSON-RPC `data.message`, HTTP body, ACP error reply) so
/// callers see the same text everywhere.
pub fn forbidden_message(required: &BTreeSet<String>, granted: &BTreeSet<String>) -> String {
    let missing: Vec<&str> = required.difference(granted).map(String::as_str).collect();
    if missing.is_empty() {
        "missing required scope".to_string()
    } else {
        format!("missing required scope(s): {}", missing.join(", "))
    }
}

/// Structured `forbidden` payload shared across adapter error envelopes
/// (MCP JSON-RPC `error.data`, A2A JSON-RPC `error.data`, REST `error`
/// body). Producing it from one place keeps the field layout
/// (`kind`/`required_scopes`/`granted_scopes`/`missing_scopes`) stable
/// for clients that parse it programmatically.
pub fn forbidden_data_payload(
    required: &BTreeSet<String>,
    granted: &BTreeSet<String>,
) -> serde_json::Value {
    let missing: Vec<&str> = required.difference(granted).map(String::as_str).collect();
    serde_json::json!({
        "kind": "forbidden",
        "required_scopes": required.iter().collect::<Vec<_>>(),
        "granted_scopes": granted.iter().collect::<Vec<_>>(),
        "missing_scopes": missing,
    })
}

impl Display for DispatchError {
    fn fmt(&self, f: &mut Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message())
    }
}

impl std::error::Error for DispatchError {}
