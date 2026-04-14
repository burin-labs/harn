use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;
use std::{cell::RefCell, path::PathBuf};

use crate::chunk::CompiledFunction;
use crate::mcp::VmMcpClientHandle;

/// An async builtin function for the VM.
pub type VmAsyncBuiltinFn = Rc<
    dyn Fn(
        Vec<VmValue>,
    ) -> std::pin::Pin<Box<dyn std::future::Future<Output = Result<VmValue, VmError>>>>,
>;

/// The raw join handle type for spawned tasks.
pub type VmJoinHandle = tokio::task::JoinHandle<Result<(VmValue, String), VmError>>;

/// A spawned async task handle with cancellation support.
pub struct VmTaskHandle {
    pub handle: VmJoinHandle,
    /// Cooperative cancellation token. Set to true to request graceful shutdown.
    pub cancel_token: Arc<AtomicBool>,
}

/// A channel handle for the VM (uses tokio mpsc).
#[derive(Debug, Clone)]
pub struct VmChannelHandle {
    pub name: String,
    pub sender: Arc<tokio::sync::mpsc::Sender<VmValue>>,
    pub receiver: Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<VmValue>>>,
    pub closed: Arc<AtomicBool>,
}

/// An atomic integer handle for the VM.
#[derive(Debug, Clone)]
pub struct VmAtomicHandle {
    pub value: Arc<AtomicI64>,
}

/// A lazy integer range — Python-style. Stores only `(start, end, inclusive)`
/// so the in-memory footprint is O(1) regardless of the range's length.
/// `len()`, indexing (`r[k]`), `.contains(x)`, `.first()`, `.last()` are all
/// O(1); direct iteration walks step-by-step without materializing a list.
///
/// Empty-range convention (Python-consistent):
/// - Inclusive empty when `start > end`.
/// - Exclusive empty when `start >= end`.
///
/// Negative / reversed ranges are NOT supported in v1: `5 to 1` is simply
/// empty. Authors who want reverse iteration should call `.to_list().reverse()`.
#[derive(Debug, Clone, Copy)]
pub struct VmRange {
    pub start: i64,
    pub end: i64,
    pub inclusive: bool,
}

impl VmRange {
    /// Number of elements this range yields.
    ///
    /// Uses saturating arithmetic so that pathological ranges near
    /// `i64::MAX`/`i64::MIN` do not panic on overflow. Because a range's
    /// element count must fit in `i64` the returned length saturates at
    /// `i64::MAX` for ranges whose width exceeds that (e.g. `i64::MIN to
    /// i64::MAX` inclusive). Callers that later narrow to `usize` for
    /// allocation should still guard against huge lengths — see
    /// `to_vec` / `get` for the indexable-range invariants.
    pub fn len(&self) -> i64 {
        if self.inclusive {
            if self.start > self.end {
                0
            } else {
                self.end.saturating_sub(self.start).saturating_add(1)
            }
        } else if self.start >= self.end {
            0
        } else {
            self.end.saturating_sub(self.start)
        }
    }

    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }

    /// Element at the given 0-based index, bounds-checked.
    /// Returns `None` when out of bounds or when `start + idx` would
    /// overflow (which can only happen when `len()` saturated).
    pub fn get(&self, idx: i64) -> Option<i64> {
        if idx < 0 || idx >= self.len() {
            None
        } else {
            self.start.checked_add(idx)
        }
    }

    /// First element or `None` when empty.
    pub fn first(&self) -> Option<i64> {
        if self.is_empty() {
            None
        } else {
            Some(self.start)
        }
    }

    /// Last element or `None` when empty.
    pub fn last(&self) -> Option<i64> {
        if self.is_empty() {
            None
        } else if self.inclusive {
            Some(self.end)
        } else {
            Some(self.end - 1)
        }
    }

    /// Whether `v` falls inside the range (O(1)).
    pub fn contains(&self, v: i64) -> bool {
        if self.is_empty() {
            return false;
        }
        if self.inclusive {
            v >= self.start && v <= self.end
        } else {
            v >= self.start && v < self.end
        }
    }

    /// Materialize to a `Vec<VmValue>` — the explicit escape hatch.
    ///
    /// Uses `checked_add` on the per-element index so a range near
    /// `i64::MAX` stops at the representable bound instead of panicking.
    /// Callers should still treat a very long range as unwise to
    /// materialize (the whole point of `VmRange` is to avoid this).
    pub fn to_vec(&self) -> Vec<VmValue> {
        let len = self.len();
        if len <= 0 {
            return Vec::new();
        }
        let cap = len as usize;
        let mut out = Vec::with_capacity(cap);
        for i in 0..len {
            match self.start.checked_add(i) {
                Some(v) => out.push(VmValue::Int(v)),
                None => break,
            }
        }
        out
    }
}

/// A generator object: lazily produces values via yield.
/// The generator body runs as a spawned task that sends values through a channel.
#[derive(Debug, Clone)]
pub struct VmGenerator {
    /// Whether the generator has finished (returned or exhausted).
    pub done: Rc<std::cell::Cell<bool>>,
    /// Receiver end of the yield channel (generator sends values here).
    /// Wrapped in a shared async mutex so recv() can be called without holding
    /// a RefCell borrow across await points.
    pub receiver: Rc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<VmValue>>>,
}

/// VM runtime value.
#[derive(Debug, Clone)]
pub enum VmValue {
    Int(i64),
    Float(f64),
    String(Rc<str>),
    Bool(bool),
    Nil,
    List(Rc<Vec<VmValue>>),
    Dict(Rc<BTreeMap<String, VmValue>>),
    Closure(Rc<VmClosure>),
    /// Reference to a registered builtin function, used when a builtin name is
    /// referenced as a value (e.g. `snake_dict.rekey(snake_to_camel)`). The
    /// contained string is the builtin's registered name.
    BuiltinRef(Rc<str>),
    Duration(u64),
    EnumVariant {
        enum_name: String,
        variant: String,
        fields: Vec<VmValue>,
    },
    StructInstance {
        struct_name: String,
        fields: BTreeMap<String, VmValue>,
    },
    TaskHandle(String),
    Channel(VmChannelHandle),
    Atomic(VmAtomicHandle),
    McpClient(VmMcpClientHandle),
    Set(Rc<Vec<VmValue>>),
    Generator(VmGenerator),
    Range(VmRange),
    /// Lazy iterator handle. Single-pass, fused. See `crate::vm::iter::VmIter`.
    Iter(Rc<RefCell<crate::vm::iter::VmIter>>),
    /// Two-element pair value. Produced by `pair(a, b)`, yielded by the
    /// Dict iterator source, and (later) by `zip` / `enumerate` combinators.
    /// Accessed via `.first` / `.second`, and destructurable in
    /// `for (a, b) in ...` loops.
    Pair(Rc<(VmValue, VmValue)>),
}

/// A compiled closure value.
#[derive(Debug, Clone)]
pub struct VmClosure {
    pub func: CompiledFunction,
    pub env: VmEnv,
    /// Source directory for this closure's originating module.
    /// When set, `render()` and other source-relative builtins resolve
    /// paths relative to this directory instead of the entry pipeline.
    pub source_dir: Option<PathBuf>,
    /// Module-local named functions that should resolve before builtin fallback.
    /// This lets selectively imported functions keep private sibling helpers
    /// without exporting them into the caller's environment.
    pub module_functions: Option<ModuleFunctionRegistry>,
    /// Shared, mutable module-level env: holds top-level `var` / `let`
    /// bindings declared at the module root (caches, counters, lazily
    /// initialized registries). All closures created from the same
    /// module import point at the same `Rc<RefCell<VmEnv>>`, so a
    /// mutation inside one function is visible to every other function
    /// in that module on subsequent calls. `closure.env` still holds
    /// the per-closure lexical snapshot (captured function args from
    /// enclosing scopes, etc.) and is unchanged by this — `module_state`
    /// is a separate lookup layer consulted after the local env and
    /// before globals. Created in `import_declarations` after the
    /// module's init chunk runs, so the initial values from `var x = ...`
    /// land in it.
    pub module_state: Option<ModuleState>,
}

pub type ModuleFunctionRegistry = Rc<RefCell<BTreeMap<String, Rc<VmClosure>>>>;
pub type ModuleState = Rc<RefCell<VmEnv>>;

/// VM environment for variable storage.
#[derive(Debug, Clone)]
pub struct VmEnv {
    pub(crate) scopes: Vec<Scope>,
}

#[derive(Debug, Clone)]
pub(crate) struct Scope {
    pub(crate) vars: BTreeMap<String, (VmValue, bool)>, // (value, mutable)
}

impl Default for VmEnv {
    fn default() -> Self {
        Self::new()
    }
}

impl VmEnv {
    pub fn new() -> Self {
        Self {
            scopes: vec![Scope {
                vars: BTreeMap::new(),
            }],
        }
    }

    pub fn push_scope(&mut self) {
        self.scopes.push(Scope {
            vars: BTreeMap::new(),
        });
    }

    pub fn pop_scope(&mut self) {
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
    }

    pub fn scope_depth(&self) -> usize {
        self.scopes.len()
    }

    pub fn truncate_scopes(&mut self, target_depth: usize) {
        let min_depth = target_depth.max(1);
        while self.scopes.len() > min_depth {
            self.scopes.pop();
        }
    }

    pub fn get(&self, name: &str) -> Option<VmValue> {
        for scope in self.scopes.iter().rev() {
            if let Some((val, _)) = scope.vars.get(name) {
                return Some(val.clone());
            }
        }
        None
    }

    pub fn define(&mut self, name: &str, value: VmValue, mutable: bool) -> Result<(), VmError> {
        if let Some(scope) = self.scopes.last_mut() {
            if let Some((_, existing_mutable)) = scope.vars.get(name) {
                if !existing_mutable && !mutable {
                    return Err(VmError::Runtime(format!(
                        "Cannot redeclare immutable variable '{name}' in the same scope (use 'var' for mutable bindings)"
                    )));
                }
            }
            scope.vars.insert(name.to_string(), (value, mutable));
        }
        Ok(())
    }

    pub fn all_variables(&self) -> BTreeMap<String, VmValue> {
        let mut vars = BTreeMap::new();
        for scope in &self.scopes {
            for (name, (value, _)) in &scope.vars {
                vars.insert(name.clone(), value.clone());
            }
        }
        vars
    }

    pub fn assign(&mut self, name: &str, value: VmValue) -> Result<(), VmError> {
        for scope in self.scopes.iter_mut().rev() {
            if let Some((_, mutable)) = scope.vars.get(name) {
                if !mutable {
                    return Err(VmError::ImmutableAssignment(name.to_string()));
                }
                scope.vars.insert(name.to_string(), (value, true));
                return Ok(());
            }
        }
        Err(VmError::UndefinedVariable(name.to_string()))
    }
}

/// Compute Levenshtein edit distance between two strings.
fn levenshtein(a: &str, b: &str) -> usize {
    let a: Vec<char> = a.chars().collect();
    let b: Vec<char> = b.chars().collect();
    let (m, n) = (a.len(), b.len());
    let mut prev = (0..=n).collect::<Vec<_>>();
    let mut curr = vec![0; n + 1];
    for i in 1..=m {
        curr[0] = i;
        for j in 1..=n {
            let cost = if a[i - 1] == b[j - 1] { 0 } else { 1 };
            curr[j] = (prev[j] + 1).min(curr[j - 1] + 1).min(prev[j - 1] + cost);
        }
        std::mem::swap(&mut prev, &mut curr);
    }
    prev[n]
}

/// Find the closest match from a list of candidates using Levenshtein distance.
/// Returns `Some(suggestion)` if a candidate is within `max_dist` edits.
pub fn closest_match<'a>(name: &str, candidates: impl Iterator<Item = &'a str>) -> Option<String> {
    let max_dist = match name.len() {
        0..=2 => 1,
        3..=5 => 2,
        _ => 3,
    };
    candidates
        .filter(|c| *c != name && !c.starts_with("__"))
        .map(|c| (c, levenshtein(name, c)))
        .filter(|(_, d)| *d <= max_dist)
        // Prefer smallest distance, then closest length to original, then alphabetical
        .min_by(|(a, da), (b, db)| {
            da.cmp(db)
                .then_with(|| {
                    let a_diff = (a.len() as isize - name.len() as isize).unsigned_abs();
                    let b_diff = (b.len() as isize - name.len() as isize).unsigned_abs();
                    a_diff.cmp(&b_diff)
                })
                .then_with(|| a.cmp(b))
        })
        .map(|(c, _)| c.to_string())
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

impl VmValue {
    pub fn is_truthy(&self) -> bool {
        match self {
            VmValue::Bool(b) => *b,
            VmValue::Nil => false,
            VmValue::Int(n) => *n != 0,
            VmValue::Float(n) => *n != 0.0,
            VmValue::String(s) => !s.is_empty(),
            VmValue::List(l) => !l.is_empty(),
            VmValue::Dict(d) => !d.is_empty(),
            VmValue::Closure(_) => true,
            VmValue::BuiltinRef(_) => true,
            VmValue::Duration(ms) => *ms > 0,
            VmValue::EnumVariant { .. } => true,
            VmValue::StructInstance { .. } => true,
            VmValue::TaskHandle(_) => true,
            VmValue::Channel(_) => true,
            VmValue::Atomic(_) => true,
            VmValue::McpClient(_) => true,
            VmValue::Set(s) => !s.is_empty(),
            VmValue::Generator(_) => true,
            // Match Python semantics: range objects are always truthy,
            // even the empty range (analogous to generators / iterators).
            VmValue::Range(_) => true,
            VmValue::Iter(_) => true,
            VmValue::Pair(_) => true,
        }
    }

    pub fn type_name(&self) -> &'static str {
        match self {
            VmValue::String(_) => "string",
            VmValue::Int(_) => "int",
            VmValue::Float(_) => "float",
            VmValue::Bool(_) => "bool",
            VmValue::Nil => "nil",
            VmValue::List(_) => "list",
            VmValue::Dict(_) => "dict",
            VmValue::Closure(_) => "closure",
            VmValue::BuiltinRef(_) => "builtin",
            VmValue::Duration(_) => "duration",
            VmValue::EnumVariant { .. } => "enum",
            VmValue::StructInstance { .. } => "struct",
            VmValue::TaskHandle(_) => "task_handle",
            VmValue::Channel(_) => "channel",
            VmValue::Atomic(_) => "atomic",
            VmValue::McpClient(_) => "mcp_client",
            VmValue::Set(_) => "set",
            VmValue::Generator(_) => "generator",
            VmValue::Range(_) => "range",
            VmValue::Iter(_) => "iter",
            VmValue::Pair(_) => "pair",
        }
    }

    pub fn display(&self) -> String {
        let mut out = String::new();
        self.write_display(&mut out);
        out
    }

    /// Writes the display representation directly into `out`,
    /// avoiding intermediate Vec<String> allocations for collections.
    pub fn write_display(&self, out: &mut String) {
        use std::fmt::Write;
        match self {
            VmValue::Int(n) => {
                let _ = write!(out, "{n}");
            }
            VmValue::Float(n) => {
                if *n == (*n as i64) as f64 && n.abs() < 1e15 {
                    let _ = write!(out, "{n:.1}");
                } else {
                    let _ = write!(out, "{n}");
                }
            }
            VmValue::String(s) => out.push_str(s),
            VmValue::Bool(b) => out.push_str(if *b { "true" } else { "false" }),
            VmValue::Nil => out.push_str("nil"),
            VmValue::List(items) => {
                out.push('[');
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    item.write_display(out);
                }
                out.push(']');
            }
            VmValue::Dict(map) => {
                out.push('{');
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(k);
                    out.push_str(": ");
                    v.write_display(out);
                }
                out.push('}');
            }
            VmValue::Closure(c) => {
                let _ = write!(out, "<fn({})>", c.func.params.join(", "));
            }
            VmValue::BuiltinRef(name) => {
                let _ = write!(out, "<builtin {name}>");
            }
            VmValue::Duration(ms) => {
                if *ms >= 3_600_000 && ms % 3_600_000 == 0 {
                    let _ = write!(out, "{}h", ms / 3_600_000);
                } else if *ms >= 60_000 && ms % 60_000 == 0 {
                    let _ = write!(out, "{}m", ms / 60_000);
                } else if *ms >= 1000 && ms % 1000 == 0 {
                    let _ = write!(out, "{}s", ms / 1000);
                } else {
                    let _ = write!(out, "{}ms", ms);
                }
            }
            VmValue::EnumVariant {
                enum_name,
                variant,
                fields,
            } => {
                if fields.is_empty() {
                    let _ = write!(out, "{enum_name}.{variant}");
                } else {
                    let _ = write!(out, "{enum_name}.{variant}(");
                    for (i, v) in fields.iter().enumerate() {
                        if i > 0 {
                            out.push_str(", ");
                        }
                        v.write_display(out);
                    }
                    out.push(')');
                }
            }
            VmValue::StructInstance {
                struct_name,
                fields,
            } => {
                let _ = write!(out, "{struct_name} {{");
                for (i, (k, v)) in fields.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    out.push_str(k);
                    out.push_str(": ");
                    v.write_display(out);
                }
                out.push('}');
            }
            VmValue::TaskHandle(id) => {
                let _ = write!(out, "<task:{id}>");
            }
            VmValue::Channel(ch) => {
                let _ = write!(out, "<channel:{}>", ch.name);
            }
            VmValue::Atomic(a) => {
                let _ = write!(out, "<atomic:{}>", a.value.load(Ordering::SeqCst));
            }
            VmValue::McpClient(c) => {
                let _ = write!(out, "<mcp_client:{}>", c.name);
            }
            VmValue::Set(items) => {
                out.push_str("set(");
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        out.push_str(", ");
                    }
                    item.write_display(out);
                }
                out.push(')');
            }
            VmValue::Generator(g) => {
                if g.done.get() {
                    out.push_str("<generator (done)>");
                } else {
                    out.push_str("<generator>");
                }
            }
            // Print form mirrors source syntax: `1 to 5` / `0 to 3 exclusive`.
            // `.to_list()` is the explicit path to materialize for display.
            VmValue::Range(r) => {
                let _ = write!(out, "{} to {}", r.start, r.end);
                if !r.inclusive {
                    out.push_str(" exclusive");
                }
            }
            VmValue::Iter(h) => {
                if matches!(&*h.borrow(), crate::vm::iter::VmIter::Exhausted) {
                    out.push_str("<iter (exhausted)>");
                } else {
                    out.push_str("<iter>");
                }
            }
            VmValue::Pair(p) => {
                out.push('(');
                p.0.write_display(out);
                out.push_str(", ");
                p.1.write_display(out);
                out.push(')');
            }
        }
    }

    /// Get the value as a BTreeMap reference, if it's a Dict.
    pub fn as_dict(&self) -> Option<&BTreeMap<String, VmValue>> {
        if let VmValue::Dict(d) = self {
            Some(d)
        } else {
            None
        }
    }

    pub fn as_int(&self) -> Option<i64> {
        if let VmValue::Int(n) = self {
            Some(*n)
        } else {
            None
        }
    }
}

/// Sync builtin function for the VM.
pub type VmBuiltinFn = Rc<dyn Fn(&[VmValue], &mut String) -> Result<VmValue, VmError>>;

/// Reference / identity equality. For heap-allocated refcounted values
/// (List/Dict/Set/Closure) returns true only when both operands share the
/// same underlying `Rc` allocation. For primitive scalars, falls back to
/// structural equality (since primitives have no distinct identity).
pub fn values_identical(a: &VmValue, b: &VmValue) -> bool {
    match (a, b) {
        (VmValue::List(x), VmValue::List(y)) => Rc::ptr_eq(x, y),
        (VmValue::Dict(x), VmValue::Dict(y)) => Rc::ptr_eq(x, y),
        (VmValue::Set(x), VmValue::Set(y)) => Rc::ptr_eq(x, y),
        (VmValue::Closure(x), VmValue::Closure(y)) => Rc::ptr_eq(x, y),
        (VmValue::String(x), VmValue::String(y)) => Rc::ptr_eq(x, y) || x == y,
        (VmValue::BuiltinRef(x), VmValue::BuiltinRef(y)) => x == y,
        (VmValue::Pair(x), VmValue::Pair(y)) => Rc::ptr_eq(x, y),
        // Primitives: identity collapses to structural equality.
        _ => values_equal(a, b),
    }
}

/// Stable identity key for a value. Different allocations produce different
/// keys; two values with the same heap identity produce the same key. For
/// primitives the key is derived from the displayed value plus type name so
/// logically-equal primitives always compare equal.
pub fn value_identity_key(v: &VmValue) -> String {
    match v {
        VmValue::List(x) => format!("list@{:p}", Rc::as_ptr(x)),
        VmValue::Dict(x) => format!("dict@{:p}", Rc::as_ptr(x)),
        VmValue::Set(x) => format!("set@{:p}", Rc::as_ptr(x)),
        VmValue::Closure(x) => format!("closure@{:p}", Rc::as_ptr(x)),
        VmValue::String(x) => format!("string@{:p}", x.as_ptr()),
        VmValue::BuiltinRef(name) => format!("builtin@{name}"),
        other => format!("{}@{}", other.type_name(), other.display()),
    }
}

/// Canonical string form used as the keying material for `hash_value`.
/// Different types never collide (the type name is prepended) and collection
/// order is preserved so structurally-equal values always produce the same
/// key. Not intended for cross-process stability; depends on the in-process
/// iteration order for collections (Dict uses BTreeMap so keys are sorted).
pub fn value_structural_hash_key(v: &VmValue) -> String {
    let mut out = String::new();
    write_structural_hash_key(v, &mut out);
    out
}

/// Writes the structural hash key for a value directly into `out`,
/// avoiding intermediate allocations. Uses length-prefixed encoding
/// for strings and dict keys to prevent separator collisions.
fn write_structural_hash_key(v: &VmValue, out: &mut String) {
    match v {
        VmValue::Nil => out.push('N'),
        VmValue::Bool(b) => {
            out.push(if *b { 'T' } else { 'F' });
        }
        VmValue::Int(n) => {
            out.push('i');
            out.push_str(&n.to_string());
            out.push(';');
        }
        VmValue::Float(n) => {
            out.push('f');
            out.push_str(&n.to_bits().to_string());
            out.push(';');
        }
        VmValue::String(s) => {
            // Length-prefixed: s<len>:<content> — no ambiguity from content
            out.push('s');
            out.push_str(&s.len().to_string());
            out.push(':');
            out.push_str(s);
        }
        VmValue::Duration(ms) => {
            out.push('d');
            out.push_str(&ms.to_string());
            out.push(';');
        }
        VmValue::List(items) => {
            out.push('L');
            for item in items.iter() {
                write_structural_hash_key(item, out);
                out.push(',');
            }
            out.push(']');
        }
        VmValue::Dict(map) => {
            out.push('D');
            for (k, v) in map.iter() {
                // Length-prefixed key
                out.push_str(&k.len().to_string());
                out.push(':');
                out.push_str(k);
                out.push('=');
                write_structural_hash_key(v, out);
                out.push(',');
            }
            out.push('}');
        }
        VmValue::Set(items) => {
            // Sets need sorted keys for order-independence
            let mut keys: Vec<String> = items.iter().map(value_structural_hash_key).collect();
            keys.sort();
            out.push('S');
            for k in &keys {
                out.push_str(k);
                out.push(',');
            }
            out.push('}');
        }
        other => {
            let tn = other.type_name();
            out.push('o');
            out.push_str(&tn.len().to_string());
            out.push(':');
            out.push_str(tn);
            let d = other.display();
            out.push_str(&d.len().to_string());
            out.push(':');
            out.push_str(&d);
        }
    }
}

pub fn values_equal(a: &VmValue, b: &VmValue) -> bool {
    match (a, b) {
        (VmValue::Int(x), VmValue::Int(y)) => x == y,
        (VmValue::Float(x), VmValue::Float(y)) => x == y,
        (VmValue::String(x), VmValue::String(y)) => x == y,
        (VmValue::Bool(x), VmValue::Bool(y)) => x == y,
        (VmValue::Nil, VmValue::Nil) => true,
        (VmValue::Int(x), VmValue::Float(y)) => (*x as f64) == *y,
        (VmValue::Float(x), VmValue::Int(y)) => *x == (*y as f64),
        (VmValue::TaskHandle(a), VmValue::TaskHandle(b)) => a == b,
        (VmValue::Channel(_), VmValue::Channel(_)) => false, // channels are never equal
        (VmValue::Atomic(a), VmValue::Atomic(b)) => {
            a.value.load(Ordering::SeqCst) == b.value.load(Ordering::SeqCst)
        }
        (VmValue::List(a), VmValue::List(b)) => {
            a.len() == b.len() && a.iter().zip(b.iter()).all(|(x, y)| values_equal(x, y))
        }
        (VmValue::Dict(a), VmValue::Dict(b)) => {
            a.len() == b.len()
                && a.iter()
                    .zip(b.iter())
                    .all(|((k1, v1), (k2, v2))| k1 == k2 && values_equal(v1, v2))
        }
        (
            VmValue::EnumVariant {
                enum_name: a_e,
                variant: a_v,
                fields: a_f,
            },
            VmValue::EnumVariant {
                enum_name: b_e,
                variant: b_v,
                fields: b_f,
            },
        ) => {
            a_e == b_e
                && a_v == b_v
                && a_f.len() == b_f.len()
                && a_f.iter().zip(b_f.iter()).all(|(x, y)| values_equal(x, y))
        }
        (
            VmValue::StructInstance {
                struct_name: a_s,
                fields: a_f,
            },
            VmValue::StructInstance {
                struct_name: b_s,
                fields: b_f,
            },
        ) => {
            a_s == b_s
                && a_f.len() == b_f.len()
                && a_f
                    .iter()
                    .zip(b_f.iter())
                    .all(|((k1, v1), (k2, v2))| k1 == k2 && values_equal(v1, v2))
        }
        (VmValue::Set(a), VmValue::Set(b)) => {
            a.len() == b.len() && a.iter().all(|x| b.iter().any(|y| values_equal(x, y)))
        }
        (VmValue::Generator(_), VmValue::Generator(_)) => false, // generators are never equal
        (VmValue::Range(a), VmValue::Range(b)) => {
            a.start == b.start && a.end == b.end && a.inclusive == b.inclusive
        }
        (VmValue::Iter(a), VmValue::Iter(b)) => Rc::ptr_eq(a, b),
        (VmValue::Pair(a), VmValue::Pair(b)) => {
            values_equal(&a.0, &b.0) && values_equal(&a.1, &b.1)
        }
        _ => false,
    }
}

pub fn compare_values(a: &VmValue, b: &VmValue) -> i32 {
    match (a, b) {
        (VmValue::Int(x), VmValue::Int(y)) => x.cmp(y) as i32,
        (VmValue::Float(x), VmValue::Float(y)) => {
            if x < y {
                -1
            } else if x > y {
                1
            } else {
                0
            }
        }
        (VmValue::Int(x), VmValue::Float(y)) => {
            let x = *x as f64;
            if x < *y {
                -1
            } else if x > *y {
                1
            } else {
                0
            }
        }
        (VmValue::Float(x), VmValue::Int(y)) => {
            let y = *y as f64;
            if *x < y {
                -1
            } else if *x > y {
                1
            } else {
                0
            }
        }
        (VmValue::String(x), VmValue::String(y)) => x.cmp(y) as i32,
        (VmValue::Pair(x), VmValue::Pair(y)) => {
            let c = compare_values(&x.0, &y.0);
            if c != 0 {
                c
            } else {
                compare_values(&x.1, &y.1)
            }
        }
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn s(val: &str) -> VmValue {
        VmValue::String(Rc::from(val))
    }
    fn i(val: i64) -> VmValue {
        VmValue::Int(val)
    }
    fn list(items: Vec<VmValue>) -> VmValue {
        VmValue::List(Rc::new(items))
    }
    fn dict(pairs: Vec<(&str, VmValue)>) -> VmValue {
        VmValue::Dict(Rc::new(
            pairs.into_iter().map(|(k, v)| (k.to_string(), v)).collect(),
        ))
    }

    #[test]
    fn hash_key_cross_type_distinct() {
        // Int(1) vs String("1") vs Bool(true) must all differ
        let k_int = value_structural_hash_key(&i(1));
        let k_str = value_structural_hash_key(&s("1"));
        let k_bool = value_structural_hash_key(&VmValue::Bool(true));
        assert_ne!(k_int, k_str);
        assert_ne!(k_int, k_bool);
        assert_ne!(k_str, k_bool);
    }

    #[test]
    fn hash_key_string_with_separator_chars() {
        // ["a,string:b"] (1-element list) vs ["a", "b"] (2-element list)
        let one_elem = list(vec![s("a,string:b")]);
        let two_elem = list(vec![s("a"), s("b")]);
        assert_ne!(
            value_structural_hash_key(&one_elem),
            value_structural_hash_key(&two_elem),
            "length-prefixed strings must prevent separator collisions"
        );
    }

    #[test]
    fn hash_key_dict_key_with_equals() {
        // Dict with key "a=b" vs dict with key "a" and value containing "b"
        let d1 = dict(vec![("a=b", i(1))]);
        let d2 = dict(vec![("a", i(1))]);
        assert_ne!(
            value_structural_hash_key(&d1),
            value_structural_hash_key(&d2)
        );
    }

    #[test]
    fn hash_key_nested_list_vs_flat() {
        // [[1]] vs [1]
        let nested = list(vec![list(vec![i(1)])]);
        let flat = list(vec![i(1)]);
        assert_ne!(
            value_structural_hash_key(&nested),
            value_structural_hash_key(&flat)
        );
    }

    #[test]
    fn hash_key_nil() {
        assert_eq!(
            value_structural_hash_key(&VmValue::Nil),
            value_structural_hash_key(&VmValue::Nil)
        );
    }

    #[test]
    fn hash_key_float_zero_vs_neg_zero() {
        let pos = VmValue::Float(0.0);
        let neg = VmValue::Float(-0.0);
        // 0.0 and -0.0 have different bit representations
        assert_ne!(
            value_structural_hash_key(&pos),
            value_structural_hash_key(&neg)
        );
    }

    #[test]
    fn hash_key_equal_values_match() {
        let a = list(vec![s("hello"), i(42), VmValue::Bool(false)]);
        let b = list(vec![s("hello"), i(42), VmValue::Bool(false)]);
        assert_eq!(value_structural_hash_key(&a), value_structural_hash_key(&b));
    }

    #[test]
    fn hash_key_dict_with_comma_key() {
        let d1 = dict(vec![("a,b", i(1))]);
        let d2 = dict(vec![("a", i(1))]);
        assert_ne!(
            value_structural_hash_key(&d1),
            value_structural_hash_key(&d2)
        );
    }

    // --- VmRange arithmetic safety at i64 boundaries ---
    //
    // These guard the saturating/checked arithmetic in `VmRange::len` and
    // `VmRange::get` / `VmRange::to_vec`. Before the saturating rewrite the
    // inclusive `i64::MIN to 0` case panicked in debug builds on
    // `(end - start) + 1`.

    #[test]
    fn vm_range_len_inclusive_saturates_at_i64_max() {
        let r = VmRange {
            start: i64::MIN,
            end: 0,
            inclusive: true,
        };
        // True width overflows i64; saturating at i64::MAX keeps this total.
        assert_eq!(r.len(), i64::MAX);
    }

    #[test]
    fn vm_range_len_exclusive_full_range_saturates() {
        let r = VmRange {
            start: i64::MIN,
            end: i64::MAX,
            inclusive: false,
        };
        assert_eq!(r.len(), i64::MAX);
    }

    #[test]
    fn vm_range_len_inclusive_full_range_saturates() {
        let r = VmRange {
            start: i64::MIN,
            end: i64::MAX,
            inclusive: true,
        };
        assert_eq!(r.len(), i64::MAX);
    }

    #[test]
    fn vm_range_get_near_max_does_not_overflow() {
        let r = VmRange {
            start: i64::MAX - 2,
            end: i64::MAX,
            inclusive: true,
        };
        assert_eq!(r.len(), 3);
        assert_eq!(r.get(0), Some(i64::MAX - 2));
        assert_eq!(r.get(2), Some(i64::MAX));
        assert_eq!(r.get(3), None);
    }

    #[test]
    fn vm_range_reversed_is_empty() {
        let r = VmRange {
            start: 5,
            end: 1,
            inclusive: true,
        };
        assert!(r.is_empty());
        assert_eq!(r.len(), 0);
        assert_eq!(r.first(), None);
        assert_eq!(r.last(), None);
    }

    #[test]
    fn vm_range_contains_near_bounds() {
        let r = VmRange {
            start: 1,
            end: 5,
            inclusive: true,
        };
        assert!(r.contains(1));
        assert!(r.contains(5));
        assert!(!r.contains(0));
        assert!(!r.contains(6));
        let r = VmRange {
            start: 1,
            end: 5,
            inclusive: false,
        };
        assert!(r.contains(1));
        assert!(r.contains(4));
        assert!(!r.contains(5));
    }

    #[test]
    fn vm_range_to_vec_matches_direct_iteration() {
        let r = VmRange {
            start: -2,
            end: 2,
            inclusive: true,
        };
        let v = r.to_vec();
        assert_eq!(v.len(), 5);
        assert_eq!(
            v.iter()
                .map(|x| match x {
                    VmValue::Int(n) => *n,
                    _ => panic!("non-int in range"),
                })
                .collect::<Vec<_>>(),
            vec![-2, -1, 0, 1, 2]
        );
    }
}
