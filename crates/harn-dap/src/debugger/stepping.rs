use std::collections::BTreeMap;
use std::rc::Rc;

use harn_vm::llm::enable_tracing;
use harn_vm::{
    clear_host_call_bridge, register_checkpoint_builtins, register_http_builtins,
    register_llm_builtins, register_metadata_builtins, register_store_builtins, register_vm_stdlib,
    set_host_call_bridge, DebugAction, Vm, VmError,
};
use serde_json::json;

use super::breakpoints::check_condition;
use super::state::{Debugger, ProgramState, StepMode};
use crate::protocol::*;

impl Debugger {
    pub(crate) fn compile_program(&mut self, source: &str) -> Result<(), String> {
        let chunk = harn_vm::compile_source(source)?;

        let mut vm = Vm::new();
        register_vm_stdlib(&mut vm);
        register_http_builtins(&mut vm);
        register_llm_builtins(&mut vm);

        // Root metadata/store/checkpoint state at the nearest harn.toml
        // (falling back to the source file's directory) so pipelines that
        // call store_get/store_set, metadata_*, or checkpoint_* builtins
        // during a debug session behave the same as under `harn run`.
        let source_parent = self
            .source_path
            .as_ref()
            .and_then(|p| std::path::Path::new(p).parent().map(|p| p.to_path_buf()))
            .unwrap_or_else(|| std::path::PathBuf::from("."));
        let project_root = harn_vm::stdlib::process::find_project_root(&source_parent);
        let store_base = project_root.as_deref().unwrap_or(&source_parent);
        register_store_builtins(&mut vm, store_base);
        register_metadata_builtins(&mut vm, store_base);
        let pipeline_name = self
            .source_path
            .as_ref()
            .and_then(|p| std::path::Path::new(p).file_stem().and_then(|s| s.to_str()))
            .unwrap_or("default");
        register_checkpoint_builtins(&mut vm, store_base, pipeline_name);
        if let Some(ref root) = project_root {
            vm.set_project_root(root);
        }
        // Enable LLM trace collection so the debugger can surface llm_call
        // telemetry to the IDE via DAP `output` events with category=telemetry.
        enable_tracing();

        // Reset any prior bridge from a previous launch, then install the
        // current one (if any) so unhandled host_call ops route to the
        // DAP client via reverse requests instead of erroring out.
        clear_host_call_bridge();
        if let Some(bridge) = &self.host_bridge {
            set_host_call_bridge(Rc::new(bridge.clone()));
        }

        if let Some(ref path) = self.source_path {
            if let Some(parent) = std::path::Path::new(path).parent() {
                if !parent.as_os_str().is_empty() {
                    vm.set_source_dir(parent);
                }
            }
        }

        // Hand the VM each file's breakpoint set keyed by source path so
        // imports don't accidentally match the main script's lines.
        let mut by_file: BTreeMap<String, Vec<usize>> = BTreeMap::new();
        for bp in &self.breakpoints {
            let key = bp
                .source
                .as_ref()
                .and_then(|s| s.path.clone())
                .unwrap_or_default();
            by_file.entry(key).or_default().push(bp.line as usize);
        }
        for (key, lines) in &by_file {
            vm.set_breakpoints_for_file(key, lines.clone());
        }
        *self.latest_debug_state.borrow_mut() = None;
        let latest_debug_state = Rc::clone(&self.latest_debug_state);
        vm.set_debug_hook(move |state| {
            *latest_debug_state.borrow_mut() = Some(state.clone());
            DebugAction::Continue
        });

        // Push the initial frame but don't run -- the first continue/step drives execution.
        vm.start(&chunk);
        *self.latest_debug_state.borrow_mut() = Some(vm.debug_state());
        self.vm = Some(vm);
        Ok(())
    }

    pub(crate) fn handle_configuration_done(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let seq = self.next_seq();
        let response = DapResponse::success(seq, msg.seq, "configurationDone", None);
        self.enter_running();
        // Emit a progressStart event so the IDE shows "Running..." while
        // the VM works through its first step batch. We end the progress
        // when the VM stops (next breakpoint, terminates, or pause). DAP
        // spec: progressStart/progressUpdate/progressEnd, identified by
        // a stable progressId we hold for the lifetime of the run.
        let progress_seq = self.next_seq();
        self.active_progress_id = Some(format!("run-{}", progress_seq));
        let progress = DapResponse::event(
            progress_seq,
            "progressStart",
            Some(json!({
                "progressId": self.active_progress_id.clone().unwrap(),
                "title": "Running script",
                "cancellable": false,
            })),
        );
        vec![response, progress]
    }

    pub(crate) fn handle_continue(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        self.step_mode = StepMode::Continue;
        self.stopped = false;
        let seq = self.next_seq();
        let response = DapResponse::success(
            seq,
            msg.seq,
            "continue",
            Some(json!({ "allThreadsContinued": true })),
        );
        self.enter_running();
        vec![response]
    }

    pub(crate) fn handle_next(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        self.step_mode = StepMode::StepOver;
        if let Some(vm) = &mut self.vm {
            vm.set_step_over();
        }
        let seq = self.next_seq();
        let response = DapResponse::success(seq, msg.seq, "next", None);
        self.enter_running();
        vec![response]
    }

    pub(crate) fn handle_step_in(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        self.step_mode = StepMode::StepIn;
        if let Some(vm) = &mut self.vm {
            vm.set_step_mode(true);
        }
        let seq = self.next_seq();
        let response = DapResponse::success(seq, msg.seq, "stepIn", None);
        self.enter_running();
        vec![response]
    }

    pub(crate) fn handle_step_out(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        self.step_mode = StepMode::StepOut;
        if let Some(vm) = &mut self.vm {
            vm.set_step_out();
        }
        let seq = self.next_seq();
        let response = DapResponse::success(seq, msg.seq, "stepOut", None);
        self.enter_running();
        vec![response]
    }

    /// Break into the currently running program at the next VM step.
    ///
    /// With message interleaving, this works even mid-run: main.rs drains
    /// the message channel between VM steps, so a pause request lands
    /// while the VM is in flight. We set `pending_pause` so the next
    /// `step_running_vm` call honors it without advancing the VM.
    pub(crate) fn handle_pause(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let seq = self.next_seq();
        let mut responses = vec![DapResponse::success(seq, msg.seq, "pause", None)];

        if self.stopped {
            // Already stopped -- just emit a stopped event so the IDE
            // updates its UI immediately.
            let stop_seq = self.next_seq();
            responses.push(DapResponse::event(
                stop_seq,
                "stopped",
                Some(json!({
                    "reason": "pause",
                    "threadId": 1,
                    "allThreadsStopped": true,
                })),
            ));
        } else {
            // Defer: next step_running_vm tick will stop with reason=pause.
            self.pending_pause = true;
            self.step_mode = StepMode::StepIn;
            if let Some(vm) = &mut self.vm {
                vm.set_step_mode(true);
            }
        }

        responses
    }

    /// Honor a deferred pause request: stop on the current instruction
    /// without advancing, emit a stopped event with reason="pause".
    fn handle_pause_stop(&mut self) -> Vec<DapResponse> {
        let mut responses = Vec::new();
        let state = self.current_debug_state();
        self.stopped = true;
        self.running = false;
        self.current_line = state.line as i64;
        self.variables = state.variables;
        self.program_state = ProgramState::Stopped;
        self.flush_output_into(&mut responses);
        self.end_progress(&mut responses);
        let seq = self.next_seq();
        responses.push(DapResponse::event(
            seq,
            "stopped",
            Some(json!({
                "reason": "pause",
                "threadId": 1,
                "allThreadsStopped": true,
            })),
        ));
        responses
    }

    /// Take ONE VM step and return any DAP events the step produced.
    /// Stops the run (sets `running = false`) when the program hits a
    /// breakpoint, terminates, or errors. Designed to be called in a
    /// tight loop by main, interleaved with `request_rx.try_recv()` so
    /// pause/disconnect/setBreakpoints get handled mid-run instead of
    /// being starved by a blocking inner loop.
    pub fn step_running_vm(&mut self) -> Vec<DapResponse> {
        if !self.running || self.vm.is_none() {
            return Vec::new();
        }

        // Honor a pending pause request before taking the step -- we
        // don't actually advance the VM, just stop with reason="pause".
        if self.pending_pause {
            self.pending_pause = false;
            return self.handle_pause_stop();
        }

        let mut responses = Vec::new();
        let step_result = {
            let vm = self.vm.as_mut().unwrap();
            self.runtime.block_on(async { vm.step_execute().await })
        };

        for tele in self.drain_telemetry_events() {
            responses.push(tele);
        }
        self.maybe_progress_update(&mut responses);

        match step_result {
            Ok(Some((_val, stopped))) if stopped => {
                let state = self.current_debug_state();
                let current_line = state.line as i64;
                let vars = state.variables;
                if !check_condition(&self.bp_conditions, current_line, &vars) {
                    return responses;
                }
                self.stopped = true;
                self.running = false;
                self.current_line = current_line;
                self.variables = vars;
                self.program_state = ProgramState::Stopped;
                self.flush_output_into(&mut responses);
                self.end_progress(&mut responses);
                let seq = self.next_seq();
                responses.push(DapResponse::event(
                    seq,
                    "stopped",
                    Some(json!({
                        "reason": "breakpoint",
                        "threadId": 1,
                        "allThreadsStopped": true,
                    })),
                ));
            }
            Ok(Some((_val, _))) => {
                // Program reached its natural end.
                self.flush_output_into(&mut responses);
                self.program_state = ProgramState::Terminated;
                self.running = false;
                self.end_progress(&mut responses);
                let seq = self.next_seq();
                responses.push(DapResponse::event(seq, "terminated", None));
            }
            Ok(None) => {
                // Mid-instruction continuation; just keep stepping.
            }
            Err(e) => {
                self.running = false;
                self.end_progress(&mut responses);
                if self.break_on_exceptions && matches!(&e, VmError::Thrown(_)) {
                    let error_msg = e.to_string();
                    let state = self.current_debug_state();
                    self.stopped = true;
                    self.current_line = state.line as i64;
                    self.variables = state.variables;
                    self.program_state = ProgramState::Stopped;
                    let seq = self.next_seq();
                    responses.push(DapResponse::event(
                        seq,
                        "output",
                        Some(json!({
                            "category": "stderr",
                            "output": format!("Exception: {error_msg}\n"),
                        })),
                    ));
                    let seq = self.next_seq();
                    responses.push(DapResponse::event(
                        seq,
                        "stopped",
                        Some(json!({
                            "reason": "exception",
                            "description": error_msg,
                            "threadId": 1,
                            "allThreadsStopped": true,
                        })),
                    ));
                    return responses;
                }
                let seq = self.next_seq();
                responses.push(DapResponse::event(
                    seq,
                    "output",
                    Some(json!({
                        "category": "stderr",
                        "output": format!("Error: {e}\n"),
                    })),
                ));
                self.program_state = ProgramState::Terminated;
                let seq = self.next_seq();
                responses.push(DapResponse::event(seq, "terminated", None));
            }
        }

        responses
    }
}
