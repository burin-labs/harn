use std::collections::BTreeMap;
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Instant;

use crate::chunk::{Chunk, CompiledFunction, Constant, Op};
use crate::value::{
    compare_values, values_equal, VmAsyncBuiltinFn, VmBuiltinFn, VmClosure, VmEnv, VmError,
    VmTaskHandle, VmValue,
};

/// Call frame for function execution.
struct CallFrame {
    chunk: Chunk,
    ip: usize,
    stack_base: usize,
    saved_env: VmEnv,
    /// Function name for stack traces (empty for top-level pipeline).
    fn_name: String,
}

/// Exception handler for try/catch.
struct ExceptionHandler {
    catch_ip: usize,
    stack_depth: usize,
    frame_depth: usize,
    /// If non-empty, this catch only handles errors whose enum_name matches.
    error_type: String,
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
    /// Source directory for resolving imports.
    source_dir: Option<std::path::PathBuf>,
    /// Already-imported file paths (cycle prevention).
    imported_paths: Vec<std::path::PathBuf>,
    /// Source file path for error reporting.
    source_file: Option<String>,
    /// Source text for error reporting.
    source_text: Option<String>,
    /// Optional bridge for delegating unknown builtins in bridge mode.
    bridge: Option<Rc<crate::bridge::HostBridge>>,
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
            source_dir: None,
            imported_paths: Vec::new(),
            source_file: None,
            source_text: None,
            bridge: None,
        }
    }

    /// Set the bridge for delegating unknown builtins in bridge mode.
    pub fn set_bridge(&mut self, bridge: Rc<crate::bridge::HostBridge>) {
        self.bridge = Some(bridge);
    }

    /// Set source info for error reporting (file path and source text).
    pub fn set_source_info(&mut self, file: &str, text: &str) {
        self.source_file = Some(file.to_string());
        self.source_text = Some(text.to_string());
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
            let name = if frame.fn_name.is_empty() {
                if i == 0 {
                    "pipeline".to_string()
                } else {
                    format!("fn_{}", i)
                }
            } else {
                frame.fn_name.clone()
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
            fn_name: String::new(),
        });
    }

    /// Register a sync builtin function.
    pub fn register_builtin<F>(&mut self, name: &str, f: F)
    where
        F: Fn(&[VmValue], &mut String) -> Result<VmValue, VmError> + 'static,
    {
        self.builtins.insert(name.to_string(), Rc::new(f));
    }

    /// Remove a sync builtin (so an async version can take precedence).
    pub fn unregister_builtin(&mut self, name: &str) {
        self.builtins.remove(name);
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
            source_dir: None,
            imported_paths: Vec::new(),
            source_file: self.source_file.clone(),
            source_text: self.source_text.clone(),
            bridge: self.bridge.clone(),
        }
    }

    /// Set the source directory for import resolution.
    pub fn set_source_dir(&mut self, dir: &std::path::Path) {
        self.source_dir = Some(dir.to_path_buf());
    }

    /// Set a global variable in the VM's environment.
    pub fn set_global(&mut self, name: &str, value: VmValue) {
        self.env.define(name, value, false);
    }

    /// Execute an import, reading and running the file's declarations.
    fn execute_import<'a>(
        &'a mut self,
        path: &'a str,
        selected_names: Option<&'a [String]>,
    ) -> Pin<Box<dyn Future<Output = Result<(), VmError>> + 'a>> {
        Box::pin(async move {
            use std::path::PathBuf;

            // Resolve the file path
            let base = self
                .source_dir
                .clone()
                .unwrap_or_else(|| PathBuf::from("."));
            let mut file_path = base.join(path);

            // Try with .harn extension if no extension
            if !file_path.exists() && file_path.extension().is_none() {
                file_path.set_extension("harn");
            }

            // Try .harn/packages/ fallback (then .burin/packages/ for compat)
            if !file_path.exists() {
                for pkg_dir in [".harn/packages", ".burin/packages"] {
                    let pkg_path = base.join(pkg_dir).join(path);
                    if pkg_path.exists() {
                        file_path = if pkg_path.is_dir() {
                            let lib = pkg_path.join("lib.harn");
                            if lib.exists() {
                                lib
                            } else {
                                pkg_path
                            }
                        } else {
                            pkg_path
                        };
                        break;
                    }
                    let mut pkg_harn = pkg_path.clone();
                    pkg_harn.set_extension("harn");
                    if pkg_harn.exists() {
                        file_path = pkg_harn;
                        break;
                    }
                }
            }

            // Cycle detection
            let canonical = file_path
                .canonicalize()
                .unwrap_or_else(|_| file_path.clone());
            if self.imported_paths.contains(&canonical) {
                return Ok(()); // already imported
            }
            self.imported_paths.push(canonical);

            // Read, lex, parse
            let source = std::fs::read_to_string(&file_path).map_err(|e| {
                VmError::Runtime(format!(
                    "Import error: cannot read '{}': {e}",
                    file_path.display()
                ))
            })?;

            let mut lexer = harn_lexer::Lexer::new(&source);
            let tokens = lexer
                .tokenize()
                .map_err(|e| VmError::Runtime(format!("Import lex error: {e}")))?;
            let mut parser = harn_parser::Parser::new(tokens);
            let program = parser
                .parse()
                .map_err(|e| VmError::Runtime(format!("Import parse error: {e}")))?;

            // Check if the module has any pub fn declarations
            let has_pub = program
                .iter()
                .any(|n| matches!(&n.node, harn_parser::Node::FnDecl { is_pub: true, .. }));

            // Execute top-level declarations from the imported file
            for node in &program {
                match &node.node {
                    harn_parser::Node::FnDecl {
                        name,
                        params,
                        body,
                        is_pub,
                        ..
                    } => {
                        // For selective imports: import any function that was explicitly named
                        // For wildcard imports: if module has pub fns, only import pub ones;
                        //   if no pub fns, import everything (backward compat)
                        if selected_names.is_none() && has_pub && !is_pub {
                            continue;
                        }
                        if let Some(names) = selected_names {
                            if !names.contains(name) {
                                continue;
                            }
                        }
                        // Compile the function body into a closure and define it
                        let mut compiler = crate::Compiler::new();
                        let func_chunk = compiler
                            .compile_fn_body(params, body)
                            .map_err(|e| VmError::Runtime(format!("Import compile error: {e}")))?;
                        let closure = VmClosure {
                            func: func_chunk,
                            env: self.env.clone(),
                        };
                        self.env
                            .define(name, VmValue::Closure(Rc::new(closure)), false);
                    }
                    harn_parser::Node::ImportDecl { path: sub_path } => {
                        // Handle nested imports - set source_dir relative to the imported file
                        let old_dir = self.source_dir.clone();
                        if let Some(parent) = file_path.parent() {
                            self.source_dir = Some(parent.to_path_buf());
                        }
                        self.execute_import(sub_path, None).await?;
                        self.source_dir = old_dir;
                    }
                    harn_parser::Node::SelectiveImport {
                        names,
                        path: sub_path,
                    } => {
                        let old_dir = self.source_dir.clone();
                        if let Some(parent) = file_path.parent() {
                            self.source_dir = Some(parent.to_path_buf());
                        }
                        self.execute_import(sub_path, Some(names)).await?;
                        self.source_dir = old_dir;
                    }
                    _ => {} // Skip other top-level nodes (pipelines, enums, etc.)
                }
            }

            Ok(())
        })
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
            // Check if this is a typed catch that doesn't match the thrown value
            if !handler.error_type.is_empty() {
                let matches = match &thrown_value {
                    VmValue::EnumVariant { enum_name, .. } => *enum_name == handler.error_type,
                    _ => false,
                };
                if !matches {
                    // This handler doesn't match — try the next one
                    return self.handle_error(error);
                }
            }

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
            fn_name: String::new(),
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
                Constant::Duration(ms) => VmValue::Duration(*ms),
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
            match self.env.get(&name) {
                Some(val) => self.stack.push(val),
                None => return Err(VmError::UndefinedVariable(name)),
            }
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
                    } else if let Some(bridge) = &self.bridge {
                        // Bridge mode: delegate unknown builtins to the host
                        let args_json: Vec<serde_json::Value> =
                            args.iter().map(crate::llm::vm_value_to_json).collect();
                        let result = bridge
                            .call(
                                "builtin_call",
                                serde_json::json!({"name": name.as_ref(), "args": args_json}),
                            )
                            .await?;
                        self.stack
                            .push(crate::bridge::json_result_to_vm_value(&result));
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
        } else if op == Op::TailCall as u8 {
            let frame = self.frames.last_mut().unwrap();
            let argc = frame.chunk.code[frame.ip] as usize;
            frame.ip += 1;

            let args: Vec<VmValue> = self.stack.split_off(self.stack.len().saturating_sub(argc));
            let callee = self.pop()?;

            // Resolve the callee to a closure (or fall through to builtin)
            let resolved_closure = match &callee {
                VmValue::Closure(cl) => Some(Rc::clone(cl)),
                VmValue::String(name) => {
                    if let Some(VmValue::Closure(cl)) = self.env.get(name) {
                        Some(cl)
                    } else {
                        None
                    }
                }
                _ => None,
            };

            if let Some(closure) = resolved_closure {
                // Tail call optimization: replace current frame instead of pushing.
                // Pop the current frame and reuse its stack_base and saved_env.
                let popped = self.frames.pop().unwrap();
                let stack_base = popped.stack_base;
                let parent_env = popped.saved_env;

                // Clear this frame's stack data
                self.stack.truncate(stack_base);

                // Set up the callee's environment
                let mut call_env = Self::merge_env_into_closure(&parent_env, &closure);
                call_env.push_scope();
                for (i, param) in closure.func.params.iter().enumerate() {
                    let val = args.get(i).cloned().unwrap_or(VmValue::Nil);
                    call_env.define(param, val, false);
                }
                self.env = call_env;

                // Push replacement frame at the same stack depth
                self.frames.push(CallFrame {
                    chunk: closure.func.chunk.clone(),
                    ip: 0,
                    stack_base,
                    saved_env: parent_env,
                    fn_name: closure.func.name.clone(),
                });
                // Continue the loop — execution proceeds in the new frame
            } else {
                // Not a closure — fall back to regular call behavior for builtins.
                match callee {
                    VmValue::String(name) => {
                        if let Some(builtin) = self.builtins.get(name.as_ref()).cloned() {
                            let result = builtin(&args, &mut self.output)?;
                            self.stack.push(result);
                        } else if let Some(async_builtin) =
                            self.async_builtins.get(name.as_ref()).cloned()
                        {
                            let result = async_builtin(args).await?;
                            self.stack.push(result);
                        } else if let Some(bridge) = &self.bridge {
                            let args_json: Vec<serde_json::Value> =
                                args.iter().map(crate::llm::vm_value_to_json).collect();
                            let result = bridge
                                .call(
                                    "builtin_call",
                                    serde_json::json!({"name": name.as_ref(), "args": args_json}),
                                )
                                .await?;
                            self.stack
                                .push(crate::bridge::json_result_to_vm_value(&result));
                        } else {
                            return Err(VmError::UndefinedBuiltin(name.to_string()));
                        }
                    }
                    _ => {
                        return Err(VmError::TypeError(format!(
                            "Cannot call {}",
                            callee.display()
                        )));
                    }
                }
                // Result is on stack; the following Return opcode will return it.
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
                (VmValue::List(items), VmValue::Int(i)) => {
                    if *i < 0 {
                        let pos = items.len() as i64 + *i;
                        if pos < 0 {
                            VmValue::Nil
                        } else {
                            items.get(pos as usize).cloned().unwrap_or(VmValue::Nil)
                        }
                    } else {
                        items.get(*i as usize).cloned().unwrap_or(VmValue::Nil)
                    }
                }
                (VmValue::Dict(map), _) => map.get(&idx.display()).cloned().unwrap_or(VmValue::Nil),
                (VmValue::String(s), VmValue::Int(i)) => {
                    if *i < 0 {
                        let pos = s.chars().count() as i64 + *i;
                        if pos < 0 {
                            VmValue::Nil
                        } else {
                            s.chars()
                                .nth(pos as usize)
                                .map(|c| VmValue::String(Rc::from(c.to_string())))
                                .unwrap_or(VmValue::Nil)
                        }
                    } else {
                        s.chars()
                            .nth(*i as usize)
                            .map(|c| VmValue::String(Rc::from(c.to_string())))
                            .unwrap_or(VmValue::Nil)
                    }
                }
                _ => {
                    return Err(VmError::TypeError(format!(
                        "cannot index into {} with {}",
                        obj.type_name(),
                        idx.type_name()
                    )));
                }
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
                VmValue::EnumVariant {
                    variant, fields, ..
                } => match name.as_str() {
                    "variant" => VmValue::String(Rc::from(variant.as_str())),
                    "fields" => VmValue::List(Rc::new(fields.clone())),
                    _ => VmValue::Nil,
                },
                VmValue::StructInstance { fields, .. } => {
                    fields.get(&name).cloned().unwrap_or(VmValue::Nil)
                }
                VmValue::Nil => {
                    return Err(VmError::TypeError(format!(
                        "cannot access property `{name}` on nil"
                    )));
                }
                _ => {
                    return Err(VmError::TypeError(format!(
                        "cannot access property `{name}` on {}",
                        obj.type_name()
                    )));
                }
            };
            self.stack.push(result);
        } else if op == Op::GetPropertyOpt as u8 {
            // Optional chaining: obj?.property — returns nil if obj is nil
            let frame = self.frames.last_mut().unwrap();
            let idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let name = Self::const_string(&frame.chunk.constants[idx])?;
            let obj = self.pop()?;
            let result = match &obj {
                VmValue::Nil => VmValue::Nil,
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
                VmValue::EnumVariant {
                    variant, fields, ..
                } => match name.as_str() {
                    "variant" => VmValue::String(Rc::from(variant.as_str())),
                    "fields" => VmValue::List(Rc::new(fields.clone())),
                    _ => VmValue::Nil,
                },
                VmValue::StructInstance { fields, .. } => {
                    fields.get(&name).cloned().unwrap_or(VmValue::Nil)
                }
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
                    _ => {
                        return Err(VmError::TypeError(format!(
                            "cannot set property `{prop_name}` on {}",
                            obj.type_name()
                        )));
                    }
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
                            } else {
                                return Err(VmError::Runtime(format!(
                                    "Index {} out of bounds for list of length {}",
                                    i,
                                    items.len()
                                )));
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
                    } else if let Some(async_builtin) =
                        self.async_builtins.get(name.as_ref()).cloned()
                    {
                        let result = async_builtin(vec![value]).await?;
                        self.stack.push(result);
                    } else if let Some(bridge) = &self.bridge {
                        let args_json = vec![crate::llm::vm_value_to_json(&value)];
                        let result = bridge
                            .call(
                                "builtin_call",
                                serde_json::json!({"name": name.as_ref(), "args": args_json}),
                            )
                            .await?;
                        self.stack
                            .push(crate::bridge::json_result_to_vm_value(&result));
                    } else {
                        return Err(VmError::UndefinedBuiltin(name.to_string()));
                    }
                }
                _ => {
                    return Err(VmError::TypeError(format!(
                        "cannot pipe into {}",
                        callable.type_name()
                    )));
                }
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
        } else if op == Op::PopIterator as u8 {
            self.iterators.pop();
        } else if op == Op::Throw as u8 {
            let val = self.pop()?;
            return Err(VmError::Thrown(val));
        } else if op == Op::TryCatchSetup as u8 {
            let frame = self.frames.last_mut().unwrap();
            let catch_offset = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            // Read the error type name index (extra u16)
            let type_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let error_type = match &frame.chunk.constants[type_idx] {
                Constant::String(s) => s.clone(),
                _ => String::new(),
            };
            self.exception_handlers.push(ExceptionHandler {
                catch_ip: catch_offset,
                stack_depth: self.stack.len(),
                frame_depth: self.frames.len(),
                error_type,
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
        } else if op == Op::Import as u8 {
            let frame = self.frames.last_mut().unwrap();
            let path_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let import_path = Self::const_string(&frame.chunk.constants[path_idx])?;
            self.execute_import(&import_path, None).await?;
        } else if op == Op::SelectiveImport as u8 {
            let frame = self.frames.last_mut().unwrap();
            let path_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let names_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let import_path = Self::const_string(&frame.chunk.constants[path_idx])?;
            let names_str = Self::const_string(&frame.chunk.constants[names_idx])?;
            let names: Vec<String> = names_str.split(',').map(|s| s.to_string()).collect();
            self.execute_import(&import_path, Some(&names)).await?;
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
        } else if op == Op::BuildEnum as u8 {
            let frame = self.frames.last_mut().unwrap();
            let enum_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let variant_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let field_count = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let enum_name = Self::const_string(&frame.chunk.constants[enum_idx])?;
            let variant = Self::const_string(&frame.chunk.constants[variant_idx])?;
            let fields = self
                .stack
                .split_off(self.stack.len().saturating_sub(field_count));
            self.stack.push(VmValue::EnumVariant {
                enum_name,
                variant,
                fields,
            });
        } else if op == Op::MatchEnum as u8 {
            let frame = self.frames.last_mut().unwrap();
            let enum_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let variant_idx = frame.chunk.read_u16(frame.ip) as usize;
            frame.ip += 2;
            let enum_name = Self::const_string(&frame.chunk.constants[enum_idx])?;
            let variant_name = Self::const_string(&frame.chunk.constants[variant_idx])?;
            let val = self.pop()?;
            let matches = match &val {
                VmValue::EnumVariant {
                    enum_name: en,
                    variant: vn,
                    ..
                } => *en == enum_name && *vn == variant_name,
                _ => false,
            };
            // Push the value back (we only peeked conceptually), then push the bool
            self.stack.push(val);
            self.stack.push(VmValue::Bool(matches));
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
            fn_name: closure.func.name.clone(),
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

    /// Format a runtime error with stack trace and source context.
    ///
    /// Produces rustc-style output with source context and caret highlighting:
    /// ```text
    /// error: Undefined variable: x
    ///   --> file.harn:5:12
    ///    |
    ///  5 |     let y = x + 1
    ///    |             ^
    ///    = note: called from greet at file.harn:2:1
    /// ```
    pub fn format_runtime_error(&self, error: &VmError) -> String {
        let source = match &self.source_text {
            Some(s) => s.as_str(),
            None => return format!("error: {error}"),
        };
        let filename = self.source_file.as_deref().unwrap_or("<unknown>");

        let error_msg = format!("{error}");
        let mut out = String::new();

        // Error header
        out.push_str(&format!("error: {error_msg}\n"));

        // Get the error location from the top frame (line, column)
        let frames: Vec<(&str, usize, usize)> = self
            .frames
            .iter()
            .map(|f| {
                let idx = if f.ip > 0 { f.ip - 1 } else { 0 };
                let line = if idx < f.chunk.lines.len() {
                    f.chunk.lines[idx] as usize
                } else {
                    0
                };
                let col = if idx < f.chunk.columns.len() {
                    f.chunk.columns[idx] as usize
                } else {
                    0
                };
                (f.fn_name.as_str(), line, col)
            })
            .collect();

        if let Some((_name, line, col)) = frames.last() {
            let line = *line;
            let col = *col;
            if line > 0 {
                let display_col = if col > 0 { col } else { 1 };
                let gutter_width = line.to_string().len();
                out.push_str(&format!(
                    "{:>width$}--> {filename}:{line}:{display_col}\n",
                    " ",
                    width = gutter_width + 1,
                ));
                // Show source line with caret
                if let Some(source_line) = source.lines().nth(line.saturating_sub(1)) {
                    out.push_str(&format!("{:>width$} |\n", " ", width = gutter_width + 1));
                    out.push_str(&format!(
                        "{:>width$} | {source_line}\n",
                        line,
                        width = gutter_width + 1,
                    ));
                    // Render caret line
                    let caret_col = if col > 0 { col } else { 1 };
                    let trimmed = source_line.trim();
                    let leading = source_line
                        .len()
                        .saturating_sub(source_line.trim_start().len());
                    // Calculate how many carets to show
                    let caret_len = if col > 0 {
                        // Try to find a reasonable token length at this column
                        Self::token_len_at(source_line, col)
                    } else {
                        // No column info: underline the trimmed content
                        trimmed.len().max(1)
                    };
                    let padding = if col > 0 {
                        " ".repeat(caret_col.saturating_sub(1))
                    } else {
                        " ".repeat(leading)
                    };
                    let carets = "^".repeat(caret_len);
                    out.push_str(&format!(
                        "{:>width$} | {padding}{carets}\n",
                        " ",
                        width = gutter_width + 1,
                    ));
                }
            }
        }

        // Show call stack (bottom-up, skipping top frame which is already shown)
        if frames.len() > 1 {
            for (name, line, _col) in frames.iter().rev().skip(1) {
                let display_name = if name.is_empty() { "pipeline" } else { name };
                if *line > 0 {
                    out.push_str(&format!(
                        "  = note: called from {display_name} at {filename}:{line}\n"
                    ));
                }
            }
        }

        out
    }

    /// Estimate the length of the token at the given 1-based column position
    /// in a source line. Scans forward from that position to find a word/operator
    /// boundary.
    fn token_len_at(source_line: &str, col: usize) -> usize {
        let chars: Vec<char> = source_line.chars().collect();
        let start = col.saturating_sub(1);
        if start >= chars.len() {
            return 1;
        }
        let first = chars[start];
        if first.is_alphanumeric() || first == '_' {
            // Scan forward through identifier/number chars
            let mut end = start + 1;
            while end < chars.len() && (chars[end].is_alphanumeric() || chars[end] == '_') {
                end += 1;
            }
            end - start
        } else {
            // Operator or punctuation: just one caret
            1
        }
    }
}

impl Default for Vm {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::compiler::Compiler;
    use crate::stdlib::register_vm_stdlib;
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
