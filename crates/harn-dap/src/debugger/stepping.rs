use std::collections::BTreeMap;
use std::rc::Rc;

use harn_vm::llm::enable_tracing;
use harn_vm::{
    clear_host_call_bridge, register_checkpoint_builtins, register_http_builtins,
    register_llm_builtins, register_metadata_builtins, register_store_builtins, register_vm_stdlib,
    set_host_call_bridge, DebugAction, Vm, VmError,
};
use serde_json::json;

use super::breakpoints::BreakpointAction;
use super::state::{Debugger, ProgramState, StepMode};
use crate::protocol::*;

impl Debugger {
    pub(crate) fn compile_program(&mut self, source: &str) -> Result<(), String> {
        let mut chunk = harn_vm::compile_source(source)?;
        // Tag the main program's chunk with its source path so
        // `Vm::breakpoint_matches` can match DAP breakpoints keyed by the
        // absolute path the client sent (otherwise `current_source_file()`
        // is `None` for the entry chunk and only wildcard-keyed breakpoints
        // ever fire). Imported modules already get this via
        // `compile_fn_body`; this covers the entry script too.
        if let Some(ref path) = self.source_path {
            chunk.source_file = Some(path.clone());
        }

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

    /// Handle DAP `restartFrame` — rewind a specific frame back to its
    /// entry point, so stepping resumes from the first instruction of
    /// the function with its original args re-bound. Pairs with
    /// `setVariable` to give pipeline authors an "edit the prompt and
    /// rerun just this function" loop. Side effects already performed
    /// by the restarted frame (tool calls, file writes, LLM round
    /// trips) are *not* rolled back — the IDE surfaces that caveat on
    /// the menu item.
    pub(crate) fn handle_restart_frame(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let frame_id = msg
            .arguments
            .as_ref()
            .and_then(|a| a.get("frameId"))
            .and_then(|f| f.as_i64())
            .unwrap_or(1);
        // DAP stackTrace IDs are 1-based in our implementation; the
        // VM's restart_frame takes a 0-based index into `frames`,
        // which is emitted in caller-first order. The frame at index 0
        // is the outermost pipeline frame, and IDs count from 1 at
        // the innermost — invert the id back to a vm index.
        let Some(vm) = self.vm.as_mut() else {
            return vec![self.dap_error(msg, "restartFrame", "no active VM session")];
        };
        let frame_count = vm.frame_count();
        if frame_id < 1 || (frame_id as usize) > frame_count {
            return vec![self.dap_error(
                msg,
                "restartFrame",
                &format!("frameId {frame_id} out of range (have {frame_count} frames)"),
            )];
        }
        let vm_index = frame_count - frame_id as usize;
        match vm.restart_frame(vm_index) {
            Ok(()) => {
                self.var_refs.clear();
                self.next_var_ref = 100;
                // restartFrame resumes execution; match the semantics
                // of `continue`. The IDE will request a fresh
                // stackTrace / scopes when the next stopped event
                // fires, so we don't need to pre-emit any state.
                self.step_mode = StepMode::Continue;
                self.stopped = false;
                let seq = self.next_seq();
                let response = DapResponse::success(seq, msg.seq, "restartFrame", None);
                // Emit a `continued` event so the IDE clears its
                // "paused" chrome without waiting for the next stop.
                let evt_seq = self.next_seq();
                let continued = DapResponse::event(
                    evt_seq,
                    "continued",
                    Some(json!({
                        "threadId": self.current_thread_id as i64,
                        "allThreadsContinued": true,
                    })),
                );
                self.enter_running();
                vec![response, continued]
            }
            Err(err) => vec![self.dap_error(msg, "restartFrame", &err.to_string())],
        }
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
                    "threadId": self.current_thread_id as i64,
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
                "threadId": self.current_thread_id as i64,
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
                // Defer the real stop decision to classify_breakpoint_hit,
                // which evaluates hit-count → condition → logpoint in
                // the order VS Code uses. Steps and exceptions bypass
                // this path entirely because the VM only reports
                // stopped=true on real breakpoint hits.
                let bp_touches_line = self.breakpoints.iter().any(|bp| bp.line == current_line);
                if bp_touches_line {
                    match self.classify_breakpoint_hit(current_line, &vars) {
                        BreakpointAction::Stop => {}
                        BreakpointAction::Skip => return responses,
                        BreakpointAction::LogAndContinue(rendered) => {
                            let seq = self.next_seq();
                            responses.push(DapResponse::event(
                                seq,
                                "output",
                                Some(json!({
                                    "category": "console",
                                    "output": format!("{rendered}\n"),
                                })),
                            ));
                            return responses;
                        }
                        BreakpointAction::Diagnostic(msg) => {
                            let seq = self.next_seq();
                            responses.push(DapResponse::event(
                                seq,
                                "output",
                                Some(json!({
                                    "category": "console",
                                    "output": format!("{msg}\n"),
                                })),
                            ));
                            return responses;
                        }
                    }
                }
                // Pick a DAP stop reason that matches what actually stopped
                // execution. Step requests complete by landing the VM on
                // the next line while no breakpoint matches; conflating
                // those with real breakpoint hits (all labeled "breakpoint")
                // made the IDE status bar show "Paused on breakpoint" even
                // after a step-out far past the last breakpoint. When a
                // step is in flight and the current line isn't a registered
                // breakpoint, emit reason="step" instead.
                let step_in_flight = self.step_mode != StepMode::Continue;
                let line_is_bp = self.breakpoints.iter().any(|bp| bp.line == current_line);
                let reason = if step_in_flight && !line_is_bp {
                    "step"
                } else {
                    "breakpoint"
                };
                // Stepping always completes at the next stop — reset so
                // the following continue/breakpoint is classified cleanly.
                self.step_mode = StepMode::Continue;
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
                        "reason": reason,
                        "threadId": self.current_thread_id as i64,
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
                            "threadId": self.current_thread_id as i64,
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
