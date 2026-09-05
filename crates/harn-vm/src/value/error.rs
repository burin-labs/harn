use harn_lexer::Span;

use super::{VmDictExt, VmValue};

/// Bound expressing how many arguments a callable accepts. Used in
/// [`VmError::ArityMismatch`] so error messages can render the exact
/// signature contract the caller violated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ArityExpect {
    /// Exactly N parameters, no defaults, no rest.
    Exact(usize),
    /// `min..=max`: some params have defaults but the upper bound is fixed.
    Range { min: usize, max: usize },
    /// At least N parameters; further args land in a rest list. Used for
    /// `print` / `log` / variadics.
    AtLeast(usize),
}

impl std::fmt::Display for ArityExpect {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            ArityExpect::Exact(n) => write!(f, "{n}"),
            ArityExpect::Range { min, max } => write!(f, "{min}..={max}"),
            ArityExpect::AtLeast(n) => write!(f, "at least {n}"),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ArityMismatchError {
    pub callee: String,
    pub expected: ArityExpect,
    pub got: usize,
    pub span: Option<Span>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeadlockDiagnostic {
    SelfDeadlock,
    WaitForGraph,
}

impl DeadlockDiagnostic {
    fn code(self) -> &'static str {
        match self {
            Self::SelfDeadlock => "HARN-ORC-011",
            Self::WaitForGraph => "HARN-ORC-012",
        }
    }
}

/// Payload for [`VmError::Deadlock`]. `kind` is the primitive kind
/// (`"mutex"`, `"channel"`) or `"task"`; `key` is the primitive key or task
/// id; `detail` names the specific footgun.
#[derive(Debug, Clone)]
pub struct DeadlockError {
    pub diagnostic: DeadlockDiagnostic,
    pub kind: String,
    pub key: String,
    pub detail: String,
}

impl DeadlockError {
    pub(crate) fn self_deadlock(
        kind: impl Into<String>,
        key: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            diagnostic: DeadlockDiagnostic::SelfDeadlock,
            kind: kind.into(),
            key: key.into(),
            detail: detail.into(),
        }
    }

    pub(crate) fn wait_for_graph(
        kind: impl Into<String>,
        key: impl Into<String>,
        detail: impl Into<String>,
    ) -> Self {
        Self {
            diagnostic: DeadlockDiagnostic::WaitForGraph,
            kind: kind.into(),
            key: key.into(),
            detail: detail.into(),
        }
    }
}

#[derive(Debug, Clone)]
pub struct ArgTypeMismatchError {
    pub callee: String,
    pub param: String,
    pub expected: String,
    pub got: &'static str,
    pub span: Option<Span>,
}

/// A value did not satisfy the type declared on the `let` / `const` it was
/// being bound to.
///
/// Deliberately separate from [`ArgTypeMismatchError`]: both report a declared
/// type rejecting a value, but the reader needs to know which binding site
/// failed, and "parameter" is the wrong noun for half of them.
#[derive(Debug, Clone)]
pub struct BindingTypeMismatchError {
    /// The declared name, or the rendered pattern for a destructuring binding.
    pub binding: String,
    pub expected: String,
    pub got: &'static str,
    pub span: Option<Span>,
}

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
    /// A host-imposed deadline expired while executing the VM. Unlike a Harn
    /// `deadline` block, this control-plane stop cannot be caught by user code.
    ExecutionDeadlineExceeded,
    /// A Harn program requested that its embedding process terminate with this
    /// status code. Like a host deadline, this is control flow rather than a
    /// catchable Harn error; the embedding boundary owns the final cleanup and
    /// process exit.
    ProcessExit(i32),
    /// A host dropped a polled top-level execution future. Interpreter state is
    /// intentionally not resumed after arbitrary async cancellation. Discard
    /// this VM; see [`crate::Vm::execute_with_timeout`] for ambient-state
    /// cleanup requirements.
    AbandonedExecution,
    /// A stable MCP handler needs client input before it can complete. The MCP
    /// server boundary converts this uncatchable control signal into an
    /// `input_required` result and re-enters the handler on retry.
    McpInputRequired(Box<crate::mcp_input::McpInputRequired>),
    Thrown(VmValue),
    /// Thrown with error category for structured error handling.
    CategorizedError {
        message: String,
        category: ErrorCategory,
    },
    /// A provider stream failed before its protocol supplied a terminal
    /// sentinel. Carries transport phase and deadline provenance structurally
    /// so retry, transcript, and host projections never need to reclassify
    /// rendered prose.
    ProviderStreamFailure(Box<ProviderStreamFailure>),
    /// A streamed model response became impossible under its output schema.
    /// Carries the validator's closed reason kind and exact detail so retries,
    /// caught values, and transcript receipts never rebuild cause data from
    /// the human-readable message.
    SchemaStreamAbort(Box<SchemaStreamAbort>),
    /// A spawn required a platform sandbox mechanism the host could not
    /// supply. Carries the mechanism, the requested profile, and the
    /// unsatisfied requirement structurally, so an embedder renders its own
    /// remedy sentence instead of Harn guessing which control its operator has.
    SandboxMechanismUnavailable(Box<crate::process_sandbox::SandboxMechanismUnavailable>),
    DaemonQueueFull {
        daemon_id: String,
        capacity: usize,
    },
    /// A deterministic, provably-unresolvable self-deadlock caught before the
    /// VM would block forever (Rust's borrow checker prevents data races but
    /// not deadlocks; this is the Go-runtime "all goroutines asleep" analogue
    /// for the cases we can prove). Boxed — like [`VmError::ArityMismatch`] —
    /// so the rare three-`String` payload doesn't enlarge `VmError` on the
    /// pervasive `Result<VmValue, VmError>` hot path. Carries `HARN-ORC-011`.
    Deadlock(Box<DeadlockError>),
    Return(VmValue),
    InvalidInstruction(u8),
    /// Wrong number of arguments at a call site. Distinct from
    /// [`VmError::TypeError`] so the runtime can match-and-recover (and
    /// so error UX renders `expected 2..=3 got 1` consistently).
    ArityMismatch(Box<ArityMismatchError>),
    /// Argument value did not satisfy the declared parameter type.
    /// `expected` is a pretty-printed type expression; `got` is the value's
    /// runtime type name (`VmValue::type_name`). Used for both
    /// user-defined function parameters (with declared types) and
    /// registry-known builtin parameters.
    ArgTypeMismatch(Box<ArgTypeMismatchError>),
    /// Initializer value did not satisfy the type declared on its binding.
    /// The binding-site counterpart of [`VmError::ArgTypeMismatch`]; a
    /// declared type is checked wherever it is written.
    BindingTypeMismatch(Box<BindingTypeMismatchError>),
}

impl VmError {
    /// Whether this error is VM control flow that user `catch` blocks and
    /// error-as-data combinators must propagate unchanged.
    pub fn is_uncatchable_control_flow(&self) -> bool {
        matches!(
            self,
            Self::ExecutionDeadlineExceeded | Self::ProcessExit(_) | Self::McpInputRequired(_)
        )
    }

    /// The requested host-process exit status, when this is an explicit Harn
    /// `exit(code)` control signal.
    pub fn process_exit_code(&self) -> Option<i32> {
        match self {
            Self::ProcessExit(code) => Some(*code),
            _ => None,
        }
    }

    /// The `VmValue` a `catch` binding (or a `parallel settle` result) observes
    /// for this error: the raw thrown value for [`VmError::Thrown`] (so a
    /// structured error — e.g. a `{category, message}` dict from `throw_error` —
    /// keeps its shape and category), a structured `{category, message}` dict for
    /// [`VmError::CategorizedError`] (so consumers branch on the typed category
    /// rather than substring-matching rendered prose), otherwise the rendered
    /// message.
    ///
    /// Single source of truth for VM-error-to-value lowering so every seam that
    /// surfaces a caught error to Harn (`try`/`catch` via `handle_error`,
    /// `parallel settle`) exposes identical, structure-preserving values. Before
    /// this was shared, `parallel settle` stringified errors via `to_string()`,
    /// so a categorized error thrown in a settle branch lost its category (a
    /// `cancelled`/`internal` fault that must propagate looked `generic`).
    ///
    /// The `category` key uses [`ErrorCategory::as_str`] — a canonical,
    /// exhaustively-matched snake_case contract — so a new variant added there
    /// is compiler-forced to name its key. `message` preserves the original
    /// rendered text, so a `catch` that stringifies the caught value still reads
    /// sensibly (the dict renders both fields).
    pub fn thrown_value(&self) -> VmValue {
        match self {
            VmError::Thrown(v) => v.clone(),
            VmError::CategorizedError { message, category } => {
                let mut dict = std::collections::BTreeMap::new();
                dict.put_str("category", category.as_str());
                dict.put_str("message", message);
                VmValue::dict(dict)
            }
            VmError::ProviderStreamFailure(failure) => failure.thrown_value(),
            VmError::SchemaStreamAbort(abort) => abort.thrown_value(),
            VmError::SandboxMechanismUnavailable(refusal) => refusal.thrown_value(),
            other => VmValue::String(arcstr::ArcStr::from(other.to_string())),
        }
    }

    pub fn provider_stream_failure(&self) -> Option<&ProviderStreamFailure> {
        match self {
            Self::ProviderStreamFailure(failure) => Some(failure),
            _ => None,
        }
    }

    pub fn schema_stream_abort(&self) -> Option<&SchemaStreamAbort> {
        match self {
            Self::SchemaStreamAbort(abort) => Some(abort),
            _ => None,
        }
    }

    /// The typed sandbox-mechanism refusal, when a spawn was refused because
    /// the platform mechanism could not be attached.
    pub fn sandbox_mechanism_unavailable(
        &self,
    ) -> Option<&crate::process_sandbox::SandboxMechanismUnavailable> {
        match self {
            Self::SandboxMechanismUnavailable(refusal) => Some(refusal),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderStreamPhase {
    AwaitingFirstChunk,
    Streaming,
}

impl ProviderStreamPhase {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::AwaitingFirstChunk => "awaiting_first_chunk",
            Self::Streaming => "streaming",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderStreamFailureReason {
    Read,
    PrematureEof,
    Deadline,
}

impl ProviderStreamFailureReason {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Read => "read",
            Self::PrematureEof => "premature_eof",
            Self::Deadline => "deadline",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProviderStreamDeadline {
    Total,
    FirstChunk,
    Idle,
}

impl ProviderStreamDeadline {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Total => "total",
            Self::FirstChunk => "first_chunk",
            Self::Idle => "idle",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProviderStreamFailure {
    pub provider: String,
    pub phase: ProviderStreamPhase,
    pub reason: ProviderStreamFailureReason,
    pub deadline: Option<ProviderStreamDeadline>,
    pub partial: bool,
    pub detail: String,
}

impl ProviderStreamFailure {
    pub fn category(&self) -> ErrorCategory {
        if self.deadline.is_some() {
            ErrorCategory::Timeout
        } else {
            ErrorCategory::TransientNetwork
        }
    }

    fn thrown_value(&self) -> VmValue {
        let mut dict = std::collections::BTreeMap::new();
        dict.put_str("category", self.category().as_str());
        dict.put_str("message", self.to_string());
        dict.put_str("source", "provider_stream");
        dict.put_str("phase", self.phase.as_str());
        dict.put_str("reason", self.reason.as_str());
        dict.insert(
            "deadline".to_string(),
            self.deadline
                .map(|deadline| VmValue::String(arcstr::ArcStr::from(deadline.as_str())))
                .unwrap_or(VmValue::Nil),
        );
        dict.insert("partial".to_string(), VmValue::Bool(self.partial));
        VmValue::dict(dict)
    }
}

impl std::fmt::Display for ProviderStreamFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "{} provider stream failure (phase={}, reason={}",
            self.provider,
            self.phase.as_str(),
            self.reason.as_str()
        )?;
        if let Some(deadline) = self.deadline {
            write!(f, ", deadline={}", deadline.as_str())?;
        }
        write!(f, ", partial={}): {}", self.partial, self.detail)
    }
}

/// Stable cause classes for JSON and schema validation failures.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SchemaValidationReasonKind {
    InvalidJson,
    InvalidSchema,
    WrongType,
    MissingRequired,
    UnexpectedProperty,
    MaxLength,
    MinLength,
    ConstraintViolation,
}

impl SchemaValidationReasonKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::InvalidJson => "invalid_json",
            Self::InvalidSchema => "invalid_schema",
            Self::WrongType => "wrong_type",
            Self::MissingRequired => "missing_required",
            Self::UnexpectedProperty => "unexpected_property",
            Self::MaxLength => "max_length",
            Self::MinLength => "min_length",
            Self::ConstraintViolation => "constraint_violation",
        }
    }
}

/// Typed cause retained when incremental schema validation aborts a provider
/// stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SchemaStreamAbort {
    pub provider: String,
    pub model: String,
    pub reason_kind: SchemaValidationReasonKind,
    pub reason: String,
    pub path: String,
    pub chunks_consumed: usize,
}

impl SchemaStreamAbort {
    pub fn category(&self) -> ErrorCategory {
        ErrorCategory::SchemaStreamAborted
    }

    fn thrown_value(&self) -> VmValue {
        let mut cause = std::collections::BTreeMap::new();
        cause.put_str("kind", self.reason_kind.as_str());
        cause.put_str("detail", self.reason.as_str());
        cause.put_str("path", self.path.as_str());
        cause.insert(
            "chunks_consumed".to_string(),
            VmValue::Int(self.chunks_consumed as i64),
        );
        cause.put_str("provider", self.provider.as_str());
        cause.put_str("model", self.model.as_str());

        let mut dict = std::collections::BTreeMap::new();
        dict.put_str("category", self.category().as_str());
        dict.put_str("message", self.to_string());
        dict.put_str("source", "schema_stream");
        dict.insert("schema_failure".to_string(), VmValue::dict(cause));
        VmValue::dict(dict)
    }
}

impl std::fmt::Display for SchemaStreamAbort {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "schema_stream_aborted at {}: {} (provider={} model={} chunks_consumed={})",
            self.path, self.reason, self.provider, self.model, self.chunks_consumed
        )
    }
}

/// Error categories for structured error handling in agent orchestration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorCategory {
    /// Network/connection timeout
    Timeout,
    /// Authentication/authorization failure
    Auth,
    /// Provider rejected the request. Correct it before retrying.
    InvalidRequest,
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
    /// A shared local resource is temporarily unavailable, such as a
    /// contended database write lock.
    ResourceBusy,
    /// A persistent store was written by a newer incompatible schema owner.
    /// Retrying cannot help; the caller must upgrade or deliberately degrade.
    SchemaIncompatible,
    /// LLM output failed schema validation. Retryable via `schema_retries`.
    SchemaValidation,
    /// LLM streaming response was aborted mid-stream because the partial
    /// JSON content could not conceivably satisfy `output_schema`. Surfaced
    /// by `llm_call` when `schema_stream_abort` is on (the default for
    /// schema-bearing calls). Consumes one `schema_retries` budget slot;
    /// the retry replays the prompt with a corrective nudge that cites
    /// the abort path + reason.
    SchemaStreamAborted,
    /// Tool execution failure
    ToolError,
    /// Tool was rejected by the host (not permitted / not in allowlist)
    ToolRejected,
    /// Outbound network egress was blocked by policy.
    EgressBlocked,
    /// Operation was cancelled
    Cancelled,
    /// Channel was closed before the operation could complete.
    ChannelClosed,
    /// Resource not found
    NotFound,
    /// Circuit breaker is open
    CircuitOpen,
    /// LLM cost or token budget would be exceeded
    BudgetExceeded,
    /// An internal engine/wiring bug — an undefined builtin, corrupt bytecode,
    /// or another VM invariant violation that no amount of retrying or model
    /// reasoning can fix. Distinct from `Generic` so callers (notably the agent
    /// loop) can re-raise it loudly instead of folding it into a tool-error
    /// observation and marching on to a `done` status. This is the category
    /// that keeps a mis-wired builtin (e.g. a `#[harn_builtin]` def missing
    /// from its install array) from shipping silently inert.
    Internal,
    /// A host environment / infrastructure problem that is not the workload's
    /// code defect: a required developer-toolchain root or cache lies outside
    /// the sandbox profile, a needed system binary is missing, or another
    /// machine-provisioning gap. Distinct from `ToolRejected` (the host
    /// deliberately refused an action) and `Internal` (an engine bug): the fix
    /// is to widen the sandbox/config or provision the host, not to change the
    /// agent's code. Callers (and embedders) branch on this to avoid blaming
    /// the model for an environment gap.
    Environment,
    /// Generic/unclassified error
    Generic,
}

impl ErrorCategory {
    /// Every category, in declaration order.
    ///
    /// Sibling taxonomies (`ToolCallErrorCategory::ALL`,
    /// `AgentTerminalKind::ALL`) already publish theirs, and code that has to
    /// decide something for EVERY category — a wire projection, a docs table,
    /// a round-trip guard — needs to enumerate them. While this list lived in
    /// one module's test scope, the tool-call wire projection could not consult
    /// it, and a category with no decided wire bucket went unnoticed (#5537).
    pub const ALL: [Self; 22] = [
        Self::Timeout,
        Self::Auth,
        Self::InvalidRequest,
        Self::RateLimit,
        Self::Overloaded,
        Self::ServerError,
        Self::TransientNetwork,
        Self::ResourceBusy,
        Self::SchemaIncompatible,
        Self::SchemaValidation,
        Self::SchemaStreamAborted,
        Self::ToolError,
        Self::ToolRejected,
        Self::EgressBlocked,
        Self::Cancelled,
        Self::ChannelClosed,
        Self::NotFound,
        Self::CircuitOpen,
        Self::BudgetExceeded,
        Self::Internal,
        Self::Environment,
        Self::Generic,
    ];

    pub fn as_str(&self) -> &'static str {
        match self {
            ErrorCategory::Timeout => "timeout",
            ErrorCategory::Auth => "auth",
            ErrorCategory::InvalidRequest => "invalid_request",
            ErrorCategory::RateLimit => "rate_limit",
            ErrorCategory::Overloaded => "overloaded",
            ErrorCategory::ServerError => "server_error",
            ErrorCategory::TransientNetwork => "transient_network",
            ErrorCategory::ResourceBusy => "resource_busy",
            ErrorCategory::SchemaIncompatible => "schema_incompatible",
            ErrorCategory::SchemaValidation => "schema_validation",
            ErrorCategory::SchemaStreamAborted => "schema_stream_aborted",
            ErrorCategory::ToolError => "tool_error",
            ErrorCategory::ToolRejected => "tool_rejected",
            ErrorCategory::EgressBlocked => "egress_blocked",
            ErrorCategory::Cancelled => "cancelled",
            ErrorCategory::ChannelClosed => "channel_closed",
            ErrorCategory::NotFound => "not_found",
            ErrorCategory::CircuitOpen => "circuit_open",
            ErrorCategory::BudgetExceeded => "budget_exceeded",
            ErrorCategory::Internal => "internal",
            ErrorCategory::Environment => "environment",
            ErrorCategory::Generic => "generic",
        }
    }

    pub fn parse(s: &str) -> Self {
        match s {
            "timeout" => ErrorCategory::Timeout,
            "auth" => ErrorCategory::Auth,
            "invalid_request" => ErrorCategory::InvalidRequest,
            "rate_limit" => ErrorCategory::RateLimit,
            "overloaded" => ErrorCategory::Overloaded,
            "server_error" => ErrorCategory::ServerError,
            "transient_network" => ErrorCategory::TransientNetwork,
            "resource_busy" => ErrorCategory::ResourceBusy,
            "schema_incompatible" => ErrorCategory::SchemaIncompatible,
            "schema_validation" => ErrorCategory::SchemaValidation,
            "schema_stream_aborted" => ErrorCategory::SchemaStreamAborted,
            "tool_error" => ErrorCategory::ToolError,
            "tool_rejected" => ErrorCategory::ToolRejected,
            "egress_blocked" => ErrorCategory::EgressBlocked,
            "cancelled" => ErrorCategory::Cancelled,
            "channel_closed" => ErrorCategory::ChannelClosed,
            "not_found" => ErrorCategory::NotFound,
            "circuit_open" => ErrorCategory::CircuitOpen,
            "budget_exceeded" => ErrorCategory::BudgetExceeded,
            "internal" => ErrorCategory::Internal,
            "environment" => ErrorCategory::Environment,
            _ => ErrorCategory::Generic,
        }
    }

    /// Whether this category represents an internal engine/wiring bug that must
    /// be surfaced rather than retried or swallowed as a recoverable failure.
    pub fn is_internal(&self) -> bool {
        matches!(self, ErrorCategory::Internal)
    }

    /// Whether an error of this category is worth retrying because the
    /// underlying condition is transient. Agent loops consult this to decide
    /// whether to back off and retry vs surface the error to the user.
    pub fn is_transient(&self) -> bool {
        matches!(
            self,
            ErrorCategory::Timeout
                | ErrorCategory::RateLimit
                | ErrorCategory::Overloaded
                | ErrorCategory::ServerError
                | ErrorCategory::TransientNetwork
                | ErrorCategory::ResourceBusy
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
        VmError::ExecutionDeadlineExceeded => ErrorCategory::Timeout,
        // ProcessExit is uncatchable control flow rather than an agent-facing
        // failure. Keep this fallback total for callers that classify an
        // arbitrary VmError without treating the request as retryable.
        VmError::ProcessExit(_) => ErrorCategory::Generic,
        VmError::AbandonedExecution => ErrorCategory::Cancelled,
        VmError::CategorizedError { category, .. } => category.clone(),
        VmError::ProviderStreamFailure(failure) => failure.category(),
        VmError::SchemaStreamAbort(abort) => abort.category(),
        VmError::Thrown(VmValue::Dict(d)) => d
            .get("category")
            .map(|v| ErrorCategory::parse(&v.display()))
            .unwrap_or(ErrorCategory::Generic),
        VmError::Thrown(VmValue::String(s)) => classify_error_message(s),
        VmError::Runtime(msg) => classify_error_message(msg),
        // Engine/wiring bugs: an undefined builtin (declared but not installed,
        // or a typo in stdlib/host code) or corrupt bytecode. No retry or model
        // reasoning fixes these, so they get their own category the agent loop
        // re-raises instead of swallowing.
        VmError::UndefinedBuiltin(_) | VmError::InvalidInstruction(_) => ErrorCategory::Internal,
        // A deadlock is permanently non-retryable and not provider-related —
        // `Generic` is the correct "surface it, don't back off" bucket.
        VmError::Deadlock(_) => ErrorCategory::Generic,
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
    // 2. Internal engine/wiring bug surfaced as a plain message. Some call
    //    sites build `Runtime("Undefined builtin: …")` strings instead of the
    //    structured `VmError::UndefinedBuiltin` variant; classify both the same
    //    so the agent loop re-raises rather than swallows.
    if msg.contains("Undefined builtin") {
        return ErrorCategory::Internal;
    }
    // 3. Well-known error identifiers from major APIs
    //    (Anthropic, OpenAI, and standard HTTP patterns)
    let lower = msg.to_lowercase();
    if lower.contains("cancelled") || lower.contains("canceled") {
        return ErrorCategory::Cancelled;
    }
    if msg.contains("ChannelClosed") || lower.contains("channel closed") {
        return ErrorCategory::ChannelClosed;
    }
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
    // OpenRouter reports an unknown model as HTTP 400 with the body
    // "<id> is not a valid model ID" — no status-code or typed-error signal
    // that `classify_by_http_status` / the checks above can latch onto. Map
    // the prose to NotFound so it lines up with Cerebras's 404 path (and with
    // `errors::is_model_unavailable`'s reason taxonomy).
    if lower.contains("is not a valid model id") || lower.contains("invalid model id") {
        return ErrorCategory::NotFound;
    }
    if msg.contains("circuit_open") {
        return ErrorCategory::CircuitOpen;
    }
    // Network-level transient patterns (pre-HTTP-status, pre-provider-framing).
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
        if let Some(category) = error_category_for_http_status(code) {
            return Some(category);
        }
    }
    None
}

/// Map an explicit provider HTTP status to the shared runtime category.
///
/// Callers that already own a typed status should use this instead of
/// formatting it into prose and asking [`classify_error_message`] to recover
/// the same fact. Keeping the mapping here also prevents mock and live provider
/// envelopes from drifting apart.
pub(crate) fn error_category_for_http_status(status: u16) -> Option<ErrorCategory> {
    match status {
        401 | 403 => Some(ErrorCategory::Auth),
        404 | 410 => Some(ErrorCategory::NotFound),
        408 | 504 | 522 | 524 => Some(ErrorCategory::Timeout),
        429 => Some(ErrorCategory::RateLimit),
        503 | 529 => Some(ErrorCategory::Overloaded),
        500 | 502 => Some(ErrorCategory::ServerError),
        _ => None,
    }
}

/// Extract plausible HTTP status codes from an error message.
#[expect(
    clippy::string_slice,
    reason = "i..i + 3 spans bytes verified to be ASCII digits"
)]
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
            VmError::ExecutionDeadlineExceeded => write!(f, "Execution deadline exceeded"),
            VmError::ProcessExit(code) => write!(f, "Process exit requested: {code}"),
            VmError::AbandonedExecution => write!(
                f,
                "Execution future was abandoned; discard this VM and reset its exclusively owned execution context"
            ),
            VmError::McpInputRequired(_) => write!(f, "MCP client input required"),
            VmError::Thrown(v) => write!(f, "Thrown: {}", v.display()),
            VmError::CategorizedError { message, category } => {
                write!(f, "Error [{}]: {}", category.as_str(), message)
            }
            VmError::ProviderStreamFailure(failure) => failure.fmt(f),
            VmError::SchemaStreamAbort(abort) => abort.fmt(f),
            VmError::SandboxMechanismUnavailable(refusal) => {
                write!(f, "Error [{}]: {}", refusal.category().as_str(), refusal)
            }
            VmError::DaemonQueueFull {
                daemon_id,
                capacity,
            } => write!(
                f,
                "Daemon queue full: daemon '{daemon_id}' reached its event_queue_capacity of {capacity}"
            ),
            VmError::Deadlock(err) => match err.diagnostic {
                DeadlockDiagnostic::SelfDeadlock => write!(
                    f,
                    "{}: deadlock detected: {} ({} '{}') — this wait can never complete and would block forever",
                    err.diagnostic.code(),
                    err.detail,
                    err.kind,
                    err.key
                ),
                DeadlockDiagnostic::WaitForGraph => write!(
                    f,
                    "{}: wait-for deadlock detected: {} ({} '{}') — no active task can make progress",
                    err.diagnostic.code(),
                    err.detail,
                    err.kind,
                    err.key
                ),
            },
            VmError::Return(_) => write!(f, "Return from function"),
            VmError::InvalidInstruction(op) => write!(f, "Invalid instruction: 0x{op:02x}"),
            VmError::ArityMismatch(err) => {
                let arg_word = match err.expected {
                    ArityExpect::Exact(1) | ArityExpect::AtLeast(1) => "argument",
                    _ => "arguments",
                };
                write!(
                    f,
                    "Arity mismatch: '{}' expects {} {}, got {}{}",
                    err.callee,
                    err.expected,
                    arg_word,
                    err.got,
                    fmt_span_suffix(&err.span)
                )
            }
            VmError::ArgTypeMismatch(err) => {
                write!(
                    f,
                    "Type error: '{}' parameter `{}` expects {}, got {}{}",
                    err.callee,
                    err.param,
                    err.expected,
                    err.got,
                    fmt_span_suffix(&err.span)
                )
            }
            VmError::BindingTypeMismatch(err) => {
                write!(
                    f,
                    "Type error: binding `{}` expects {}, got {}{}",
                    err.binding,
                    err.expected,
                    err.got,
                    fmt_span_suffix(&err.span)
                )
            }
        }
    }
}

fn fmt_span_suffix(span: &Option<Span>) -> String {
    match span {
        Some(s) => format!(" (at byte {}..{})", s.start, s.end),
        None => String::new(),
    }
}

impl std::error::Error for VmError {}

#[cfg(test)]
mod tests {
    use super::*;

    /// A new variant must be added to [`ErrorCategory::ALL`], or the guards below
    /// silently stop covering it. This match is the tripwire: it fails to
    /// compile until the variant is named, and the arm points at the list.
    #[test]
    fn all_categories_is_exhaustive() {
        for category in &ErrorCategory::ALL {
            match category {
                ErrorCategory::Timeout
                | ErrorCategory::Auth
                | ErrorCategory::InvalidRequest
                | ErrorCategory::RateLimit
                | ErrorCategory::Overloaded
                | ErrorCategory::ServerError
                | ErrorCategory::TransientNetwork
                | ErrorCategory::ResourceBusy
                | ErrorCategory::SchemaIncompatible
                | ErrorCategory::SchemaValidation
                | ErrorCategory::SchemaStreamAborted
                | ErrorCategory::ToolError
                | ErrorCategory::ToolRejected
                | ErrorCategory::EgressBlocked
                | ErrorCategory::Cancelled
                | ErrorCategory::ChannelClosed
                | ErrorCategory::NotFound
                | ErrorCategory::CircuitOpen
                | ErrorCategory::BudgetExceeded
                | ErrorCategory::Internal
                | ErrorCategory::Environment
                | ErrorCategory::Generic => {}
            }
        }
        assert_eq!(
            ErrorCategory::ALL.len(),
            22,
            "a category was added or removed — update `ErrorCategory::ALL` and the \
             `Error categories` table in docs/src/builtins.md"
        );
    }

    #[test]
    fn every_category_round_trips_through_parse() {
        for category in &ErrorCategory::ALL {
            assert_eq!(
                &ErrorCategory::parse(category.as_str()),
                category,
                "`{}` does not round-trip — `parse` is missing an arm, so a \
                 host handing this category back to Harn silently gets \
                 `generic`",
                category.as_str()
            );
        }
    }

    /// Scripts branch on these strings, so an undocumented category is a
    /// caller writing a match that cannot handle a value the runtime emits.
    /// `error_category()` used to advertise 10 of the full category set — a dead-port probe
    /// returning `transient_network` fell outside its own documented list.
    #[test]
    fn every_category_is_documented_in_builtins_md() {
        let path = concat!(env!("CARGO_MANIFEST_DIR"), "/../../docs/src/builtins.md");
        let doc =
            std::fs::read_to_string(path).unwrap_or_else(|err| panic!("cannot read {path}: {err}"));
        let table = doc
            .split_once("### Error categories")
            .unwrap_or_else(|| {
                panic!("docs/src/builtins.md lost its `### Error categories` section")
            })
            .1;
        let table = table.split_once("\n## ").map_or(table, |(head, _)| head);
        for category in &ErrorCategory::ALL {
            let row = format!("| `{}` |", category.as_str());
            assert!(
                table.contains(&row),
                "`{}` is missing from the `Error categories` table in \
                 docs/src/builtins.md",
                category.as_str()
            );
        }
    }

    #[test]
    fn classifies_cancelled_messages() {
        assert_eq!(
            classify_error_message("Bridge: operation cancelled"),
            ErrorCategory::Cancelled
        );
        assert_eq!(
            classify_error_message("operation canceled by host"),
            ErrorCategory::Cancelled
        );
    }

    #[test]
    fn classifies_undefined_builtin_as_internal() {
        // Structured variant (dispatch table miss / uninstalled builtin).
        assert_eq!(
            error_to_category(&VmError::UndefinedBuiltin("__host_agent_foo".into())),
            ErrorCategory::Internal
        );
        // Corrupt bytecode / compiler-VM opcode drift.
        assert_eq!(
            error_to_category(&VmError::InvalidInstruction(200)),
            ErrorCategory::Internal
        );
        // Stringly form: some call sites build a `Runtime("Undefined builtin: …")`
        // message instead of the structured variant — both must classify the same.
        assert_eq!(
            error_to_category(&VmError::Runtime(
                "Undefined builtin: __host_agent_foo (did you mean `bar`?)".into()
            )),
            ErrorCategory::Internal
        );
        assert_eq!(
            classify_error_message("Undefined builtin: __host_agent_foo"),
            ErrorCategory::Internal
        );
        // Internal errors are never treated as transient/retryable.
        assert!(!ErrorCategory::Internal.is_transient());
        assert!(ErrorCategory::Internal.is_internal());
        // Round-trips through the string form the agent loop compares against.
        assert_eq!(ErrorCategory::Internal.as_str(), "internal");
        assert_eq!(ErrorCategory::parse("internal"), ErrorCategory::Internal);
    }

    #[test]
    fn classifies_openrouter_invalid_model_id_as_not_found() {
        // OpenRouter reports an unknown model as HTTP 400 + prose. The 400 is
        // not classified by status, so the prose substring must lift it to
        // NotFound to match Cerebras's 404 path.
        assert_eq!(
            classify_error_message(
                "openrouter API error: qwen/qwen3-coder-bogus is not a valid model ID"
            ),
            ErrorCategory::NotFound
        );
        assert_eq!(
            classify_error_message("invalid model id supplied"),
            ErrorCategory::NotFound
        );
    }

    #[test]
    fn categorized_error_lowers_to_structured_dict() {
        // A caught `CategorizedError` must surface as a `{category, message}`
        // dict so `.harn` consumers branch on the typed category instead of
        // substring-matching the rendered prose (issue #4420).
        let err = categorized_error(
            "sandbox violation: /etc/passwd",
            ErrorCategory::ToolRejected,
        );
        let VmValue::Dict(dict) = err.thrown_value() else {
            panic!(
                "categorized error must lower to a dict, got {:?}",
                err.thrown_value()
            );
        };
        assert_eq!(
            dict.get("category").map(|v| v.display()).as_deref(),
            Some("tool_rejected"),
        );
        assert_eq!(
            dict.get("message").map(|v| v.display()).as_deref(),
            Some("sandbox violation: /etc/passwd"),
        );
        // The key is the canonical, exhaustively-matched `ErrorCategory::as_str`
        // contract — not the Display prose. A stringified catch still renders the
        // message + category (so generic catch-and-log stays sensible).
        let rendered = categorized_error("boom", ErrorCategory::Cancelled)
            .thrown_value()
            .display();
        assert!(rendered.contains("cancelled"), "rendered dict: {rendered}");
        assert!(rendered.contains("boom"), "rendered dict: {rendered}");
    }

    #[test]
    fn thrown_value_passes_structured_thrown_through_unchanged() {
        // A user `throw` of a structured value keeps its exact shape — the
        // lowering seam must not stringify or re-wrap it.
        let original = VmValue::dict(std::collections::BTreeMap::from([(
            "code".to_string(),
            VmValue::Int(7),
        )]));
        let VmValue::Dict(dict) = VmError::Thrown(original).thrown_value() else {
            panic!("thrown dict must pass through as a dict");
        };
        assert!(matches!(dict.get("code"), Some(VmValue::Int(7))));
    }

    #[test]
    fn deadlock_renders_with_stable_code() {
        let err = VmError::Deadlock(Box::new(DeadlockError::self_deadlock(
            "mutex",
            "__default__",
            "re-entrant acquire",
        )));
        assert!(
            err.to_string().starts_with("HARN-ORC-011"),
            "deadlock Display must carry the stable code: {err}"
        );
    }

    #[test]
    fn deadlock_maps_to_generic_category() {
        let err = VmError::Deadlock(Box::new(DeadlockError::self_deadlock(
            "task",
            "task_1",
            "self-join",
        )));
        let category = error_to_category(&err);
        assert_eq!(category, ErrorCategory::Generic);
        assert!(
            !category.is_transient(),
            "a deadlock must not be treated as a retryable transient error"
        );
    }
}
