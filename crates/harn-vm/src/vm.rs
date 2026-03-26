use std::collections::BTreeMap;
use std::io::BufRead;

use crate::chunk::{Chunk, CompiledFunction, Constant, Op};

/// VM runtime value.
#[derive(Debug, Clone)]
pub enum VmValue {
    Int(i64),
    Float(f64),
    String(String),
    Bool(bool),
    Nil,
    List(Vec<VmValue>),
    Dict(BTreeMap<String, VmValue>),
    Closure(VmClosure),
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

    #[allow(dead_code)]
    fn pop_scope(&mut self) {
        if self.scopes.len() > 1 {
            self.scopes.pop();
        }
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
    UndefinedVariable(String),
    UndefinedBuiltin(String),
    ImmutableAssignment(String),
    TypeError(String),
    DivisionByZero,
    Thrown(VmValue),
    Return(VmValue),
    InvalidInstruction(u8),
}

impl std::fmt::Display for VmError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            VmError::StackUnderflow => write!(f, "Stack underflow"),
            VmError::UndefinedVariable(n) => write!(f, "Undefined variable: {n}"),
            VmError::UndefinedBuiltin(n) => write!(f, "Undefined builtin: {n}"),
            VmError::ImmutableAssignment(n) => {
                write!(f, "Cannot assign to immutable binding: {n}")
            }
            VmError::TypeError(msg) => write!(f, "Type error: {msg}"),
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
            VmValue::StructInstance { fields, .. } => !fields.is_empty(),
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
            VmValue::String(s) => s.clone(),
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
pub type VmBuiltinFn = Box<dyn Fn(&[VmValue], &mut String) -> Result<VmValue, VmError>>;

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

/// The Harn bytecode virtual machine.
pub struct Vm {
    stack: Vec<VmValue>,
    env: VmEnv,
    output: String,
    builtins: BTreeMap<String, VmBuiltinFn>,
    /// Iterator state for for-in loops.
    iterators: Vec<(Vec<VmValue>, usize)>,
    /// Call frame stack.
    frames: Vec<CallFrame>,
    /// Exception handler stack.
    exception_handlers: Vec<ExceptionHandler>,
}

impl Vm {
    pub fn new() -> Self {
        Self {
            stack: Vec::with_capacity(256),
            env: VmEnv::new(),
            output: String::new(),
            builtins: BTreeMap::new(),
            iterators: Vec::new(),
            frames: Vec::new(),
            exception_handlers: Vec::new(),
        }
    }

    /// Register a builtin function.
    pub fn register_builtin<F>(&mut self, name: &str, f: F)
    where
        F: Fn(&[VmValue], &mut String) -> Result<VmValue, VmError> + 'static,
    {
        self.builtins.insert(name.to_string(), Box::new(f));
    }

    /// Get the captured output.
    pub fn output(&self) -> &str {
        &self.output
    }

    /// Execute a compiled chunk.
    pub fn execute(&mut self, chunk: &Chunk) -> Result<VmValue, VmError> {
        self.run_chunk(chunk)
    }

    /// Convert a VmError into either a handled exception (returning Ok) or a propagated error.
    fn handle_error(&mut self, error: VmError) -> Result<Option<VmValue>, VmError> {
        // Extract the thrown value from the error
        let thrown_value = match &error {
            VmError::Thrown(v) => v.clone(),
            other => VmValue::String(other.to_string()),
        };

        if let Some(handler) = self.exception_handlers.pop() {
            // Unwind call frames back to the handler's frame depth
            while self.frames.len() > handler.frame_depth {
                if let Some(frame) = self.frames.pop() {
                    self.env = frame.saved_env;
                }
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

    fn run_chunk(&mut self, chunk: &Chunk) -> Result<VmValue, VmError> {
        // Push initial frame
        self.frames.push(CallFrame {
            chunk: chunk.clone(),
            ip: 0,
            stack_base: self.stack.len(),
            saved_env: self.env.clone(),
        });

        loop {
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

            match self.execute_op(op) {
                Ok(Some(val)) => return Ok(val),
                Ok(None) => continue,
                Err(VmError::Return(val)) => {
                    // Pop the current frame
                    if let Some(popped_frame) = self.frames.pop() {
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
    fn execute_op(&mut self, op: u8) -> Result<Option<VmValue>, VmError> {
        // We need to borrow frame fields, but we also need &mut self for other ops.
        // Strategy: read what we need from the frame first, then do the work.

        if op == Op::Constant as u8 {
            let frame = self.frames.last_mut().unwrap();
            let idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let val = match &frame.chunk.constants[idx] {
                Constant::Int(n) => VmValue::Int(*n),
                Constant::Float(n) => VmValue::Float(*n),
                Constant::String(s) => VmValue::String(s.clone()),
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
            self.stack.push(self.div(a, b));
        } else if op == Op::Negate as u8 {
            let v = self.pop()?;
            self.stack.push(match v {
                VmValue::Int(n) => VmValue::Int(-n),
                VmValue::Float(n) => VmValue::Float(-n),
                _ => VmValue::Nil,
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
                    // Check closures in env
                    if let Some(VmValue::Closure(closure)) = self.env.get(&name) {
                        self.push_closure_frame(&closure, &args, &functions)?;
                        // Don't push result - frame will handle it on return
                    } else if let Some(builtin) = self.builtins.get(&name) {
                        let result = builtin(&args, &mut self.output)?;
                        self.stack.push(result);
                    } else {
                        return Err(VmError::UndefinedBuiltin(name));
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
            self.stack.push(VmValue::Closure(closure));
        } else if op == Op::BuildList as u8 {
            let frame = self.frames.last_mut().unwrap();
            let count = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let items = self.stack.split_off(self.stack.len().saturating_sub(count));
            self.stack.push(VmValue::List(items));
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
            self.stack.push(VmValue::Dict(map));
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
            let result = self.call_method(obj, &method, &args, &functions)?;
            self.stack.push(result);
        } else if op == Op::Concat as u8 {
            let frame = self.frames.last_mut().unwrap();
            let count = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let parts = self.stack.split_off(self.stack.len().saturating_sub(count));
            let result: String = parts.iter().map(|p| p.display()).collect();
            self.stack.push(VmValue::String(result));
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
                    } else if let Some(builtin) = self.builtins.get(&name) {
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
                VmValue::List(items) => items,
                VmValue::Dict(map) => map
                    .into_iter()
                    .map(|(k, v)| {
                        VmValue::Dict(BTreeMap::from([
                            ("key".to_string(), VmValue::String(k)),
                            ("value".to_string(), v),
                        ]))
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
        } else {
            return Err(VmError::InvalidInstruction(op));
        }

        Ok(None)
    }

    /// Push a new call frame for a closure invocation.
    fn push_closure_frame(
        &mut self,
        closure: &VmClosure,
        args: &[VmValue],
        _parent_functions: &[CompiledFunction],
    ) -> Result<(), VmError> {
        let saved_env = self.env.clone();

        // Start with the closure's captured env, but also include
        // the caller's definitions (for recursion and outer scope access).
        let mut call_env = closure.env.clone();
        for scope in &saved_env.scopes {
            for (name, (val, mutable)) in &scope.vars {
                if call_env.get(name).is_none() {
                    call_env.define(name, val.clone(), *mutable);
                }
            }
        }
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

    /// Call a closure synchronously (used by method calls like .map/.filter etc.)
    /// This still uses recursive execution for simplicity in method dispatch.
    fn call_closure_sync(
        &mut self,
        closure: &VmClosure,
        args: &[VmValue],
        _parent_functions: &[CompiledFunction],
    ) -> Result<VmValue, VmError> {
        let saved_env = self.env.clone();
        let saved_frames = std::mem::take(&mut self.frames);
        let saved_handlers = std::mem::take(&mut self.exception_handlers);

        let mut call_env = closure.env.clone();
        for scope in &saved_env.scopes {
            for (name, (val, mutable)) in &scope.vars {
                if call_env.get(name).is_none() {
                    call_env.define(name, val.clone(), *mutable);
                }
            }
        }
        call_env.push_scope();

        for (i, param) in closure.func.params.iter().enumerate() {
            let val = args.get(i).cloned().unwrap_or(VmValue::Nil);
            call_env.define(param, val, false);
        }

        self.env = call_env;
        let result = self.run_chunk(&closure.func.chunk);

        self.env = saved_env;
        self.frames = saved_frames;
        self.exception_handlers = saved_handlers;

        result
    }

    fn call_method(
        &mut self,
        obj: VmValue,
        method: &str,
        args: &[VmValue],
        functions: &[CompiledFunction],
    ) -> Result<VmValue, VmError> {
        match &obj {
            VmValue::String(s) => match method {
                "count" => Ok(VmValue::Int(s.chars().count() as i64)),
                "empty" => Ok(VmValue::Bool(s.is_empty())),
                "contains" => Ok(VmValue::Bool(
                    s.contains(&args.first().map(|a| a.display()).unwrap_or_default()),
                )),
                "replace" if args.len() >= 2 => Ok(VmValue::String(
                    s.replace(&args[0].display(), &args[1].display()),
                )),
                "split" => {
                    let sep = args.first().map(|a| a.display()).unwrap_or(",".into());
                    Ok(VmValue::List(
                        s.split(&sep)
                            .map(|p| VmValue::String(p.to_string()))
                            .collect(),
                    ))
                }
                "trim" => Ok(VmValue::String(s.trim().to_string())),
                "starts_with" => Ok(VmValue::Bool(
                    s.starts_with(&args.first().map(|a| a.display()).unwrap_or_default()),
                )),
                "ends_with" => Ok(VmValue::Bool(
                    s.ends_with(&args.first().map(|a| a.display()).unwrap_or_default()),
                )),
                "lowercase" => Ok(VmValue::String(s.to_lowercase())),
                "uppercase" => Ok(VmValue::String(s.to_uppercase())),
                "substring" => {
                    let start = args.first().and_then(|a| a.as_int()).unwrap_or(0);
                    let len = s.chars().count() as i64;
                    let start = start.max(0).min(len) as usize;
                    let end = args.get(1).and_then(|a| a.as_int()).unwrap_or(len).min(len) as usize;
                    let end = end.max(start);
                    Ok(VmValue::String(
                        s.chars().skip(start).take(end - start).collect(),
                    ))
                }
                _ => Ok(VmValue::Nil),
            },
            VmValue::List(items) => match method {
                "count" => Ok(VmValue::Int(items.len() as i64)),
                "empty" => Ok(VmValue::Bool(items.is_empty())),
                "map" => {
                    if let Some(VmValue::Closure(closure)) = args.first() {
                        let mut results = Vec::new();
                        for item in items {
                            results.push(self.call_closure_sync(
                                closure,
                                &[item.clone()],
                                functions,
                            )?);
                        }
                        Ok(VmValue::List(results))
                    } else {
                        Ok(VmValue::Nil)
                    }
                }
                "filter" => {
                    if let Some(VmValue::Closure(closure)) = args.first() {
                        let mut results = Vec::new();
                        for item in items {
                            let result =
                                self.call_closure_sync(closure, &[item.clone()], functions)?;
                            if result.is_truthy() {
                                results.push(item.clone());
                            }
                        }
                        Ok(VmValue::List(results))
                    } else {
                        Ok(VmValue::Nil)
                    }
                }
                "reduce" => {
                    if args.len() >= 2 {
                        if let VmValue::Closure(closure) = &args[1] {
                            let mut acc = args[0].clone();
                            for item in items {
                                acc = self.call_closure_sync(
                                    closure,
                                    &[acc, item.clone()],
                                    functions,
                                )?;
                            }
                            return Ok(acc);
                        }
                    }
                    Ok(VmValue::Nil)
                }
                "find" => {
                    if let Some(VmValue::Closure(closure)) = args.first() {
                        for item in items {
                            let result =
                                self.call_closure_sync(closure, &[item.clone()], functions)?;
                            if result.is_truthy() {
                                return Ok(item.clone());
                            }
                        }
                    }
                    Ok(VmValue::Nil)
                }
                "any" => {
                    if let Some(VmValue::Closure(closure)) = args.first() {
                        for item in items {
                            let result =
                                self.call_closure_sync(closure, &[item.clone()], functions)?;
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
                        for item in items {
                            let result =
                                self.call_closure_sync(closure, &[item.clone()], functions)?;
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
                        for item in items {
                            let result =
                                self.call_closure_sync(closure, &[item.clone()], functions)?;
                            if let VmValue::List(inner) = result {
                                results.extend(inner);
                            } else {
                                results.push(result);
                            }
                        }
                        Ok(VmValue::List(results))
                    } else {
                        Ok(VmValue::Nil)
                    }
                }
                _ => Ok(VmValue::Nil),
            },
            VmValue::Dict(map) => match method {
                "keys" => Ok(VmValue::List(
                    map.keys().map(|k| VmValue::String(k.clone())).collect(),
                )),
                "values" => Ok(VmValue::List(map.values().cloned().collect())),
                "entries" => Ok(VmValue::List(
                    map.iter()
                        .map(|(k, v)| {
                            VmValue::Dict(BTreeMap::from([
                                ("key".to_string(), VmValue::String(k.clone())),
                                ("value".to_string(), v.clone()),
                            ]))
                        })
                        .collect(),
                )),
                "count" => Ok(VmValue::Int(map.len() as i64)),
                "has" => Ok(VmValue::Bool(map.contains_key(
                    &args.first().map(|a| a.display()).unwrap_or_default(),
                ))),
                "merge" => {
                    if let Some(VmValue::Dict(other)) = args.first() {
                        let mut result = map.clone();
                        result.extend(other.iter().map(|(k, v)| (k.clone(), v.clone())));
                        Ok(VmValue::Dict(result))
                    } else {
                        Ok(VmValue::Dict(map.clone()))
                    }
                }
                _ => Ok(VmValue::Nil),
            },
            _ => Ok(VmValue::Nil),
        }
    }

    // --- Arithmetic helpers ---

    fn add(&self, a: VmValue, b: VmValue) -> VmValue {
        match (&a, &b) {
            (VmValue::Int(x), VmValue::Int(y)) => VmValue::Int(x.wrapping_add(*y)),
            (VmValue::Float(x), VmValue::Float(y)) => VmValue::Float(x + y),
            (VmValue::Int(x), VmValue::Float(y)) => VmValue::Float(*x as f64 + y),
            (VmValue::Float(x), VmValue::Int(y)) => VmValue::Float(x + *y as f64),
            (VmValue::String(x), _) => VmValue::String(format!("{x}{}", b.display())),
            (VmValue::List(x), VmValue::List(y)) => {
                let mut result = x.clone();
                result.extend(y.iter().cloned());
                VmValue::List(result)
            }
            _ => VmValue::String(format!("{}{}", a.display(), b.display())),
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

    fn div(&self, a: VmValue, b: VmValue) -> VmValue {
        match (&a, &b) {
            (VmValue::Int(_), VmValue::Int(y)) if *y == 0 => VmValue::Nil,
            (VmValue::Int(x), VmValue::Int(y)) => VmValue::Int(x / y),
            (VmValue::Float(_), VmValue::Float(y)) if *y == 0.0 => VmValue::Nil,
            (VmValue::Float(x), VmValue::Float(y)) => VmValue::Float(x / y),
            (VmValue::Int(x), VmValue::Float(y)) if *y != 0.0 => VmValue::Float(*x as f64 / y),
            (VmValue::Float(x), VmValue::Int(y)) if *y != 0 => VmValue::Float(x / *y as f64),
            _ => VmValue::Nil,
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
        Ok(VmValue::String(val.type_name().to_string()))
    });
    vm.register_builtin("to_string", |args, _out| {
        let val = args.first().unwrap_or(&VmValue::Nil);
        Ok(VmValue::String(val.display()))
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
        Ok(VmValue::String(vm_value_to_json(val)))
    });

    vm.register_builtin("json_parse", |args, _out| {
        let text = args.first().map(|a| a.display()).unwrap_or_default();
        match serde_json::from_str::<serde_json::Value>(&text) {
            Ok(jv) => Ok(json_to_vm_value(&jv)),
            Err(e) => Err(VmError::Thrown(VmValue::String(format!(
                "JSON parse error: {e}"
            )))),
        }
    });

    vm.register_builtin("env", |args, _out| {
        let name = args.first().map(|a| a.display()).unwrap_or_default();
        match std::env::var(&name) {
            Ok(val) => Ok(VmValue::String(val)),
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
            Ok(content) => Ok(VmValue::String(content)),
            Err(e) => Err(VmError::Thrown(VmValue::String(format!(
                "Failed to read file {path}: {e}"
            )))),
        }
    });

    vm.register_builtin("write_file", |args, _out| {
        if args.len() >= 2 {
            let path = args[0].display();
            let content = args[1].display();
            std::fs::write(&path, &content).map_err(|e| {
                VmError::Thrown(VmValue::String(format!("Failed to write file {path}: {e}")))
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
            let re = regex::Regex::new(&pattern)
                .map_err(|e| VmError::Thrown(VmValue::String(format!("Invalid regex: {e}"))))?;
            let matches: Vec<VmValue> = re
                .find_iter(&text)
                .map(|m| VmValue::String(m.as_str().to_string()))
                .collect();
            if matches.is_empty() {
                return Ok(VmValue::Nil);
            }
            return Ok(VmValue::List(matches));
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("regex_replace", |args, _out| {
        if args.len() >= 3 {
            let pattern = args[0].display();
            let replacement = args[1].display();
            let text = args[2].display();
            let re = regex::Regex::new(&pattern)
                .map_err(|e| VmError::Thrown(VmValue::String(format!("Invalid regex: {e}"))))?;
            return Ok(VmValue::String(
                re.replace_all(&text, replacement.as_str()).into_owned(),
            ));
        }
        Ok(VmValue::Nil)
    });

    vm.register_builtin("prompt_user", |args, out| {
        let msg = args.first().map(|a| a.display()).unwrap_or_default();
        out.push_str(&msg);
        let mut input = String::new();
        if std::io::stdin().lock().read_line(&mut input).is_ok() {
            Ok(VmValue::String(input.trim_end().to_string()))
        } else {
            Ok(VmValue::Nil)
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
        Ok(VmValue::List(items))
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
        serde_json::Value::String(s) => VmValue::String(s.clone()),
        serde_json::Value::Array(arr) => VmValue::List(arr.iter().map(json_to_vm_value).collect()),
        serde_json::Value::Object(map) => {
            let mut m = BTreeMap::new();
            for (k, v) in map {
                m.insert(k.clone(), json_to_vm_value(v));
            }
            VmValue::Dict(m)
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
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let program = parser.parse().unwrap();
        let chunk = Compiler::new().compile(&program).unwrap();

        let mut vm = Vm::new();
        register_vm_stdlib(&mut vm);
        let result = vm.execute(&chunk).unwrap();
        (vm.output().to_string(), result)
    }

    fn run_output(source: &str) -> String {
        run_harn(source).0.trim_end().to_string()
    }

    fn run_harn_result(source: &str) -> Result<(String, VmValue), VmError> {
        let mut lexer = Lexer::new(source);
        let tokens = lexer.tokenize().unwrap();
        let mut parser = Parser::new(tokens);
        let program = parser.parse().unwrap();
        let chunk = Compiler::new().compile(&program).unwrap();

        let mut vm = Vm::new();
        register_vm_stdlib(&mut vm);
        let result = vm.execute(&chunk)?;
        Ok((vm.output().to_string(), result))
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
}
