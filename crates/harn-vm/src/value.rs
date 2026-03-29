use std::collections::BTreeMap;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::Arc;

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
}

/// A compiled closure value.
#[derive(Debug, Clone)]
pub struct VmClosure {
    pub func: CompiledFunction,
    pub env: VmEnv,
}

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

/// VM runtime errors.
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
    /// Rate limit exceeded
    RateLimit,
    /// Tool execution failure
    ToolError,
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
            ErrorCategory::ToolError => "tool_error",
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
            "tool_error" => ErrorCategory::ToolError,
            "cancelled" => ErrorCategory::Cancelled,
            "not_found" => ErrorCategory::NotFound,
            "circuit_open" => ErrorCategory::CircuitOpen,
            _ => ErrorCategory::Generic,
        }
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
fn classify_error_message(msg: &str) -> ErrorCategory {
    // 1. HTTP status codes — most reliable signal
    if let Some(cat) = classify_by_http_status(msg) {
        return cat;
    }
    // 2. Well-known error identifiers from major APIs
    //    (Anthropic, OpenAI, and standard HTTP patterns)
    if msg.contains("Deadline exceeded") || msg.contains("context deadline exceeded") {
        return ErrorCategory::Timeout;
    }
    if msg.contains("overloaded_error") || msg.contains("api_error") {
        // Anthropic-specific error types
        return ErrorCategory::RateLimit;
    }
    if msg.contains("insufficient_quota") || msg.contains("billing_hard_limit_reached") {
        // OpenAI-specific error types
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
            429 | 503 => ErrorCategory::RateLimit,
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
            VmValue::Duration(ms) => *ms > 0,
            VmValue::EnumVariant { .. } => true,
            VmValue::StructInstance { .. } => true,
            VmValue::TaskHandle(_) => true,
            VmValue::Channel(_) => true,
            VmValue::Atomic(_) => true,
            VmValue::McpClient(_) => true,
            VmValue::Set(s) => !s.is_empty(),
            VmValue::Generator(_) => true,
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
            VmValue::Duration(_) => "duration",
            VmValue::EnumVariant { .. } => "enum",
            VmValue::StructInstance { .. } => "struct",
            VmValue::TaskHandle(_) => "task_handle",
            VmValue::Channel(_) => "channel",
            VmValue::Atomic(_) => "atomic",
            VmValue::McpClient(_) => "mcp_client",
            VmValue::Set(_) => "set",
            VmValue::Generator(_) => "generator",
        }
    }

    pub fn display(&self) -> String {
        match self {
            VmValue::Int(n) => n.to_string(),
            VmValue::Float(n) => {
                if *n == (*n as i64) as f64 && n.abs() < 1e15 {
                    format!("{:.1}", n)
                } else {
                    n.to_string()
                }
            }
            VmValue::String(s) => s.to_string(),
            VmValue::Bool(b) => (if *b { "true" } else { "false" }).to_string(),
            VmValue::Nil => "nil".to_string(),
            VmValue::List(items) => {
                let inner: Vec<String> = items.iter().map(|i| i.display()).collect();
                format!("[{}]", inner.join(", "))
            }
            VmValue::Dict(map) => {
                let inner: Vec<String> = map
                    .iter()
                    .map(|(k, v)| format!("{k}: {}", v.display()))
                    .collect();
                format!("{{{}}}", inner.join(", "))
            }
            VmValue::Closure(c) => format!("<fn({})>", c.func.params.join(", ")),
            VmValue::Duration(ms) => {
                if *ms >= 3_600_000 && ms % 3_600_000 == 0 {
                    format!("{}h", ms / 3_600_000)
                } else if *ms >= 60_000 && ms % 60_000 == 0 {
                    format!("{}m", ms / 60_000)
                } else if *ms >= 1000 && ms % 1000 == 0 {
                    format!("{}s", ms / 1000)
                } else {
                    format!("{}ms", ms)
                }
            }
            VmValue::EnumVariant {
                enum_name,
                variant,
                fields,
            } => {
                if fields.is_empty() {
                    format!("{enum_name}.{variant}")
                } else {
                    let inner: Vec<String> = fields.iter().map(|v| v.display()).collect();
                    format!("{enum_name}.{variant}({})", inner.join(", "))
                }
            }
            VmValue::StructInstance {
                struct_name,
                fields,
            } => {
                let inner: Vec<String> = fields
                    .iter()
                    .map(|(k, v)| format!("{k}: {}", v.display()))
                    .collect();
                format!("{struct_name} {{{}}}", inner.join(", "))
            }
            VmValue::TaskHandle(id) => format!("<task:{id}>"),
            VmValue::Channel(ch) => format!("<channel:{}>", ch.name),
            VmValue::Atomic(a) => format!("<atomic:{}>", a.value.load(Ordering::SeqCst)),
            VmValue::McpClient(c) => format!("<mcp_client:{}>", c.name),
            VmValue::Set(items) => {
                let inner: Vec<String> = items.iter().map(|i| i.display()).collect();
                format!("set({})", inner.join(", "))
            }
            VmValue::Generator(g) => {
                if g.done.get() {
                    "<generator (done)>".to_string()
                } else {
                    "<generator>".to_string()
                }
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
        _ => 0,
    }
}
