use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Duration;

use harn_vm::{DebugState, Vm, VmValue};

use crate::host_bridge::DapHostBridge;

/// Execution state for stepping.
#[derive(Debug, Clone, PartialEq)]
pub enum StepMode {
    /// Run until a breakpoint or end.
    Continue,
    /// Stop at the next line.
    StepOver,
    /// Stop at the next statement (step into functions).
    StepIn,
    /// Run until returning from the current function.
    StepOut,
}

/// Program state.
pub(crate) enum ProgramState {
    /// Not yet started.
    NotStarted,
    /// Running (VM is initialized).
    Running,
    /// Stopped at a debug point.
    Stopped,
    /// Program has terminated.
    Terminated,
}

/// A segment in an expression path for evaluation.
pub(crate) enum PathSegment {
    Field(String),
    Index(i64),
}

/// The debug adapter implementation.
pub struct Debugger {
    pub(crate) seq: i64,
    pub(crate) source_path: Option<String>,
    pub(crate) source_content: Option<String>,
    /// DAP sourceReference -> source key.
    pub(crate) source_refs: BTreeMap<i64, String>,
    /// Source key -> DAP sourceReference for stable stack frame/source requests.
    pub(crate) source_ref_by_path: BTreeMap<String, i64>,
    /// Next source reference ID. Starts above variable refs to keep logs readable.
    pub(crate) next_source_ref: i64,
    pub(crate) breakpoints: Vec<crate::protocol::Breakpoint>,
    pub(crate) next_bp_id: i64,
    pub(crate) vm: Option<Vm>,
    /// Variables captured at the current stop point.
    pub(crate) variables: BTreeMap<String, VmValue>,
    /// Current execution state.
    pub(crate) stopped: bool,
    /// Current line in the source.
    pub(crate) current_line: i64,
    /// Step mode.
    pub(crate) step_mode: StepMode,
    /// Output captured during execution.
    pub(crate) output: String,
    /// Program state.
    pub(crate) program_state: ProgramState,
    /// Structured variable references: reference_id -> children
    pub(crate) var_refs: BTreeMap<i64, Vec<(String, VmValue)>>,
    /// Tokio runtime for async VM execution.
    ///
    /// Built lazily so protocol-only debugger sessions and unit tests do
    /// not open runtime I/O/timer descriptors they never use.
    pub(crate) runtime: Option<tokio::runtime::Runtime>,
    /// Next variable reference ID (start at 100 to avoid conflict with scope refs).
    pub(crate) next_var_ref: i64,
    /// Whether to break on thrown exceptions.
    pub(crate) break_on_exceptions: bool,
    /// Active per-kind exception-filter set (#111). Populated by
    /// setExceptionBreakpoints; consulted when the agent loop emits
    /// a typed exception (tool_error, llm_refusal, budget_exceeded,
    /// parse_failure). Optional per-filter condition lives alongside
    /// so a filter can gate on e.g. `err.kind == "disk_full"`.
    pub(crate) exception_filters: BTreeMap<String, Option<String>>,
    /// Latest VM debug snapshot captured through the VM debug hook.
    pub(crate) latest_debug_state: Rc<RefCell<Option<DebugState>>>,
    /// Optional bridge that round-trips unhandled `host_call` ops to the
    /// DAP client as reverse requests. When `None`, scripts only see the
    /// harn-vm fallbacks.
    pub(crate) host_bridge: Option<DapHostBridge>,
    /// True when the VM is in a "should keep stepping" state (after
    /// continue/next/stepIn/stepOut/configurationDone). The main loop
    /// drives one VM step per iteration while this is set, draining any
    /// pending DAP messages between steps so pause/disconnect/etc. get
    /// serviced mid-run instead of being starved.
    pub(crate) running: bool,
    /// Snapshotted breakpoint conditions for the active run. Refreshed
    /// each time we transition idle->running so condition edits between
    /// runs take effect.
    pub(crate) bp_conditions: Vec<(i64, Option<String>)>,
    /// Per-breakpoint-id hit counter, keyed by `Breakpoint.id`. Increments
    /// on every raw VM hit *before* condition/logpoint evaluation so
    /// `hitCondition` expressions see a monotonic count even when the
    /// user's condition causes the breakpoint to skip.
    pub(crate) bp_hit_counts: BTreeMap<i64, u64>,
    /// Function-name breakpoints. DAP `setFunctionBreakpoints` stores
    /// the full list here; it's mirrored onto the VM on each launch /
    /// edit so `Vm::function_breakpoints` stays in lockstep.
    pub(crate) function_breakpoints: Vec<crate::protocol::FunctionBreakpoint>,
    /// Armed state for triggered breakpoints (#102). A BP with
    /// `triggered_by: [A, B]` stays disarmed until A or B fires at
    /// least once; then flips to armed for the rest of the run.
    /// Reset on enter_running / setBreakpoints edit. Entry present
    /// (even if false) means the BP is known to be triggered; absent
    /// means the BP has no trigger and is always armed.
    pub(crate) armed_breakpoints: BTreeMap<i64, bool>,
    /// Set by handle_pause; the next VM step honors it by emitting a
    /// stopped event with reason="pause" and clearing the flag.
    pub(crate) pending_pause: bool,
    /// progressId of the in-flight DAP progressStart, if any. The IDE
    /// uses it to display a "still working" indicator and reset its own
    /// per-request timeouts. Cleared on stop/terminate via progressEnd.
    pub(crate) active_progress_id: Option<String>,
    /// Number of VM steps taken since the most recent progressStart;
    /// used to throttle progressUpdate emission to one per ~256 steps
    /// so we don't flood the IDE with no-op events.
    pub(crate) steps_since_progress_update: u32,
    /// DAP thread registry: stable `thread_id -> display_name` map.
    /// Seeded with `{1 -> "main"}` so single-session flows see the same
    /// id they always have. ACP sessions get registered via
    /// `register_thread` as they open; exits unregister. The registry is
    /// authoritative for `handle_threads` and drives the `threadId`
    /// field on stopped/continued/stepping events.
    pub(crate) threads: BTreeMap<u64, String>,
    /// Inverse map of `threads`, keyed by ACP session id so we can
    /// look up a thread id without scanning. Kept in lockstep with
    /// `threads` by `register_thread` / `unregister_thread`.
    #[allow(dead_code)]
    pub(crate) session_to_thread: BTreeMap<String, u64>,
    /// Monotonic allocator for DAP thread ids. Starts at 2 because id 1
    /// is permanently the synthetic "main" thread.
    #[allow(dead_code)]
    pub(crate) next_thread_id: u64,
    /// The thread id responsible for the current stop / step request.
    /// Today we only run one VM so this is always 1, but every DAP
    /// response that carries a `threadId` reads from this field so the
    /// wire format is already session-accurate — a future multi-VM
    /// implementation only has to swap the value when routing requests.
    pub(crate) current_thread_id: u64,
}

impl Debugger {
    pub fn new() -> Self {
        Self {
            seq: 1,
            source_path: None,
            source_content: None,
            source_refs: BTreeMap::new(),
            source_ref_by_path: BTreeMap::new(),
            next_source_ref: 1000,
            breakpoints: Vec::new(),
            next_bp_id: 1,
            vm: None,
            variables: BTreeMap::new(),
            stopped: false,
            current_line: 0,
            step_mode: StepMode::Continue,
            output: String::new(),
            program_state: ProgramState::NotStarted,
            var_refs: BTreeMap::new(),
            next_var_ref: 100,
            runtime: None,
            break_on_exceptions: false,
            exception_filters: BTreeMap::new(),
            latest_debug_state: Rc::new(RefCell::new(None)),
            host_bridge: None,
            running: false,
            bp_conditions: Vec::new(),
            bp_hit_counts: BTreeMap::new(),
            function_breakpoints: Vec::new(),
            armed_breakpoints: BTreeMap::new(),
            pending_pause: false,
            active_progress_id: None,
            steps_since_progress_update: 0,
            threads: {
                let mut m = BTreeMap::new();
                m.insert(1, "main".to_string());
                m
            },
            session_to_thread: BTreeMap::new(),
            next_thread_id: 2,
            current_thread_id: 1,
        }
    }

    /// Register a new ACP session as a DAP thread. Returns the allocated
    /// thread id. Idempotent: calling twice with the same session id
    /// returns the existing id without re-emitting.
    ///
    /// Callers that want to surface the registration on the DAP wire
    /// should pair this with [`Debugger::thread_started_event`] and emit
    /// the response. We keep the two steps separate so the debugger can
    /// sequence thread events alongside its normal seq allocation.
    ///
    /// Marked allow(dead_code) because #86 ships the public API ahead
    /// of the co-requisite ACP-session wiring that will invoke it; the
    /// DAP wire format is already session-accurate.
    #[allow(dead_code)]
    pub fn register_thread(&mut self, session_id: &str) -> u64 {
        if let Some(&id) = self.session_to_thread.get(session_id) {
            return id;
        }
        let id = self.next_thread_id;
        self.next_thread_id += 1;
        self.threads.insert(id, session_id.to_string());
        self.session_to_thread.insert(session_id.to_string(), id);
        id
    }

    /// Drop a session from the thread registry. Returns the freed id so
    /// the caller can emit a matching `thread` exited event. Refuses to
    /// unregister the synthetic `main` thread (id 1).
    #[allow(dead_code)]
    pub fn unregister_thread(&mut self, session_id: &str) -> Option<u64> {
        let id = self.session_to_thread.remove(session_id)?;
        if id == 1 {
            // Keep main alive; put it back.
            self.session_to_thread.insert(session_id.to_string(), id);
            return None;
        }
        self.threads.remove(&id);
        Some(id)
    }

    /// Build a DAP `thread` event announcing a newly-started session.
    /// Paired with [`Debugger::register_thread`] by host code that wants
    /// the IDE to light up a new row in its Threads pane.
    #[allow(dead_code)]
    pub fn thread_started_event(&mut self, thread_id: u64) -> crate::protocol::DapResponse {
        let seq = self.next_seq();
        crate::protocol::DapResponse::event(
            seq,
            "thread",
            Some(serde_json::json!({
                "reason": "started",
                "threadId": thread_id,
            })),
        )
    }

    /// Build a DAP `thread` event announcing an exited session. Pair
    /// with [`Debugger::unregister_thread`].
    #[allow(dead_code)]
    pub fn thread_exited_event(&mut self, thread_id: u64) -> crate::protocol::DapResponse {
        let seq = self.next_seq();
        crate::protocol::DapResponse::event(
            seq,
            "thread",
            Some(serde_json::json!({
                "reason": "exited",
                "threadId": thread_id,
            })),
        )
    }

    pub(crate) fn next_seq(&mut self) -> i64 {
        let s = self.seq;
        self.seq += 1;
        s
    }

    pub(crate) fn ensure_runtime(&mut self) {
        if self.runtime.is_none() {
            self.runtime = Some(
                tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                    .unwrap(),
            );
        }
    }

    /// True when the main loop should keep stepping this debugger's VM.
    /// Drives the message-interleave loop in `main.rs` -- when false, the
    /// loop blocks on `request_rx.recv()` instead of busy-stepping.
    pub fn is_running(&self) -> bool {
        self.running && self.vm.is_some()
    }

    /// Install a host bridge. Cloned into an `Rc` and registered with
    /// harn-vm via `set_host_call_bridge` whenever a fresh VM is built.
    pub fn attach_host_bridge(&mut self, bridge: std::sync::Arc<DapHostBridge>) {
        self.host_bridge = Some((*bridge).clone());
    }

    pub(crate) fn current_debug_state(&self) -> DebugState {
        self.latest_debug_state
            .borrow()
            .clone()
            .or_else(|| self.vm.as_ref().map(|vm| vm.debug_state()))
            .unwrap_or(DebugState {
                line: self.current_line.max(0) as usize,
                variables: self.variables.clone(),
                frame_name: "pipeline".to_string(),
                frame_depth: 0,
            })
    }
}

impl Drop for Debugger {
    fn drop(&mut self) {
        self.running = false;
        if let Some(vm) = self.vm.as_mut() {
            vm.signal_cancel();
        }
        self.vm = None;
        if let Some(runtime) = self.runtime.take() {
            runtime.shutdown_timeout(Duration::ZERO);
        }
    }
}
