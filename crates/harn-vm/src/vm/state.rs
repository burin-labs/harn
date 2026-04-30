use std::collections::{BTreeMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::time::Instant;

use crate::chunk::{Chunk, ChunkRef, Constant};
use crate::value::{
    ModuleFunctionRegistry, VmAsyncBuiltinFn, VmBuiltinFn, VmEnv, VmError, VmTaskHandle, VmValue,
};
use crate::BuiltinId;

use super::debug::DebugHook;
use super::modules::LoadedModule;

/// RAII guard that starts a tracing span on creation and ends it on drop.
pub(crate) struct ScopeSpan(u64);

impl ScopeSpan {
    pub(crate) fn new(kind: crate::tracing::SpanKind, name: String) -> Self {
        Self(crate::tracing::span_start(kind, name))
    }
}

impl Drop for ScopeSpan {
    fn drop(&mut self) {
        crate::tracing::span_end(self.0);
    }
}

#[derive(Clone)]
pub(crate) struct LocalSlot {
    pub(crate) value: VmValue,
    pub(crate) initialized: bool,
    pub(crate) synced: bool,
}

/// Call frame for function execution.
pub(crate) struct CallFrame {
    pub(crate) chunk: ChunkRef,
    pub(crate) ip: usize,
    pub(crate) stack_base: usize,
    pub(crate) saved_env: VmEnv,
    /// Env snapshot captured at call-time, *after* argument binding. Used
    /// by the debugger's `restartFrame` to rewind this frame to its
    /// entry state (re-binding args from the original values) without
    /// re-entering the call site. Cheap to clone because `VmEnv` is
    /// already cloned into `saved_env` on every call. `None` for
    /// scratch frames (evaluate, import init) where restart isn't
    /// meaningful.
    pub(crate) initial_env: Option<VmEnv>,
    pub(crate) initial_local_slots: Option<Vec<LocalSlot>>,
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
    /// Slot-indexed locals for compiler-resolved names in this frame.
    pub(crate) local_slots: Vec<LocalSlot>,
    /// Env scope index that corresponds to compiler local scope depth 0.
    pub(crate) local_scope_base: usize,
    /// Current compiler local scope depth, updated by PushScope/PopScope.
    pub(crate) local_scope_depth: usize,
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

/// Iterator state for for-in loops.
pub(crate) enum IterState {
    Vec {
        items: Rc<Vec<VmValue>>,
        idx: usize,
    },
    Dict {
        entries: Rc<BTreeMap<String, VmValue>>,
        keys: Vec<String>,
        idx: usize,
    },
    Channel {
        receiver: std::sync::Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<VmValue>>>,
        closed: std::sync::Arc<std::sync::atomic::AtomicBool>,
    },
    Generator {
        gen: crate::value::VmGenerator,
    },
    Stream {
        stream: crate::value::VmStream,
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
pub(crate) enum VmBuiltinDispatch {
    Sync(VmBuiltinFn),
    Async(VmAsyncBuiltinFn),
}

#[derive(Clone)]
pub(crate) struct VmBuiltinEntry {
    pub(crate) name: Rc<str>,
    pub(crate) dispatch: VmBuiltinDispatch,
}

/// The Harn bytecode virtual machine.
pub struct Vm {
    pub(crate) stack: Vec<VmValue>,
    pub(crate) env: VmEnv,
    pub(crate) output: String,
    pub(crate) builtins: BTreeMap<String, VmBuiltinFn>,
    pub(crate) async_builtins: BTreeMap<String, VmAsyncBuiltinFn>,
    /// Numeric side index for builtins. Name-keyed maps remain authoritative;
    /// this index is the hot path for direct builtin bytecode and callback refs.
    pub(crate) builtins_by_id: BTreeMap<BuiltinId, VmBuiltinEntry>,
    /// IDs with detected name collisions. Collided names safely fall back to
    /// the authoritative name-keyed lookup path.
    pub(crate) builtin_id_collisions: HashSet<BuiltinId>,
    /// Iterator state for for-in loops.
    pub(crate) iterators: Vec<IterState>,
    /// Call frame stack.
    pub(crate) frames: Vec<CallFrame>,
    /// Exception handler stack.
    pub(crate) exception_handlers: Vec<ExceptionHandler>,
    /// Spawned async task handles.
    pub(crate) spawned_tasks: BTreeMap<String, VmTaskHandle>,
    /// Shared process-local synchronization primitives inherited by child VMs.
    pub(crate) sync_runtime: Arc<crate::synchronization::VmSyncRuntime>,
    /// Shared process-local cells, maps, and mailboxes inherited by child VMs.
    pub(crate) shared_state_runtime: Rc<crate::shared_state::VmSharedStateRuntime>,
    /// Permits acquired by lexical synchronization blocks in this VM.
    pub(crate) held_sync_guards: Vec<crate::synchronization::VmSyncHeldGuard>,
    /// Counter for generating unique task IDs.
    pub(crate) task_counter: u64,
    /// Counter for logical runtime-context task groups.
    pub(crate) runtime_context_counter: u64,
    /// Logical runtime task context visible through `runtime_context()`.
    pub(crate) runtime_context: crate::runtime_context::RuntimeContext,
    /// Active deadline stack: (deadline_instant, frame_depth).
    pub(crate) deadlines: Vec<(Instant, usize)>,
    /// Breakpoints, keyed by source-file path so a breakpoint at line N
    /// in `auto.harn` doesn't also fire when execution hits line N in an
    /// imported lib. The empty-string key is a wildcard used by callers
    /// that don't track source paths (legacy `set_breakpoints` API).
    pub(crate) breakpoints: BTreeMap<String, std::collections::BTreeSet<usize>>,
    /// Function-name breakpoints. Any closure call whose
    /// `CompiledFunction.name` matches an entry here raises a stop on
    /// entry, regardless of the call site's file or line. Lets the IDE
    /// break on `llm_call` / `host_run_pipeline` / any user pipeline
    /// function without pinning down a source location first.
    pub(crate) function_breakpoints: std::collections::BTreeSet<String>,
    /// Latched on `push_closure_frame` when the callee's name matches
    /// `function_breakpoints`; consumed by the next step so the stop is
    /// reported with reason="function breakpoint" and the breakpoint
    /// name available for the DAP `stopped` event.
    pub(crate) pending_function_bp: Option<String>,
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
    /// Source text keyed by canonical or synthetic module path for debugger retrieval.
    pub(crate) source_cache: BTreeMap<std::path::PathBuf, String>,
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
    /// Remaining instruction-boundary checks before a requested host
    /// cancellation is forcefully raised. This gives `is_cancelled()` loops a
    /// deterministic chance to return cleanly without letting non-cooperative
    /// CPU-bound code run forever.
    pub(crate) cancel_grace_instructions_remaining: Option<usize>,
    /// Captured stack trace from the most recent error (fn_name, line, col).
    pub(crate) error_stack_trace: Vec<(String, usize, usize, Option<String>)>,
    /// Yield channel sender for generator execution. When set, `Op::Yield`
    /// sends values through this channel instead of being a no-op.
    pub(crate) yield_sender: Option<tokio::sync::mpsc::Sender<Result<VmValue, VmError>>>,
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
    pub(crate) fn fresh_local_slots(chunk: &Chunk) -> Vec<LocalSlot> {
        chunk
            .local_slots
            .iter()
            .map(|_| LocalSlot {
                value: VmValue::Nil,
                initialized: false,
                synced: false,
            })
            .collect()
    }

    pub(crate) fn bind_param_slots(
        slots: &mut [LocalSlot],
        func: &crate::chunk::CompiledFunction,
        args: &[VmValue],
        synced: bool,
    ) {
        let default_start = func.default_start.unwrap_or(func.params.len());
        let param_count = func.params.len();
        for (i, _param) in func.params.iter().enumerate() {
            if i >= slots.len() {
                break;
            }
            if func.has_rest_param && i == param_count - 1 {
                let rest_args = if i < args.len() {
                    args[i..].to_vec()
                } else {
                    Vec::new()
                };
                slots[i].value = VmValue::List(Rc::new(rest_args));
                slots[i].initialized = true;
                slots[i].synced = synced;
            } else if i < args.len() {
                slots[i].value = args[i].clone();
                slots[i].initialized = true;
                slots[i].synced = synced;
            } else if i < default_start {
                slots[i].value = VmValue::Nil;
                slots[i].initialized = true;
                slots[i].synced = synced;
            }
        }
    }

    pub(crate) fn visible_variables(&self) -> BTreeMap<String, VmValue> {
        let mut vars = self.env.all_variables();
        let Some(frame) = self.frames.last() else {
            return vars;
        };
        for (slot, info) in frame.local_slots.iter().zip(frame.chunk.local_slots.iter()) {
            if slot.initialized && info.scope_depth <= frame.local_scope_depth {
                vars.insert(info.name.clone(), slot.value.clone());
            }
        }
        vars
    }

    pub(crate) fn sync_current_frame_locals_to_env(&mut self) {
        let Some(frame) = self.frames.last_mut() else {
            return;
        };
        let local_scope_base = frame.local_scope_base;
        let local_scope_depth = frame.local_scope_depth;
        let entries = frame
            .local_slots
            .iter_mut()
            .zip(frame.chunk.local_slots.iter())
            .filter_map(|(slot, info)| {
                if slot.initialized && !slot.synced && info.scope_depth <= local_scope_depth {
                    slot.synced = true;
                    Some((
                        local_scope_base + info.scope_depth,
                        info.name.clone(),
                        slot.value.clone(),
                        info.mutable,
                    ))
                } else {
                    None
                }
            })
            .collect::<Vec<_>>();
        for (scope_idx, name, value, mutable) in entries {
            while self.env.scopes.len() <= scope_idx {
                self.env.push_scope();
            }
            self.env.scopes[scope_idx]
                .vars
                .insert(name, (value, mutable));
        }
    }

    pub(crate) fn closure_call_env_for_current_frame(
        &self,
        closure: &crate::value::VmClosure,
    ) -> VmEnv {
        if closure.module_state.is_some() {
            return closure.env.clone();
        }
        let mut call_env = Self::closure_call_env(&self.env, closure);
        let Some(frame) = self.frames.last() else {
            return call_env;
        };
        for (slot, info) in frame
            .local_slots
            .iter()
            .zip(frame.chunk.local_slots.iter())
            .filter(|(slot, info)| slot.initialized && info.scope_depth <= frame.local_scope_depth)
        {
            if matches!(slot.value, VmValue::Closure(_)) && call_env.get(&info.name).is_none() {
                let _ = call_env.define(&info.name, slot.value.clone(), info.mutable);
            }
        }
        call_env
    }

    pub(crate) fn active_local_slot_value(&self, name: &str) -> Option<VmValue> {
        let frame = self.frames.last()?;
        for (idx, info) in frame.chunk.local_slots.iter().enumerate().rev() {
            if info.name == name && info.scope_depth <= frame.local_scope_depth {
                let slot = frame.local_slots.get(idx)?;
                if slot.initialized {
                    return Some(slot.value.clone());
                }
            }
        }
        None
    }

    pub(crate) fn assign_active_local_slot(
        &mut self,
        name: &str,
        value: VmValue,
        debug: bool,
    ) -> Result<bool, VmError> {
        let Some(frame) = self.frames.last_mut() else {
            return Ok(false);
        };
        for (idx, info) in frame.chunk.local_slots.iter().enumerate().rev() {
            if info.name == name && info.scope_depth <= frame.local_scope_depth {
                if !debug && !info.mutable {
                    return Err(VmError::ImmutableAssignment(name.to_string()));
                }
                if let Some(slot) = frame.local_slots.get_mut(idx) {
                    slot.value = value;
                    slot.initialized = true;
                    slot.synced = false;
                    return Ok(true);
                }
            }
        }
        Ok(false)
    }

    pub fn new() -> Self {
        Self {
            stack: Vec::with_capacity(256),
            env: VmEnv::new(),
            output: String::new(),
            builtins: BTreeMap::new(),
            async_builtins: BTreeMap::new(),
            builtins_by_id: BTreeMap::new(),
            builtin_id_collisions: HashSet::new(),
            iterators: Vec::new(),
            frames: Vec::new(),
            exception_handlers: Vec::new(),
            spawned_tasks: BTreeMap::new(),
            sync_runtime: Arc::new(crate::synchronization::VmSyncRuntime::new()),
            shared_state_runtime: Rc::new(crate::shared_state::VmSharedStateRuntime::new()),
            held_sync_guards: Vec::new(),
            task_counter: 0,
            runtime_context_counter: 0,
            runtime_context: crate::runtime_context::RuntimeContext::root(),
            deadlines: Vec::new(),
            breakpoints: BTreeMap::new(),
            function_breakpoints: std::collections::BTreeSet::new(),
            pending_function_bp: None,
            step_mode: false,
            step_frame_depth: 0,
            stopped: false,
            last_line: 0,
            source_dir: None,
            imported_paths: Vec::new(),
            module_cache: BTreeMap::new(),
            source_cache: BTreeMap::new(),
            source_file: None,
            source_text: None,
            bridge: None,
            denied_builtins: HashSet::new(),
            cancel_token: None,
            cancel_grace_instructions_remaining: None,
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
        self.source_cache
            .insert(std::path::PathBuf::from(file), text.to_string());
    }

    /// Initialize execution (push the initial frame).
    pub fn start(&mut self, chunk: &Chunk) {
        let initial_env = self.env.clone();
        self.frames.push(CallFrame {
            chunk: Rc::new(chunk.clone()),
            ip: 0,
            stack_base: self.stack.len(),
            saved_env: self.env.clone(),
            // The top-level pipeline frame captures env at start so
            // restartFrame on the outermost frame rewinds to the
            // pre-pipeline state — basically "restart session" in
            // debugger terms.
            initial_env: Some(initial_env),
            initial_local_slots: Some(Self::fresh_local_slots(chunk)),
            saved_iterator_depth: self.iterators.len(),
            fn_name: String::new(),
            argc: 0,
            saved_source_dir: None,
            module_functions: None,
            module_state: None,
            local_slots: Self::fresh_local_slots(chunk),
            local_scope_base: self.env.scope_depth().saturating_sub(1),
            local_scope_depth: 0,
        });
    }

    /// Create a child VM that shares builtins and env but has fresh execution state.
    /// Used for parallel/spawn to fork the VM for concurrent tasks.
    pub(crate) fn child_vm(&self) -> Vm {
        Vm {
            stack: Vec::with_capacity(64),
            env: self.env.clone(),
            output: String::new(),
            builtins: self.builtins.clone(),
            async_builtins: self.async_builtins.clone(),
            builtins_by_id: self.builtins_by_id.clone(),
            builtin_id_collisions: self.builtin_id_collisions.clone(),
            iterators: Vec::new(),
            frames: Vec::new(),
            exception_handlers: Vec::new(),
            spawned_tasks: BTreeMap::new(),
            sync_runtime: self.sync_runtime.clone(),
            shared_state_runtime: self.shared_state_runtime.clone(),
            held_sync_guards: Vec::new(),
            task_counter: 0,
            runtime_context_counter: self.runtime_context_counter,
            runtime_context: self.runtime_context.clone(),
            deadlines: self.deadlines.clone(),
            breakpoints: BTreeMap::new(),
            function_breakpoints: std::collections::BTreeSet::new(),
            pending_function_bp: None,
            step_mode: false,
            step_frame_depth: 0,
            stopped: false,
            last_line: 0,
            source_dir: self.source_dir.clone(),
            imported_paths: Vec::new(),
            module_cache: self.module_cache.clone(),
            source_cache: self.source_cache.clone(),
            source_file: self.source_file.clone(),
            source_text: self.source_text.clone(),
            bridge: self.bridge.clone(),
            denied_builtins: self.denied_builtins.clone(),
            cancel_token: self.cancel_token.clone(),
            cancel_grace_instructions_remaining: None,
            error_stack_trace: Vec::new(),
            yield_sender: None,
            project_root: self.project_root.clone(),
            globals: self.globals.clone(),
            debug_hook: None,
        }
    }

    /// Create a child VM for external adapters that need to invoke Harn
    /// closures while sharing the parent's builtins, globals, and module state.
    pub(crate) fn child_vm_for_host(&self) -> Vm {
        self.child_vm()
    }

    /// Request cancellation for every outstanding child task owned by this VM
    /// and then abort the join handles. This prevents un-awaited spawned tasks
    /// from outliving their parent execution scope.
    pub(crate) fn cancel_spawned_tasks(&mut self) {
        for (_, task) in std::mem::take(&mut self.spawned_tasks) {
            task.cancel_token
                .store(true, std::sync::atomic::Ordering::SeqCst);
            task.handle.abort();
        }
    }

    /// Set the source directory for import resolution and introspection.
    /// Also auto-detects the project root if not already set.
    pub fn set_source_dir(&mut self, dir: &std::path::Path) {
        let dir = crate::stdlib::process::normalize_context_path(dir);
        self.source_dir = Some(dir.clone());
        crate::stdlib::set_thread_source_dir(&dir);
        // Auto-detect project root if not explicitly set.
        if self.project_root.is_none() {
            self.project_root = crate::stdlib::process::find_project_root(&dir);
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

    pub(crate) fn pop(&mut self) -> Result<VmValue, VmError> {
        self.stack.pop().ok_or(VmError::StackUnderflow)
    }

    pub(crate) fn peek(&self) -> Result<&VmValue, VmError> {
        self.stack.last().ok_or(VmError::StackUnderflow)
    }

    pub(crate) fn const_string(c: &Constant) -> Result<String, VmError> {
        match c {
            Constant::String(s) => Ok(s.clone()),
            _ => Err(VmError::TypeError("expected string constant".into())),
        }
    }

    pub(crate) fn release_sync_guards_for_current_scope(&mut self) {
        let depth = self.env.scope_depth();
        self.held_sync_guards
            .retain(|guard| guard.env_scope_depth < depth);
    }

    pub(crate) fn release_sync_guards_after_unwind(
        &mut self,
        frame_depth: usize,
        env_scope_depth: usize,
    ) {
        self.held_sync_guards.retain(|guard| {
            guard.frame_depth <= frame_depth && guard.env_scope_depth <= env_scope_depth
        });
    }

    pub(crate) fn release_sync_guards_for_frame(&mut self, frame_depth: usize) {
        self.held_sync_guards
            .retain(|guard| guard.frame_depth != frame_depth);
    }
}

impl Drop for Vm {
    fn drop(&mut self) {
        self.cancel_spawned_tasks();
    }
}

impl Default for Vm {
    fn default() -> Self {
        Self::new()
    }
}
