use std::collections::BTreeMap;
use std::future::Future;
use std::io::BufRead;
use std::pin::Pin;
use std::rc::Rc;
use std::sync::atomic::{AtomicBool, AtomicI64, AtomicU8, Ordering};
use std::sync::Arc;
use std::time::Instant;

use crate::chunk::{Chunk, CompiledFunction, Constant, Op};

/// An async builtin function for the VM.
pub type VmAsyncBuiltinFn =
    Rc<dyn Fn(Vec<VmValue>) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>>>>>;

/// A spawned async task handle.
pub type VmTaskHandle = tokio::task::JoinHandle<Result<(VmValue, String), VmError>>;

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
    scopes: Vec<Scope>,
}

#[derive(Debug, Clone)]
struct Scope {
    vars: BTreeMap<String, (VmValue, bool)>, // (value, mutable)
}

impl VmEnv {
    fn new() -> Self {
        Self {
            scopes: vec![Scope {
                vars: BTreeMap::new(),
            }],
        }
    }

    fn push_scope(&mut self) {
        self.scopes.push(Scope {
            vars: BTreeMap::new(),
        });
    }

    fn get(&self, name: &str) -> Option<VmValue> {
        for scope in self.scopes.iter().rev() {
            if let Some((val, _)) = scope.vars.get(name) {
                return Some(val.clone());
            }
        }
        None
    }

    fn define(&mut self, name: &str, value: VmValue, mutable: bool) {
        if let Some(scope) = self.scopes.last_mut() {
            scope.vars.insert(name.to_string(), (value, mutable));
        }
    }

    fn all_variables(&self) -> BTreeMap<String, VmValue> {
        let mut vars = BTreeMap::new();
        for scope in &self.scopes {
            for (name, (value, _)) in &scope.vars {
                vars.insert(name.clone(), value.clone());
            }
        }
        vars
    }

    fn assign(&mut self, name: &str, value: VmValue) -> Result<(), VmError> {
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
    Return(VmValue),
    InvalidInstruction(u8),
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

    fn as_int(&self) -> Option<i64> {
        if let VmValue::Int(n) = self {
            Some(*n)
        } else {
            None
        }
    }
}

fn values_equal(a: &VmValue, b: &VmValue) -> bool {
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
        _ => false,
    }
}

fn compare_values(a: &VmValue, b: &VmValue) -> i32 {
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

/// Sync builtin function for the VM.
pub type VmBuiltinFn = Rc<dyn Fn(&[VmValue], &mut String) -> Result<VmValue, VmError>>;

/// Call frame for function execution.
struct CallFrame {
    chunk: Chunk,
    ip: usize,
    stack_base: usize,
    saved_env: VmEnv,
}

/// Exception handler for try/catch.
struct ExceptionHandler {
    catch_ip: usize,
    stack_depth: usize,
    frame_depth: usize,
}

/// Debug action returned by the debug hook.
#[derive(Debug, Clone, PartialEq)]
pub enum DebugAction {
    /// Continue execution normally.
    Continue,
    /// Stop (breakpoint hit, step complete).
    Stop,
}

/// Information about current execution state for the debugger.
#[derive(Debug, Clone)]
pub struct DebugState {
    pub line: usize,
    pub variables: BTreeMap<String, VmValue>,
    pub frame_name: String,
    pub frame_depth: usize,
}

/// The Harn bytecode virtual machine.
pub struct Vm {
    stack: Vec<VmValue>,
    env: VmEnv,
    output: String,
    builtins: BTreeMap<String, VmBuiltinFn>,
    async_builtins: BTreeMap<String, VmAsyncBuiltinFn>,
    /// Iterator state for for-in loops.
    iterators: Vec<(Vec<VmValue>, usize)>,
    /// Call frame stack.
    frames: Vec<CallFrame>,
    /// Exception handler stack.
    exception_handlers: Vec<ExceptionHandler>,
    /// Spawned async task handles.
    spawned_tasks: BTreeMap<String, VmTaskHandle>,
    /// Counter for generating unique task IDs.
    task_counter: u64,
    /// Active deadline stack: (deadline_instant, frame_depth).
    deadlines: Vec<(Instant, usize)>,
    /// Breakpoints (source line numbers).
    breakpoints: Vec<usize>,
    /// Whether the VM is in step mode.
    step_mode: bool,
    /// The frame depth at which stepping started (for step-over).
    step_frame_depth: usize,
    /// Whether the VM is currently stopped at a debug point.
    stopped: bool,
    /// Last source line executed (to detect line changes).
    last_line: usize,
}

impl Vm {
    pub fn new() -> Self {
        Self {
            stack: Vec::with_capacity(256),
            env: VmEnv::new(),
            output: String::new(),
            builtins: BTreeMap::new(),
            async_builtins: BTreeMap::new(),
            iterators: Vec::new(),
            frames: Vec::new(),
            exception_handlers: Vec::new(),
            spawned_tasks: BTreeMap::new(),
            task_counter: 0,
            deadlines: Vec::new(),
            breakpoints: Vec::new(),
            step_mode: false,
            step_frame_depth: 0,
            stopped: false,
            last_line: 0,
        }
    }

    /// Set breakpoints by source line number.
    pub fn set_breakpoints(&mut self, lines: Vec<usize>) {
        self.breakpoints = lines;
    }

    /// Enable step mode (stop at next line).
    pub fn set_step_mode(&mut self, step: bool) {
        self.step_mode = step;
        self.step_frame_depth = self.frames.len();
    }

    /// Enable step-over mode (stop at next line at same or lower frame depth).
    pub fn set_step_over(&mut self) {
        self.step_mode = true;
        self.step_frame_depth = self.frames.len();
    }

    /// Enable step-out mode (stop when returning from current frame).
    pub fn set_step_out(&mut self) {
        self.step_mode = true;
        self.step_frame_depth = self.frames.len().saturating_sub(1);
    }

    /// Check if the VM is stopped at a debug point.
    pub fn is_stopped(&self) -> bool {
        self.stopped
    }

    /// Get the current debug state (variables, line, etc.).
    pub fn debug_state(&self) -> DebugState {
        let line = self.current_line();
        let variables = self.env.all_variables();
        let frame_name = if self.frames.len() > 1 {
            format!("frame_{}", self.frames.len() - 1)
        } else {
            "pipeline".to_string()
        };
        DebugState {
            line,
            variables,
            frame_name,
            frame_depth: self.frames.len(),
        }
    }

    /// Get all stack frames for the debugger.
    pub fn debug_stack_frames(&self) -> Vec<(String, usize)> {
        let mut frames = Vec::new();
        for (i, frame) in self.frames.iter().enumerate() {
            let line = if frame.ip > 0 && frame.ip - 1 < frame.chunk.lines.len() {
                frame.chunk.lines[frame.ip - 1] as usize
            } else {
                0
            };
            let name = if i == 0 {
                "pipeline".to_string()
            } else {
                format!("fn_{}", i)
            };
            frames.push((name, line));
        }
        frames
    }

    /// Get the current source line.
    fn current_line(&self) -> usize {
        if let Some(frame) = self.frames.last() {
            let ip = if frame.ip > 0 { frame.ip - 1 } else { 0 };
            if ip < frame.chunk.lines.len() {
                return frame.chunk.lines[ip] as usize;
            }
        }
        0
    }

    /// Execute one instruction, returning whether to stop (breakpoint/step).
    /// Returns Ok(None) to continue, Ok(Some(val)) on program end, Err on error.
    pub async fn step_execute(&mut self) -> Result<Option<(VmValue, bool)>, VmError> {
        // Check if we need to stop at this line
        let current_line = self.current_line();
        let line_changed = current_line != self.last_line && current_line > 0;

        if line_changed {
            self.last_line = current_line;

            // Check breakpoints
            if self.breakpoints.contains(&current_line) {
                self.stopped = true;
                return Ok(Some((VmValue::Nil, true))); // true = stopped
            }

            // Check step mode
            if self.step_mode && self.frames.len() <= self.step_frame_depth + 1 {
                self.step_mode = false;
                self.stopped = true;
                return Ok(Some((VmValue::Nil, true))); // true = stopped
            }
        }

        // Execute one instruction cycle
        self.stopped = false;
        self.execute_one_cycle().await
    }

    /// Execute a single instruction cycle.
    async fn execute_one_cycle(&mut self) -> Result<Option<(VmValue, bool)>, VmError> {
        // Check deadline
        if let Some(&(deadline, _)) = self.deadlines.last() {
            if Instant::now() > deadline {
                self.deadlines.pop();
                let err = VmError::Thrown(VmValue::String(Rc::from("Deadline exceeded")));
                match self.handle_error(err) {
                    Ok(None) => return Ok(None),
                    Ok(Some(val)) => return Ok(Some((val, false))),
                    Err(e) => return Err(e),
                }
            }
        }

        // Get current frame
        let frame = match self.frames.last_mut() {
            Some(f) => f,
            None => {
                let val = self.stack.pop().unwrap_or(VmValue::Nil);
                return Ok(Some((val, false)));
            }
        };

        // Check if we've reached end of chunk
        if frame.ip >= frame.chunk.code.len() {
            let val = self.stack.pop().unwrap_or(VmValue::Nil);
            let popped_frame = self.frames.pop().unwrap();
            if self.frames.is_empty() {
                return Ok(Some((val, false)));
            } else {
                self.env = popped_frame.saved_env;
                self.stack.truncate(popped_frame.stack_base);
                self.stack.push(val);
                return Ok(None);
            }
        }

        let op = frame.chunk.code[frame.ip];
        frame.ip += 1;

        match self.execute_op(op).await {
            Ok(Some(val)) => Ok(Some((val, false))),
            Ok(None) => Ok(None),
            Err(VmError::Return(val)) => {
                if let Some(popped_frame) = self.frames.pop() {
                    let current_depth = self.frames.len();
                    self.exception_handlers
                        .retain(|h| h.frame_depth <= current_depth);
                    if self.frames.is_empty() {
                        return Ok(Some((val, false)));
                    }
                    self.env = popped_frame.saved_env;
                    self.stack.truncate(popped_frame.stack_base);
                    self.stack.push(val);
                    Ok(None)
                } else {
                    Ok(Some((val, false)))
                }
            }
            Err(e) => match self.handle_error(e) {
                Ok(None) => Ok(None),
                Ok(Some(val)) => Ok(Some((val, false))),
                Err(e) => Err(e),
            },
        }
    }

    /// Initialize execution (push the initial frame).
    pub fn start(&mut self, chunk: &Chunk) {
        self.frames.push(CallFrame {
            chunk: chunk.clone(),
            ip: 0,
            stack_base: self.stack.len(),
            saved_env: self.env.clone(),
        });
    }

    /// Register a sync builtin function.
    pub fn register_builtin<F>(&mut self, name: &str, f: F)
    where
        F: Fn(&[VmValue], &mut String) -> Result<VmValue, VmError> + 'static,
    {
        self.builtins.insert(name.to_string(), Rc::new(f));
    }

    /// Register an async builtin function.
    pub fn register_async_builtin<F, Fut>(&mut self, name: &str, f: F)
    where
        F: Fn(Vec<VmValue>) -> Fut + 'static,
        Fut: Future<Output = Result<VmValue, VmError>> + 'static,
    {
        self.async_builtins
            .insert(name.to_string(), Rc::new(move |args| Box::pin(f(args))));
    }

    /// Create a child VM that shares builtins and env but has fresh execution state.
    /// Used for parallel/spawn to fork the VM for concurrent tasks.
    fn child_vm(&self) -> Vm {
        Vm {
            stack: Vec::with_capacity(64),
            env: self.env.clone(),
            output: String::new(),
            builtins: self.builtins.clone(),
            async_builtins: self.async_builtins.clone(),
            iterators: Vec::new(),
            frames: Vec::new(),
            exception_handlers: Vec::new(),
            spawned_tasks: BTreeMap::new(),
            task_counter: 0,
            deadlines: Vec::new(),
            breakpoints: Vec::new(),
            step_mode: false,
            step_frame_depth: 0,
            stopped: false,
            last_line: 0,
        }
    }

    /// Get the captured output.
    pub fn output(&self) -> &str {
        &self.output
    }

    /// Execute a compiled chunk.
    pub async fn execute(&mut self, chunk: &Chunk) -> Result<VmValue, VmError> {
        self.run_chunk(chunk).await
    }

    /// Convert a VmError into either a handled exception (returning Ok) or a propagated error.
    fn handle_error(&mut self, error: VmError) -> Result<Option<VmValue>, VmError> {
        // Extract the thrown value from the error
        let thrown_value = match &error {
            VmError::Thrown(v) => v.clone(),
            other => VmValue::String(Rc::from(other.to_string())),
        };

        if let Some(handler) = self.exception_handlers.pop() {
            // Unwind call frames back to the handler's frame depth
            while self.frames.len() > handler.frame_depth {
                if let Some(frame) = self.frames.pop() {
                    self.env = frame.saved_env;
                }
            }

            // Clean up deadlines from unwound frames
            while self
                .deadlines
                .last()
                .is_some_and(|d| d.1 > handler.frame_depth)
            {
                self.deadlines.pop();
            }

            // Restore stack to handler's depth
            self.stack.truncate(handler.stack_depth);

            // Push the error value onto the stack (catch body can access it)
            self.stack.push(thrown_value);

            // Set the IP in the current frame to the catch handler
            if let Some(frame) = self.frames.last_mut() {
                frame.ip = handler.catch_ip;
            }

            Ok(None) // Continue execution
        } else {
            Err(error) // No handler, propagate
        }
    }

    async fn run_chunk(&mut self, chunk: &Chunk) -> Result<VmValue, VmError> {
        // Push initial frame
        self.frames.push(CallFrame {
            chunk: chunk.clone(),
            ip: 0,
            stack_base: self.stack.len(),
            saved_env: self.env.clone(),
        });

        loop {
            // Check deadline before each instruction
            if let Some(&(deadline, _)) = self.deadlines.last() {
                if Instant::now() > deadline {
                    self.deadlines.pop();
                    let err = VmError::Thrown(VmValue::String(Rc::from("Deadline exceeded")));
                    match self.handle_error(err) {
                        Ok(None) => continue,
                        Ok(Some(val)) => return Ok(val),
                        Err(e) => return Err(e),
                    }
                }
            }

            // Get current frame
            let frame = match self.frames.last_mut() {
                Some(f) => f,
                None => return Ok(self.stack.pop().unwrap_or(VmValue::Nil)),
            };

            // Check if we've reached end of chunk
            if frame.ip >= frame.chunk.code.len() {
                let val = self.stack.pop().unwrap_or(VmValue::Nil);
                let popped_frame = self.frames.pop().unwrap();

                if self.frames.is_empty() {
                    // We're done with the top-level chunk
                    return Ok(val);
                } else {
                    // Returning from a function call
                    self.env = popped_frame.saved_env;
                    self.stack.truncate(popped_frame.stack_base);
                    self.stack.push(val);
                    continue;
                }
            }

            let op = frame.chunk.code[frame.ip];
            frame.ip += 1;

            match self.execute_op(op).await {
                Ok(Some(val)) => return Ok(val),
                Ok(None) => continue,
                Err(VmError::Return(val)) => {
                    // Pop the current frame
                    if let Some(popped_frame) = self.frames.pop() {
                        // Clean up exception handlers from the returned frame
                        let current_depth = self.frames.len();
                        self.exception_handlers
                            .retain(|h| h.frame_depth <= current_depth);

                        if self.frames.is_empty() {
                            return Ok(val);
                        }
                        self.env = popped_frame.saved_env;
                        self.stack.truncate(popped_frame.stack_base);
                        self.stack.push(val);
                    } else {
                        return Ok(val);
                    }
                }
                Err(e) => {
                    match self.handle_error(e) {
                        Ok(None) => continue, // Handler found, continue
                        Ok(Some(val)) => return Ok(val),
                        Err(e) => return Err(e), // No handler, propagate
                    }
                }
            }
        }
    }

    /// Execute a single opcode. Returns:
    /// - Ok(None): continue execution
    /// - Ok(Some(val)): return this value (top-level exit)
    /// - Err(e): error occurred
    async fn execute_op(&mut self, op: u8) -> Result<Option<VmValue>, VmError> {
        // We need to borrow frame fields, but we also need &mut self for other ops.
        // Strategy: read what we need from the frame first, then do the work.

        if op == Op::Constant as u8 {
            let frame = self.frames.last_mut().unwrap();
            let idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let val = match &frame.chunk.constants[idx] {
                Constant::Int(n) => VmValue::Int(*n),
                Constant::Float(n) => VmValue::Float(*n),
                Constant::String(s) => VmValue::String(Rc::from(s.as_str())),
                Constant::Bool(b) => VmValue::Bool(*b),
                Constant::Nil => VmValue::Nil,
            };
            self.stack.push(val);
        } else if op == Op::Nil as u8 {
            self.stack.push(VmValue::Nil);
        } else if op == Op::True as u8 {
            self.stack.push(VmValue::Bool(true));
        } else if op == Op::False as u8 {
            self.stack.push(VmValue::Bool(false));
        } else if op == Op::GetVar as u8 {
            let frame = self.frames.last_mut().unwrap();
            let idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let name = match &frame.chunk.constants[idx] {
                Constant::String(s) => s.clone(),
                _ => return Err(VmError::TypeError("expected string constant".into())),
            };
            let val = self.env.get(&name).unwrap_or(VmValue::Nil);
            self.stack.push(val);
        } else if op == Op::DefLet as u8 {
            let frame = self.frames.last_mut().unwrap();
            let idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let name = Self::const_string(&frame.chunk.constants[idx])?;
            let val = self.pop()?;
            self.env.define(&name, val, false);
        } else if op == Op::DefVar as u8 {
            let frame = self.frames.last_mut().unwrap();
            let idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let name = Self::const_string(&frame.chunk.constants[idx])?;
            let val = self.pop()?;
            self.env.define(&name, val, true);
        } else if op == Op::SetVar as u8 {
            let frame = self.frames.last_mut().unwrap();
            let idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let name = Self::const_string(&frame.chunk.constants[idx])?;
            let val = self.pop()?;
            self.env.assign(&name, val)?;
        } else if op == Op::Add as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(self.add(a, b));
        } else if op == Op::Sub as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(self.sub(a, b));
        } else if op == Op::Mul as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(self.mul(a, b));
        } else if op == Op::Div as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(self.div(a, b)?);
        } else if op == Op::Mod as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(self.modulo(a, b)?);
        } else if op == Op::Negate as u8 {
            let v = self.pop()?;
            self.stack.push(match v {
                VmValue::Int(n) => VmValue::Int(n.wrapping_neg()),
                VmValue::Float(n) => VmValue::Float(-n),
                _ => {
                    return Err(VmError::Runtime(format!(
                        "Cannot negate value of type {}",
                        v.type_name()
                    )))
                }
            });
        } else if op == Op::Equal as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(VmValue::Bool(values_equal(&a, &b)));
        } else if op == Op::NotEqual as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(VmValue::Bool(!values_equal(&a, &b)));
        } else if op == Op::Less as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(VmValue::Bool(compare_values(&a, &b) < 0));
        } else if op == Op::Greater as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(VmValue::Bool(compare_values(&a, &b) > 0));
        } else if op == Op::LessEqual as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(VmValue::Bool(compare_values(&a, &b) <= 0));
        } else if op == Op::GreaterEqual as u8 {
            let b = self.pop()?;
            let a = self.pop()?;
            self.stack.push(VmValue::Bool(compare_values(&a, &b) >= 0));
        } else if op == Op::Not as u8 {
            let v = self.pop()?;
            self.stack.push(VmValue::Bool(!v.is_truthy()));
        } else if op == Op::Jump as u8 {
            let frame = self.frames.last_mut().unwrap();
            let target = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip = target;
        } else if op == Op::JumpIfFalse as u8 {
            let frame = self.frames.last_mut().unwrap();
            let target = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let val = self.peek()?;
            if !val.is_truthy() {
                let frame = self.frames.last_mut().unwrap();
                frame.ip = target;
            }
        } else if op == Op::JumpIfTrue as u8 {
            let frame = self.frames.last_mut().unwrap();
            let target = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let val = self.peek()?;
            if val.is_truthy() {
                let frame = self.frames.last_mut().unwrap();
                frame.ip = target;
            }
        } else if op == Op::Pop as u8 {
            self.pop()?;
        } else if op == Op::Call as u8 {
            let frame = self.frames.last_mut().unwrap();
            let argc = frame.chunk.code[frame.ip] as usize;
            frame.ip += 1;
            // Clone the functions list so we don't borrow frame across call
            let functions = frame.chunk.functions.clone();

            // Arguments are on stack above the function name/value
            let args: Vec<VmValue> = self.stack.split_off(self.stack.len().saturating_sub(argc));
            let callee = self.pop()?;

            match callee {
                VmValue::String(name) => {
                    if name.as_ref() == "await" {
                        let task_id = args.first().and_then(|a| match a {
                            VmValue::TaskHandle(id) => Some(id.clone()),
                            _ => None,
                        });
                        if let Some(id) = task_id {
                            if let Some(handle) = self.spawned_tasks.remove(&id) {
                                let (result, task_output) = handle.await.map_err(|e| {
                                    VmError::Runtime(format!("Task join error: {e}"))
                                })??;
                                self.output.push_str(&task_output);
                                self.stack.push(result);
                            } else {
                                self.stack.push(VmValue::Nil);
                            }
                        } else {
                            self.stack
                                .push(args.into_iter().next().unwrap_or(VmValue::Nil));
                        }
                    } else if name.as_ref() == "cancel" {
                        if let Some(VmValue::TaskHandle(id)) = args.first() {
                            if let Some(handle) = self.spawned_tasks.remove(id) {
                                handle.abort();
                            }
                        }
                        self.stack.push(VmValue::Nil);
                    } else if let Some(VmValue::Closure(closure)) = self.env.get(&name) {
                        // Check closures in env
                        self.push_closure_frame(&closure, &args, &functions)?;
                        // Don't push result - frame will handle it on return
                    } else if let Some(builtin) = self.builtins.get(name.as_ref()).cloned() {
                        let result = builtin(&args, &mut self.output)?;
                        self.stack.push(result);
                    } else if let Some(async_builtin) =
                        self.async_builtins.get(name.as_ref()).cloned()
                    {
                        let result = async_builtin(args).await?;
                        self.stack.push(result);
                    } else {
                        return Err(VmError::UndefinedBuiltin(name.to_string()));
                    }
                }
                VmValue::Closure(closure) => {
                    self.push_closure_frame(&closure, &args, &functions)?;
                }
                _ => {
                    return Err(VmError::TypeError(format!(
                        "Cannot call {}",
                        callee.display()
                    )));
                }
            }
        } else if op == Op::Return as u8 {
            let val = self.pop().unwrap_or(VmValue::Nil);
            return Err(VmError::Return(val));
        } else if op == Op::Closure as u8 {
            let frame = self.frames.last_mut().unwrap();
            let fn_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let func = frame.chunk.functions[fn_idx].clone();
            let closure = VmClosure {
                func,
                env: self.env.clone(),
            };
            self.stack.push(VmValue::Closure(Rc::new(closure)));
        } else if op == Op::BuildList as u8 {
            let frame = self.frames.last_mut().unwrap();
            let count = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let items = self.stack.split_off(self.stack.len().saturating_sub(count));
            self.stack.push(VmValue::List(Rc::new(items)));
        } else if op == Op::BuildDict as u8 {
            let frame = self.frames.last_mut().unwrap();
            let count = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let pairs = self
                .stack
                .split_off(self.stack.len().saturating_sub(count * 2));
            let mut map = BTreeMap::new();
            for pair in pairs.chunks(2) {
                if pair.len() == 2 {
                    let key = pair[0].display();
                    map.insert(key, pair[1].clone());
                }
            }
            self.stack.push(VmValue::Dict(Rc::new(map)));
        } else if op == Op::Subscript as u8 {
            let idx = self.pop()?;
            let obj = self.pop()?;
            let result = match (&obj, &idx) {
                (VmValue::List(items), VmValue::Int(i)) if *i >= 0 => {
                    items.get(*i as usize).cloned().unwrap_or(VmValue::Nil)
                }
                (VmValue::Dict(map), _) => map.get(&idx.display()).cloned().unwrap_or(VmValue::Nil),
                _ => VmValue::Nil,
            };
            self.stack.push(result);
        } else if op == Op::GetProperty as u8 {
            let frame = self.frames.last_mut().unwrap();
            let idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let name = Self::const_string(&frame.chunk.constants[idx])?;
            let obj = self.pop()?;
            let result = match &obj {
                VmValue::Dict(map) => map.get(&name).cloned().unwrap_or(VmValue::Nil),
                VmValue::List(items) => match name.as_str() {
                    "count" => VmValue::Int(items.len() as i64),
                    "empty" => VmValue::Bool(items.is_empty()),
                    "first" => items.first().cloned().unwrap_or(VmValue::Nil),
                    "last" => items.last().cloned().unwrap_or(VmValue::Nil),
                    _ => VmValue::Nil,
                },
                VmValue::String(s) => match name.as_str() {
                    "count" => VmValue::Int(s.chars().count() as i64),
                    "empty" => VmValue::Bool(s.is_empty()),
                    _ => VmValue::Nil,
                },
                _ => VmValue::Nil,
            };
            self.stack.push(result);
        } else if op == Op::SetProperty as u8 {
            let frame = self.frames.last_mut().unwrap();
            let prop_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let var_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let prop_name = Self::const_string(&frame.chunk.constants[prop_idx])?;
            let var_name = Self::const_string(&frame.chunk.constants[var_idx])?;
            let new_value = self.pop()?;
            if let Some(obj) = self.env.get(&var_name) {
                match obj {
                    VmValue::Dict(map) => {
                        let mut new_map = (*map).clone();
                        new_map.insert(prop_name, new_value);
                        self.env
                            .assign(&var_name, VmValue::Dict(Rc::new(new_map)))?;
                    }
                    VmValue::StructInstance {
                        struct_name,
                        fields,
                    } => {
                        let mut new_fields = fields.clone();
                        new_fields.insert(prop_name, new_value);
                        self.env.assign(
                            &var_name,
                            VmValue::StructInstance {
                                struct_name,
                                fields: new_fields,
                            },
                        )?;
                    }
                    _ => {}
                }
            }
        } else if op == Op::SetSubscript as u8 {
            let frame = self.frames.last_mut().unwrap();
            let var_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let var_name = Self::const_string(&frame.chunk.constants[var_idx])?;
            let index = self.pop()?;
            let new_value = self.pop()?;
            if let Some(obj) = self.env.get(&var_name) {
                match obj {
                    VmValue::List(items) => {
                        if let Some(i) = index.as_int() {
                            let mut new_items = (*items).clone();
                            let idx = if i < 0 {
                                (new_items.len() as i64 + i).max(0) as usize
                            } else {
                                i as usize
                            };
                            if idx < new_items.len() {
                                new_items[idx] = new_value;
                                self.env
                                    .assign(&var_name, VmValue::List(Rc::new(new_items)))?;
                            }
                        }
                    }
                    VmValue::Dict(map) => {
                        let key = index.display();
                        let mut new_map = (*map).clone();
                        new_map.insert(key, new_value);
                        self.env
                            .assign(&var_name, VmValue::Dict(Rc::new(new_map)))?;
                    }
                    _ => {}
                }
            }
        } else if op == Op::MethodCall as u8 {
            let frame = self.frames.last_mut().unwrap();
            let name_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let argc = frame.chunk.code[frame.ip] as usize;
            frame.ip += 1;
            let method = Self::const_string(&frame.chunk.constants[name_idx])?;
            let functions = frame.chunk.functions.clone();
            let args: Vec<VmValue> = self.stack.split_off(self.stack.len().saturating_sub(argc));
            let obj = self.pop()?;
            let result = self.call_method(obj, &method, &args, &functions).await?;
            self.stack.push(result);
        } else if op == Op::Concat as u8 {
            let frame = self.frames.last_mut().unwrap();
            let count = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let parts = self.stack.split_off(self.stack.len().saturating_sub(count));
            let result: String = parts.iter().map(|p| p.display()).collect();
            self.stack.push(VmValue::String(Rc::from(result)));
        } else if op == Op::Pipe as u8 {
            let callable = self.pop()?;
            let value = self.pop()?;
            let functions = self.frames.last().unwrap().chunk.functions.clone();
            match callable {
                VmValue::Closure(closure) => {
                    self.push_closure_frame(&closure, &[value], &functions)?;
                }
                VmValue::String(name) => {
                    if let Some(VmValue::Closure(closure)) = self.env.get(&name) {
                        self.push_closure_frame(&closure, &[value], &functions)?;
                    } else if let Some(builtin) = self.builtins.get(name.as_ref()) {
                        let result = builtin(&[value], &mut self.output)?;
                        self.stack.push(result);
                    } else {
                        self.stack.push(VmValue::Nil);
                    }
                }
                _ => self.stack.push(VmValue::Nil),
            }
        } else if op == Op::Dup as u8 {
            let val = self.peek()?.clone();
            self.stack.push(val);
        } else if op == Op::Swap as u8 {
            let len = self.stack.len();
            if len >= 2 {
                self.stack.swap(len - 1, len - 2);
            }
        } else if op == Op::IterInit as u8 {
            let iterable = self.pop()?;
            let items = match iterable {
                VmValue::List(items) => (*items).clone(),
                VmValue::Dict(map) => map
                    .iter()
                    .map(|(k, v)| {
                        VmValue::Dict(Rc::new(BTreeMap::from([
                            ("key".to_string(), VmValue::String(Rc::from(k.as_str()))),
                            ("value".to_string(), v.clone()),
                        ])))
                    })
                    .collect(),
                _ => Vec::new(),
            };
            self.iterators.push((items, 0));
        } else if op == Op::IterNext as u8 {
            let frame = self.frames.last_mut().unwrap();
            let target = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            if let Some((items, idx)) = self.iterators.last_mut() {
                if *idx < items.len() {
                    let item = items[*idx].clone();
                    *idx += 1;
                    self.stack.push(item);
                } else {
                    self.iterators.pop();
                    let frame = self.frames.last_mut().unwrap();
                    frame.ip = target;
                }
            } else {
                let frame = self.frames.last_mut().unwrap();
                frame.ip = target;
            }
        } else if op == Op::Throw as u8 {
            let val = self.pop()?;
            return Err(VmError::Thrown(val));
        } else if op == Op::TryCatchSetup as u8 {
            let frame = self.frames.last_mut().unwrap();
            let catch_offset = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            self.exception_handlers.push(ExceptionHandler {
                catch_ip: catch_offset,
                stack_depth: self.stack.len(),
                frame_depth: self.frames.len(),
            });
        } else if op == Op::PopHandler as u8 {
            self.exception_handlers.pop();
        } else if op == Op::Parallel as u8 {
            let closure = self.pop()?;
            let count_val = self.pop()?;
            let count = match &count_val {
                VmValue::Int(n) => (*n).max(0) as usize,
                _ => 0,
            };
            if let VmValue::Closure(closure) = closure {
                let mut handles = Vec::with_capacity(count);
                for i in 0..count {
                    let mut child = self.child_vm();
                    let closure = closure.clone();
                    handles.push(tokio::task::spawn_local(async move {
                        let result = child
                            .call_closure(&closure, &[VmValue::Int(i as i64)], &[])
                            .await?;
                        Ok((result, std::mem::take(&mut child.output)))
                    }));
                }
                let mut results = vec![VmValue::Nil; count];
                for (i, handle) in handles.into_iter().enumerate() {
                    let (val, task_output): (VmValue, String) = handle
                        .await
                        .map_err(|e| VmError::Runtime(format!("Parallel task error: {e}")))??;
                    self.output.push_str(&task_output);
                    results[i] = val;
                }
                self.stack.push(VmValue::List(Rc::new(results)));
            } else {
                self.stack.push(VmValue::Nil);
            }
        } else if op == Op::ParallelMap as u8 {
            let closure = self.pop()?;
            let list_val = self.pop()?;
            match (&list_val, &closure) {
                (VmValue::List(items), VmValue::Closure(closure)) => {
                    let len = items.len();
                    let mut handles = Vec::with_capacity(len);
                    for item in items.iter() {
                        let mut child = self.child_vm();
                        let closure = closure.clone();
                        let item = item.clone();
                        handles.push(tokio::task::spawn_local(async move {
                            let result = child.call_closure(&closure, &[item], &[]).await?;
                            Ok((result, std::mem::take(&mut child.output)))
                        }));
                    }
                    let mut results = Vec::with_capacity(len);
                    for handle in handles {
                        let (val, task_output): (VmValue, String) = handle
                            .await
                            .map_err(|e| VmError::Runtime(format!("Parallel map error: {e}")))??;
                        self.output.push_str(&task_output);
                        results.push(val);
                    }
                    self.stack.push(VmValue::List(Rc::new(results)));
                }
                _ => self.stack.push(VmValue::Nil),
            }
        } else if op == Op::Spawn as u8 {
            let closure = self.pop()?;
            if let VmValue::Closure(closure) = closure {
                self.task_counter += 1;
                let task_id = format!("vm_task_{}", self.task_counter);
                let mut child = self.child_vm();
                let handle = tokio::task::spawn_local(async move {
                    let result = child.call_closure(&closure, &[], &[]).await?;
                    Ok((result, std::mem::take(&mut child.output)))
                });
                self.spawned_tasks.insert(task_id.clone(), handle);
                self.stack.push(VmValue::TaskHandle(task_id));
            } else {
                self.stack.push(VmValue::Nil);
            }
        } else if op == Op::DeadlineSetup as u8 {
            let dur_val = self.pop()?;
            let ms = match &dur_val {
                VmValue::Duration(ms) => *ms,
                VmValue::Int(n) => (*n).max(0) as u64,
                _ => 30_000,
            };
            let deadline = Instant::now() + std::time::Duration::from_millis(ms);
            self.deadlines.push((deadline, self.frames.len()));
        } else if op == Op::DeadlineEnd as u8 {
            self.deadlines.pop();
        } else {
            return Err(VmError::InvalidInstruction(op));
        }

        Ok(None)
    }

    const MAX_FRAMES: usize = 512;

    /// Merge the caller's env into a closure's captured env for function calls.
    fn merge_env_into_closure(caller_env: &VmEnv, closure: &VmClosure) -> VmEnv {
        let mut call_env = closure.env.clone();
        for scope in &caller_env.scopes {
            for (name, (val, mutable)) in &scope.vars {
                if call_env.get(name).is_none() {
                    call_env.define(name, val.clone(), *mutable);
                }
            }
        }
        call_env
    }

    /// Push a new call frame for a closure invocation.
    fn push_closure_frame(
        &mut self,
        closure: &VmClosure,
        args: &[VmValue],
        _parent_functions: &[CompiledFunction],
    ) -> Result<(), VmError> {
        if self.frames.len() >= Self::MAX_FRAMES {
            return Err(VmError::StackOverflow);
        }
        let saved_env = self.env.clone();

        let mut call_env = Self::merge_env_into_closure(&saved_env, closure);
        call_env.push_scope();

        for (i, param) in closure.func.params.iter().enumerate() {
            let val = args.get(i).cloned().unwrap_or(VmValue::Nil);
            call_env.define(param, val, false);
        }

        self.env = call_env;

        self.frames.push(CallFrame {
            chunk: closure.func.chunk.clone(),
            ip: 0,
            stack_base: self.stack.len(),
            saved_env,
        });

        Ok(())
    }

    fn pop(&mut self) -> Result<VmValue, VmError> {
        self.stack.pop().ok_or(VmError::StackUnderflow)
    }

    fn peek(&self) -> Result<&VmValue, VmError> {
        self.stack.last().ok_or(VmError::StackUnderflow)
    }

    fn const_string(c: &Constant) -> Result<String, VmError> {
        match c {
            Constant::String(s) => Ok(s.clone()),
            _ => Err(VmError::TypeError("expected string constant".into())),
        }
    }

    /// Call a closure (used by method calls like .map/.filter etc.)
    /// Uses recursive execution for simplicity in method dispatch.
    fn call_closure<'a>(
        &'a mut self,
        closure: &'a VmClosure,
        args: &'a [VmValue],
        _parent_functions: &'a [CompiledFunction],
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + 'a>> {
        Box::pin(async move {
            let saved_env = self.env.clone();
            let saved_frames = std::mem::take(&mut self.frames);
            let saved_handlers = std::mem::take(&mut self.exception_handlers);
            let saved_iterators = std::mem::take(&mut self.iterators);
            let saved_deadlines = std::mem::take(&mut self.deadlines);

            let mut call_env = Self::merge_env_into_closure(&saved_env, closure);
            call_env.push_scope();

            for (i, param) in closure.func.params.iter().enumerate() {
                let val = args.get(i).cloned().unwrap_or(VmValue::Nil);
                call_env.define(param, val, false);
            }

            self.env = call_env;
            let result = self.run_chunk(&closure.func.chunk).await;

            self.env = saved_env;
            self.frames = saved_frames;
            self.exception_handlers = saved_handlers;
            self.iterators = saved_iterators;
            self.deadlines = saved_deadlines;

            result
        })
    }

    fn call_method<'a>(
        &'a mut self,
        obj: VmValue,
        method: &'a str,
        args: &'a [VmValue],
        functions: &'a [CompiledFunction],
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + 'a>> {
        Box::pin(async move {
            match &obj {
                VmValue::String(s) => match method {
                    "count" => Ok(VmValue::Int(s.chars().count() as i64)),
                    "empty" => Ok(VmValue::Bool(s.is_empty())),
                    "contains" => Ok(VmValue::Bool(
                        s.contains(&*args.first().map(|a| a.display()).unwrap_or_default()),
                    )),
                    "replace" if args.len() >= 2 => Ok(VmValue::String(Rc::from(
                        s.replace(&args[0].display(), &args[1].display()),
                    ))),
                    "split" => {
                        let sep = args.first().map(|a| a.display()).unwrap_or(",".into());
                        Ok(VmValue::List(Rc::new(
                            s.split(&*sep)
                                .map(|p| VmValue::String(Rc::from(p)))
                                .collect(),
                        )))
                    }
                    "trim" => Ok(VmValue::String(Rc::from(s.trim()))),
                    "starts_with" => Ok(VmValue::Bool(
                        s.starts_with(&*args.first().map(|a| a.display()).unwrap_or_default()),
                    )),
                    "ends_with" => Ok(VmValue::Bool(
                        s.ends_with(&*args.first().map(|a| a.display()).unwrap_or_default()),
                    )),
                    "lowercase" => Ok(VmValue::String(Rc::from(s.to_lowercase()))),
                    "uppercase" => Ok(VmValue::String(Rc::from(s.to_uppercase()))),
                    "substring" => {
                        let start = args.first().and_then(|a| a.as_int()).unwrap_or(0);
                        let len = s.chars().count() as i64;
                        let start = start.max(0).min(len) as usize;
                        let end =
                            args.get(1).and_then(|a| a.as_int()).unwrap_or(len).min(len) as usize;
                        let end = end.max(start);
                        let substr: String = s.chars().skip(start).take(end - start).collect();
                        Ok(VmValue::String(Rc::from(substr)))
                    }
                    "index_of" => {
                        let needle = args.first().map(|a| a.display()).unwrap_or_default();
                        Ok(VmValue::Int(
                            s.find(&needle).map(|i| i as i64).unwrap_or(-1),
                        ))
                    }
                    "chars" => Ok(VmValue::List(Rc::new(
                        s.chars()
                            .map(|c| VmValue::String(Rc::from(c.to_string())))
                            .collect(),
                    ))),
                    "repeat" => {
                        let n = args.first().and_then(|a| a.as_int()).unwrap_or(1);
                        Ok(VmValue::String(Rc::from(s.repeat(n.max(0) as usize))))
                    }
                    "reverse" => Ok(VmValue::String(Rc::from(
                        s.chars().rev().collect::<String>(),
                    ))),
                    "pad_left" => {
                        let width = args.first().and_then(|a| a.as_int()).unwrap_or(0) as usize;
                        let pad_char = args
                            .get(1)
                            .map(|a| a.display())
                            .and_then(|s| s.chars().next())
                            .unwrap_or(' ');
                        let current_len = s.chars().count();
                        if current_len >= width {
                            Ok(VmValue::String(Rc::clone(s)))
                        } else {
                            let padding: String =
                                std::iter::repeat_n(pad_char, width - current_len).collect();
                            Ok(VmValue::String(Rc::from(format!("{padding}{s}"))))
                        }
                    }
                    "pad_right" => {
                        let width = args.first().and_then(|a| a.as_int()).unwrap_or(0) as usize;
                        let pad_char = args
                            .get(1)
                            .map(|a| a.display())
                            .and_then(|s| s.chars().next())
                            .unwrap_or(' ');
                        let current_len = s.chars().count();
                        if current_len >= width {
                            Ok(VmValue::String(Rc::clone(s)))
                        } else {
                            let padding: String =
                                std::iter::repeat_n(pad_char, width - current_len).collect();
                            Ok(VmValue::String(Rc::from(format!("{s}{padding}"))))
                        }
                    }
                    _ => Ok(VmValue::Nil),
                },
                VmValue::List(items) => match method {
                    "count" => Ok(VmValue::Int(items.len() as i64)),
                    "empty" => Ok(VmValue::Bool(items.is_empty())),
                    "map" => {
                        if let Some(VmValue::Closure(closure)) = args.first() {
                            let mut results = Vec::new();
                            for item in items.iter() {
                                results.push(
                                    self.call_closure(closure, &[item.clone()], functions)
                                        .await?,
                                );
                            }
                            Ok(VmValue::List(Rc::new(results)))
                        } else {
                            Ok(VmValue::Nil)
                        }
                    }
                    "filter" => {
                        if let Some(VmValue::Closure(closure)) = args.first() {
                            let mut results = Vec::new();
                            for item in items.iter() {
                                let result = self
                                    .call_closure(closure, &[item.clone()], functions)
                                    .await?;
                                if result.is_truthy() {
                                    results.push(item.clone());
                                }
                            }
                            Ok(VmValue::List(Rc::new(results)))
                        } else {
                            Ok(VmValue::Nil)
                        }
                    }
                    "reduce" => {
                        if args.len() >= 2 {
                            if let VmValue::Closure(closure) = &args[1] {
                                let mut acc = args[0].clone();
                                for item in items.iter() {
                                    acc = self
                                        .call_closure(closure, &[acc, item.clone()], functions)
                                        .await?;
                                }
                                return Ok(acc);
                            }
                        }
                        Ok(VmValue::Nil)
                    }
                    "find" => {
                        if let Some(VmValue::Closure(closure)) = args.first() {
                            for item in items.iter() {
                                let result = self
                                    .call_closure(closure, &[item.clone()], functions)
                                    .await?;
                                if result.is_truthy() {
                                    return Ok(item.clone());
                                }
                            }
                        }
                        Ok(VmValue::Nil)
                    }
                    "any" => {
                        if let Some(VmValue::Closure(closure)) = args.first() {
                            for item in items.iter() {
                                let result = self
                                    .call_closure(closure, &[item.clone()], functions)
                                    .await?;
                                if result.is_truthy() {
                                    return Ok(VmValue::Bool(true));
                                }
                            }
                            Ok(VmValue::Bool(false))
                        } else {
                            Ok(VmValue::Bool(false))
                        }
                    }
                    "all" => {
                        if let Some(VmValue::Closure(closure)) = args.first() {
                            for item in items.iter() {
                                let result = self
                                    .call_closure(closure, &[item.clone()], functions)
                                    .await?;
                                if !result.is_truthy() {
                                    return Ok(VmValue::Bool(false));
                                }
                            }
                            Ok(VmValue::Bool(true))
                        } else {
                            Ok(VmValue::Bool(true))
                        }
                    }
                    "flat_map" => {
                        if let Some(VmValue::Closure(closure)) = args.first() {
                            let mut results = Vec::new();
                            for item in items.iter() {
                                let result = self
                                    .call_closure(closure, &[item.clone()], functions)
                                    .await?;
                                if let VmValue::List(inner) = result {
                                    results.extend(inner.iter().cloned());
                                } else {
                                    results.push(result);
                                }
                            }
                            Ok(VmValue::List(Rc::new(results)))
                        } else {
                            Ok(VmValue::Nil)
                        }
                    }
                    "sort" => {
                        let mut sorted: Vec<VmValue> = items.iter().cloned().collect();
                        sorted.sort_by(|a, b| compare_values(a, b).cmp(&0));
                        Ok(VmValue::List(Rc::new(sorted)))
                    }
                    "sort_by" => {
                        if let Some(VmValue::Closure(closure)) = args.first() {
                            let mut keyed: Vec<(VmValue, VmValue)> = Vec::new();
                            for item in items.iter() {
                                let key = self
                                    .call_closure(closure, &[item.clone()], functions)
                                    .await?;
                                keyed.push((item.clone(), key));
                            }
                            keyed.sort_by(|(_, ka), (_, kb)| compare_values(ka, kb).cmp(&0));
                            Ok(VmValue::List(Rc::new(
                                keyed.into_iter().map(|(v, _)| v).collect(),
                            )))
                        } else {
                            Ok(VmValue::Nil)
                        }
                    }
                    "reverse" => {
                        let mut rev: Vec<VmValue> = items.iter().cloned().collect();
                        rev.reverse();
                        Ok(VmValue::List(Rc::new(rev)))
                    }
                    "join" => {
                        let sep = if args.is_empty() {
                            String::new()
                        } else {
                            args[0].display()
                        };
                        let joined: String = items
                            .iter()
                            .map(|v| v.display())
                            .collect::<Vec<_>>()
                            .join(&sep);
                        Ok(VmValue::String(Rc::from(joined)))
                    }
                    "contains" => {
                        let needle = args.first().unwrap_or(&VmValue::Nil);
                        Ok(VmValue::Bool(items.iter().any(|v| values_equal(v, needle))))
                    }
                    "index_of" => {
                        let needle = args.first().unwrap_or(&VmValue::Nil);
                        let idx = items.iter().position(|v| values_equal(v, needle));
                        Ok(VmValue::Int(idx.map(|i| i as i64).unwrap_or(-1)))
                    }
                    "enumerate" => {
                        let result: Vec<VmValue> = items
                            .iter()
                            .enumerate()
                            .map(|(i, v)| {
                                VmValue::Dict(Rc::new(BTreeMap::from([
                                    ("index".to_string(), VmValue::Int(i as i64)),
                                    ("value".to_string(), v.clone()),
                                ])))
                            })
                            .collect();
                        Ok(VmValue::List(Rc::new(result)))
                    }
                    "zip" => {
                        if let Some(VmValue::List(other)) = args.first() {
                            let result: Vec<VmValue> = items
                                .iter()
                                .zip(other.iter())
                                .map(|(a, b)| VmValue::List(Rc::new(vec![a.clone(), b.clone()])))
                                .collect();
                            Ok(VmValue::List(Rc::new(result)))
                        } else {
                            Ok(VmValue::List(Rc::new(Vec::new())))
                        }
                    }
                    "slice" => {
                        let len = items.len() as i64;
                        let start_raw = args.first().and_then(|a| a.as_int()).unwrap_or(0);
                        let start = if start_raw < 0 {
                            (len + start_raw).max(0) as usize
                        } else {
                            (start_raw.min(len)) as usize
                        };
                        let end = if args.len() > 1 {
                            let end_raw = args[1].as_int().unwrap_or(len);
                            if end_raw < 0 {
                                (len + end_raw).max(0) as usize
                            } else {
                                (end_raw.min(len)) as usize
                            }
                        } else {
                            len as usize
                        };
                        let end = end.max(start);
                        Ok(VmValue::List(Rc::new(items[start..end].to_vec())))
                    }
                    "unique" => {
                        let mut seen: Vec<VmValue> = Vec::new();
                        let mut result = Vec::new();
                        for item in items.iter() {
                            if !seen.iter().any(|s| values_equal(s, item)) {
                                seen.push(item.clone());
                                result.push(item.clone());
                            }
                        }
                        Ok(VmValue::List(Rc::new(result)))
                    }
                    "take" => {
                        let n = args.first().and_then(|a| a.as_int()).unwrap_or(0).max(0) as usize;
                        Ok(VmValue::List(Rc::new(
                            items.iter().take(n).cloned().collect(),
                        )))
                    }
                    "skip" => {
                        let n = args.first().and_then(|a| a.as_int()).unwrap_or(0).max(0) as usize;
                        Ok(VmValue::List(Rc::new(
                            items.iter().skip(n).cloned().collect(),
                        )))
                    }
                    "sum" => {
                        let mut int_sum: i64 = 0;
                        let mut has_float = false;
                        let mut float_sum: f64 = 0.0;
                        for item in items.iter() {
                            match item {
                                VmValue::Int(n) => {
                                    int_sum = int_sum.wrapping_add(*n);
                                    float_sum += *n as f64;
                                }
                                VmValue::Float(n) => {
                                    has_float = true;
                                    float_sum += n;
                                }
                                _ => {}
                            }
                        }
                        if has_float {
                            Ok(VmValue::Float(float_sum))
                        } else {
                            Ok(VmValue::Int(int_sum))
                        }
                    }
                    "min" => {
                        if items.is_empty() {
                            return Ok(VmValue::Nil);
                        }
                        let mut min_val = items[0].clone();
                        for item in &items[1..] {
                            if compare_values(item, &min_val) < 0 {
                                min_val = item.clone();
                            }
                        }
                        Ok(min_val)
                    }
                    "max" => {
                        if items.is_empty() {
                            return Ok(VmValue::Nil);
                        }
                        let mut max_val = items[0].clone();
                        for item in &items[1..] {
                            if compare_values(item, &max_val) > 0 {
                                max_val = item.clone();
                            }
                        }
                        Ok(max_val)
                    }
                    "flatten" => {
                        let mut result = Vec::new();
                        for item in items.iter() {
                            if let VmValue::List(inner) = item {
                                result.extend(inner.iter().cloned());
                            } else {
                                result.push(item.clone());
                            }
                        }
                        Ok(VmValue::List(Rc::new(result)))
                    }
                    "push" => {
                        let mut new_list: Vec<VmValue> = items.iter().cloned().collect();
                        if let Some(item) = args.first() {
                            new_list.push(item.clone());
                        }
                        Ok(VmValue::List(Rc::new(new_list)))
                    }
                    "pop" => {
                        let mut new_list: Vec<VmValue> = items.iter().cloned().collect();
                        new_list.pop();
                        Ok(VmValue::List(Rc::new(new_list)))
                    }
                    _ => Ok(VmValue::Nil),
                },
                VmValue::Dict(map) => match method {
                    "keys" => Ok(VmValue::List(Rc::new(
                        map.keys()
                            .map(|k| VmValue::String(Rc::from(k.as_str())))
                            .collect(),
                    ))),
                    "values" => Ok(VmValue::List(Rc::new(map.values().cloned().collect()))),
                    "entries" => Ok(VmValue::List(Rc::new(
                        map.iter()
                            .map(|(k, v)| {
                                VmValue::Dict(Rc::new(BTreeMap::from([
                                    ("key".to_string(), VmValue::String(Rc::from(k.as_str()))),
                                    ("value".to_string(), v.clone()),
                                ])))
                            })
                            .collect(),
                    ))),
                    "count" => Ok(VmValue::Int(map.len() as i64)),
                    "has" => Ok(VmValue::Bool(map.contains_key(
                        &args.first().map(|a| a.display()).unwrap_or_default(),
                    ))),
                    "merge" => {
                        if let Some(VmValue::Dict(other)) = args.first() {
                            let mut result = (**map).clone();
                            result.extend(other.iter().map(|(k, v)| (k.clone(), v.clone())));
                            Ok(VmValue::Dict(Rc::new(result)))
                        } else {
                            Ok(VmValue::Dict(Rc::clone(map)))
                        }
                    }
                    "map_values" => {
                        if let Some(VmValue::Closure(closure)) = args.first() {
                            let mut result = BTreeMap::new();
                            for (k, v) in map.iter() {
                                let mapped =
                                    self.call_closure(closure, &[v.clone()], functions).await?;
                                result.insert(k.clone(), mapped);
                            }
                            Ok(VmValue::Dict(Rc::new(result)))
                        } else {
                            Ok(VmValue::Nil)
                        }
                    }
                    "filter" => {
                        if let Some(VmValue::Closure(closure)) = args.first() {
                            let mut result = BTreeMap::new();
                            for (k, v) in map.iter() {
                                let keep =
                                    self.call_closure(closure, &[v.clone()], functions).await?;
                                if keep.is_truthy() {
                                    result.insert(k.clone(), v.clone());
                                }
                            }
                            Ok(VmValue::Dict(Rc::new(result)))
                        } else {
                            Ok(VmValue::Nil)
                        }
                    }
                    "remove" => {
                        let key = args.first().map(|a| a.display()).unwrap_or_default();
                        let mut result = (**map).clone();
                        result.remove(&key);
                        Ok(VmValue::Dict(Rc::new(result)))
                    }
                    "get" => {
                        let key = args.first().map(|a| a.display()).unwrap_or_default();
                        let default = args.get(1).cloned().unwrap_or(VmValue::Nil);
                        Ok(map.get(&key).cloned().unwrap_or(default))
                    }
                    _ => Ok(VmValue::Nil),
                },
                _ => Ok(VmValue::Nil),
            }
        })
    }

    // --- Arithmetic helpers ---

    fn add(&self, a: VmValue, b: VmValue) -> VmValue {
        match (&a, &b) {
            (VmValue::Int(x), VmValue::Int(y)) => VmValue::Int(x.wrapping_add(*y)),
            (VmValue::Float(x), VmValue::Float(y)) => VmValue::Float(x + y),
            (VmValue::Int(x), VmValue::Float(y)) => VmValue::Float(*x as f64 + y),
            (VmValue::Float(x), VmValue::Int(y)) => VmValue::Float(x + *y as f64),
            (VmValue::String(x), _) => VmValue::String(Rc::from(format!("{x}{}", b.display()))),
            (VmValue::List(x), VmValue::List(y)) => {
                let mut result = (**x).clone();
                result.extend(y.iter().cloned());
                VmValue::List(Rc::new(result))
            }
            (VmValue::Dict(x), VmValue::Dict(y)) => {
                let mut result = (**x).clone();
                result.extend(y.iter().map(|(k, v)| (k.clone(), v.clone())));
                VmValue::Dict(Rc::new(result))
            }
            _ => VmValue::String(Rc::from(format!("{}{}", a.display(), b.display()))),
        }
    }

    fn sub(&self, a: VmValue, b: VmValue) -> VmValue {
        match (&a, &b) {
            (VmValue::Int(x), VmValue::Int(y)) => VmValue::Int(x.wrapping_sub(*y)),
            (VmValue::Float(x), VmValue::Float(y)) => VmValue::Float(x - y),
            (VmValue::Int(x), VmValue::Float(y)) => VmValue::Float(*x as f64 - y),
            (VmValue::Float(x), VmValue::Int(y)) => VmValue::Float(x - *y as f64),
            _ => VmValue::Nil,
        }
    }

    fn mul(&self, a: VmValue, b: VmValue) -> VmValue {
        match (&a, &b) {
            (VmValue::Int(x), VmValue::Int(y)) => VmValue::Int(x.wrapping_mul(*y)),
            (VmValue::Float(x), VmValue::Float(y)) => VmValue::Float(x * y),
            (VmValue::Int(x), VmValue::Float(y)) => VmValue::Float(*x as f64 * y),
            (VmValue::Float(x), VmValue::Int(y)) => VmValue::Float(x * *y as f64),
            _ => VmValue::Nil,
        }
    }

    fn div(&self, a: VmValue, b: VmValue) -> Result<VmValue, VmError> {
        match (&a, &b) {
            (VmValue::Int(_), VmValue::Int(y)) if *y == 0 => Err(VmError::DivisionByZero),
            (VmValue::Int(x), VmValue::Int(y)) => Ok(VmValue::Int(x / y)),
            (VmValue::Float(_), VmValue::Float(y)) if *y == 0.0 => Err(VmError::DivisionByZero),
            (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x / y)),
            (VmValue::Int(_), VmValue::Float(y)) if *y == 0.0 => Err(VmError::DivisionByZero),
            (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float(*x as f64 / y)),
            (VmValue::Float(_), VmValue::Int(y)) if *y == 0 => Err(VmError::DivisionByZero),
            (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x / *y as f64)),
            _ => Err(VmError::Runtime(format!(
                "Cannot divide {} by {}",
                a.type_name(),
                b.type_name()
            ))),
        }
    }

    fn modulo(&self, a: VmValue, b: VmValue) -> Result<VmValue, VmError> {
        match (&a, &b) {
            (VmValue::Int(_), VmValue::Int(y)) if *y == 0 => Err(VmError::DivisionByZero),
            (VmValue::Int(x), VmValue::Int(y)) => Ok(VmValue::Int(x % y)),
            (VmValue::Float(_), VmValue::Float(y)) if *y == 0.0 => Err(VmError::DivisionByZero),
            (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x % y)),
            (VmValue::Int(_), VmValue::Float(y)) if *y == 0.0 => Err(VmError::DivisionByZero),
            (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float(*x as f64 % y)),
            (VmValue::Float(_), VmValue::Int(y)) if *y == 0 => Err(VmError::DivisionByZero),
            (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x % *y as f64)),
            _ => Err(VmError::Runtime(format!(
                "Cannot modulo {} by {}",
                a.type_name(),
                b.type_name()
            ))),
        }
    }
}

impl Default for Vm {
    fn default() -> Self {
        Self::new()
    }
}

/// Register standard builtins on a VM.
pub fn register_vm_stdlib(vm: &mut Vm) {
    vm.register_builtin("log", |args, out| {
        let msg = args.first().map(|a| a.display()).unwrap_or_default();
        out.push_str(&format!("[harn] {msg}\n"));
        Ok(VmValue::Nil)
    });
    vm.register_builtin("print", |args, out| {
        let msg = args.first().map(|a| a.display()).unwrap_or_default();
        out.push_str(&msg);
        Ok(VmValue::Nil)
    });
    vm.register_builtin("println", |args, out| {
        let msg = args.first().map(|a| a.display()).unwrap_or_default();
        out.push_str(&format!("{msg}\n"));
        Ok(VmValue::Nil)
    });
    vm.register_builtin("type_of", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        Ok(VmValue::String(Rc::from(val.type_name())))
    });
    vm.register_builtin("to_string", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        Ok(VmValue::String(Rc::from(val.display())))
    });
    vm.register_builtin("to_int", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        match val {
            VmValue::Int(n) => Ok(VmValue::Int(*n)),
            VmValue::Float(n) => Ok(VmValue::Int(*n as i64)),
            VmValue::String(s) => Ok(s.parse::<i64>().map(VmValue::Int).unwrap_or(VmValue::Nil)),
            _ => Ok(VmValue::Nil),
        }
    });
    vm.register_builtin("to_float", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        match val {
            VmValue::Float(n) => Ok(VmValue::Float(*n)),
            VmValue::Int(n) => Ok(VmValue::Float(*n as f64)),
            VmValue::String(s) => Ok(s.parse::<f64>().map(VmValue::Float).unwrap_or(VmValue::Nil)),
            _ => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("json_stringify", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        Ok(VmValue::String(Rc::from(vm_value_to_json(val))))
    });

    vm.register_builtin("json_parse", |args, _out| {
        let text = args.first().map(|a| a.display()).unwrap_or_default();
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(jv) => Ok(json_to_vm_value(&jv)),
            Err(e) => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "JSON parse error: {e}"
            ))))),
        }
    });

    vm.register_builtin("env", |args, _out| {
        let name = args.first().map(|a| a.display()).unwrap_or_default();
        match std::env::var(&name) {
            Ok(val) => Ok(VmValue::String(Rc::from(val))),
            Err(_) => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("timestamp", |_args, _out| {
        use std::time::{SystemTime, UNIX_EPOCH};
        let secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map(|d| d.as_secs_f64())
            .unwrap_or(0.0);
        Ok(VmValue::Float(secs))
    });

    vm.register_builtin("read_file", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        match std::fs::read_to_string(&path) {
            Ok(content) => Ok(VmValue::String(Rc::from(content))),
            Err(e) => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "Failed to read file {path}: {e}"
            ))))),
        }
    });

    vm.register_builtin("write_file", |args, _out| {
        if args.len() >= 2 {
            let path = args[0].display();
            let content = args[1].display();
            std::fs::write(&path, &content).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "Failed to write file {path}: {e}"
                ))))
            })?;
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("exit", |args, _out| {
        let code = args.first().and_then(|a| a.as_int()).unwrap_or(0);
        std::process::exit(code as i32);
    });

    vm.register_builtin("regex_match", |args, _out| {
        if args.len() >= 2 {
            let pattern = args[0].display();
            let text = args[1].display();
            let re = regex::Regex::new(&pattern).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!("Invalid regex: {e}"))))
            })?;
            let matches: Vec<VmValue> = re
                .find_iter(&text)
                .map(|m| VmValue::String(Rc::from(m.as_str())))
                .collect();
            if matches.is_empty() {
                return Ok(VmValue::Nil);
            }
            return Ok(VmValue::List(Rc::new(matches)));
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("regex_replace", |args, _out| {
        if args.len() >= 3 {
            let pattern = args[0].display();
            let replacement = args[1].display();
            let text = args[2].display();
            let re = regex::Regex::new(&pattern).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!("Invalid regex: {e}"))))
            })?;
            return Ok(VmValue::String(Rc::from(
                re.replace_all(&text, replacement.as_str()).into_owned(),
            )));
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("prompt_user", |args, out| {
        let msg = args.first().map(|a| a.display()).unwrap_or_default();
        out.push_str(&msg);
        let mut input = String::new();
        if std::io::stdin().lock().read_line(&mut input).is_ok() {
            Ok(VmValue::String(Rc::from(input.trim_end())))
        } else {
            Ok(VmValue::Nil)
        }
    });

    // --- Math builtins ---

    vm.register_builtin("abs", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::Int(n) => Ok(VmValue::Int(n.wrapping_abs())),
            VmValue::Float(n) => Ok(VmValue::Float(n.abs())),
            _ => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("min", |args, _out| {
        if args.len() >= 2 {
            match (&args[0], &args[1]) {
                (VmValue::Int(x), VmValue::Int(y)) => Ok(VmValue::Int(*x.min(y))),
                (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x.min(*y))),
                (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float((*x as f64).min(*y))),
                (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x.min(*y as f64))),
                _ => Ok(VmValue::Nil),
            }
        } else {
            Ok(VmValue::Nil)
        }
    });

    vm.register_builtin("max", |args, _out| {
        if args.len() >= 2 {
            match (&args[0], &args[1]) {
                (VmValue::Int(x), VmValue::Int(y)) => Ok(VmValue::Int(*x.max(y))),
                (VmValue::Float(x), VmValue::Float(y)) => Ok(VmValue::Float(x.max(*y))),
                (VmValue::Int(x), VmValue::Float(y)) => Ok(VmValue::Float((*x as f64).max(*y))),
                (VmValue::Float(x), VmValue::Int(y)) => Ok(VmValue::Float(x.max(*y as f64))),
                _ => Ok(VmValue::Nil),
            }
        } else {
            Ok(VmValue::Nil)
        }
    });

    vm.register_builtin("floor", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::Float(n) => Ok(VmValue::Int(n.floor() as i64)),
            VmValue::Int(n) => Ok(VmValue::Int(*n)),
            _ => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("ceil", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::Float(n) => Ok(VmValue::Int(n.ceil() as i64)),
            VmValue::Int(n) => Ok(VmValue::Int(*n)),
            _ => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("round", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::Float(n) => Ok(VmValue::Int(n.round() as i64)),
            VmValue::Int(n) => Ok(VmValue::Int(*n)),
            _ => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("sqrt", |args, _out| {
        match args.first().unwrap_or(&VmValue::Nil) {
            VmValue::Float(n) => Ok(VmValue::Float(n.sqrt())),
            VmValue::Int(n) => Ok(VmValue::Float((*n as f64).sqrt())),
            _ => Ok(VmValue::Nil),
        }
    });

    vm.register_builtin("pow", |args, _out| {
        if args.len() >= 2 {
            match (&args[0], &args[1]) {
                (VmValue::Int(base), VmValue::Int(exp)) => {
                    if *exp >= 0 && *exp <= u32::MAX as i64 {
                        Ok(VmValue::Int(base.wrapping_pow(*exp as u32)))
                    } else {
                        Ok(VmValue::Float((*base as f64).powf(*exp as f64)))
                    }
                }
                (VmValue::Float(base), VmValue::Int(exp)) => {
                    if *exp >= i32::MIN as i64 && *exp <= i32::MAX as i64 {
                        Ok(VmValue::Float(base.powi(*exp as i32)))
                    } else {
                        Ok(VmValue::Float(base.powf(*exp as f64)))
                    }
                }
                (VmValue::Int(base), VmValue::Float(exp)) => {
                    Ok(VmValue::Float((*base as f64).powf(*exp)))
                }
                (VmValue::Float(base), VmValue::Float(exp)) => Ok(VmValue::Float(base.powf(*exp))),
                _ => Ok(VmValue::Nil),
            }
        } else {
            Ok(VmValue::Nil)
        }
    });

    vm.register_builtin("random", |_args, _out| {
        use rand::Rng;
        let val: f64 = rand::thread_rng().gen();
        Ok(VmValue::Float(val))
    });

    vm.register_builtin("random_int", |args, _out| {
        use rand::Rng;
        if args.len() >= 2 {
            let min = args[0].as_int().unwrap_or(0);
            let max = args[1].as_int().unwrap_or(0);
            if min <= max {
                let val = rand::thread_rng().gen_range(min..=max);
                return Ok(VmValue::Int(val));
            }
        }
        Ok(VmValue::Nil)
    });

    // --- Assert builtins ---

    vm.register_builtin("assert", |args, _out| {
        let condition = args.first().unwrap_or(&VmValue::Nil);
        if !condition.is_truthy() {
            let msg = args
                .get(1)
                .map(|a| a.display())
                .unwrap_or_else(|| "Assertion failed".to_string());
            return Err(VmError::Thrown(VmValue::String(Rc::from(msg))));
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("assert_eq", |args, _out| {
        if args.len() >= 2 {
            if !values_equal(&args[0], &args[1]) {
                let msg = args.get(2).map(|a| a.display()).unwrap_or_else(|| {
                    format!(
                        "Assertion failed: expected {}, got {}",
                        args[1].display(),
                        args[0].display()
                    )
                });
                return Err(VmError::Thrown(VmValue::String(Rc::from(msg))));
            }
            Ok(VmValue::Nil)
        } else {
            Err(VmError::Thrown(VmValue::String(Rc::from(
                "assert_eq requires at least 2 arguments",
            ))))
        }
    });

    vm.register_builtin("assert_ne", |args, _out| {
        if args.len() >= 2 {
            if values_equal(&args[0], &args[1]) {
                let msg = args.get(2).map(|a| a.display()).unwrap_or_else(|| {
                    format!(
                        "Assertion failed: values should not be equal: {}",
                        args[0].display()
                    )
                });
                return Err(VmError::Thrown(VmValue::String(Rc::from(msg))));
            }
            Ok(VmValue::Nil)
        } else {
            Err(VmError::Thrown(VmValue::String(Rc::from(
                "assert_ne requires at least 2 arguments",
            ))))
        }
    });

    vm.register_builtin("__range__", |args, _out| {
        let start = args.first().and_then(|a| a.as_int()).unwrap_or(0);
        let end = args.get(1).and_then(|a| a.as_int()).unwrap_or(0);
        let inclusive = args.get(2).map(|a| a.is_truthy()).unwrap_or(false);
        let items: Vec<VmValue> = if inclusive {
            (start..=end).map(VmValue::Int).collect()
        } else {
            (start..end).map(VmValue::Int).collect()
        };
        Ok(VmValue::List(Rc::new(items)))
    });

    // =========================================================================
    // File system builtins
    // =========================================================================

    vm.register_builtin("file_exists", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        Ok(VmValue::Bool(std::path::Path::new(&path).exists()))
    });

    vm.register_builtin("delete_file", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        let p = std::path::Path::new(&path);
        if p.is_dir() {
            std::fs::remove_dir_all(&path).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "Failed to delete directory {path}: {e}"
                ))))
            })?;
        } else {
            std::fs::remove_file(&path).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "Failed to delete file {path}: {e}"
                ))))
            })?;
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("append_file", |args, _out| {
        use std::io::Write;
        if args.len() >= 2 {
            let path = args[0].display();
            let content = args[1].display();
            let mut file = std::fs::OpenOptions::new()
                .append(true)
                .create(true)
                .open(&path)
                .map_err(|e| {
                    VmError::Thrown(VmValue::String(Rc::from(format!(
                        "Failed to open file {path}: {e}"
                    ))))
                })?;
            file.write_all(content.as_bytes()).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "Failed to append to file {path}: {e}"
                ))))
            })?;
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("list_dir", |args, _out| {
        let path = args
            .first()
            .map(|a| a.display())
            .unwrap_or_else(|| ".".to_string());
        let entries = std::fs::read_dir(&path).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "Failed to list directory {path}: {e}"
            ))))
        })?;
        let mut result = Vec::new();
        for entry in entries {
            let entry =
                entry.map_err(|e| VmError::Thrown(VmValue::String(Rc::from(e.to_string()))))?;
            let name = entry.file_name().to_string_lossy().to_string();
            result.push(VmValue::String(Rc::from(name.as_str())));
        }
        result.sort_by_key(|a| a.display());
        Ok(VmValue::List(Rc::new(result)))
    });

    vm.register_builtin("mkdir", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        std::fs::create_dir_all(&path).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "Failed to create directory {path}: {e}"
            ))))
        })?;
        Ok(VmValue::Nil)
    });

    vm.register_builtin("path_join", |args, _out| {
        let mut path = std::path::PathBuf::new();
        for arg in args {
            path.push(arg.display());
        }
        Ok(VmValue::String(Rc::from(
            path.to_string_lossy().to_string().as_str(),
        )))
    });

    vm.register_builtin("copy_file", |args, _out| {
        if args.len() >= 2 {
            let src = args[0].display();
            let dst = args[1].display();
            std::fs::copy(&src, &dst).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "Failed to copy {src} to {dst}: {e}"
                ))))
            })?;
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("temp_dir", |_args, _out| {
        Ok(VmValue::String(Rc::from(
            std::env::temp_dir().to_string_lossy().to_string().as_str(),
        )))
    });

    vm.register_builtin("stat", |args, _out| {
        let path = args.first().map(|a| a.display()).unwrap_or_default();
        let metadata = std::fs::metadata(&path).map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "Failed to stat {path}: {e}"
            ))))
        })?;
        let mut info = BTreeMap::new();
        info.insert("size".to_string(), VmValue::Int(metadata.len() as i64));
        info.insert("is_file".to_string(), VmValue::Bool(metadata.is_file()));
        info.insert("is_dir".to_string(), VmValue::Bool(metadata.is_dir()));
        info.insert(
            "readonly".to_string(),
            VmValue::Bool(metadata.permissions().readonly()),
        );
        if let Ok(modified) = metadata.modified() {
            if let Ok(dur) = modified.duration_since(std::time::UNIX_EPOCH) {
                info.insert("modified".to_string(), VmValue::Float(dur.as_secs_f64()));
            }
        }
        Ok(VmValue::Dict(Rc::new(info)))
    });

    // =========================================================================
    // Process execution builtins
    // =========================================================================

    vm.register_builtin("exec", |args, _out| {
        if args.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "exec: command is required",
            ))));
        }
        let cmd = args[0].display();
        let cmd_args: Vec<String> = args[1..].iter().map(|a| a.display()).collect();
        let output = std::process::Command::new(&cmd)
            .args(&cmd_args)
            .output()
            .map_err(|e| VmError::Thrown(VmValue::String(Rc::from(format!("exec failed: {e}")))))?;
        Ok(vm_output_to_value(output))
    });

    vm.register_builtin("shell", |args, _out| {
        let cmd = args.first().map(|a| a.display()).unwrap_or_default();
        if cmd.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "shell: command string is required",
            ))));
        }
        let shell = if cfg!(target_os = "windows") {
            "cmd"
        } else {
            "sh"
        };
        let flag = if cfg!(target_os = "windows") {
            "/C"
        } else {
            "-c"
        };
        let output = std::process::Command::new(shell)
            .arg(flag)
            .arg(&cmd)
            .output()
            .map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!("shell failed: {e}"))))
            })?;
        Ok(vm_output_to_value(output))
    });

    // =========================================================================
    // Date/time builtins
    // =========================================================================

    vm.register_builtin("date_now", |_args, _out| {
        use std::time::{SystemTime, UNIX_EPOCH};
        let now = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default();
        let total_secs = now.as_secs();
        let (y, m, d, hour, minute, second, dow) = vm_civil_from_timestamp(total_secs);
        let mut result = BTreeMap::new();
        result.insert("year".to_string(), VmValue::Int(y));
        result.insert("month".to_string(), VmValue::Int(m));
        result.insert("day".to_string(), VmValue::Int(d));
        result.insert("hour".to_string(), VmValue::Int(hour));
        result.insert("minute".to_string(), VmValue::Int(minute));
        result.insert("second".to_string(), VmValue::Int(second));
        result.insert("weekday".to_string(), VmValue::Int(dow));
        result.insert("timestamp".to_string(), VmValue::Float(now.as_secs_f64()));
        Ok(VmValue::Dict(Rc::new(result)))
    });

    vm.register_builtin("date_format", |args, _out| {
        let ts = match args.first() {
            Some(VmValue::Float(f)) => *f,
            Some(VmValue::Int(n)) => *n as f64,
            Some(VmValue::Dict(map)) => map
                .get("timestamp")
                .and_then(|v| match v {
                    VmValue::Float(f) => Some(*f),
                    VmValue::Int(n) => Some(*n as f64),
                    _ => None,
                })
                .unwrap_or(0.0),
            _ => 0.0,
        };
        let fmt = args
            .get(1)
            .map(|a| a.display())
            .unwrap_or_else(|| "%Y-%m-%d %H:%M:%S".to_string());

        let (y, m, d, hour, minute, second, _dow) = vm_civil_from_timestamp(ts as u64);

        let result = fmt
            .replace("%Y", &format!("{y:04}"))
            .replace("%m", &format!("{m:02}"))
            .replace("%d", &format!("{d:02}"))
            .replace("%H", &format!("{hour:02}"))
            .replace("%M", &format!("{minute:02}"))
            .replace("%S", &format!("{second:02}"));

        Ok(VmValue::String(Rc::from(result.as_str())))
    });

    vm.register_builtin("date_parse", |args, _out| {
        let s = args.first().map(|a| a.display()).unwrap_or_default();
        let parts: Vec<&str> = s.split(|c: char| !c.is_ascii_digit()).collect();
        let parts: Vec<i64> = parts.iter().filter_map(|p| p.parse().ok()).collect();
        if parts.len() < 3 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "Cannot parse date: {s}"
            )))));
        }
        let (y, m, d) = (parts[0], parts[1], parts[2]);
        let hour = parts.get(3).copied().unwrap_or(0);
        let minute = parts.get(4).copied().unwrap_or(0);
        let second = parts.get(5).copied().unwrap_or(0);

        let (y_adj, m_adj) = if m <= 2 {
            (y - 1, (m + 9) as u64)
        } else {
            (y, (m - 3) as u64)
        };
        let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
        let yoe = (y_adj - era * 400) as u64;
        let doy = (153 * m_adj + 2) / 5 + d as u64 - 1;
        let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
        let days = era * 146097 + doe as i64 - 719468;
        let ts = days * 86400 + hour * 3600 + minute * 60 + second;
        Ok(VmValue::Float(ts as f64))
    });

    // =========================================================================
    // String formatting
    // =========================================================================

    vm.register_builtin("format", |args, _out| {
        let template = args.first().map(|a| a.display()).unwrap_or_default();
        let mut result = String::with_capacity(template.len());
        let mut arg_iter = args.iter().skip(1);
        let mut rest = template.as_str();
        while let Some(pos) = rest.find("{}") {
            result.push_str(&rest[..pos]);
            if let Some(arg) = arg_iter.next() {
                result.push_str(&arg.display());
            } else {
                result.push_str("{}");
            }
            rest = &rest[pos + 2..];
        }
        result.push_str(rest);
        Ok(VmValue::String(Rc::from(result.as_str())))
    });

    // =========================================================================
    // Logging builtins
    // =========================================================================

    vm.register_builtin("log_debug", |args, out| {
        vm_write_log("debug", 0, args, out);
        Ok(VmValue::Nil)
    });

    vm.register_builtin("log_info", |args, out| {
        vm_write_log("info", 1, args, out);
        Ok(VmValue::Nil)
    });

    vm.register_builtin("log_warn", |args, out| {
        vm_write_log("warn", 2, args, out);
        Ok(VmValue::Nil)
    });

    vm.register_builtin("log_error", |args, out| {
        vm_write_log("error", 3, args, out);
        Ok(VmValue::Nil)
    });

    vm.register_builtin("log_set_level", |args, _out| {
        let level_str = args.first().map(|a| a.display()).unwrap_or_default();
        match vm_level_to_u8(&level_str) {
            Some(n) => {
                VM_MIN_LOG_LEVEL.store(n, Ordering::Relaxed);
                Ok(VmValue::Nil)
            }
            None => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                "log_set_level: invalid level '{}'. Expected debug, info, warn, or error",
                level_str
            ))))),
        }
    });

    // =========================================================================
    // Tracing builtins
    // =========================================================================

    vm.register_builtin("trace_start", |args, _out| {
        use rand::Rng;
        let name = args.first().map(|a| a.display()).unwrap_or_default();
        let trace_id = VM_TRACE_STACK.with(|stack| {
            stack
                .borrow()
                .last()
                .map(|t| t.trace_id.clone())
                .unwrap_or_else(|| {
                    let val: u32 = rand::thread_rng().gen();
                    format!("{val:08x}")
                })
        });
        let span_id = {
            let val: u32 = rand::thread_rng().gen();
            format!("{val:08x}")
        };
        let start_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        VM_TRACE_STACK.with(|stack| {
            stack.borrow_mut().push(VmTraceContext {
                trace_id: trace_id.clone(),
                span_id: span_id.clone(),
            });
        });

        let mut span = BTreeMap::new();
        span.insert(
            "trace_id".to_string(),
            VmValue::String(Rc::from(trace_id.as_str())),
        );
        span.insert(
            "span_id".to_string(),
            VmValue::String(Rc::from(span_id.as_str())),
        );
        span.insert("name".to_string(), VmValue::String(Rc::from(name.as_str())));
        span.insert("start_ms".to_string(), VmValue::Int(start_ms));
        Ok(VmValue::Dict(Rc::new(span)))
    });

    vm.register_builtin("trace_end", |args, out| {
        let span = match args.first() {
            Some(VmValue::Dict(d)) => d,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "trace_end: argument must be a span dict from trace_start",
                ))));
            }
        };

        let end_ms = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap_or_default()
            .as_millis() as i64;

        let start_ms = span
            .get("start_ms")
            .and_then(|v| v.as_int())
            .unwrap_or(end_ms);
        let duration_ms = end_ms - start_ms;
        let name = span.get("name").map(|v| v.display()).unwrap_or_default();
        let trace_id = span
            .get("trace_id")
            .map(|v| v.display())
            .unwrap_or_default();
        let span_id = span.get("span_id").map(|v| v.display()).unwrap_or_default();

        VM_TRACE_STACK.with(|stack| {
            stack.borrow_mut().pop();
        });

        let level_num = 1_u8;
        if level_num >= VM_MIN_LOG_LEVEL.load(Ordering::Relaxed) {
            let mut fields = BTreeMap::new();
            fields.insert(
                "trace_id".to_string(),
                VmValue::String(Rc::from(trace_id.as_str())),
            );
            fields.insert(
                "span_id".to_string(),
                VmValue::String(Rc::from(span_id.as_str())),
            );
            fields.insert("name".to_string(), VmValue::String(Rc::from(name.as_str())));
            fields.insert("duration_ms".to_string(), VmValue::Int(duration_ms));
            let line = vm_build_log_line("info", "span_end", Some(&fields));
            out.push_str(&line);
        }

        Ok(VmValue::Nil)
    });

    vm.register_builtin("trace_id", |_args, _out| {
        let id = VM_TRACE_STACK.with(|stack| stack.borrow().last().map(|t| t.trace_id.clone()));
        match id {
            Some(trace_id) => Ok(VmValue::String(Rc::from(trace_id.as_str()))),
            None => Ok(VmValue::Nil),
        }
    });

    // =========================================================================
    // Tool registry builtins
    // =========================================================================

    vm.register_builtin("tool_registry", |_args, _out| {
        let mut registry = BTreeMap::new();
        registry.insert(
            "_type".to_string(),
            VmValue::String(Rc::from("tool_registry")),
        );
        registry.insert("tools".to_string(), VmValue::List(Rc::new(Vec::new())));
        Ok(VmValue::Dict(Rc::new(registry)))
    });

    vm.register_builtin("tool_add", |args, _out| {
        if args.len() < 4 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "tool_add: requires registry, name, description, and handler",
            ))));
        }

        let registry = match &args[0] {
            VmValue::Dict(map) => (**map).clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_add: first argument must be a tool registry",
                ))));
            }
        };

        match registry.get("_type") {
            Some(VmValue::String(t)) if &**t == "tool_registry" => {}
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_add: first argument must be a tool registry",
                ))));
            }
        }

        let name = args[1].display();
        let description = args[2].display();
        let handler = args[3].clone();
        let parameters = if args.len() > 4 {
            args[4].clone()
        } else {
            VmValue::Dict(Rc::new(BTreeMap::new()))
        };

        let mut tool_entry = BTreeMap::new();
        tool_entry.insert("name".to_string(), VmValue::String(Rc::from(name.as_str())));
        tool_entry.insert(
            "description".to_string(),
            VmValue::String(Rc::from(description.as_str())),
        );
        tool_entry.insert("handler".to_string(), handler);
        tool_entry.insert("parameters".to_string(), parameters);

        let mut tools: Vec<VmValue> = match registry.get("tools") {
            Some(VmValue::List(list)) => list
                .iter()
                .filter(|t| {
                    if let VmValue::Dict(e) = t {
                        e.get("name").map(|v| v.display()).as_deref() != Some(name.as_str())
                    } else {
                        true
                    }
                })
                .cloned()
                .collect(),
            _ => Vec::new(),
        };
        tools.push(VmValue::Dict(Rc::new(tool_entry)));

        let mut new_registry = registry;
        new_registry.insert("tools".to_string(), VmValue::List(Rc::new(tools)));
        Ok(VmValue::Dict(Rc::new(new_registry)))
    });

    vm.register_builtin("tool_list", |args, _out| {
        let registry = match args.first() {
            Some(VmValue::Dict(map)) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_list: requires a tool registry",
                ))));
            }
        };
        vm_validate_registry("tool_list", registry)?;

        let tools = vm_get_tools(registry);
        let mut result = Vec::new();
        for tool in tools {
            if let VmValue::Dict(entry) = tool {
                let mut desc = BTreeMap::new();
                if let Some(name) = entry.get("name") {
                    desc.insert("name".to_string(), name.clone());
                }
                if let Some(description) = entry.get("description") {
                    desc.insert("description".to_string(), description.clone());
                }
                if let Some(parameters) = entry.get("parameters") {
                    desc.insert("parameters".to_string(), parameters.clone());
                }
                result.push(VmValue::Dict(Rc::new(desc)));
            }
        }
        Ok(VmValue::List(Rc::new(result)))
    });

    vm.register_builtin("tool_find", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "tool_find: requires registry and name",
            ))));
        }

        let registry = match &args[0] {
            VmValue::Dict(map) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_find: first argument must be a tool registry",
                ))));
            }
        };
        vm_validate_registry("tool_find", registry)?;

        let target_name = args[1].display();
        let tools = vm_get_tools(registry);

        for tool in tools {
            if let VmValue::Dict(entry) = tool {
                if let Some(VmValue::String(name)) = entry.get("name") {
                    if &**name == target_name.as_str() {
                        return Ok(tool.clone());
                    }
                }
            }
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("tool_describe", |args, _out| {
        let registry = match args.first() {
            Some(VmValue::Dict(map)) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_describe: requires a tool registry",
                ))));
            }
        };
        vm_validate_registry("tool_describe", registry)?;

        let tools = vm_get_tools(registry);

        if tools.is_empty() {
            return Ok(VmValue::String(Rc::from("Available tools:\n(none)")));
        }

        let mut tool_infos: Vec<(String, String, String)> = Vec::new();
        for tool in tools {
            if let VmValue::Dict(entry) = tool {
                let name = entry.get("name").map(|v| v.display()).unwrap_or_default();
                let description = entry
                    .get("description")
                    .map(|v| v.display())
                    .unwrap_or_default();
                let params_str = vm_format_parameters(entry.get("parameters"));
                tool_infos.push((name, params_str, description));
            }
        }

        tool_infos.sort_by(|a, b| a.0.cmp(&b.0));

        let mut lines = vec!["Available tools:".to_string()];
        for (name, params, desc) in &tool_infos {
            lines.push(format!("- {name}({params}): {desc}"));
        }

        Ok(VmValue::String(Rc::from(lines.join("\n").as_str())))
    });

    vm.register_builtin("tool_remove", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "tool_remove: requires registry and name",
            ))));
        }

        let registry = match &args[0] {
            VmValue::Dict(map) => (**map).clone(),
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_remove: first argument must be a tool registry",
                ))));
            }
        };
        vm_validate_registry("tool_remove", &registry)?;

        let target_name = args[1].display();

        let tools = match registry.get("tools") {
            Some(VmValue::List(list)) => (**list).clone(),
            _ => Vec::new(),
        };

        let filtered: Vec<VmValue> = tools
            .into_iter()
            .filter(|tool| {
                if let VmValue::Dict(entry) = tool {
                    if let Some(VmValue::String(name)) = entry.get("name") {
                        return &**name != target_name.as_str();
                    }
                }
                true
            })
            .collect();

        let mut new_registry = registry;
        new_registry.insert("tools".to_string(), VmValue::List(Rc::new(filtered)));
        Ok(VmValue::Dict(Rc::new(new_registry)))
    });

    vm.register_builtin("tool_count", |args, _out| {
        let registry = match args.first() {
            Some(VmValue::Dict(map)) => map,
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_count: requires a tool registry",
                ))));
            }
        };
        vm_validate_registry("tool_count", registry)?;
        let count = vm_get_tools(registry).len();
        Ok(VmValue::Int(count as i64))
    });

    vm.register_builtin("tool_schema", |args, _out| {
        let registry = match args.first() {
            Some(VmValue::Dict(map)) => {
                vm_validate_registry("tool_schema", map)?;
                map
            }
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_schema: requires a tool registry",
                ))));
            }
        };

        let components = args.get(1).and_then(|v| v.as_dict()).cloned();

        let tools = match registry.get("tools") {
            Some(VmValue::List(list)) => list,
            _ => return Ok(VmValue::Dict(Rc::new(vm_build_empty_schema()))),
        };

        let mut tool_schemas = Vec::new();
        for tool in tools.iter() {
            if let VmValue::Dict(entry) = tool {
                let name = entry.get("name").map(|v| v.display()).unwrap_or_default();
                let description = entry
                    .get("description")
                    .map(|v| v.display())
                    .unwrap_or_default();

                let input_schema =
                    vm_build_input_schema(entry.get("parameters"), components.as_ref());

                let mut tool_def = BTreeMap::new();
                tool_def.insert("name".to_string(), VmValue::String(Rc::from(name.as_str())));
                tool_def.insert(
                    "description".to_string(),
                    VmValue::String(Rc::from(description.as_str())),
                );
                tool_def.insert("inputSchema".to_string(), input_schema);
                tool_schemas.push(VmValue::Dict(Rc::new(tool_def)));
            }
        }

        let mut schema = BTreeMap::new();
        schema.insert(
            "schema_version".to_string(),
            VmValue::String(Rc::from("harn-tools/1.0")),
        );

        if let Some(comps) = &components {
            let mut comp_wrapper = BTreeMap::new();
            comp_wrapper.insert("schemas".to_string(), VmValue::Dict(Rc::new(comps.clone())));
            schema.insert(
                "components".to_string(),
                VmValue::Dict(Rc::new(comp_wrapper)),
            );
        }

        schema.insert("tools".to_string(), VmValue::List(Rc::new(tool_schemas)));
        Ok(VmValue::Dict(Rc::new(schema)))
    });

    vm.register_builtin("tool_parse_call", |args, _out| {
        let text = args.first().map(|a| a.display()).unwrap_or_default();

        let mut results = Vec::new();
        let mut search_from = 0;

        while let Some(start) = text[search_from..].find("<tool_call>") {
            let abs_start = search_from + start + "<tool_call>".len();
            if let Some(end) = text[abs_start..].find("</tool_call>") {
                let json_str = text[abs_start..abs_start + end].trim();
                if let Ok(jv) = serde_json::from_str::<serde_json::Value>(json_str) {
                    results.push(json_to_vm_value(&jv));
                }
                search_from = abs_start + end + "</tool_call>".len();
            } else {
                break;
            }
        }

        Ok(VmValue::List(Rc::new(results)))
    });

    vm.register_builtin("tool_format_result", |args, _out| {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "tool_format_result: requires name and result",
            ))));
        }
        let name = args[0].display();
        let result = args[1].display();

        let json_name = vm_escape_json_str(&name);
        let json_result = vm_escape_json_str(&result);
        Ok(VmValue::String(Rc::from(
            format!(
                "<tool_result>{{\"name\": \"{json_name}\", \"result\": \"{json_result}\"}}</tool_result>"
            )
            .as_str(),
        )))
    });

    vm.register_builtin("tool_prompt", |args, _out| {
        let registry = match args.first() {
            Some(VmValue::Dict(map)) => {
                vm_validate_registry("tool_prompt", map)?;
                map
            }
            _ => {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "tool_prompt: requires a tool registry",
                ))));
            }
        };

        let tools = match registry.get("tools") {
            Some(VmValue::List(list)) => list,
            _ => {
                return Ok(VmValue::String(Rc::from("No tools are available.")));
            }
        };

        if tools.is_empty() {
            return Ok(VmValue::String(Rc::from("No tools are available.")));
        }

        let mut prompt = String::from("# Available Tools\n\n");
        prompt.push_str("You have access to the following tools. To use a tool, output a tool call in this exact format:\n\n");
        prompt.push_str("<tool_call>{\"name\": \"tool_name\", \"arguments\": {\"param\": \"value\"}}</tool_call>\n\n");
        prompt.push_str("You may make multiple tool calls in a single response. Wait for tool results before proceeding.\n\n");
        prompt.push_str("## Tools\n\n");

        let mut tool_infos: Vec<(&BTreeMap<String, VmValue>, String)> = Vec::new();
        for tool in tools.iter() {
            if let VmValue::Dict(entry) = tool {
                let name = entry.get("name").map(|v| v.display()).unwrap_or_default();
                tool_infos.push((entry, name));
            }
        }
        tool_infos.sort_by(|a, b| a.1.cmp(&b.1));

        for (entry, name) in &tool_infos {
            let description = entry
                .get("description")
                .map(|v| v.display())
                .unwrap_or_default();
            let params_str = vm_format_parameters(entry.get("parameters"));

            prompt.push_str(&format!("### {name}\n"));
            prompt.push_str(&format!("{description}\n"));
            if !params_str.is_empty() {
                prompt.push_str(&format!("Parameters: {params_str}\n"));
            }
            prompt.push('\n');
        }

        Ok(VmValue::String(Rc::from(prompt.trim_end())))
    });

    // =========================================================================
    // Channel builtins (sync)
    // =========================================================================

    vm.register_builtin("channel", |args, _out| {
        let name = args
            .first()
            .map(|a| a.display())
            .unwrap_or_else(|| "default".to_string());
        let capacity = args.get(1).and_then(|a| a.as_int()).unwrap_or(256) as usize;
        let capacity = capacity.max(1);
        let (tx, rx) = tokio::sync::mpsc::channel(capacity);
        #[allow(clippy::arc_with_non_send_sync)]
        Ok(VmValue::Channel(VmChannelHandle {
            name,
            sender: Arc::new(tx),
            receiver: Arc::new(tokio::sync::Mutex::new(rx)),
            closed: Arc::new(AtomicBool::new(false)),
        }))
    });

    vm.register_builtin("close_channel", |args, _out| {
        if args.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "close_channel: requires a channel",
            ))));
        }
        if let VmValue::Channel(ch) = &args[0] {
            ch.closed.store(true, Ordering::SeqCst);
            Ok(VmValue::Nil)
        } else {
            Err(VmError::Thrown(VmValue::String(Rc::from(
                "close_channel: first argument must be a channel",
            ))))
        }
    });

    vm.register_builtin("try_receive", |args, _out| {
        if args.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "try_receive: requires a channel",
            ))));
        }
        if let VmValue::Channel(ch) = &args[0] {
            match ch.receiver.try_lock() {
                Ok(mut rx) => match rx.try_recv() {
                    Ok(val) => Ok(val),
                    Err(_) => Ok(VmValue::Nil),
                },
                Err(_) => Ok(VmValue::Nil),
            }
        } else {
            Err(VmError::Thrown(VmValue::String(Rc::from(
                "try_receive: first argument must be a channel",
            ))))
        }
    });

    // =========================================================================
    // Atomic builtins
    // =========================================================================

    vm.register_builtin("atomic", |args, _out| {
        let initial = match args.first() {
            Some(VmValue::Int(n)) => *n,
            Some(VmValue::Float(f)) => *f as i64,
            Some(VmValue::Bool(b)) => {
                if *b {
                    1
                } else {
                    0
                }
            }
            _ => 0,
        };
        Ok(VmValue::Atomic(VmAtomicHandle {
            value: Arc::new(AtomicI64::new(initial)),
        }))
    });

    vm.register_builtin("atomic_get", |args, _out| {
        if let Some(VmValue::Atomic(a)) = args.first() {
            Ok(VmValue::Int(a.value.load(Ordering::SeqCst)))
        } else {
            Ok(VmValue::Nil)
        }
    });

    vm.register_builtin("atomic_set", |args, _out| {
        if args.len() >= 2 {
            if let (VmValue::Atomic(a), Some(val)) = (&args[0], args[1].as_int()) {
                let old = a.value.swap(val, Ordering::SeqCst);
                return Ok(VmValue::Int(old));
            }
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("atomic_add", |args, _out| {
        if args.len() >= 2 {
            if let (VmValue::Atomic(a), Some(delta)) = (&args[0], args[1].as_int()) {
                let prev = a.value.fetch_add(delta, Ordering::SeqCst);
                return Ok(VmValue::Int(prev));
            }
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("atomic_cas", |args, _out| {
        if args.len() >= 3 {
            if let (VmValue::Atomic(a), Some(expected), Some(new_val)) =
                (&args[0], args[1].as_int(), args[2].as_int())
            {
                let result =
                    a.value
                        .compare_exchange(expected, new_val, Ordering::SeqCst, Ordering::SeqCst);
                return Ok(VmValue::Bool(result.is_ok()));
            }
        }
        Ok(VmValue::Bool(false))
    });

    // =========================================================================
    // Async builtins
    // =========================================================================

    // sleep(ms)
    vm.register_async_builtin("sleep", |args| async move {
        let ms = match args.first() {
            Some(VmValue::Duration(ms)) => *ms,
            Some(VmValue::Int(n)) => *n as u64,
            _ => 0,
        };
        if ms > 0 {
            tokio::time::sleep(tokio::time::Duration::from_millis(ms)).await;
        }
        Ok(VmValue::Nil)
    });

    // send(channel, value)
    vm.register_async_builtin("send", |args| async move {
        if args.len() < 2 {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "send: requires channel and value",
            ))));
        }
        if let VmValue::Channel(ch) = &args[0] {
            if ch.closed.load(Ordering::SeqCst) {
                return Ok(VmValue::Bool(false));
            }
            let val = args[1].clone();
            match ch.sender.send(val).await {
                Ok(()) => Ok(VmValue::Bool(true)),
                Err(_) => Ok(VmValue::Bool(false)),
            }
        } else {
            Err(VmError::Thrown(VmValue::String(Rc::from(
                "send: first argument must be a channel",
            ))))
        }
    });

    // receive(channel)
    vm.register_async_builtin("receive", |args| async move {
        if args.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "receive: requires a channel",
            ))));
        }
        if let VmValue::Channel(ch) = &args[0] {
            if ch.closed.load(Ordering::SeqCst) {
                let mut rx = ch.receiver.lock().await;
                return match rx.try_recv() {
                    Ok(val) => Ok(val),
                    Err(_) => Ok(VmValue::Nil),
                };
            }
            let mut rx = ch.receiver.lock().await;
            match rx.recv().await {
                Some(val) => Ok(val),
                None => Ok(VmValue::Nil),
            }
        } else {
            Err(VmError::Thrown(VmValue::String(Rc::from(
                "receive: first argument must be a channel",
            ))))
        }
    });

    // select(channel1, channel2, ...)
    vm.register_async_builtin("select", |args| async move {
        if args.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "select: requires at least one channel",
            ))));
        }

        let mut channels: Vec<&VmChannelHandle> = Vec::new();
        for arg in &args {
            if let VmValue::Channel(ch) = arg {
                channels.push(ch);
            } else {
                return Err(VmError::Thrown(VmValue::String(Rc::from(
                    "select: all arguments must be channels",
                ))));
            }
        }

        loop {
            let mut all_closed = true;
            for (i, ch) in channels.iter().enumerate() {
                if let Ok(mut rx) = ch.receiver.try_lock() {
                    match rx.try_recv() {
                        Ok(val) => {
                            let mut result = BTreeMap::new();
                            result.insert("index".to_string(), VmValue::Int(i as i64));
                            result.insert("value".to_string(), val);
                            result.insert(
                                "channel".to_string(),
                                VmValue::String(Rc::from(ch.name.as_str())),
                            );
                            return Ok(VmValue::Dict(Rc::new(result)));
                        }
                        Err(tokio::sync::mpsc::error::TryRecvError::Empty) => {
                            all_closed = false;
                        }
                        Err(tokio::sync::mpsc::error::TryRecvError::Disconnected) => {}
                    }
                }
            }
            if all_closed {
                return Ok(VmValue::Nil);
            }
            tokio::time::sleep(tokio::time::Duration::from_millis(1)).await;
        }
    });

    // =========================================================================
    // HTTP builtins (async)
    // =========================================================================

    vm.register_async_builtin("http_get", |args| async move {
        let url = args.first().map(|a| a.display()).unwrap_or_default();
        if url.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "http_get: URL is required",
            ))));
        }
        let options = match args.get(1) {
            Some(VmValue::Dict(d)) => (**d).clone(),
            _ => BTreeMap::new(),
        };
        vm_execute_http_request("GET", &url, &options).await
    });

    vm.register_async_builtin("http_post", |args| async move {
        let url = args.first().map(|a| a.display()).unwrap_or_default();
        if url.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "http_post: URL is required",
            ))));
        }
        let body = args.get(1).map(|a| a.display()).unwrap_or_default();
        let mut options = match args.get(2) {
            Some(VmValue::Dict(d)) => (**d).clone(),
            _ => BTreeMap::new(),
        };
        options.insert("body".to_string(), VmValue::String(Rc::from(body.as_str())));
        vm_execute_http_request("POST", &url, &options).await
    });

    vm.register_async_builtin("http_put", |args| async move {
        let url = args.first().map(|a| a.display()).unwrap_or_default();
        if url.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "http_put: URL is required",
            ))));
        }
        let body = args.get(1).map(|a| a.display()).unwrap_or_default();
        let mut options = match args.get(2) {
            Some(VmValue::Dict(d)) => (**d).clone(),
            _ => BTreeMap::new(),
        };
        options.insert("body".to_string(), VmValue::String(Rc::from(body.as_str())));
        vm_execute_http_request("PUT", &url, &options).await
    });

    vm.register_async_builtin("http_patch", |args| async move {
        let url = args.first().map(|a| a.display()).unwrap_or_default();
        if url.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "http_patch: URL is required",
            ))));
        }
        let body = args.get(1).map(|a| a.display()).unwrap_or_default();
        let mut options = match args.get(2) {
            Some(VmValue::Dict(d)) => (**d).clone(),
            _ => BTreeMap::new(),
        };
        options.insert("body".to_string(), VmValue::String(Rc::from(body.as_str())));
        vm_execute_http_request("PATCH", &url, &options).await
    });

    vm.register_async_builtin("http_delete", |args| async move {
        let url = args.first().map(|a| a.display()).unwrap_or_default();
        if url.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "http_delete: URL is required",
            ))));
        }
        let options = match args.get(1) {
            Some(VmValue::Dict(d)) => (**d).clone(),
            _ => BTreeMap::new(),
        };
        vm_execute_http_request("DELETE", &url, &options).await
    });

    vm.register_async_builtin("http_request", |args| async move {
        let method = args
            .first()
            .map(|a| a.display())
            .unwrap_or_default()
            .to_uppercase();
        if method.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "http_request: method is required",
            ))));
        }
        let url = args.get(1).map(|a| a.display()).unwrap_or_default();
        if url.is_empty() {
            return Err(VmError::Thrown(VmValue::String(Rc::from(
                "http_request: URL is required",
            ))));
        }
        let options = match args.get(2) {
            Some(VmValue::Dict(d)) => (**d).clone(),
            _ => BTreeMap::new(),
        };
        vm_execute_http_request(&method, &url, &options).await
    });

    // =========================================================================
    // LLM builtins (async)
    // =========================================================================

    vm.register_async_builtin("llm_call", |args| async move {
        let prompt = args.first().map(|a| a.display()).unwrap_or_default();
        let system = args.get(1).map(|a| a.display());
        let options = args.get(2).and_then(|a| a.as_dict()).cloned();

        let provider = vm_resolve_provider(&options);
        let model = vm_resolve_model(&options, &provider);
        let api_key = vm_resolve_api_key(&provider)?;
        let max_tokens = options
            .as_ref()
            .and_then(|o| o.get("max_tokens"))
            .and_then(|v| v.as_int())
            .unwrap_or(4096);

        vm_call_llm(
            &provider,
            &model,
            &api_key,
            &prompt,
            system.as_deref(),
            max_tokens,
        )
        .await
    });

    vm.register_async_builtin("agent_loop", |args| async move {
        let prompt = args.first().map(|a| a.display()).unwrap_or_default();
        let system = args.get(1).map(|a| a.display());
        let options = args.get(2).and_then(|a| a.as_dict()).cloned();

        let provider = vm_resolve_provider(&options);
        let model = vm_resolve_model(&options, &provider);
        let api_key = vm_resolve_api_key(&provider)?;
        let max_iterations = options
            .as_ref()
            .and_then(|o| o.get("max_iterations"))
            .and_then(|v| v.as_int())
            .unwrap_or(50) as usize;
        let persistent = options
            .as_ref()
            .and_then(|o| o.get("persistent"))
            .map(|v| v.is_truthy())
            .unwrap_or(false);
        let max_nudges = options
            .as_ref()
            .and_then(|o| o.get("max_nudges"))
            .and_then(|v| v.as_int())
            .unwrap_or(3) as usize;
        let custom_nudge = options
            .as_ref()
            .and_then(|o| o.get("nudge"))
            .map(|v| v.display());
        let max_tokens = options
            .as_ref()
            .and_then(|o| o.get("max_tokens"))
            .and_then(|v| v.as_int())
            .unwrap_or(4096);

        let mut system_prompt = system.unwrap_or_default();
        if persistent {
            system_prompt.push_str(
                "\n\nIMPORTANT: You MUST keep working until the task is complete. \
                 Do NOT stop to explain or summarize — take action. \
                 Output ##DONE## only when the task is fully complete and verified.",
            );
        }

        let mut messages: Vec<serde_json::Value> = vec![serde_json::json!({
            "role": "user",
            "content": prompt,
        })];

        let mut total_text = String::new();
        let mut consecutive_text_only = 0usize;

        for _iteration in 0..max_iterations {
            let response = vm_call_llm_messages(
                &provider,
                &model,
                &api_key,
                &messages,
                if system_prompt.is_empty() {
                    None
                } else {
                    Some(&system_prompt)
                },
                max_tokens,
            )
            .await?;

            let text = response.display();
            total_text.push_str(&text);

            messages.push(serde_json::json!({
                "role": "assistant",
                "content": text,
            }));

            if persistent && text.contains("##DONE##") {
                break;
            }

            if !persistent {
                break;
            }

            consecutive_text_only += 1;
            if consecutive_text_only > max_nudges {
                break;
            }

            let nudge = if let Some(ref custom) = custom_nudge {
                custom.clone()
            } else {
                "You have not output ##DONE## yet — the task is not complete. \
                 Use your tools to continue working. \
                 Only output ##DONE## when the task is fully complete and verified."
                    .to_string()
            };

            messages.push(serde_json::json!({
                "role": "user",
                "content": nudge,
            }));
        }

        Ok(VmValue::String(Rc::from(total_text.as_str())))
    });
}

fn escape_json_string_vm(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

fn vm_value_to_json(val: &VmValue) -> String {
    match val {
        VmValue::String(s) => escape_json_string_vm(s),
        VmValue::Int(n) => n.to_string(),
        VmValue::Float(n) => n.to_string(),
        VmValue::Bool(b) => b.to_string(),
        VmValue::Nil => "null".to_string(),
        VmValue::List(items) => {
            let inner: Vec<String> = items.iter().map(vm_value_to_json).collect();
            format!("[{}]", inner.join(","))
        }
        VmValue::Dict(map) => {
            let inner: Vec<String> = map
                .iter()
                .map(|(k, v)| format!("{}:{}", escape_json_string_vm(k), vm_value_to_json(v)))
                .collect();
            format!("{{{}}}", inner.join(","))
        }
        _ => "null".to_string(),
    }
}

fn json_to_vm_value(jv: &serde_json::Value) -> VmValue {
    match jv {
        serde_json::Value::Null => VmValue::Nil,
        serde_json::Value::Bool(b) => VmValue::Bool(*b),
        serde_json::Value::Number(n) => {
            if let Some(i) = n.as_i64() {
                VmValue::Int(i)
            } else {
                VmValue::Float(n.as_f64().unwrap_or(0.0))
            }
        }
        serde_json::Value::String(s) => VmValue::String(Rc::from(s.as_str())),
        serde_json::Value::Array(arr) => {
            VmValue::List(Rc::new(arr.iter().map(json_to_vm_value).collect()))
        }
        serde_json::Value::Object(map) => {
            let mut m = BTreeMap::new();
            for (k, v) in map {
                m.insert(k.clone(), json_to_vm_value(v));
            }
            VmValue::Dict(Rc::new(m))
        }
    }
}

// =============================================================================
// Helper: convert process::Output to VmValue dict
// =============================================================================

fn vm_output_to_value(output: std::process::Output) -> VmValue {
    let mut result = BTreeMap::new();
    result.insert(
        "stdout".to_string(),
        VmValue::String(Rc::from(
            String::from_utf8_lossy(&output.stdout).to_string().as_str(),
        )),
    );
    result.insert(
        "stderr".to_string(),
        VmValue::String(Rc::from(
            String::from_utf8_lossy(&output.stderr).to_string().as_str(),
        )),
    );
    result.insert(
        "status".to_string(),
        VmValue::Int(output.status.code().unwrap_or(-1) as i64),
    );
    result.insert(
        "success".to_string(),
        VmValue::Bool(output.status.success()),
    );
    VmValue::Dict(Rc::new(result))
}

// =============================================================================
// Helper: civil date from timestamp (Howard Hinnant's algorithm)
// =============================================================================

fn vm_civil_from_timestamp(total_secs: u64) -> (i64, i64, i64, i64, i64, i64, i64) {
    let days = total_secs / 86400;
    let time_of_day = total_secs % 86400;
    let hour = (time_of_day / 3600) as i64;
    let minute = ((time_of_day % 3600) / 60) as i64;
    let second = (time_of_day % 60) as i64;

    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = (doy - (153 * mp + 2) / 5 + 1) as i64;
    let m = if mp < 10 { mp + 3 } else { mp - 9 } as i64;
    let y = if m <= 2 { y + 1 } else { y };
    let dow = ((days + 4) % 7) as i64;

    (y, m, d, hour, minute, second, dow)
}

// =============================================================================
// Logging helpers for VM
// =============================================================================

static VM_MIN_LOG_LEVEL: AtomicU8 = AtomicU8::new(0);

#[derive(Clone)]
struct VmTraceContext {
    trace_id: String,
    span_id: String,
}

thread_local! {
    static VM_TRACE_STACK: std::cell::RefCell<Vec<VmTraceContext>> = const { std::cell::RefCell::new(Vec::new()) };
}

fn vm_level_to_u8(level: &str) -> Option<u8> {
    match level {
        "debug" => Some(0),
        "info" => Some(1),
        "warn" => Some(2),
        "error" => Some(3),
        _ => None,
    }
}

fn vm_format_timestamp_utc() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    let total_secs = now.as_secs();
    let millis = now.subsec_millis();

    let days = total_secs / 86400;
    let time_of_day = total_secs % 86400;
    let hour = time_of_day / 3600;
    let minute = (time_of_day % 3600) / 60;
    let second = time_of_day % 60;

    let z = days as i64 + 719468;
    let era = if z >= 0 { z } else { z - 146096 } / 146097;
    let doe = (z - era * 146097) as u64;
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146096) / 365;
    let y = yoe as i64 + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };

    format!("{y:04}-{m:02}-{d:02}T{hour:02}:{minute:02}:{second:02}.{millis:03}Z")
}

fn vm_escape_json_str(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for ch in s.chars() {
        match ch {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if c.is_control() => {
                out.push_str(&format!("\\u{:04x}", c as u32));
            }
            c => out.push(c),
        }
    }
    out
}

fn vm_escape_json_str_quoted(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    out.push_str(&vm_escape_json_str(s));
    out.push('"');
    out
}

fn vm_value_to_json_fragment(val: &VmValue) -> String {
    match val {
        VmValue::String(s) => vm_escape_json_str_quoted(s),
        VmValue::Int(n) => n.to_string(),
        VmValue::Float(n) => {
            if n.is_finite() {
                n.to_string()
            } else {
                "null".to_string()
            }
        }
        VmValue::Bool(b) => b.to_string(),
        VmValue::Nil => "null".to_string(),
        _ => vm_escape_json_str_quoted(&val.display()),
    }
}

fn vm_build_log_line(level: &str, msg: &str, fields: Option<&BTreeMap<String, VmValue>>) -> String {
    let ts = vm_format_timestamp_utc();
    let mut parts: Vec<String> = Vec::new();
    parts.push(format!("\"ts\":{}", vm_escape_json_str_quoted(&ts)));
    parts.push(format!("\"level\":{}", vm_escape_json_str_quoted(level)));
    parts.push(format!("\"msg\":{}", vm_escape_json_str_quoted(msg)));

    VM_TRACE_STACK.with(|stack| {
        if let Some(trace) = stack.borrow().last() {
            parts.push(format!(
                "\"trace_id\":{}",
                vm_escape_json_str_quoted(&trace.trace_id)
            ));
            parts.push(format!(
                "\"span_id\":{}",
                vm_escape_json_str_quoted(&trace.span_id)
            ));
        }
    });

    if let Some(dict) = fields {
        for (k, v) in dict {
            parts.push(format!(
                "{}:{}",
                vm_escape_json_str_quoted(k),
                vm_value_to_json_fragment(v)
            ));
        }
    }

    format!("{{{}}}\n", parts.join(","))
}

fn vm_write_log(level: &str, level_num: u8, args: &[VmValue], out: &mut String) {
    if level_num < VM_MIN_LOG_LEVEL.load(Ordering::Relaxed) {
        return;
    }
    let msg = args.first().map(|a| a.display()).unwrap_or_default();
    let fields = args.get(1).and_then(|v| {
        if let VmValue::Dict(d) = v {
            Some(&**d)
        } else {
            None
        }
    });
    let line = vm_build_log_line(level, &msg, fields);
    out.push_str(&line);
}

// =============================================================================
// Tool registry helpers for VM
// =============================================================================

fn vm_validate_registry(name: &str, dict: &BTreeMap<String, VmValue>) -> Result<(), VmError> {
    match dict.get("_type") {
        Some(VmValue::String(t)) if &**t == "tool_registry" => Ok(()),
        _ => Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "{name}: argument must be a tool registry (created with tool_registry())"
        ))))),
    }
}

fn vm_get_tools(dict: &BTreeMap<String, VmValue>) -> &[VmValue] {
    match dict.get("tools") {
        Some(VmValue::List(list)) => list,
        _ => &[],
    }
}

fn vm_format_parameters(params: Option<&VmValue>) -> String {
    match params {
        Some(VmValue::Dict(map)) if !map.is_empty() => {
            let mut pairs: Vec<(String, String)> =
                map.iter().map(|(k, v)| (k.clone(), v.display())).collect();
            pairs.sort_by(|a, b| a.0.cmp(&b.0));
            pairs
                .iter()
                .map(|(k, v)| format!("{k}: {v}"))
                .collect::<Vec<_>>()
                .join(", ")
        }
        _ => String::new(),
    }
}

fn vm_build_empty_schema() -> BTreeMap<String, VmValue> {
    let mut schema = BTreeMap::new();
    schema.insert(
        "schema_version".to_string(),
        VmValue::String(Rc::from("harn-tools/1.0")),
    );
    schema.insert("tools".to_string(), VmValue::List(Rc::new(Vec::new())));
    schema
}

fn vm_build_input_schema(
    params: Option<&VmValue>,
    components: Option<&BTreeMap<String, VmValue>>,
) -> VmValue {
    let mut schema = BTreeMap::new();
    schema.insert("type".to_string(), VmValue::String(Rc::from("object")));

    let params_map = match params {
        Some(VmValue::Dict(map)) if !map.is_empty() => map,
        _ => {
            schema.insert(
                "properties".to_string(),
                VmValue::Dict(Rc::new(BTreeMap::new())),
            );
            return VmValue::Dict(Rc::new(schema));
        }
    };

    let mut properties = BTreeMap::new();
    let mut required = Vec::new();

    for (key, val) in params_map.iter() {
        let prop = vm_resolve_param_type(val, components);
        properties.insert(key.clone(), prop);
        required.push(VmValue::String(Rc::from(key.as_str())));
    }

    schema.insert("properties".to_string(), VmValue::Dict(Rc::new(properties)));
    if !required.is_empty() {
        required.sort_by_key(|a| a.display());
        schema.insert("required".to_string(), VmValue::List(Rc::new(required)));
    }

    VmValue::Dict(Rc::new(schema))
}

fn vm_resolve_param_type(val: &VmValue, components: Option<&BTreeMap<String, VmValue>>) -> VmValue {
    match val {
        VmValue::String(type_name) => {
            let json_type = vm_harn_type_to_json_schema(type_name);
            let mut prop = BTreeMap::new();
            prop.insert("type".to_string(), VmValue::String(Rc::from(json_type)));
            VmValue::Dict(Rc::new(prop))
        }
        VmValue::Dict(map) => {
            if let Some(VmValue::String(ref_name)) = map.get("$ref") {
                if let Some(comps) = components {
                    if let Some(resolved) = comps.get(&**ref_name) {
                        return resolved.clone();
                    }
                }
                let mut prop = BTreeMap::new();
                prop.insert(
                    "$ref".to_string(),
                    VmValue::String(Rc::from(
                        format!("#/components/schemas/{ref_name}").as_str(),
                    )),
                );
                VmValue::Dict(Rc::new(prop))
            } else {
                VmValue::Dict(Rc::new((**map).clone()))
            }
        }
        _ => {
            let mut prop = BTreeMap::new();
            prop.insert("type".to_string(), VmValue::String(Rc::from("string")));
            VmValue::Dict(Rc::new(prop))
        }
    }
}

fn vm_harn_type_to_json_schema(harn_type: &str) -> &str {
    match harn_type {
        "int" => "integer",
        "float" => "number",
        "bool" | "boolean" => "boolean",
        "list" | "array" => "array",
        "dict" | "object" => "object",
        _ => "string",
    }
}

// =============================================================================
// HTTP request helper for VM
// =============================================================================

fn vm_get_int_option(options: &BTreeMap<String, VmValue>, key: &str, default: i64) -> i64 {
    options.get(key).and_then(|v| v.as_int()).unwrap_or(default)
}

fn vm_get_bool_option(options: &BTreeMap<String, VmValue>, key: &str, default: bool) -> bool {
    match options.get(key) {
        Some(VmValue::Bool(b)) => *b,
        _ => default,
    }
}

async fn vm_execute_http_request(
    method: &str,
    url: &str,
    options: &BTreeMap<String, VmValue>,
) -> Result<VmValue, VmError> {
    use std::time::Duration;

    if !url.starts_with("http://") && !url.starts_with("https://") {
        return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
            "http: URL must start with http:// or https://, got '{url}'"
        )))));
    }

    let timeout_ms = vm_get_int_option(options, "timeout", 30_000).max(0) as u64;
    let retries = vm_get_int_option(options, "retries", 0).max(0) as u32;
    let backoff_ms = vm_get_int_option(options, "backoff", 1000).max(0) as u64;
    let follow_redirects = vm_get_bool_option(options, "follow_redirects", true);
    let max_redirects = vm_get_int_option(options, "max_redirects", 10).max(0) as usize;

    let redirect_policy = if follow_redirects {
        reqwest::redirect::Policy::limited(max_redirects)
    } else {
        reqwest::redirect::Policy::none()
    };

    let client = reqwest::Client::builder()
        .timeout(Duration::from_millis(timeout_ms))
        .redirect(redirect_policy)
        .build()
        .map_err(|e| {
            VmError::Thrown(VmValue::String(Rc::from(format!(
                "http: failed to build client: {e}"
            ))))
        })?;

    let req_method = method.parse::<reqwest::Method>().map_err(|e| {
        VmError::Thrown(VmValue::String(Rc::from(format!(
            "http: invalid method '{method}': {e}"
        ))))
    })?;

    let mut header_map = reqwest::header::HeaderMap::new();

    // Apply auth
    if let Some(auth_val) = options.get("auth") {
        match auth_val {
            VmValue::String(s) => {
                let hv = reqwest::header::HeaderValue::from_str(s).map_err(|e| {
                    VmError::Thrown(VmValue::String(Rc::from(format!(
                        "http: invalid auth header value: {e}"
                    ))))
                })?;
                header_map.insert(reqwest::header::AUTHORIZATION, hv);
            }
            VmValue::Dict(d) => {
                if let Some(bearer) = d.get("bearer") {
                    let token = bearer.display();
                    let hv = reqwest::header::HeaderValue::from_str(&format!("Bearer {token}"))
                        .map_err(|e| {
                            VmError::Thrown(VmValue::String(Rc::from(format!(
                                "http: invalid bearer token: {e}"
                            ))))
                        })?;
                    header_map.insert(reqwest::header::AUTHORIZATION, hv);
                } else if let Some(VmValue::Dict(basic)) = d.get("basic") {
                    let user = basic.get("user").map(|v| v.display()).unwrap_or_default();
                    let password = basic
                        .get("password")
                        .map(|v| v.display())
                        .unwrap_or_default();
                    use base64::Engine;
                    let encoded = base64::engine::general_purpose::STANDARD
                        .encode(format!("{user}:{password}"));
                    let hv = reqwest::header::HeaderValue::from_str(&format!("Basic {encoded}"))
                        .map_err(|e| {
                            VmError::Thrown(VmValue::String(Rc::from(format!(
                                "http: invalid basic auth: {e}"
                            ))))
                        })?;
                    header_map.insert(reqwest::header::AUTHORIZATION, hv);
                }
            }
            _ => {}
        }
    }

    // Apply explicit headers
    if let Some(VmValue::Dict(hdrs)) = options.get("headers") {
        for (k, v) in hdrs.iter() {
            let name = reqwest::header::HeaderName::from_bytes(k.as_bytes()).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "http: invalid header name '{k}': {e}"
                ))))
            })?;
            let val = reqwest::header::HeaderValue::from_str(&v.display()).map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "http: invalid header value for '{k}': {e}"
                ))))
            })?;
            header_map.insert(name, val);
        }
    }

    let body_str = options.get("body").map(|v| v.display());

    let mut last_err: Option<VmError> = None;
    let total_attempts = 1 + retries;

    for attempt in 0..total_attempts {
        if attempt > 0 {
            use rand::Rng;
            let base_delay = backoff_ms.saturating_mul(1u64 << (attempt - 1).min(30));
            let jitter: f64 = rand::thread_rng().gen_range(0.75..=1.25);
            let delay_ms = ((base_delay as f64 * jitter) as u64).min(60_000);
            tokio::time::sleep(Duration::from_millis(delay_ms)).await;
        }

        let mut req = client.request(req_method.clone(), url);
        req = req.headers(header_map.clone());
        if let Some(ref b) = body_str {
            req = req.body(b.clone());
        }

        match req.send().await {
            Ok(response) => {
                let status_code = response.status().as_u16();
                let ok = (200..300).contains(&status_code);
                let status = status_code as i64;

                let mut resp_headers = BTreeMap::new();
                for (name, value) in response.headers() {
                    if let Ok(v) = value.to_str() {
                        resp_headers
                            .insert(name.as_str().to_string(), VmValue::String(Rc::from(v)));
                    }
                }

                let body_text = response.text().await.map_err(|e| {
                    VmError::Thrown(VmValue::String(Rc::from(format!(
                        "http: failed to read response body: {e}"
                    ))))
                })?;

                if status >= 500 && attempt + 1 < total_attempts {
                    last_err = Some(VmError::Thrown(VmValue::String(Rc::from(format!(
                        "http: server error {status}"
                    )))));
                    continue;
                }

                let mut result = BTreeMap::new();
                result.insert("status".to_string(), VmValue::Int(status));
                result.insert("headers".to_string(), VmValue::Dict(Rc::new(resp_headers)));
                result.insert(
                    "body".to_string(),
                    VmValue::String(Rc::from(body_text.as_str())),
                );
                result.insert("ok".to_string(), VmValue::Bool(ok));
                return Ok(VmValue::Dict(Rc::new(result)));
            }
            Err(e) => {
                let retryable = e.is_timeout() || e.is_connect();
                if retryable && attempt + 1 < total_attempts {
                    last_err = Some(VmError::Thrown(VmValue::String(Rc::from(format!(
                        "http: request failed: {e}"
                    )))));
                    continue;
                }
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "http: request failed: {e}"
                )))));
            }
        }
    }

    Err(last_err
        .unwrap_or_else(|| VmError::Thrown(VmValue::String(Rc::from("http: request failed")))))
}

// =============================================================================
// LLM helpers for VM
// =============================================================================

fn vm_resolve_provider(options: &Option<BTreeMap<String, VmValue>>) -> String {
    options
        .as_ref()
        .and_then(|o| o.get("provider"))
        .map(|v| v.display())
        .unwrap_or_else(|| "anthropic".to_string())
}

fn vm_resolve_model(options: &Option<BTreeMap<String, VmValue>>, provider: &str) -> String {
    options
        .as_ref()
        .and_then(|o| o.get("model"))
        .map(|v| v.display())
        .unwrap_or_else(|| match provider {
            "openai" => "gpt-4o".to_string(),
            "ollama" => "llama3.2".to_string(),
            "openrouter" => "anthropic/claude-sonnet-4-20250514".to_string(),
            _ => "claude-sonnet-4-20250514".to_string(),
        })
}

fn vm_resolve_api_key(provider: &str) -> Result<String, VmError> {
    match provider {
        "ollama" => Ok(String::new()),
        "openai" => std::env::var("OPENAI_API_KEY").map_err(|_| {
            VmError::Thrown(VmValue::String(Rc::from(
                "Missing API key: set OPENAI_API_KEY environment variable",
            )))
        }),
        "openrouter" => std::env::var("OPENROUTER_API_KEY").map_err(|_| {
            VmError::Thrown(VmValue::String(Rc::from(
                "Missing API key: set OPENROUTER_API_KEY environment variable",
            )))
        }),
        _ => std::env::var("ANTHROPIC_API_KEY").map_err(|_| {
            VmError::Thrown(VmValue::String(Rc::from(
                "Missing API key: set ANTHROPIC_API_KEY environment variable",
            )))
        }),
    }
}

async fn vm_call_llm(
    provider: &str,
    model: &str,
    api_key: &str,
    prompt: &str,
    system: Option<&str>,
    max_tokens: i64,
) -> Result<VmValue, VmError> {
    let messages = vec![serde_json::json!({
        "role": "user",
        "content": prompt,
    })];
    vm_call_llm_messages(provider, model, api_key, &messages, system, max_tokens).await
}

async fn vm_call_llm_messages(
    provider: &str,
    model: &str,
    api_key: &str,
    messages: &[serde_json::Value],
    system: Option<&str>,
    max_tokens: i64,
) -> Result<VmValue, VmError> {
    let client = reqwest::Client::new();

    match provider {
        "openai" | "ollama" | "openrouter" => {
            let base_url = match provider {
                "ollama" => std::env::var("OLLAMA_HOST")
                    .unwrap_or_else(|_| "http://localhost:11434".to_string()),
                "openrouter" => "https://openrouter.ai/api".to_string(),
                _ => "https://api.openai.com".to_string(),
            };

            let mut msgs = Vec::new();
            if let Some(sys) = system {
                msgs.push(serde_json::json!({"role": "system", "content": sys}));
            }
            msgs.extend(messages.iter().cloned());

            let body = serde_json::json!({
                "model": model,
                "messages": msgs,
                "max_tokens": max_tokens,
            });

            let mut req = client
                .post(format!("{base_url}/v1/chat/completions"))
                .header("Content-Type", "application/json")
                .json(&body);

            if !api_key.is_empty() {
                req = req.header("Authorization", format!("Bearer {api_key}"));
            }

            let response = req.send().await.map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "{provider} API error: {e}"
                ))))
            })?;

            let json: serde_json::Value = response.json().await.map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "{provider} response parse error: {e}"
                ))))
            })?;

            if let Some(err) = json["error"]["message"].as_str() {
                return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                    "{provider} API error: {err}"
                )))));
            }

            let text = json["choices"][0]["message"]["content"]
                .as_str()
                .unwrap_or("")
                .to_string();
            Ok(VmValue::String(Rc::from(text.as_str())))
        }
        _ => {
            let mut body = serde_json::json!({
                "model": model,
                "messages": messages,
                "max_tokens": max_tokens,
            });
            if let Some(sys) = system {
                body["system"] = serde_json::json!(sys);
            }

            let response = client
                .post("https://api.anthropic.com/v1/messages")
                .header("x-api-key", api_key)
                .header("anthropic-version", "2023-06-01")
                .header("Content-Type", "application/json")
                .json(&body)
                .send()
                .await
                .map_err(|e| {
                    VmError::Thrown(VmValue::String(Rc::from(format!(
                        "Anthropic API error: {e}"
                    ))))
                })?;

            let json: serde_json::Value = response.json().await.map_err(|e| {
                VmError::Thrown(VmValue::String(Rc::from(format!(
                    "Anthropic response parse error: {e}"
                ))))
            })?;

            let text = json["content"]
                .as_array()
                .map(|blocks| {
                    blocks
                        .iter()
                        .filter_map(|b| b["text"].as_str())
                        .collect::<Vec<_>>()
                        .join("")
                })
                .unwrap_or_default();

            if text.is_empty() {
                if let Some(err) = json["error"]["message"].as_str() {
                    return Err(VmError::Thrown(VmValue::String(Rc::from(format!(
                        "Anthropic API error: {err}"
                    )))));
                }
            }

            Ok(VmValue::String(Rc::from(text.as_str())))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::Compiler;
    use harn_lexer::Lexer;
    use harn_parser::Parser;

    fn run_harn(source: &str) -> (String, VmValue) {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async {
                    let mut lexer = Lexer::new(source);
                    let tokens = lexer.tokenize().unwrap();
                    let mut parser = Parser::new(tokens);
                    let program = parser.parse().unwrap();
                    let chunk = Compiler::new().compile(&program).unwrap();

                    let mut vm = Vm::new();
                    register_vm_stdlib(&mut vm);
                    let result = vm.execute(&chunk).await.unwrap();
                    (vm.output().to_string(), result)
                })
                .await
        })
    }

    fn run_output(source: &str) -> String {
        run_harn(source).0.trim_end().to_string()
    }

    fn run_harn_result(source: &str) -> Result<(String, VmValue), VmError> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async {
                    let mut lexer = Lexer::new(source);
                    let tokens = lexer.tokenize().unwrap();
                    let mut parser = Parser::new(tokens);
                    let program = parser.parse().unwrap();
                    let chunk = Compiler::new().compile(&program).unwrap();

                    let mut vm = Vm::new();
                    register_vm_stdlib(&mut vm);
                    let result = vm.execute(&chunk).await?;
                    Ok((vm.output().to_string(), result))
                })
                .await
        })
    }

    #[test]
    fn test_arithmetic() {
        let out =
            run_output("pipeline t(task) { log(2 + 3)\nlog(10 - 4)\nlog(3 * 5)\nlog(10 / 3) }");
        assert_eq!(out, "[harn] 5\n[harn] 6\n[harn] 15\n[harn] 3");
    }

    #[test]
    fn test_mixed_arithmetic() {
        let out = run_output("pipeline t(task) { log(3 + 1.5)\nlog(10 - 2.5) }");
        assert_eq!(out, "[harn] 4.5\n[harn] 7.5");
    }

    #[test]
    fn test_comparisons() {
        let out =
            run_output("pipeline t(task) { log(1 < 2)\nlog(2 > 3)\nlog(1 == 1)\nlog(1 != 2) }");
        assert_eq!(out, "[harn] true\n[harn] false\n[harn] true\n[harn] true");
    }

    #[test]
    fn test_let_var() {
        let out = run_output("pipeline t(task) { let x = 42\nlog(x)\nvar y = 1\ny = 2\nlog(y) }");
        assert_eq!(out, "[harn] 42\n[harn] 2");
    }

    #[test]
    fn test_if_else() {
        let out = run_output(
            r#"pipeline t(task) { if true { log("yes") } if false { log("wrong") } else { log("no") } }"#,
        );
        assert_eq!(out, "[harn] yes\n[harn] no");
    }

    #[test]
    fn test_while_loop() {
        let out = run_output("pipeline t(task) { var i = 0\n while i < 5 { i = i + 1 }\n log(i) }");
        assert_eq!(out, "[harn] 5");
    }

    #[test]
    fn test_for_in() {
        let out = run_output("pipeline t(task) { for item in [1, 2, 3] { log(item) } }");
        assert_eq!(out, "[harn] 1\n[harn] 2\n[harn] 3");
    }

    #[test]
    fn test_fn_decl_and_call() {
        let out = run_output("pipeline t(task) { fn add(a, b) { return a + b }\nlog(add(3, 4)) }");
        assert_eq!(out, "[harn] 7");
    }

    #[test]
    fn test_closure() {
        let out = run_output("pipeline t(task) { let double = { x -> x * 2 }\nlog(double(5)) }");
        assert_eq!(out, "[harn] 10");
    }

    #[test]
    fn test_closure_capture() {
        let out = run_output(
            "pipeline t(task) { let base = 10\nfn offset(x) { return x + base }\nlog(offset(5)) }",
        );
        assert_eq!(out, "[harn] 15");
    }

    #[test]
    fn test_string_concat() {
        let out = run_output(
            r#"pipeline t(task) { let a = "hello" + " " + "world"
log(a) }"#,
        );
        assert_eq!(out, "[harn] hello world");
    }

    #[test]
    fn test_list_map() {
        let out = run_output(
            "pipeline t(task) { let doubled = [1, 2, 3].map({ x -> x * 2 })\nlog(doubled) }",
        );
        assert_eq!(out, "[harn] [2, 4, 6]");
    }

    #[test]
    fn test_list_filter() {
        let out = run_output(
            "pipeline t(task) { let big = [1, 2, 3, 4, 5].filter({ x -> x > 3 })\nlog(big) }",
        );
        assert_eq!(out, "[harn] [4, 5]");
    }

    #[test]
    fn test_list_reduce() {
        let out = run_output(
            "pipeline t(task) { let sum = [1, 2, 3, 4].reduce(0, { acc, x -> acc + x })\nlog(sum) }",
        );
        assert_eq!(out, "[harn] 10");
    }

    #[test]
    fn test_dict_access() {
        let out = run_output(
            r#"pipeline t(task) { let d = {name: "test", value: 42}
log(d.name)
log(d.value) }"#,
        );
        assert_eq!(out, "[harn] test\n[harn] 42");
    }

    #[test]
    fn test_dict_methods() {
        let out = run_output(
            r#"pipeline t(task) { let d = {a: 1, b: 2}
log(d.keys())
log(d.values())
log(d.has("a"))
log(d.has("z")) }"#,
        );
        assert_eq!(
            out,
            "[harn] [a, b]\n[harn] [1, 2]\n[harn] true\n[harn] false"
        );
    }

    #[test]
    fn test_pipe_operator() {
        let out = run_output(
            "pipeline t(task) { fn double(x) { return x * 2 }\nlet r = 5 |> double\nlog(r) }",
        );
        assert_eq!(out, "[harn] 10");
    }

    #[test]
    fn test_pipe_with_closure() {
        let out = run_output(
            r#"pipeline t(task) { let r = "hello world" |> { s -> s.split(" ") }
log(r) }"#,
        );
        assert_eq!(out, "[harn] [hello, world]");
    }

    #[test]
    fn test_nil_coalescing() {
        let out = run_output(
            r#"pipeline t(task) { let a = nil ?? "fallback"
log(a)
let b = "present" ?? "fallback"
log(b) }"#,
        );
        assert_eq!(out, "[harn] fallback\n[harn] present");
    }

    #[test]
    fn test_logical_operators() {
        let out =
            run_output("pipeline t(task) { log(true && false)\nlog(true || false)\nlog(!true) }");
        assert_eq!(out, "[harn] false\n[harn] true\n[harn] false");
    }

    #[test]
    fn test_match() {
        let out = run_output(
            r#"pipeline t(task) { let x = "b"
match x { "a" -> { log("first") } "b" -> { log("second") } "c" -> { log("third") } } }"#,
        );
        assert_eq!(out, "[harn] second");
    }

    #[test]
    fn test_subscript() {
        let out = run_output("pipeline t(task) { let arr = [10, 20, 30]\nlog(arr[1]) }");
        assert_eq!(out, "[harn] 20");
    }

    #[test]
    fn test_string_methods() {
        let out = run_output(
            r#"pipeline t(task) { log("hello world".replace("world", "harn"))
log("a,b,c".split(","))
log("  hello  ".trim())
log("hello".starts_with("hel"))
log("hello".ends_with("lo"))
log("hello".substring(1, 3)) }"#,
        );
        assert_eq!(
            out,
            "[harn] hello harn\n[harn] [a, b, c]\n[harn] hello\n[harn] true\n[harn] true\n[harn] el"
        );
    }

    #[test]
    fn test_list_properties() {
        let out = run_output(
            "pipeline t(task) { let list = [1, 2, 3]\nlog(list.count)\nlog(list.empty)\nlog(list.first)\nlog(list.last) }",
        );
        assert_eq!(out, "[harn] 3\n[harn] false\n[harn] 1\n[harn] 3");
    }

    #[test]
    fn test_recursive_function() {
        let out = run_output(
            "pipeline t(task) { fn fib(n) { if n <= 1 { return n } return fib(n - 1) + fib(n - 2) }\nlog(fib(10)) }",
        );
        assert_eq!(out, "[harn] 55");
    }

    #[test]
    fn test_ternary() {
        let out = run_output(
            r#"pipeline t(task) { let x = 5
let r = x > 0 ? "positive" : "non-positive"
log(r) }"#,
        );
        assert_eq!(out, "[harn] positive");
    }

    #[test]
    fn test_for_in_dict() {
        let out = run_output(
            "pipeline t(task) { let d = {a: 1, b: 2}\nfor entry in d { log(entry.key) } }",
        );
        assert_eq!(out, "[harn] a\n[harn] b");
    }

    #[test]
    fn test_list_any_all() {
        let out = run_output(
            "pipeline t(task) { let nums = [2, 4, 6]\nlog(nums.any({ x -> x > 5 }))\nlog(nums.all({ x -> x > 0 }))\nlog(nums.all({ x -> x > 3 })) }",
        );
        assert_eq!(out, "[harn] true\n[harn] true\n[harn] false");
    }

    #[test]
    fn test_disassembly() {
        let mut lexer = Lexer::new("pipeline t(task) { log(2 + 3) }");
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let program = parser.parse().unwrap();
        let chunk = Compiler::new().compile(&program).unwrap();
        let disasm = chunk.disassemble("test");
        assert!(disasm.contains("CONSTANT"));
        assert!(disasm.contains("ADD"));
        assert!(disasm.contains("CALL"));
    }

    // --- Error handling tests ---

    #[test]
    fn test_try_catch_basic() {
        let out = run_output(
            r#"pipeline t(task) { try { throw "oops" } catch(e) { log("caught: " + e) } }"#,
        );
        assert_eq!(out, "[harn] caught: oops");
    }

    #[test]
    fn test_try_no_error() {
        let out = run_output(
            r#"pipeline t(task) {
var result = 0
try { result = 42 } catch(e) { result = 0 }
log(result)
}"#,
        );
        assert_eq!(out, "[harn] 42");
    }

    #[test]
    fn test_throw_uncaught() {
        let result = run_harn_result(r#"pipeline t(task) { throw "boom" }"#);
        assert!(result.is_err());
    }

    // --- Additional test coverage ---

    fn run_vm(source: &str) -> String {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async {
                    let mut lexer = Lexer::new(source);
                    let tokens = lexer.tokenize().unwrap();
                    let mut parser = Parser::new(tokens);
                    let program = parser.parse().unwrap();
                    let chunk = Compiler::new().compile(&program).unwrap();
                    let mut vm = Vm::new();
                    register_vm_stdlib(&mut vm);
                    vm.execute(&chunk).await.unwrap();
                    vm.output().to_string()
                })
                .await
        })
    }

    fn run_vm_err(source: &str) -> String {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .unwrap();
        rt.block_on(async {
            let local = tokio::task::LocalSet::new();
            local
                .run_until(async {
                    let mut lexer = Lexer::new(source);
                    let tokens = lexer.tokenize().unwrap();
                    let mut parser = Parser::new(tokens);
                    let program = parser.parse().unwrap();
                    let chunk = Compiler::new().compile(&program).unwrap();
                    let mut vm = Vm::new();
                    register_vm_stdlib(&mut vm);
                    match vm.execute(&chunk).await {
                        Err(e) => format!("{}", e),
                        Ok(_) => panic!("Expected error"),
                    }
                })
                .await
        })
    }

    #[test]
    fn test_hello_world() {
        let out = run_vm(r#"pipeline default(task) { log("hello") }"#);
        assert_eq!(out, "[harn] hello\n");
    }

    #[test]
    fn test_arithmetic_new() {
        let out = run_vm("pipeline default(task) { log(2 + 3) }");
        assert_eq!(out, "[harn] 5\n");
    }

    #[test]
    fn test_string_concat_new() {
        let out = run_vm(r#"pipeline default(task) { log("a" + "b") }"#);
        assert_eq!(out, "[harn] ab\n");
    }

    #[test]
    fn test_if_else_new() {
        let out = run_vm("pipeline default(task) { if true { log(1) } else { log(2) } }");
        assert_eq!(out, "[harn] 1\n");
    }

    #[test]
    fn test_for_loop_new() {
        let out = run_vm("pipeline default(task) { for i in [1, 2, 3] { log(i) } }");
        assert_eq!(out, "[harn] 1\n[harn] 2\n[harn] 3\n");
    }

    #[test]
    fn test_while_loop_new() {
        let out = run_vm("pipeline default(task) { var i = 0\nwhile i < 3 { log(i)\ni = i + 1 } }");
        assert_eq!(out, "[harn] 0\n[harn] 1\n[harn] 2\n");
    }

    #[test]
    fn test_function_call_new() {
        let out =
            run_vm("pipeline default(task) { fn add(a, b) { return a + b }\nlog(add(2, 3)) }");
        assert_eq!(out, "[harn] 5\n");
    }

    #[test]
    fn test_closure_new() {
        let out = run_vm("pipeline default(task) { let f = { x -> x * 2 }\nlog(f(5)) }");
        assert_eq!(out, "[harn] 10\n");
    }

    #[test]
    fn test_recursion() {
        let out = run_vm("pipeline default(task) { fn fact(n) { if n <= 1 { return 1 }\nreturn n * fact(n - 1) }\nlog(fact(5)) }");
        assert_eq!(out, "[harn] 120\n");
    }

    #[test]
    fn test_try_catch_new() {
        let out = run_vm(r#"pipeline default(task) { try { throw "err" } catch (e) { log(e) } }"#);
        assert_eq!(out, "[harn] err\n");
    }

    #[test]
    fn test_try_no_error_new() {
        let out = run_vm("pipeline default(task) { try { log(1) } catch (e) { log(2) } }");
        assert_eq!(out, "[harn] 1\n");
    }

    #[test]
    fn test_list_map_new() {
        let out =
            run_vm("pipeline default(task) { let r = [1, 2, 3].map({ x -> x * 2 })\nlog(r) }");
        assert_eq!(out, "[harn] [2, 4, 6]\n");
    }

    #[test]
    fn test_list_filter_new() {
        let out = run_vm(
            "pipeline default(task) { let r = [1, 2, 3, 4].filter({ x -> x > 2 })\nlog(r) }",
        );
        assert_eq!(out, "[harn] [3, 4]\n");
    }

    #[test]
    fn test_dict_access_new() {
        let out = run_vm("pipeline default(task) { let d = {name: \"Alice\"}\nlog(d.name) }");
        assert_eq!(out, "[harn] Alice\n");
    }

    #[test]
    fn test_string_interpolation() {
        let out = run_vm("pipeline default(task) { let x = 42\nlog(\"val=${x}\") }");
        assert_eq!(out, "[harn] val=42\n");
    }

    #[test]
    fn test_match_new() {
        let out = run_vm(
            "pipeline default(task) { let x = \"b\"\nmatch x { \"a\" -> { log(1) } \"b\" -> { log(2) } } }",
        );
        assert_eq!(out, "[harn] 2\n");
    }

    #[test]
    fn test_json_roundtrip() {
        let out = run_vm("pipeline default(task) { let s = json_stringify({a: 1})\nlog(s) }");
        assert!(out.contains("\"a\""));
        assert!(out.contains("1"));
    }

    #[test]
    fn test_type_of() {
        let out = run_vm("pipeline default(task) { log(type_of(42))\nlog(type_of(\"hi\")) }");
        assert_eq!(out, "[harn] int\n[harn] string\n");
    }

    #[test]
    fn test_stack_overflow() {
        let err = run_vm_err("pipeline default(task) { fn f() { f() }\nf() }");
        assert!(
            err.contains("stack") || err.contains("overflow") || err.contains("recursion"),
            "Expected stack overflow error, got: {}",
            err
        );
    }

    #[test]
    fn test_division_by_zero() {
        let err = run_vm_err("pipeline default(task) { log(1 / 0) }");
        assert!(
            err.contains("Division by zero") || err.contains("division"),
            "Expected division by zero error, got: {}",
            err
        );
    }

    #[test]
    fn test_try_catch_nested() {
        let out = run_output(
            r#"pipeline t(task) {
try {
    try {
        throw "inner"
    } catch(e) {
        log("inner caught: " + e)
        throw "outer"
    }
} catch(e) {
    log("outer caught: " + e)
}
}"#,
        );
        assert_eq!(
            out,
            "[harn] inner caught: inner\n[harn] outer caught: outer"
        );
    }

    // --- Concurrency tests ---

    #[test]
    fn test_parallel_basic() {
        let out = run_output(
            "pipeline t(task) { let results = parallel(3) { i -> i * 10 }\nlog(results) }",
        );
        assert_eq!(out, "[harn] [0, 10, 20]");
    }

    #[test]
    fn test_parallel_no_variable() {
        let out = run_output("pipeline t(task) { let results = parallel(3) { 42 }\nlog(results) }");
        assert_eq!(out, "[harn] [42, 42, 42]");
    }

    #[test]
    fn test_parallel_map_basic() {
        let out = run_output(
            "pipeline t(task) { let results = parallel_map([1, 2, 3]) { x -> x * x }\nlog(results) }",
        );
        assert_eq!(out, "[harn] [1, 4, 9]");
    }

    #[test]
    fn test_spawn_await() {
        let out = run_output(
            r#"pipeline t(task) {
let handle = spawn { log("spawned") }
let result = await(handle)
log("done")
}"#,
        );
        assert_eq!(out, "[harn] spawned\n[harn] done");
    }

    #[test]
    fn test_spawn_cancel() {
        let out = run_output(
            r#"pipeline t(task) {
let handle = spawn { log("should be cancelled") }
cancel(handle)
log("cancelled")
}"#,
        );
        assert_eq!(out, "[harn] cancelled");
    }

    #[test]
    fn test_spawn_returns_value() {
        let out = run_output("pipeline t(task) { let h = spawn { 42 }\nlet r = await(h)\nlog(r) }");
        assert_eq!(out, "[harn] 42");
    }

    // --- Deadline tests ---

    #[test]
    fn test_deadline_success() {
        let out = run_output(
            r#"pipeline t(task) {
let result = deadline 5s { log("within deadline")
42 }
log(result)
}"#,
        );
        assert_eq!(out, "[harn] within deadline\n[harn] 42");
    }

    #[test]
    fn test_deadline_exceeded() {
        let result = run_harn_result(
            r#"pipeline t(task) {
deadline 1ms {
  var i = 0
  while i < 1000000 { i = i + 1 }
}
}"#,
        );
        assert!(result.is_err());
    }

    #[test]
    fn test_deadline_caught_by_try() {
        let out = run_output(
            r#"pipeline t(task) {
try {
  deadline 1ms {
    var i = 0
    while i < 1000000 { i = i + 1 }
  }
} catch(e) {
  log("caught")
}
}"#,
        );
        assert_eq!(out, "[harn] caught");
    }
}
