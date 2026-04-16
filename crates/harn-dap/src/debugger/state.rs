use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

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
    pub(crate) runtime: tokio::runtime::Runtime,
    /// Next variable reference ID (start at 100 to avoid conflict with scope refs).
    pub(crate) next_var_ref: i64,
    /// Whether to break on thrown exceptions.
    pub(crate) break_on_exceptions: bool,
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
}

impl Debugger {
    pub fn new() -> Self {
        Self {
            seq: 1,
            source_path: None,
            source_content: None,
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
            runtime: tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap(),
            break_on_exceptions: false,
            latest_debug_state: Rc::new(RefCell::new(None)),
            host_bridge: None,
            running: false,
            bp_conditions: Vec::new(),
            pending_pause: false,
            active_progress_id: None,
            steps_since_progress_update: 0,
        }
    }

    pub(crate) fn next_seq(&mut self) -> i64 {
        let s = self.seq;
        self.seq += 1;
        s
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
