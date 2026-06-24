use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use crate::chunk::{Chunk, ChunkRef, Constant};
use crate::runtime_limits::RuntimeLimits;
use crate::value::{
    ModuleFunctionRegistry, VmAsyncBuiltinFn, VmBuiltinFn, VmEnv, VmError, VmTaskHandle, VmValue,
};
use crate::BuiltinId;

use super::debug::DebugHook;
use super::modules::LoadedModule;
use super::VmBuiltinMetadata;

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

impl Drop for LocalSlot {
    fn drop(&mut self) {
        // Slot locals hold script values directly (e.g. a `var` bound to a
        // deeply nested list). When a frame is torn down, the default
        // recursive drop of such a value would overflow the native stack and
        // abort the process. For the overwhelmingly common scalar slot this is
        // a single `matches!` check and then the normal trivial drop; only a
        // nested container is moved out and torn down iteratively, so hot
        // frame teardown is unaffected.
        if crate::value::recursion::is_recursive_container(&self.value) {
            crate::value::recursion::dismantle(std::mem::replace(&mut self.value, VmValue::Nil));
        }
    }
}

#[derive(Clone)]
pub(crate) struct InterruptHandler {
    pub(crate) handle: i64,
    pub(crate) signals: Vec<String>,
    pub(crate) once: bool,
    pub(crate) graceful_timeout_ms: Option<u64>,
    pub(crate) handler: VmValue,
}

/// Call frame for function execution.
pub(crate) struct CallFrame {
    pub(crate) chunk: ChunkRef,
    /// VM-local inline-cache set for this frame's chunk. Computed once at
    /// frame entry so hot opcode dispatch can index cache feedback directly
    /// instead of hashing the chunk id on every cached opcode.
    pub(crate) inline_cache_set: usize,
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

pub(crate) struct InlineCacheSite {
    pub(crate) cache_set: usize,
    pub(crate) slot_count: usize,
    pub(crate) slot: Option<usize>,
}

impl CallFrame {
    #[inline]
    pub(crate) fn inline_cache_site_for_previous_op(&self) -> InlineCacheSite {
        let op_offset = self.ip.saturating_sub(1);
        InlineCacheSite {
            cache_set: self.inline_cache_set,
            slot_count: self.chunk.inline_cache_slot_count(),
            slot: self.chunk.inline_cache_slot(op_offset),
        }
    }
}

/// Exception handler for try/catch.
pub(crate) struct ExceptionHandler {
    pub(crate) catch_ip: usize,
    pub(crate) stack_depth: usize,
    pub(crate) frame_depth: usize,
    pub(crate) env_scope_depth: usize,
    /// When present, this catch only handles errors whose enum_name matches.
    pub(crate) error_type: Option<crate::value::HarnStr>,
}

/// A structured-concurrency nursery (`scope { }`). Tasks spawned while this
/// scope is innermost record their id here; `TaskScopeExit` joins them.
pub(crate) struct TaskScope {
    /// Ids of tasks spawned in this scope that have not been explicitly
    /// `await`ed away. Joined (normal exit) or cancelled (unwind) on close.
    pub(crate) task_ids: Vec<String>,
    /// Frame depth at which the scope was opened, for unwind pruning.
    pub(crate) frame_depth: usize,
    /// Env scope depth at open, for unwind pruning.
    pub(crate) env_scope_depth: usize,
}

/// Iterator state for for-in loops.
pub(crate) enum IterState {
    Vec {
        items: Arc<Vec<VmValue>>,
        idx: usize,
    },
    Dict {
        entries: Arc<crate::value::DictMap>,
        keys: Vec<String>,
        idx: usize,
    },
    Channel {
        receiver: std::sync::Arc<tokio::sync::Mutex<tokio::sync::mpsc::Receiver<VmValue>>>,
        close: std::sync::Arc<crate::value::VmChannelCloseState>,
    },
    Generator {
        gen: Arc<crate::value::VmGenerator>,
    },
    Stream {
        stream: Arc<crate::value::VmStream>,
    },
    /// Step through a lazy range without materializing a Vec.
    /// Inclusive ranges keep `end` as an actual value so `i64::MAX to i64::MAX`
    /// still yields one item instead of overflowing a one-past-end sentinel.
    Range {
        next: i64,
        end: i64,
        inclusive: bool,
        done: bool,
    },
    VmIter {
        handle: crate::vm::iter::VmIterHandle,
    },
}

#[derive(Clone)]
pub(crate) enum VmBuiltinDispatch {
    Sync(VmBuiltinFn),
    Async(VmAsyncBuiltinFn),
}

#[derive(Clone)]
pub(crate) struct VmBuiltinEntry {
    pub(crate) name: Arc<str>,
    pub(crate) dispatch: VmBuiltinDispatch,
}

/// The Harn bytecode virtual machine.
pub struct Vm {
    pub(crate) stack: Vec<VmValue>,
    pub(crate) env: VmEnv,
    pub(crate) output: String,
    pub(crate) builtins: Arc<BTreeMap<String, VmBuiltinFn>>,
    pub(crate) async_builtins: Arc<BTreeMap<String, VmAsyncBuiltinFn>>,
    pub(crate) builtin_metadata: Arc<BTreeMap<String, VmBuiltinMetadata>>,
    /// Numeric side index for builtins. Name-keyed maps remain authoritative;
    /// this index is the hot path for direct builtin bytecode and callback refs.
    pub(crate) builtins_by_id: Arc<HashMap<BuiltinId, VmBuiltinEntry>>,
    /// IDs with detected name collisions. Collided names safely fall back to
    /// the authoritative name-keyed lookup path.
    pub(crate) builtin_id_collisions: Arc<HashSet<BuiltinId>>,
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
    pub(crate) shared_state_runtime: Arc<crate::shared_state::VmSharedStateRuntime>,
    /// Per-isolate inline cache entries. `inline_cache_set_by_chunk` maps a
    /// compiled chunk identity to an index in this vector at frame entry; the
    /// dispatch loop uses the frame-local index for per-op reads/writes.
    pub(crate) inline_cache_sets: Vec<Vec<crate::chunk::InlineCacheEntry>>,
    pub(crate) inline_cache_set_by_chunk: HashMap<u64, usize>,
    /// VM-scoped pool registry inherited by child VMs and scoped into Tokio tasks.
    pub(crate) pool_registry: Arc<crate::stdlib::pool::PoolRegistry>,
    /// Shared task/channel wait graph for this VM execution tree.
    pub(crate) wait_for_graph: Arc<crate::wait_for_graph::VmWaitForGraph>,
    /// Permits acquired by lexical synchronization blocks in this VM.
    pub(crate) held_sync_guards: Vec<crate::synchronization::VmSyncHeldGuard>,
    /// Locks held by an ancestor VM that is *suspended on this VM's execution*:
    /// an inline async-builtin child runs while its parent is parked
    /// mid-instruction still holding these permits. Re-acquiring more permits
    /// than the primitive can grant is a provably-unresolvable self-deadlock, so
    /// HARN-ORC-011 fires across the child boundary. Empty for new concurrent
    /// tasks (`spawn`/`parallel`/triggers), where the parent keeps running and
    /// blocking can be legitimately resolvable.
    pub(crate) inherited_held_keys: Arc<Vec<crate::synchronization::VmSyncHeldKey>>,
    /// Structured-concurrency nursery stack. Each `scope { }` block pushes a
    /// `TaskScope`; tasks spawned while it is innermost register their id here.
    /// On normal exit (`TaskScopeExit`) the scope's tasks are joined and the
    /// first error propagates; on unwind they are cancelled. Modeled on
    /// `held_sync_guards` (push on enter, prune/cancel on frame/handler exit).
    pub(crate) task_scopes: Vec<TaskScope>,
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
    /// Imports that hit an in-progress module (an import cycle) and so could
    /// not bind inline. Drained by `flush_deferred_cyclic_imports` once the
    /// involved modules finish loading.
    pub(crate) deferred_cyclic_imports: Vec<super::modules::DeferredCyclicImport>,
    /// Loaded module cache keyed by canonical or synthetic module path.
    pub(crate) module_cache: Arc<BTreeMap<std::path::PathBuf, LoadedModule>>,
    /// Source text keyed by canonical or synthetic module path for debugger retrieval.
    pub(crate) source_cache: Arc<BTreeMap<std::path::PathBuf, String>>,
    /// Source file path for error reporting.
    pub(crate) source_file: Option<String>,
    /// Source text for error reporting.
    pub(crate) source_text: Option<String>,
    /// Line-coverage accumulator. `Some` only while a coverage session is
    /// active (see [`crate::coverage`]); folded into the global report on drop.
    pub(crate) coverage: Option<crate::coverage::Coverage>,
    /// Optional bridge for delegating unknown builtins in bridge mode.
    pub(crate) bridge: Option<Arc<crate::bridge::HostBridge>>,
    /// Builtins denied by sandbox mode (`--deny` / `--allow` flags).
    pub(crate) denied_builtins: Arc<HashSet<String>>,
    /// Cancellation token for cooperative graceful shutdown (set by parent).
    pub(crate) cancel_token: Option<std::sync::Arc<std::sync::atomic::AtomicBool>>,
    pub(crate) interrupt_signal_token: Option<std::sync::Arc<std::sync::Mutex<Option<String>>>>,
    /// Remaining instruction-boundary checks before a requested host
    /// cancellation is forcefully raised. This gives `is_cancelled()` loops a
    /// deterministic chance to return cleanly without letting non-cooperative
    /// CPU-bound code run forever.
    pub(crate) cancel_grace_instructions_remaining: Option<usize>,
    /// User-visible interrupt handlers registered through `std/signal`.
    pub(crate) interrupt_handlers: Vec<InterruptHandler>,
    pub(crate) next_interrupt_handle: i64,
    pub(crate) pending_interrupt_signal: Option<String>,
    pub(crate) interrupted: bool,
    pub(crate) dispatching_interrupt: bool,
    pub(crate) interrupt_handler_deadline: Option<Instant>,
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
    pub(crate) globals: Arc<crate::value::DictMap>,
    /// Optional debugger hook invoked when execution advances to a new source line.
    pub(crate) debug_hook: Option<parking_lot::Mutex<Box<DebugHook>>>,
    /// Effective runtime ceilings for this VM execution.
    pub(crate) runtime_limits: RuntimeLimits,
}

/// Reusable VM baseline for hosts that need many clean executions with the
/// same stable builtin/source setup.
///
/// The baseline intentionally does not snapshot execution state. Each
/// instantiation gets fresh stacks, frames, tasks, cancellation fields, sync
/// primitives, shared cells/maps/mailboxes, and debug state. Builtin tables are
/// shared through `Arc` until a per-execution rebind needs copy-on-write.
#[derive(Clone)]
pub struct VmBaseline {
    builtins: Arc<BTreeMap<String, VmBuiltinFn>>,
    async_builtins: Arc<BTreeMap<String, VmAsyncBuiltinFn>>,
    builtin_metadata: Arc<BTreeMap<String, VmBuiltinMetadata>>,
    builtins_by_id: Arc<HashMap<BuiltinId, VmBuiltinEntry>>,
    builtin_id_collisions: Arc<HashSet<BuiltinId>>,
    source_dir: Option<std::path::PathBuf>,
    source_file: Option<String>,
    source_text: Option<String>,
    project_root: Option<std::path::PathBuf>,
    globals: Arc<crate::value::DictMap>,
    denied_builtins: Arc<HashSet<String>>,
    runtime_limits: RuntimeLimits,
}

impl VmBaseline {
    pub fn from_vm(vm: &Vm) -> Self {
        Self {
            builtins: Arc::clone(&vm.builtins),
            async_builtins: Arc::clone(&vm.async_builtins),
            builtin_metadata: Arc::clone(&vm.builtin_metadata),
            builtins_by_id: Arc::clone(&vm.builtins_by_id),
            builtin_id_collisions: Arc::clone(&vm.builtin_id_collisions),
            source_dir: vm.source_dir.clone(),
            source_file: vm.source_file.clone(),
            source_text: vm.source_text.clone(),
            project_root: vm.project_root.clone(),
            globals: Arc::clone(&vm.globals),
            denied_builtins: Arc::clone(&vm.denied_builtins),
            runtime_limits: vm.runtime_limits,
        }
    }

    pub fn instantiate(&self) -> Vm {
        let mut source_cache = BTreeMap::new();
        if let (Some(file), Some(text)) = (&self.source_file, &self.source_text) {
            source_cache.insert(std::path::PathBuf::from(file), text.clone());
        }
        if let Some(dir) = &self.source_dir {
            crate::stdlib::set_thread_source_dir(dir);
        }

        let mut vm = Vm {
            stack: Vec::with_capacity(256),
            env: VmEnv::new(),
            output: String::new(),
            builtins: Arc::clone(&self.builtins),
            async_builtins: Arc::clone(&self.async_builtins),
            builtin_metadata: Arc::clone(&self.builtin_metadata),
            builtins_by_id: Arc::clone(&self.builtins_by_id),
            builtin_id_collisions: Arc::clone(&self.builtin_id_collisions),
            iterators: Vec::new(),
            frames: Vec::new(),
            exception_handlers: Vec::new(),
            spawned_tasks: BTreeMap::new(),
            sync_runtime: Arc::new(crate::synchronization::VmSyncRuntime::new()),
            shared_state_runtime: Arc::new(crate::shared_state::VmSharedStateRuntime::new()),
            inline_cache_sets: Vec::new(),
            inline_cache_set_by_chunk: HashMap::new(),
            pool_registry: crate::stdlib::pool::new_pool_registry(),
            wait_for_graph: Arc::new(crate::wait_for_graph::VmWaitForGraph::new()),
            held_sync_guards: Vec::new(),
            inherited_held_keys: Arc::new(Vec::new()),
            task_scopes: Vec::new(),
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
            source_dir: self.source_dir.clone(),
            imported_paths: Vec::new(),
            deferred_cyclic_imports: Vec::new(),
            module_cache: Arc::new(BTreeMap::new()),
            source_cache: Arc::new(source_cache),
            source_file: self.source_file.clone(),
            source_text: self.source_text.clone(),
            coverage: crate::coverage::for_primary(self.source_file.as_deref()),
            bridge: None,
            denied_builtins: Arc::clone(&self.denied_builtins),
            cancel_token: None,
            interrupt_signal_token: None,
            cancel_grace_instructions_remaining: None,
            interrupt_handlers: Vec::new(),
            next_interrupt_handle: 1,
            pending_interrupt_signal: None,
            interrupted: false,
            dispatching_interrupt: false,
            interrupt_handler_deadline: None,
            error_stack_trace: Vec::new(),
            yield_sender: None,
            project_root: self.project_root.clone(),
            globals: Arc::clone(&self.globals),
            debug_hook: None,
            runtime_limits: self.runtime_limits,
        };

        crate::stdlib::rebind_execution_state_builtins(&mut vm);
        vm
    }
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
        Self::bind_param_slots_args(slots, func, &super::CallArgs::Slice(args), synced);
    }

    pub(crate) fn bind_param_slots_args(
        slots: &mut [LocalSlot],
        func: &crate::chunk::CompiledFunction,
        args: &super::CallArgs<'_>,
        synced: bool,
    ) {
        let param_count = func.params.len();
        for (i, _param) in func.params.iter().enumerate() {
            if i >= slots.len() {
                break;
            }
            if func.has_rest_param && i == param_count - 1 {
                let rest_args = args.to_vec_from(i);
                slots[i].value = VmValue::List(std::sync::Arc::new(rest_args));
                slots[i].initialized = true;
                slots[i].synced = synced;
            } else if let Some(arg) = args.get(i) {
                slots[i].value = arg.clone();
                slots[i].initialized = true;
                slots[i].synced = synced;
            }
        }
    }

    pub(crate) fn visible_variables(&self) -> crate::value::DictMap {
        let mut vars = self.env.all_variables();
        let Some(frame) = self.frames.last() else {
            return vars;
        };
        for (slot, info) in frame.local_slots.iter().zip(frame.chunk.local_slots.iter()) {
            if slot.initialized && info.scope_depth <= frame.local_scope_depth {
                vars.insert(crate::value::intern_key(&info.name), slot.value.clone());
            }
        }
        vars
    }

    pub(crate) fn sync_current_frame_locals_to_env(&mut self) {
        let frames = &mut self.frames;
        let env = &mut self.env;
        let Some(frame) = frames.last_mut() else {
            return;
        };
        let local_scope_base = frame.local_scope_base;
        let local_scope_depth = frame.local_scope_depth;
        for (slot, info) in frame
            .local_slots
            .iter_mut()
            .zip(frame.chunk.local_slots.iter())
        {
            if slot.initialized && !slot.synced && info.scope_depth <= local_scope_depth {
                slot.synced = true;
                let scope_idx = local_scope_base + info.scope_depth;
                while env.scopes.len() <= scope_idx {
                    env.push_scope();
                }
                Arc::make_mut(&mut env.scopes[scope_idx].vars)
                    .insert(info.name.clone(), (slot.value.clone(), info.mutable));
            }
        }
    }

    pub(crate) fn closure_call_env_for_current_frame(
        &self,
        closure: &crate::value::VmClosure,
    ) -> VmEnv {
        if closure.module_state().is_some() {
            return closure.env.cloned_for_call();
        }
        let call_env = Self::closure_call_env(&self.env, closure);
        // Same compile-time short-circuit as the env walk in
        // `closure_call_env`: when the callee body never resolves an
        // outer name through the env, injecting closure-typed *slot*
        // locals from the caller's frame is wasted work too.
        if !closure.func.chunk.references_outer_names {
            return call_env;
        }
        let mut call_env = call_env;
        let Some(frame) = self.frames.last() else {
            return call_env;
        };
        for (slot, info) in frame
            .local_slots
            .iter()
            .zip(frame.chunk.local_slots.iter())
            .filter(|(slot, info)| slot.initialized && info.scope_depth <= frame.local_scope_depth)
        {
            if matches!(slot.value, VmValue::Closure(_)) && !call_env.contains(&info.name) {
                let _ = call_env.define(&info.name, slot.value.clone(), info.mutable);
            }
        }
        call_env
    }

    pub(crate) fn active_local_slot_value(&self, name: &str) -> Option<VmValue> {
        let frame = self.frames.last()?;
        let idx = self.active_local_slot_index(name)?;
        frame.local_slots.get(idx).map(|slot| slot.value.clone())
    }

    /// Returns the slot index of an initialized active local with the given
    /// name, walking from innermost to outermost scope. Used by legacy by-name
    /// hot paths that still want to mutate the slot value in place without
    /// paying a defensive `VmValue::clone` first.
    pub(crate) fn active_local_slot_index(&self, name: &str) -> Option<usize> {
        let frame = self.frames.last()?;
        for (idx, info) in frame.chunk.local_slots.iter().enumerate().rev() {
            if info.name == name && info.scope_depth <= frame.local_scope_depth {
                if let Some(slot) = frame.local_slots.get(idx) {
                    if slot.initialized {
                        return Some(idx);
                    }
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
                    crate::value::recursion::dismantle(std::mem::replace(&mut slot.value, value));
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
            builtins: Arc::new(BTreeMap::new()),
            async_builtins: Arc::new(BTreeMap::new()),
            builtin_metadata: Arc::new(BTreeMap::new()),
            builtins_by_id: Arc::new(HashMap::new()),
            builtin_id_collisions: Arc::new(HashSet::new()),
            iterators: Vec::new(),
            frames: Vec::new(),
            exception_handlers: Vec::new(),
            spawned_tasks: BTreeMap::new(),
            sync_runtime: Arc::new(crate::synchronization::VmSyncRuntime::new()),
            shared_state_runtime: Arc::new(crate::shared_state::VmSharedStateRuntime::new()),
            inline_cache_sets: Vec::new(),
            inline_cache_set_by_chunk: HashMap::new(),
            pool_registry: crate::stdlib::pool::new_pool_registry(),
            wait_for_graph: Arc::new(crate::wait_for_graph::VmWaitForGraph::new()),
            held_sync_guards: Vec::new(),
            inherited_held_keys: Arc::new(Vec::new()),
            task_scopes: Vec::new(),
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
            deferred_cyclic_imports: Vec::new(),
            module_cache: Arc::new(BTreeMap::new()),
            source_cache: Arc::new(BTreeMap::new()),
            source_file: None,
            source_text: None,
            coverage: crate::coverage::for_primary(None),
            bridge: None,
            denied_builtins: Arc::new(HashSet::new()),
            cancel_token: None,
            interrupt_signal_token: None,
            cancel_grace_instructions_remaining: None,
            interrupt_handlers: Vec::new(),
            next_interrupt_handle: 1,
            pending_interrupt_signal: None,
            interrupted: false,
            dispatching_interrupt: false,
            interrupt_handler_deadline: None,
            error_stack_trace: Vec::new(),
            yield_sender: None,
            project_root: None,
            globals: Arc::new(crate::value::DictMap::new()),
            debug_hook: None,
            runtime_limits: RuntimeLimits::default(),
        }
    }

    pub fn baseline(&self) -> VmBaseline {
        VmBaseline::from_vm(self)
    }

    /// Return the effective runtime limit profile for this VM.
    pub fn runtime_limits(&self) -> RuntimeLimits {
        self.runtime_limits
    }

    /// Return a host/debug report describing the VM's effective runtime limits.
    pub fn runtime_limit_report(&self) -> crate::RuntimeLimitsReport {
        self.runtime_limits.report()
    }

    /// Returns true if any debugging affordance is active — DAP hook,
    /// line breakpoints, or function breakpoints. Call-site code uses
    /// this to decide whether to capture per-frame restart snapshots
    /// (`initial_env`, `initial_local_slots`); without a debugger those
    /// snapshots are dead weight, so skipping them removes two
    /// allocations from every function call hot path.
    ///
    /// All three signals are stable across a function call's lifetime
    /// (they're set before pipeline execution starts), so the gate is
    /// consistent between frame creation and any later `restart_frame`
    /// invocation. The three `is_empty` checks compile to a handful of
    /// branch-predicted memory probes — cheaper than a single
    /// `BTreeMap` clone, which is what we're avoiding.
    #[inline]
    pub(crate) fn debugger_attached(&self) -> bool {
        self.debug_hook.is_some()
            || !self.breakpoints.is_empty()
            || !self.function_breakpoints.is_empty()
    }

    /// Set the bridge for delegating unknown builtins in bridge mode.
    pub fn set_bridge(&mut self, bridge: Arc<crate::bridge::HostBridge>) {
        self.bridge = Some(bridge);
    }

    /// Set builtins that are denied in sandbox mode.
    /// When called, the given builtin names will produce a permission error.
    pub fn set_denied_builtins(&mut self, denied: HashSet<String>) {
        self.denied_builtins = Arc::new(denied);
    }

    /// Set source info for error reporting (file path and source text).
    pub fn set_source_info(&mut self, file: &str, text: &str) {
        self.source_file = Some(file.to_string());
        self.source_text = Some(text.to_string());
        if let Some(cov) = self.coverage.as_mut() {
            cov.set_primary_file(file);
        }
        Arc::make_mut(&mut self.source_cache)
            .insert(std::path::PathBuf::from(file), text.to_string());
    }

    /// Initialize execution (push the initial frame).
    pub fn start(&mut self, chunk: &Chunk) {
        // The top-level pipeline frame captures env at start so
        // restartFrame on the outermost frame rewinds to the
        // pre-pipeline state — basically "restart session" in
        // debugger terms. Skipped when no debugger is attached:
        // the snapshot is dead weight in that case and dominates
        // call-overhead bench numbers (~5-10%).
        let debugger = self.debugger_attached();
        let initial_env = if debugger {
            Some(self.env.clone())
        } else {
            None
        };
        let initial_local_slots = if debugger {
            Some(Self::fresh_local_slots(chunk))
        } else {
            None
        };
        let chunk = Arc::new(chunk.clone());
        let local_slots = Self::fresh_local_slots(&chunk);
        let inline_cache_set = self.inline_cache_set_index_for_chunk(&chunk);
        self.frames.push(CallFrame {
            chunk,
            inline_cache_set,
            ip: 0,
            stack_base: self.stack.len(),
            saved_env: self.env.clone(),
            initial_env,
            initial_local_slots,
            saved_iterator_depth: self.iterators.len(),
            fn_name: String::new(),
            argc: 0,
            saved_source_dir: None,
            module_functions: None,
            module_state: None,
            local_slots,
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
            builtins: Arc::clone(&self.builtins),
            async_builtins: Arc::clone(&self.async_builtins),
            builtin_metadata: Arc::clone(&self.builtin_metadata),
            builtins_by_id: Arc::clone(&self.builtins_by_id),
            builtin_id_collisions: Arc::clone(&self.builtin_id_collisions),
            iterators: Vec::new(),
            frames: Vec::new(),
            exception_handlers: Vec::new(),
            spawned_tasks: BTreeMap::new(),
            sync_runtime: self.sync_runtime.clone(),
            shared_state_runtime: self.shared_state_runtime.clone(),
            inline_cache_sets: Vec::new(),
            inline_cache_set_by_chunk: HashMap::new(),
            pool_registry: self.pool_registry.clone(),
            wait_for_graph: self.wait_for_graph.clone(),
            held_sync_guards: Vec::new(),
            inherited_held_keys: Arc::new(Vec::new()),
            task_scopes: Vec::new(),
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
            deferred_cyclic_imports: Vec::new(),
            module_cache: Arc::clone(&self.module_cache),
            source_cache: Arc::clone(&self.source_cache),
            source_file: self.source_file.clone(),
            source_text: self.source_text.clone(),
            coverage: crate::coverage::for_primary(self.source_file.as_deref()),
            bridge: self.bridge.clone(),
            denied_builtins: Arc::clone(&self.denied_builtins),
            cancel_token: self.cancel_token.clone(),
            interrupt_signal_token: self.interrupt_signal_token.clone(),
            cancel_grace_instructions_remaining: None,
            interrupt_handlers: Vec::new(),
            next_interrupt_handle: 1,
            pending_interrupt_signal: None,
            interrupted: self.interrupted,
            dispatching_interrupt: false,
            interrupt_handler_deadline: None,
            error_stack_trace: Vec::new(),
            yield_sender: None,
            project_root: self.project_root.clone(),
            globals: Arc::clone(&self.globals),
            debug_hook: None,
            runtime_limits: self.runtime_limits,
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

    /// Return discoverable metadata for registered builtins.
    pub fn builtin_metadata(&self) -> Vec<VmBuiltinMetadata> {
        self.builtin_metadata.values().cloned().collect()
    }

    /// Return discoverable metadata for a registered builtin name.
    pub fn builtin_metadata_for(&self, name: &str) -> Option<&VmBuiltinMetadata> {
        self.builtin_metadata.get(name)
    }

    /// Set a global constant (e.g. `pi`, `e`).
    /// Stored separately from the environment so user-defined variables can shadow them.
    pub fn set_global(&mut self, name: &str, value: VmValue) {
        Arc::make_mut(&mut self.globals).insert(crate::value::intern_key(name), value);
    }

    /// Read a previously-installed global (the value `set_global` /
    /// `set_harness` recorded). Returns `None` for unknown names.
    /// Hosts use this to look up runtime-installed capability handles
    /// (e.g. the `harness` slot) without having to track them
    /// separately.
    pub fn global(&self, name: &str) -> Option<&VmValue> {
        self.globals.get(name)
    }

    /// Install the script's `Harness` capability handle as the `harness`
    /// global so the auto-call emitted by `Compiler::compile()` (for
    /// `fn main(harness: Harness)` entrypoints) can read it. Hosts that
    /// drive the VM directly (CLI, MCP server, composition runtime) call
    /// this once before `execute()`.
    pub fn set_harness(&mut self, harness: crate::harness::Harness) {
        self.set_global("harness", harness.into_vm_value());
    }

    /// Get the captured output.
    pub fn output(&self) -> &str {
        &self.output
    }

    /// Drain and return the captured output, leaving the buffer empty.
    /// Used by the async-builtin dispatch path to forward closure output
    /// from a child VM back to its parent.
    pub fn take_output(&mut self) -> String {
        std::mem::take(&mut self.output)
    }

    /// Append text to this VM's captured output. Used to forward output
    /// from child VMs (e.g. closures invoked via `call_closure_pub`)
    /// back into the parent stream.
    pub fn append_output(&mut self, text: &str) {
        self.output.push_str(text);
    }

    pub(crate) fn pop(&mut self) -> Result<VmValue, VmError> {
        self.stack.pop().ok_or(VmError::StackUnderflow)
    }

    pub(crate) fn peek(&self) -> Result<&VmValue, VmError> {
        self.stack.last().ok_or(VmError::StackUnderflow)
    }

    pub(crate) fn const_str(c: &Constant) -> Result<&str, VmError> {
        match c {
            Constant::String(s) => Ok(s.as_str()),
            _ => Err(VmError::TypeError("expected string constant".into())),
        }
    }

    pub(crate) fn release_sync_guards_for_current_scope(&mut self) {
        let depth = self.env.scope_depth();
        self.held_sync_guards
            .retain(|guard| guard.env_scope_depth < depth);
        // A `scope { }` torn down without a normal `TaskScopeExit` (break /
        // continue out of it) leaves a dangling nursery — cancel its tasks.
        self.cancel_task_scopes_where(|s| s.env_scope_depth >= depth);
    }

    pub(crate) fn release_sync_guards_after_unwind(
        &mut self,
        frame_depth: usize,
        env_scope_depth: usize,
    ) {
        self.held_sync_guards.retain(|guard| {
            guard.frame_depth <= frame_depth && guard.env_scope_depth <= env_scope_depth
        });
        // Cancel nurseries opened above the catch handler (a `throw` unwound
        // past their `TaskScopeExit`).
        self.cancel_task_scopes_where(|s| {
            !(s.frame_depth <= frame_depth && s.env_scope_depth <= env_scope_depth)
        });
    }

    pub(crate) fn release_sync_guards_for_frame(&mut self, frame_depth: usize) {
        self.held_sync_guards
            .retain(|guard| guard.frame_depth != frame_depth);
        // Cancel any nursery whose `scope {}` block belonged to the frame being
        // torn down (e.g. a `return` jumped past its `TaskScopeExit`).
        self.cancel_task_scopes_where(|s| s.frame_depth == frame_depth);
    }

    pub(crate) fn adopt_sync_permit_for_current_scope(
        &mut self,
        permit: crate::value::VmSyncPermitHandle,
    ) {
        if permit.is_released()
            || self
                .held_sync_guards
                .iter()
                .any(|guard| guard._permit.same_lease(&permit))
        {
            return;
        }
        self.held_sync_guards
            .push(crate::synchronization::VmSyncHeldGuard {
                _permit: permit,
                frame_depth: self.frames.len(),
                env_scope_depth: self.env.scope_depth(),
            });
    }

    /// Deregister a task id from every open nursery (it was explicitly
    /// `await`ed, so it must not be double-joined or cancelled at scope exit).
    pub(crate) fn deregister_task_from_scopes(&mut self, id: &str) {
        for scope in &mut self.task_scopes {
            scope.task_ids.retain(|t| t != id);
        }
    }

    /// Cancel and remove every task scope matching `doomed`, aborting its bound
    /// tasks (used when a `scope {}` is torn down without a normal join).
    fn cancel_task_scopes_where<F: Fn(&TaskScope) -> bool>(&mut self, doomed: F) {
        let mut i = 0;
        while i < self.task_scopes.len() {
            if doomed(&self.task_scopes[i]) {
                let scope = self.task_scopes.remove(i);
                for id in &scope.task_ids {
                    if let Some(task) = self.spawned_tasks.remove(id) {
                        task.cancel_token
                            .store(true, std::sync::atomic::Ordering::SeqCst);
                        task.handle.abort();
                    }
                }
            } else {
                i += 1;
            }
        }
    }

    /// Total live permits this VM already holds for `kind:key`. The held-set is
    /// tiny (bounded by lexical nesting and explicit sync acquisitions), so this
    /// scan is cheap and only runs on the rare blocking-acquire path.
    pub(crate) fn held_permits_for(&self, kind: &str, key: &str) -> u32 {
        let own: u32 = self
            .held_sync_guards
            .iter()
            .filter(|guard| {
                !guard._permit.is_released()
                    && guard._permit.kind() == kind
                    && guard._permit.key() == key
            })
            .map(|guard| guard._permit.permits())
            .sum();
        let inherited: u32 = self
            .inherited_held_keys
            .iter()
            .filter(|held| held.kind == kind && held.key == key)
            .map(|held| held.permits)
            .sum();
        own + inherited
    }

    /// Every live sync permit held by this VM *and* its suspended ancestors: the
    /// transitive held-set seen by an inline child.
    pub(crate) fn combined_held_keys(&self) -> Vec<crate::synchronization::VmSyncHeldKey> {
        let mut keys: Vec<crate::synchronization::VmSyncHeldKey> = self
            .held_sync_guards
            .iter()
            .filter_map(|guard| crate::synchronization::VmSyncHeldKey::from_permit(&guard._permit))
            .collect();
        keys.extend(self.inherited_held_keys.iter().cloned());
        keys
    }

    /// Clone a child VM for an **inline, same-task** execution (an async builtin
    /// awaited while this VM is parked, or a user closure that builtin runs and
    /// awaits). The child inherits this VM's transitive held-lock keys so a
    /// re-acquire of a parent-held lock is caught as a self-deadlock
    /// (HARN-ORC-011). Use plain `child_vm()` for new concurrent tasks.
    pub(crate) fn child_vm_inline(&self) -> Vm {
        let mut child = self.child_vm();
        child.inherited_held_keys = Arc::new(self.combined_held_keys());
        child
    }
}

impl Drop for Vm {
    fn drop(&mut self) {
        if let Some(coverage) = self.coverage.take() {
            crate::coverage::merge_into_global(coverage);
        }
        self.cancel_spawned_tasks();
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

    fn baseline_with_stdlib(source: &str) -> VmBaseline {
        let mut vm = Vm::new();
        crate::register_vm_stdlib(&mut vm);
        vm.set_source_info("baseline_test.harn", source);
        vm.set_global(
            "stable_global",
            VmValue::String(arcstr::ArcStr::from("baseline")),
        );
        vm.baseline()
    }

    #[test]
    fn vm_baseline_instantiates_clean_mutable_execution_state() {
        let baseline = baseline_with_stdlib("pipeline main() { __io_println(stable_global) }");

        let mut dirty = baseline.instantiate();
        dirty.stack.push(VmValue::Int(42));
        dirty.output.push_str("dirty");
        dirty.task_counter = 9;
        dirty.runtime_context_counter = 7;
        dirty
            .error_stack_trace
            .push(("main".to_string(), 1, 1, None));

        let clean = baseline.instantiate();
        assert!(clean.stack.is_empty());
        assert!(clean.output.is_empty());
        assert!(clean.frames.is_empty());
        assert!(clean.exception_handlers.is_empty());
        assert!(clean.spawned_tasks.is_empty());
        assert!(clean.held_sync_guards.is_empty());
        assert_eq!(clean.task_counter, 0);
        assert_eq!(clean.runtime_context_counter, 0);
        assert!(clean.deadlines.is_empty());
        assert!(clean.cancel_token.is_none());
        assert!(clean.interrupt_handlers.is_empty());
        assert!(clean.error_stack_trace.is_empty());
        assert!(clean.bridge.is_none());
        assert!(clean
            .globals
            .get("stable_global")
            .is_some_and(|value| value.display() == "baseline"));
    }

    #[tokio::test]
    async fn inline_child_inherits_held_lock_keys_but_concurrent_child_does_not() {
        let mut parent = Vm::new();
        let permit = parent
            .sync_runtime
            .acquire("mutex", "v:test", 1, 1, None, None)
            .await
            .unwrap()
            .unwrap();
        parent
            .held_sync_guards
            .push(crate::synchronization::VmSyncHeldGuard {
                _permit: permit,
                frame_depth: 0,
                env_scope_depth: 0,
            });
        assert_eq!(parent.held_permits_for("mutex", "v:test"), 1);

        // An inline child (async builtin awaited while the parent is parked, or
        // a closure the builtin runs inline) inherits the held key, so a
        // re-acquire is caught as a cross-context self-deadlock (HARN-ORC-011)
        // — even transitively through a further inline child.
        let inline = parent.child_vm_inline();
        assert_eq!(inline.held_permits_for("mutex", "v:test"), 1);
        assert_eq!(
            inline.child_vm_inline().held_permits_for("mutex", "v:test"),
            1
        );

        // A new concurrent task (spawn / parallel / trigger) does NOT inherit:
        // blocking on a parent-held lock there is legitimately resolvable, so
        // flagging it would be a false positive.
        let concurrent = parent.child_vm();
        assert_eq!(concurrent.held_permits_for("mutex", "v:test"), 0);
    }

    #[test]
    fn vm_reports_effective_runtime_limits() {
        let vm = Vm::new();

        assert_eq!(vm.runtime_limits(), RuntimeLimits::default());
        assert_eq!(
            vm.runtime_limit_report().entries.len(),
            crate::RUNTIME_LIMIT_DESCRIPTIONS.len()
        );
        assert_eq!(vm.child_vm().runtime_limits(), vm.runtime_limits());
        assert_eq!(
            vm.baseline().instantiate().runtime_limits(),
            vm.runtime_limits()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn vm_baseline_rebinds_shared_state_builtins_per_instance() {
        let local = tokio::task::LocalSet::new();
        local
            .run_until(async {
                let source = r#"
pipeline main() {
  let cell = shared_cell({scope: "task_group", key: "turn", initial: 0})
  __io_println(shared_get(cell))
  shared_set(cell, shared_get(cell) + 1)
}"#;
                let chunk = crate::compile_source(source).expect("compile");
                let baseline = baseline_with_stdlib(source);

                let mut first = baseline.instantiate();
                first.execute(&chunk).await.expect("first execute");
                assert_eq!(first.output(), "0\n");

                let mut second = baseline.instantiate();
                second.execute(&chunk).await.expect("second execute");
                assert_eq!(
                    second.output(),
                    "0\n",
                    "shared state created by the first VM must not leak into the next baseline instance"
                );
            })
            .await;
    }
}
