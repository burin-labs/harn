mod format;
mod imports;
pub mod iter;
mod methods;
mod ops;

use std::cell::RefCell;
use std::collections::{BTreeMap, HashSet};
use std::future::Future;
use std::pin::Pin;
use std::rc::Rc;
use std::time::Instant;

use crate::chunk::{Chunk, CompiledFunction, Constant};
use crate::value::{
    ErrorCategory, ModuleFunctionRegistry, VmAsyncBuiltinFn, VmBuiltinFn, VmClosure, VmEnv,
    VmError, VmTaskHandle, VmValue,
};

thread_local! {
    static CURRENT_ASYNC_BUILTIN_CHILD_VM: RefCell<Vec<Vm>> = const { RefCell::new(Vec::new()) };
}

/// RAII guard that starts a tracing span on creation and ends it on drop.
struct ScopeSpan(u64);

impl ScopeSpan {
    fn new(kind: crate::tracing::SpanKind, name: String) -> Self {
        Self(crate::tracing::span_start(kind, name))
    }
}

impl Drop for ScopeSpan {
    fn drop(&mut self) {
        crate::tracing::span_end(self.0);
    }
}

/// Call frame for function execution.
pub(crate) struct CallFrame {
    pub(crate) chunk: Chunk,
    pub(crate) ip: usize,
    pub(crate) stack_base: usize,
    pub(crate) saved_env: VmEnv,
    /// Iterator stack depth to restore when this frame unwinds.
    pub(crate) saved_iterator_depth: usize,
    /// Function name for stack traces (empty for top-level pipeline).
    pub(crate) fn_name: String,
    /// Number of arguments actually passed by the caller (for default arg support).
    pub(crate) argc: usize,
    /// Saved VM_SOURCE_DIR to restore when this frame is popped.
    /// Set when entering a closure that originated from an imported module.
    pub(crate) saved_source_dir: Option<std::path::PathBuf>,
    /// Module-local named functions available to symbolic calls within this frame.
    pub(crate) module_functions: Option<ModuleFunctionRegistry>,
    /// Shared module-level env for top-level `var` / `let` bindings of
    /// this frame's originating module. Looked up after `self.env` and
    /// before `self.globals` by `GetVar` / `SetVar`, giving each module
    /// its own live static state that persists across calls. See the
    /// `module_state` field on `VmClosure` for the full rationale.
    pub(crate) module_state: Option<crate::value::ModuleState>,
}

/// Exception handler for try/catch.
pub(crate) struct ExceptionHandler {
    pub(crate) catch_ip: usize,
    pub(crate) stack_depth: usize,
    pub(crate) frame_depth: usize,
    pub(crate) env_scope_depth: usize,
    /// If non-empty, this catch only handles errors whose enum_name matches.
    pub(crate) error_type: String,
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

type DebugHook = dyn FnMut(&DebugState) -> DebugAction;

/// Iterator state for for-in loops: either a pre-collected vec, an async channel, or a generator.
pub(crate) enum IterState {
    Vec {
        items: Vec<VmValue>,
        idx: usize,
    },
    Channel {
        receiver: std::sync::Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<VmValue>>>,
        closed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    },
    Generator {
        gen: crate::value::VmGenerator,
    },
    /// Step through a lazy range without materializing a Vec.
    /// `next` holds the value to emit on the next IterNext; `stop` is
    /// the first value that terminates the iteration (one past the end).
    Range {
        next: i64,
        stop: i64,
    },
    VmIter {
        handle: std::rc::Rc<std::cell::RefCell<crate::vm::iter::VmIter>>,
    },
}

#[derive(Clone)]
pub(crate) struct LoadedModule {
    pub(crate) functions: BTreeMap<String, Rc<VmClosure>>,
    pub(crate) public_names: HashSet<String>,
}

/// The Harn bytecode virtual machine.
pub struct Vm {
    pub(crate) stack: Vec<VmValue>,
    pub(crate) env: VmEnv,
    pub(crate) output: String,
    pub(crate) builtins: BTreeMap<String, VmBuiltinFn>,
    pub(crate) async_builtins: BTreeMap<String, VmAsyncBuiltinFn>,
    /// Iterator state for for-in loops.
    pub(crate) iterators: Vec<IterState>,
    /// Call frame stack.
    pub(crate) frames: Vec<CallFrame>,
    /// Exception handler stack.
    pub(crate) exception_handlers: Vec<ExceptionHandler>,
    /// Spawned async task handles.
    pub(crate) spawned_tasks: BTreeMap<String, VmTaskHandle>,
    /// Counter for generating unique task IDs.
    pub(crate) task_counter: u64,
    /// Active deadline stack: (deadline_instant, frame_depth).
    pub(crate) deadlines: Vec<(Instant, usize)>,
    /// Breakpoints (source line numbers).
    pub(crate) breakpoints: Vec<usize>,
    /// Whether the VM is in step mode.
    pub(crate) step_mode: bool,
    /// The frame depth at which stepping started (for step-over).
    pub(crate) step_frame_depth: usize,
    /// Whether the VM is currently stopped at a debug point.
    pub(crate) stopped: bool,
    /// Last source line executed (to detect line changes).
    pub(crate) last_line: usize,
    /// Source directory for resolving imports.
    pub(crate) source_dir: Option<std::path::PathBuf>,
    /// Modules currently being imported (cycle prevention).
    pub(crate) imported_paths: Vec<std::path::PathBuf>,
    /// Loaded module cache keyed by canonical or synthetic module path.
    pub(crate) module_cache: BTreeMap<std::path::PathBuf, LoadedModule>,
    /// Source file path for error reporting.
    pub(crate) source_file: Option<String>,
    /// Source text for error reporting.
    pub(crate) source_text: Option<String>,
    /// Optional bridge for delegating unknown builtins in bridge mode.
    pub(crate) bridge: Option<Rc<crate::bridge::HostBridge>>,
    /// Builtins denied by sandbox mode (`--deny` / `--allow` flags).
    pub(crate) denied_builtins: HashSet<String>,
    /// Cancellation token for cooperative graceful shutdown (set by parent).
    pub(crate) cancel_token: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    /// Captured stack trace from the most recent error (fn_name, line, col).
    pub(crate) error_stack_trace: Vec<(String, usize, usize, Option<String>)>,
    /// Yield channel sender for generator execution. When set, `Op::Yield`
    /// sends values through this channel instead of being a no-op.
    pub(crate) yield_sender: Option<tokio::sync::mpsc::Sender<VmValue>>,
    /// Project root directory (detected via harn.toml).
    /// Used as base directory for metadata, store, and checkpoint operations.
    pub(crate) project_root: Option<std::path::PathBuf>,
    /// Global constants (e.g. `pi`, `e`). Checked as a fallback in `GetVar`
    /// after the environment, so user-defined variables can shadow them.
    pub(crate) globals: BTreeMap<String, VmValue>,
    /// Optional debugger hook invoked when execution advances to a new source line.
    pub(crate) debug_hook: Option<Box<DebugHook>>,
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
            module_cache: BTreeMap::new(),
            source_file: None,
            source_text: None,
            bridge: None,
            denied_builtins: HashSet::new(),
            cancel_token: None,
            error_stack_trace: Vec::new(),
            yield_sender: None,
            project_root: None,
            globals: BTreeMap::new(),
            debug_hook: None,
        }
    }

    /// Set the bridge for delegating unknown builtins in bridge mode.
    pub fn set_bridge(&mut self, bridge: Rc<crate::bridge::HostBridge>) {
        self.bridge = Some(bridge);
    }

    /// Set builtins that are denied in sandbox mode.
    /// When called, the given builtin names will produce a permission error.
    pub fn set_denied_builtins(&mut self, denied: HashSet<String>) {
        self.denied_builtins = denied;
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

    /// Register a debug hook invoked whenever execution advances to a new source line.
    pub fn set_debug_hook<F>(&mut self, hook: F)
    where
        F: FnMut(&DebugState) -> DebugAction + 'static,
    {
        self.debug_hook = Some(Box::new(hook));
    }

    /// Clear the current debug hook.
    pub fn clear_debug_hook(&mut self) {
        self.debug_hook = None;
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

            let state = self.debug_state();
            if let Some(hook) = self.debug_hook.as_mut() {
                if matches!(hook(&state), DebugAction::Stop) {
                    self.stopped = true;
                    return Ok(Some((VmValue::Nil, true)));
                }
            }

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
                self.iterators.truncate(popped_frame.saved_iterator_depth);
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
                    if let Some(ref dir) = popped_frame.saved_source_dir {
                        crate::stdlib::set_thread_source_dir(dir);
                    }
                    let current_depth = self.frames.len();
                    self.exception_handlers
                        .retain(|h| h.frame_depth <= current_depth);
                    if self.frames.is_empty() {
                        return Ok(Some((val, false)));
                    }
                    self.iterators.truncate(popped_frame.saved_iterator_depth);
                    self.env = popped_frame.saved_env;
                    self.stack.truncate(popped_frame.stack_base);
                    self.stack.push(val);
                    Ok(None)
                } else {
                    Ok(Some((val, false)))
                }
            }
            Err(e) => {
                if self.error_stack_trace.is_empty() {
                    self.error_stack_trace = self.capture_stack_trace();
                }
                match self.handle_error(e) {
                    Ok(None) => {
                        self.error_stack_trace.clear();
                        Ok(None)
                    }
                    Ok(Some(val)) => Ok(Some((val, false))),
                    Err(e) => Err(self.enrich_error_with_line(e)),
                }
            }
        }
    }

    /// Initialize execution (push the initial frame).
    pub fn start(&mut self, chunk: &Chunk) {
        self.frames.push(CallFrame {
            chunk: chunk.clone(),
            ip: 0,
            stack_base: self.stack.len(),
            saved_env: self.env.clone(),
            saved_iterator_depth: self.iterators.len(),
            fn_name: String::new(),
            argc: 0,
            saved_source_dir: None,
            module_functions: None,
            module_state: None,
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
            deadlines: self.deadlines.clone(),
            breakpoints: Vec::new(),
            step_mode: false,
            step_frame_depth: 0,
            stopped: false,
            last_line: 0,
            source_dir: self.source_dir.clone(),
            imported_paths: Vec::new(),
            module_cache: self.module_cache.clone(),
            source_file: self.source_file.clone(),
            source_text: self.source_text.clone(),
            bridge: self.bridge.clone(),
            denied_builtins: self.denied_builtins.clone(),
            cancel_token: None,
            error_stack_trace: Vec::new(),
            yield_sender: None,
            project_root: self.project_root.clone(),
            globals: self.globals.clone(),
            debug_hook: None,
        }
    }

    /// Set the source directory for import resolution and introspection.
    /// Also auto-detects the project root if not already set.
    pub fn set_source_dir(&mut self, dir: &std::path::Path) {
        self.source_dir = Some(dir.to_path_buf());
        crate::stdlib::set_thread_source_dir(dir);
        // Auto-detect project root if not explicitly set.
        if self.project_root.is_none() {
            self.project_root = crate::stdlib::process::find_project_root(dir);
        }
    }

    /// Explicitly set the project root directory.
    /// Used by ACP/CLI to override auto-detection.
    pub fn set_project_root(&mut self, root: &std::path::Path) {
        self.project_root = Some(root.to_path_buf());
    }

    /// Get the project root directory, falling back to source_dir.
    pub fn project_root(&self) -> Option<&std::path::Path> {
        self.project_root.as_deref().or(self.source_dir.as_deref())
    }

    /// Return all registered builtin names (sync + async).
    pub fn builtin_names(&self) -> Vec<String> {
        let mut names: Vec<String> = self.builtins.keys().cloned().collect();
        names.extend(self.async_builtins.keys().cloned());
        names
    }

    /// Set a global constant (e.g. `pi`, `e`).
    /// Stored separately from the environment so user-defined variables can shadow them.
    pub fn set_global(&mut self, name: &str, value: VmValue) {
        self.globals.insert(name.to_string(), value);
    }

    /// Get the captured output.
    pub fn output(&self) -> &str {
        &self.output
    }

    /// Execute a compiled chunk.
    pub async fn execute(&mut self, chunk: &Chunk) -> Result<VmValue, VmError> {
        let span_id = crate::tracing::span_start(crate::tracing::SpanKind::Pipeline, "main".into());
        let result = self.run_chunk(chunk).await;
        crate::tracing::span_end(span_id);
        result
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
                    if let Some(ref dir) = frame.saved_source_dir {
                        crate::stdlib::set_thread_source_dir(dir);
                    }
                    self.iterators.truncate(frame.saved_iterator_depth);
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

            self.env.truncate_scopes(handler.env_scope_depth);

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
        self.run_chunk_entry(chunk, 0, None, None, None).await
    }

    async fn run_chunk_entry(
        &mut self,
        chunk: &Chunk,
        argc: usize,
        saved_source_dir: Option<std::path::PathBuf>,
        module_functions: Option<ModuleFunctionRegistry>,
        module_state: Option<crate::value::ModuleState>,
    ) -> Result<VmValue, VmError> {
        self.frames.push(CallFrame {
            chunk: chunk.clone(),
            ip: 0,
            stack_base: self.stack.len(),
            saved_env: self.env.clone(),
            saved_iterator_depth: self.iterators.len(),
            fn_name: String::new(),
            argc,
            saved_source_dir,
            module_functions,
            module_state,
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
                if let Some(ref dir) = popped_frame.saved_source_dir {
                    crate::stdlib::set_thread_source_dir(dir);
                }

                if self.frames.is_empty() {
                    // We're done with the top-level chunk
                    return Ok(val);
                } else {
                    // Returning from a function call
                    self.iterators.truncate(popped_frame.saved_iterator_depth);
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
                        if let Some(ref dir) = popped_frame.saved_source_dir {
                            crate::stdlib::set_thread_source_dir(dir);
                        }
                        // Clean up exception handlers from the returned frame
                        let current_depth = self.frames.len();
                        self.exception_handlers
                            .retain(|h| h.frame_depth <= current_depth);

                        if self.frames.is_empty() {
                            return Ok(val);
                        }
                        self.iterators.truncate(popped_frame.saved_iterator_depth);
                        self.env = popped_frame.saved_env;
                        self.stack.truncate(popped_frame.stack_base);
                        self.stack.push(val);
                    } else {
                        return Ok(val);
                    }
                }
                Err(e) => {
                    // Capture stack trace before error handling unwinds frames
                    if self.error_stack_trace.is_empty() {
                        self.error_stack_trace = self.capture_stack_trace();
                    }
                    match self.handle_error(e) {
                        Ok(None) => {
                            self.error_stack_trace.clear();
                            continue; // Handler found, continue
                        }
                        Ok(Some(val)) => return Ok(val),
                        Err(e) => return Err(self.enrich_error_with_line(e)),
                    }
                }
            }
        }
    }

    /// Capture the current call stack as (fn_name, line, col, source_file) tuples.
    fn capture_stack_trace(&self) -> Vec<(String, usize, usize, Option<String>)> {
        self.frames
            .iter()
            .map(|f| {
                let idx = if f.ip > 0 { f.ip - 1 } else { 0 };
                let line = f.chunk.lines.get(idx).copied().unwrap_or(0) as usize;
                let col = f.chunk.columns.get(idx).copied().unwrap_or(0) as usize;
                (f.fn_name.clone(), line, col, f.chunk.source_file.clone())
            })
            .collect()
    }

    /// Enrich a VmError with source line information from the captured stack
    /// trace. Appends ` (line N)` to error variants whose messages don't
    /// already carry location context.
    fn enrich_error_with_line(&self, error: VmError) -> VmError {
        // Determine the line from the captured stack trace (innermost frame).
        let line = self
            .error_stack_trace
            .last()
            .map(|(_, l, _, _)| *l)
            .unwrap_or_else(|| self.current_line());
        if line == 0 {
            return error;
        }
        let suffix = format!(" (line {line})");
        match error {
            VmError::Runtime(msg) => VmError::Runtime(format!("{msg}{suffix}")),
            VmError::TypeError(msg) => VmError::TypeError(format!("{msg}{suffix}")),
            VmError::DivisionByZero => VmError::Runtime(format!("Division by zero{suffix}")),
            VmError::UndefinedVariable(name) => {
                VmError::Runtime(format!("Undefined variable: {name}{suffix}"))
            }
            VmError::UndefinedBuiltin(name) => {
                VmError::Runtime(format!("Undefined builtin: {name}{suffix}"))
            }
            VmError::ImmutableAssignment(name) => VmError::Runtime(format!(
                "Cannot assign to immutable binding: {name}{suffix}"
            )),
            VmError::StackOverflow => {
                VmError::Runtime(format!("Stack overflow: too many nested calls{suffix}"))
            }
            // Leave these untouched:
            // - Thrown: user-thrown errors should not be silently modified
            // - CategorizedError: structured errors for agent orchestration
            // - Return: control flow, not a real error
            // - StackUnderflow / InvalidInstruction: internal VM bugs
            other => other,
        }
    }

    const MAX_FRAMES: usize = 512;

    /// Build the call-time env for a closure invocation.
    ///
    /// Harn is **lexically scoped for data**: a closure sees exactly the
    /// data names it captured at creation time, plus its parameters,
    /// plus names from its originating module's `module_state`, plus
    /// the module-function registry. The caller's *data* locals are
    /// intentionally not visible — that would be dynamic scoping, which
    /// is neither what Harn's TS-flavored surface suggests to users nor
    /// something real stdlib code relies on.
    ///
    /// **Exception: closure-typed bindings.** Function *names* are
    /// late-bound, Python-`LOAD_GLOBAL`-style. When a local recursive
    /// fn is declared in a pipeline body (or inside another function),
    /// the closure is created BEFORE its own name is defined in the
    /// enclosing scope, so `closure.env` captures a snapshot that is
    /// missing the self-reference. To make `fn fact(n) { fact(n-1) }`
    /// work without a letrec trick, we merge closure-typed entries
    /// from the caller's scope stack — but only closure-typed ones.
    /// Data locals are never leaked across call boundaries, so the
    /// surprising "caller's variable magically visible in callee"
    /// semantic is ruled out.
    ///
    /// Imported module closures have `module_state` set, at which
    /// point the full lexical environment is already available via
    /// `closure.env` + `module_state`, and we skip the closure merge
    /// entirely as a fast path. This is the hot path for context-
    /// builder workloads (~65% of VM CPU before this optimization).
    fn closure_call_env(caller_env: &VmEnv, closure: &VmClosure) -> VmEnv {
        if closure.module_state.is_some() {
            return closure.env.clone();
        }
        let mut call_env = closure.env.clone();
        // Late-bind only closure-typed names from the caller — enough
        // for local recursive / mutually-recursive fns to self-reference
        // without leaking caller-local data into the callee.
        for scope in &caller_env.scopes {
            for (name, (val, mutable)) in &scope.vars {
                if matches!(val, VmValue::Closure(_)) && call_env.get(name).is_none() {
                    let _ = call_env.define(name, val.clone(), *mutable);
                }
            }
        }
        call_env
    }

    fn resolve_named_closure(&self, name: &str) -> Option<Rc<VmClosure>> {
        if let Some(VmValue::Closure(closure)) = self.env.get(name) {
            return Some(closure);
        }
        self.frames
            .last()
            .and_then(|frame| frame.module_functions.as_ref())
            .and_then(|registry| registry.borrow().get(name).cloned())
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

        // If this closure originated from an imported module, switch
        // the thread-local source dir so that render() and other
        // source-relative builtins resolve relative to the module.
        let saved_source_dir = if let Some(ref dir) = closure.source_dir {
            let prev = crate::stdlib::process::VM_SOURCE_DIR.with(|sd| sd.borrow().clone());
            crate::stdlib::set_thread_source_dir(dir);
            prev
        } else {
            None
        };

        let mut call_env = Self::closure_call_env(&saved_env, closure);
        call_env.push_scope();

        let default_start = closure
            .func
            .default_start
            .unwrap_or(closure.func.params.len());
        let param_count = closure.func.params.len();
        for (i, param) in closure.func.params.iter().enumerate() {
            if closure.func.has_rest_param && i == param_count - 1 {
                // Rest parameter: collect remaining args into a list
                let rest_args = if i < args.len() {
                    args[i..].to_vec()
                } else {
                    Vec::new()
                };
                let _ = call_env.define(param, VmValue::List(std::rc::Rc::new(rest_args)), false);
            } else if i < args.len() {
                let _ = call_env.define(param, args[i].clone(), false);
            } else if i < default_start {
                let _ = call_env.define(param, VmValue::Nil, false);
            }
        }

        self.env = call_env;

        self.frames.push(CallFrame {
            chunk: closure.func.chunk.clone(),
            ip: 0,
            stack_base: self.stack.len(),
            saved_env,
            saved_iterator_depth: self.iterators.len(),
            fn_name: closure.func.name.clone(),
            argc: args.len(),
            saved_source_dir,
            module_functions: closure.module_functions.clone(),
            module_state: closure.module_state.clone(),
        });

        Ok(())
    }

    /// Create a generator value by spawning the closure body as an async task.
    /// The generator body communicates yielded values through an mpsc channel.
    pub(crate) fn create_generator(&self, closure: &VmClosure, args: &[VmValue]) -> VmValue {
        use crate::value::VmGenerator;

        // Buffer size of 1: the generator produces one value at a time.
        let (tx, rx) = tokio::sync::mpsc::channel::<VmValue>(1);

        let mut child = self.child_vm();
        child.yield_sender = Some(tx);

        // Set up the environment for the generator body. The generator
        // body runs in its own child VM; closure_call_env walks the
        // current (parent) env so locally-defined generator closures
        // can self-reference via the narrow closure-only merge. See
        // `Vm::closure_call_env`.
        let parent_env = self.env.clone();
        let mut call_env = Self::closure_call_env(&parent_env, closure);
        call_env.push_scope();

        let default_start = closure
            .func
            .default_start
            .unwrap_or(closure.func.params.len());
        let param_count = closure.func.params.len();
        for (i, param) in closure.func.params.iter().enumerate() {
            if closure.func.has_rest_param && i == param_count - 1 {
                let rest_args = if i < args.len() {
                    args[i..].to_vec()
                } else {
                    Vec::new()
                };
                let _ = call_env.define(param, VmValue::List(std::rc::Rc::new(rest_args)), false);
            } else if i < args.len() {
                let _ = call_env.define(param, args[i].clone(), false);
            } else if i < default_start {
                let _ = call_env.define(param, VmValue::Nil, false);
            }
        }
        child.env = call_env;

        let chunk = closure.func.chunk.clone();
        let saved_source_dir = if let Some(ref dir) = closure.source_dir {
            let prev = crate::stdlib::process::VM_SOURCE_DIR.with(|sd| sd.borrow().clone());
            crate::stdlib::set_thread_source_dir(dir);
            prev
        } else {
            None
        };
        let module_functions = closure.module_functions.clone();
        let module_state = closure.module_state.clone();
        let argc = args.len();
        // Spawn the generator body as an async task.
        // The task will execute until return, sending yielded values through the channel.
        tokio::task::spawn_local(async move {
            let _ = child
                .run_chunk_entry(
                    &chunk,
                    argc,
                    saved_source_dir,
                    module_functions,
                    module_state,
                )
                .await;
            // When the generator body finishes (return or fall-through),
            // the sender is dropped, signaling completion to the receiver.
        });

        VmValue::Generator(VmGenerator {
            done: Rc::new(std::cell::Cell::new(false)),
            receiver: Rc::new(tokio::sync::Mutex::new(rx)),
        })
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

            let mut call_env = Self::closure_call_env(&saved_env, closure);
            call_env.push_scope();

            let default_start = closure
                .func
                .default_start
                .unwrap_or(closure.func.params.len());
            let param_count = closure.func.params.len();
            for (i, param) in closure.func.params.iter().enumerate() {
                if closure.func.has_rest_param && i == param_count - 1 {
                    let rest_args = if i < args.len() {
                        args[i..].to_vec()
                    } else {
                        Vec::new()
                    };
                    let _ =
                        call_env.define(param, VmValue::List(std::rc::Rc::new(rest_args)), false);
                } else if i < args.len() {
                    let _ = call_env.define(param, args[i].clone(), false);
                } else if i < default_start {
                    let _ = call_env.define(param, VmValue::Nil, false);
                }
            }

            self.env = call_env;
            let argc = args.len();
            let saved_source_dir = if let Some(ref dir) = closure.source_dir {
                let prev = crate::stdlib::process::VM_SOURCE_DIR.with(|sd| sd.borrow().clone());
                crate::stdlib::set_thread_source_dir(dir);
                prev
            } else {
                None
            };
            let result = self
                .run_chunk_entry(
                    &closure.func.chunk,
                    argc,
                    saved_source_dir,
                    closure.module_functions.clone(),
                    closure.module_state.clone(),
                )
                .await;

            self.env = saved_env;
            self.frames = saved_frames;
            self.exception_handlers = saved_handlers;
            self.iterators = saved_iterators;
            self.deadlines = saved_deadlines;

            result
        })
    }

    /// Invoke a value as a callable. Supports `VmValue::Closure` and
    /// `VmValue::BuiltinRef`, so builtin names passed by reference (e.g.
    /// `dict.rekey(snake_to_camel)`) dispatch through the same code path as
    /// user-defined closures.
    #[allow(clippy::manual_async_fn)]
    fn call_callable_value<'a>(
        &'a mut self,
        callable: &'a VmValue,
        args: &'a [VmValue],
        functions: &'a [CompiledFunction],
    ) -> Pin<Box<dyn Future<Output = Result<VmValue, VmError>> + 'a>> {
        Box::pin(async move {
            match callable {
                VmValue::Closure(closure) => self.call_closure(closure, args, functions).await,
                VmValue::BuiltinRef(name) => {
                    let name_owned = name.to_string();
                    self.call_named_builtin(&name_owned, args.to_vec()).await
                }
                other => Err(VmError::TypeError(format!(
                    "expected callable, got {}",
                    other.type_name()
                ))),
            }
        })
    }

    /// Returns true if `v` is callable via `call_callable_value`.
    fn is_callable_value(v: &VmValue) -> bool {
        matches!(v, VmValue::Closure(_) | VmValue::BuiltinRef(_))
    }

    /// Public wrapper for `call_closure`, used by the MCP server to invoke
    /// tool handler closures from outside the VM execution loop.
    pub async fn call_closure_pub(
        &mut self,
        closure: &VmClosure,
        args: &[VmValue],
        functions: &[CompiledFunction],
    ) -> Result<VmValue, VmError> {
        self.call_closure(closure, args, functions).await
    }

    /// Resolve a named builtin: sync builtins → async builtins → bridge → error.
    /// Used by Call, TailCall, and Pipe handlers to avoid duplicating this lookup.
    async fn call_named_builtin(
        &mut self,
        name: &str,
        args: Vec<VmValue>,
    ) -> Result<VmValue, VmError> {
        // Auto-trace LLM calls and tool calls
        let span_kind = match name {
            "llm_call" | "llm_stream" | "agent_loop" => Some(crate::tracing::SpanKind::LlmCall),
            "mcp_call" => Some(crate::tracing::SpanKind::ToolCall),
            _ => None,
        };
        let _span = span_kind.map(|kind| ScopeSpan::new(kind, name.to_string()));

        // Sandbox check: deny builtins blocked by --deny/--allow flags.
        if self.denied_builtins.contains(name) {
            return Err(VmError::CategorizedError {
                message: format!("Tool '{}' is not permitted.", name),
                category: ErrorCategory::ToolRejected,
            });
        }
        crate::orchestration::enforce_current_policy_for_builtin(name, &args)?;
        if let Some(builtin) = self.builtins.get(name).cloned() {
            builtin(&args, &mut self.output)
        } else if let Some(async_builtin) = self.async_builtins.get(name).cloned() {
            CURRENT_ASYNC_BUILTIN_CHILD_VM.with(|slot| {
                slot.borrow_mut().push(self.child_vm());
            });
            let result = async_builtin(args).await;
            CURRENT_ASYNC_BUILTIN_CHILD_VM.with(|slot| {
                slot.borrow_mut().pop();
            });
            result
        } else if let Some(bridge) = &self.bridge {
            crate::orchestration::enforce_current_policy_for_bridge_builtin(name)?;
            let args_json: Vec<serde_json::Value> =
                args.iter().map(crate::llm::vm_value_to_json).collect();
            let result = bridge
                .call(
                    "builtin_call",
                    serde_json::json!({"name": name, "args": args_json}),
                )
                .await?;
            Ok(crate::bridge::json_result_to_vm_value(&result))
        } else {
            let all_builtins = self
                .builtins
                .keys()
                .chain(self.async_builtins.keys())
                .map(|s| s.as_str());
            if let Some(suggestion) = crate::value::closest_match(name, all_builtins) {
                return Err(VmError::Runtime(format!(
                    "Undefined builtin: {name} (did you mean `{suggestion}`?)"
                )));
            }
            Err(VmError::UndefinedBuiltin(name.to_string()))
        }
    }
}

/// Clone the VM at the top of the async-builtin child VM stack, returning a
/// fresh `Vm` instance that callers own and can use without coordinating
/// with other concurrent users of the stack. This replaces the legacy
/// `take/restore` pattern: that pattern serialized access because only one
/// consumer could hold the single stack entry at a time, which prevented
/// any form of concurrent tool-handler execution within a single
/// agent_loop iteration. Cloning is cheap — the VM struct shares its
/// heavy state (env, builtins, bridge, module_cache) via `Arc`/`Rc` — so
/// multiple concurrent handlers can each have their own execution context.
///
/// Returns `None` if no parent VM is currently pushed on the stack.
pub fn clone_async_builtin_child_vm() -> Option<Vm> {
    CURRENT_ASYNC_BUILTIN_CHILD_VM.with(|slot| slot.borrow().last().map(|vm| vm.child_vm()))
}

/// Legacy API preserved for backward compatibility with any out-of-tree
/// callers. New code should use `clone_async_builtin_child_vm()` instead
/// — `take` serializes concurrent callers because only one can hold the
/// popped value at a time. Internally this now delegates to a clone so
/// even legacy callers don't deadlock each other, but the name is kept
/// until external callers migrate.
#[deprecated(
    note = "use clone_async_builtin_child_vm() — take/restore serialized concurrent callers"
)]
pub fn take_async_builtin_child_vm() -> Option<Vm> {
    clone_async_builtin_child_vm()
}

/// Legacy API — now a no-op because `take_async_builtin_child_vm` returns
/// a clone rather than popping the stack, so there is nothing to restore.
/// Kept for backward compatibility.
#[deprecated(note = "clone_async_builtin_child_vm does not need a matching restore call")]
pub fn restore_async_builtin_child_vm(_vm: Vm) {
    // No-op: the new clone-based API doesn't require restoration since
    // the caller owns a fresh clone and the stack is never mutated.
    CURRENT_ASYNC_BUILTIN_CHILD_VM.with(|slot| {
        // Intentionally ignore — kept as a syntactic no-op block so the
        // function signature remains stable.
        let _ = slot;
    });
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
    fn test_exponentiation() {
        let out = run_output(
            "pipeline t(task) { log(2 ** 8)\nlog(2 * 3 ** 2)\nlog(2 ** 3 ** 2)\nlog(2 ** -1) }",
        );
        assert_eq!(out, "[harn] 256\n[harn] 18\n[harn] 512\n[harn] 0.5");
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
    fn test_inner_for_return_does_not_leak_iterator_into_caller() {
        let out = run_output(
            r#"pipeline t(task) {
  fn first_match() {
    for pattern in ["a", "b"] {
      return pattern
    }
    return ""
  }

  var seen = []
  for path in ["outer"] {
    seen = seen + [path + ":" + first_match()]
  }
  log(join(seen, ","))
}"#,
        );
        assert_eq!(out, "[harn] outer:a");
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
    fn test_float_division_by_zero_uses_ieee_values() {
        let out = run_vm(
            "pipeline default(task) { log(is_nan(0.0 / 0.0))\nlog(is_infinite(1.0 / 0.0))\nlog(is_infinite(-1.0 / 0.0)) }",
        );
        assert_eq!(out, "[harn] true\n[harn] true\n[harn] true\n");
    }

    #[test]
    fn test_reusing_catch_binding_name_in_same_block() {
        let out = run_vm(
            r#"pipeline default(task) {
try {
    throw "a"
} catch e {
    log(e)
}
try {
    throw "b"
} catch e {
    log(e)
}
}"#,
        );
        assert_eq!(out, "[harn] a\n[harn] b\n");
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
} catch(e2) {
    log("outer caught: " + e2)
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
    fn test_parallel_each_basic() {
        let out = run_output(
            "pipeline t(task) { let results = parallel each [1, 2, 3] { x -> x * x }\nlog(results) }",
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

    /// Helper that runs Harn source with a set of denied builtins.
    fn run_harn_with_denied(
        source: &str,
        denied: HashSet<String>,
    ) -> Result<(String, VmValue), VmError> {
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
                    vm.set_denied_builtins(denied);
                    let result = vm.execute(&chunk).await?;
                    Ok((vm.output().to_string(), result))
                })
                .await
        })
    }

    #[test]
    fn test_sandbox_deny_builtin() {
        let denied: HashSet<String> = ["push".to_string()].into_iter().collect();
        let result = run_harn_with_denied(
            r#"pipeline t(task) {
let xs = [1, 2]
push(xs, 3)
}"#,
            denied,
        );
        let err = result.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("not permitted"),
            "expected not permitted, got: {msg}"
        );
        assert!(
            msg.contains("push"),
            "expected builtin name in error, got: {msg}"
        );
    }

    #[test]
    fn test_sandbox_allowed_builtin_works() {
        // Denying "push" should not block "log"
        let denied: HashSet<String> = ["push".to_string()].into_iter().collect();
        let result = run_harn_with_denied(r#"pipeline t(task) { log("hello") }"#, denied);
        let (output, _) = result.unwrap();
        assert_eq!(output.trim(), "[harn] hello");
    }

    #[test]
    fn test_sandbox_empty_denied_set() {
        // With an empty denied set, everything should work.
        let result = run_harn_with_denied(r#"pipeline t(task) { log("ok") }"#, HashSet::new());
        let (output, _) = result.unwrap();
        assert_eq!(output.trim(), "[harn] ok");
    }

    #[test]
    fn test_sandbox_propagates_to_spawn() {
        // Denied builtins should propagate to spawned VMs.
        let denied: HashSet<String> = ["push".to_string()].into_iter().collect();
        let result = run_harn_with_denied(
            r#"pipeline t(task) {
let handle = spawn {
  let xs = [1, 2]
  push(xs, 3)
}
await(handle)
}"#,
            denied,
        );
        let err = result.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("not permitted"),
            "expected not permitted in spawned VM, got: {msg}"
        );
    }

    #[test]
    fn test_sandbox_propagates_to_parallel() {
        // Denied builtins should propagate to parallel VMs.
        let denied: HashSet<String> = ["push".to_string()].into_iter().collect();
        let result = run_harn_with_denied(
            r#"pipeline t(task) {
let results = parallel(2) { i ->
  let xs = [1, 2]
  push(xs, 3)
}
}"#,
            denied,
        );
        let err = result.unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("not permitted"),
            "expected not permitted in parallel VM, got: {msg}"
        );
    }

    #[test]
    fn test_if_else_has_lexical_block_scope() {
        let out = run_output(
            r#"pipeline t(task) {
let x = "outer"
if true {
  let x = "inner"
  log(x)
} else {
  let x = "other"
  log(x)
}
log(x)
}"#,
        );
        assert_eq!(out, "[harn] inner\n[harn] outer");
    }

    #[test]
    fn test_loop_and_catch_bindings_are_block_scoped() {
        let out = run_output(
            r#"pipeline t(task) {
let label = "outer"
for item in [1, 2] {
  let label = "loop ${item}"
  log(label)
}
try {
  throw("boom")
} catch (label) {
  log(label)
}
log(label)
}"#,
        );
        assert_eq!(
            out,
            "[harn] loop 1\n[harn] loop 2\n[harn] boom\n[harn] outer"
        );
    }
}
