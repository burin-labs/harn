use harn_vm::agent_events::WorkerEvent;
use harn_vm::llm::{take_agent_trace, AgentTraceEvent};
use serde_json::json;

use super::state::Debugger;
use crate::protocol::*;

impl Debugger {
    /// End any in-flight progress event. Called whenever the VM stops
    /// (breakpoint, pause, terminate, error) so the IDE clears its
    /// "Running..." indicator.
    pub(crate) fn end_progress(&mut self, responses: &mut Vec<DapResponse>) {
        if let Some(id) = self.active_progress_id.take() {
            let seq = self.next_seq();
            responses.push(DapResponse::event(
                seq,
                "progressEnd",
                Some(json!({ "progressId": id })),
            ));
        }
        self.steps_since_progress_update = 0;
    }

    /// Emit a progressUpdate roughly every 256 steps so the IDE sees
    /// liveness ticks during long runs and can extend its own timeouts.
    /// Cheap when there's no active progress (early return).
    pub(crate) fn maybe_progress_update(&mut self, responses: &mut Vec<DapResponse>) {
        if self.active_progress_id.is_none() {
            return;
        }
        self.steps_since_progress_update = self.steps_since_progress_update.wrapping_add(1);
        if self.steps_since_progress_update & 0xFF != 0 {
            return;
        }
        let id = self.active_progress_id.clone().unwrap();
        let line = self.current_line;
        let seq = self.next_seq();
        responses.push(DapResponse::event(
            seq,
            "progressUpdate",
            Some(json!({
                "progressId": id,
                "message": format!("line {}", line),
            })),
        ));
    }

    /// Drain agent trace events the VM has accumulated and serialize the
    /// LLM-call entries as DAP `output` events with `category: "telemetry"`.
    /// Other event kinds (tool execution, phase change, etc.) are skipped
    /// for now -- the IDE consumes only LLM telemetry. Keeping this here
    /// rather than in harn-vm preserves the rule that DAP wire-format
    /// concerns belong in harn-dap.
    pub(crate) fn drain_telemetry_events(&mut self) -> Vec<DapResponse> {
        let events = take_agent_trace();
        if events.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for event in events {
            if let AgentTraceEvent::LlmCall {
                call_id,
                model,
                input_tokens,
                output_tokens,
                cache_tokens,
                duration_ms,
                iteration,
            } = event
            {
                let payload = json!({
                    "call_id": call_id,
                    "model": model,
                    "prompt_tokens": input_tokens,
                    "completion_tokens": output_tokens,
                    "cache_tokens": cache_tokens,
                    "total_ms": duration_ms,
                    "iteration": iteration,
                });
                let body_str = serde_json::to_string(&payload).unwrap_or_default();
                let seq = self.next_seq();
                out.push(DapResponse::event(
                    seq,
                    "output",
                    Some(json!({
                        "category": "telemetry",
                        "output": body_str,
                    })),
                ));
            }
        }
        out
    }

    /// Issue #1868 — drain queued subagent observations and translate
    /// them into DAP `thread`/`stopped`/`continued` events. Called
    /// between VM steps from `step_running_vm` so subagent lifecycle
    /// surfaces on the IDE in the same frame the worker emitted it.
    ///
    /// Event mapping (see also the table in `subagents.rs`):
    /// - `WorkerSpawned` → `thread { reason: "started" }`
    /// - `WorkerSuspended` → `stopped { reason: "suspend",
    ///   description: <reason>, threadId: <subagent>,
    ///   allThreadsStopped: false }`
    /// - `WorkerWaitingForInput` → `stopped { reason: "pause", ... }`
    /// - `WorkerResumed` → `continued { allThreadsContinued: false }`
    /// - `WorkerCompleted` / `WorkerCancelled` → `thread { reason: "exited" }`
    /// - `WorkerFailed` → `stopped { reason: "exception" }` then
    ///   `thread { reason: "exited" }`
    /// - `WorkerProgressed` → no DAP emission (too noisy)
    ///
    /// When the `break-on-resume` exception filter is active, a
    /// `WorkerResumed` additionally emits a `stopped { reason:
    /// "pause", threadId: main }` so the user can step through the
    /// resume continuation on the main thread.
    pub(crate) fn drain_subagent_events(&mut self) -> Vec<DapResponse> {
        let observations = self.subagent_tracker.drain();
        if observations.is_empty() {
            return Vec::new();
        }
        let mut out = Vec::new();
        for obs in observations {
            // Upsert the thread first so every emitted event references
            // a stable `threadId`. Idempotent for the second+ event on
            // the same worker.
            let record = self.subagent_tracker.upsert_thread(
                &obs.worker_id,
                &obs.worker_name,
                obs.parent_worker_id.as_deref(),
                &obs.status,
            );
            let thread_id = record.thread_id as i64;
            // Spawn → emit `thread started` once. Subsequent events for
            // the same worker still upsert (above) but don't re-emit
            // the start.
            match obs.event {
                WorkerEvent::WorkerSpawned => {
                    let seq = self.next_seq();
                    out.push(DapResponse::event(
                        seq,
                        "thread",
                        Some(json!({
                            "reason": "started",
                            "threadId": thread_id,
                        })),
                    ));
                }
                WorkerEvent::WorkerProgressed => {
                    // Intentionally silent — progress milestones map
                    // poorly to DAP runtime states and would flood the
                    // IDE with no-op stopped/continued churn.
                }
                WorkerEvent::WorkerWaitingForInput => {
                    self.subagent_tracker
                        .mark_suspended(&obs.worker_id, Some("awaiting input"));
                    let seq = self.next_seq();
                    out.push(DapResponse::event(
                        seq,
                        "stopped",
                        Some(json!({
                            "reason": "pause",
                            "description": "awaiting input",
                            "threadId": thread_id,
                            "allThreadsStopped": false,
                            "preserveFocusHint": true,
                        })),
                    ));
                }
                WorkerEvent::WorkerSuspended => {
                    self.subagent_tracker
                        .mark_suspended(&obs.worker_id, obs.suspend_reason.as_deref());
                    let description = obs
                        .suspend_reason
                        .clone()
                        .unwrap_or_else(|| "suspended".to_string());
                    let seq = self.next_seq();
                    out.push(DapResponse::event(
                        seq,
                        "stopped",
                        Some(json!({
                            "reason": "suspend",
                            "description": description,
                            "threadId": thread_id,
                            "allThreadsStopped": false,
                            "preserveFocusHint": !self.break_on_subagent_suspend,
                        })),
                    ));
                }
                WorkerEvent::WorkerResumed => {
                    self.subagent_tracker.mark_resumed(&obs.worker_id);
                    let seq = self.next_seq();
                    out.push(DapResponse::event(
                        seq,
                        "continued",
                        Some(json!({
                            "threadId": thread_id,
                            "allThreadsContinued": false,
                        })),
                    ));
                    if self.break_on_subagent_resume {
                        // Force a stop on the main debugger thread so
                        // the user can step through the resume
                        // continuation. The next step_running_vm tick
                        // honours pending_pause and emits a proper
                        // `stopped(pause)` event for the main thread.
                        self.pending_pause = true;
                    }
                }
                WorkerEvent::WorkerCompleted | WorkerEvent::WorkerCancelled => {
                    self.subagent_tracker.mark_exited(&obs.worker_id);
                    let seq = self.next_seq();
                    out.push(DapResponse::event(
                        seq,
                        "thread",
                        Some(json!({
                            "reason": "exited",
                            "threadId": thread_id,
                        })),
                    ));
                }
                WorkerEvent::WorkerFailed => {
                    self.subagent_tracker.mark_exited(&obs.worker_id);
                    let description = obs
                        .suspend_reason
                        .clone()
                        .unwrap_or_else(|| "worker failed".to_string());
                    let seq = self.next_seq();
                    out.push(DapResponse::event(
                        seq,
                        "stopped",
                        Some(json!({
                            "reason": "exception",
                            "description": description,
                            "threadId": thread_id,
                            "allThreadsStopped": false,
                            "preserveFocusHint": true,
                        })),
                    ));
                    let seq = self.next_seq();
                    out.push(DapResponse::event(
                        seq,
                        "thread",
                        Some(json!({
                            "reason": "exited",
                            "threadId": thread_id,
                        })),
                    ));
                }
            }
        }
        out
    }

    /// Drain any new VM stdout into `output` DAP events. Used by the
    /// stop/terminate paths so the IDE doesn't lose trailing print()s.
    pub(crate) fn flush_output_into(&mut self, responses: &mut Vec<DapResponse>) {
        let output = self.vm.as_ref().unwrap().output().to_string();
        if !output.is_empty() && output != self.output {
            let new_output = output[self.output.len()..].to_string();
            if !new_output.is_empty() {
                let seq = self.next_seq();
                responses.push(DapResponse::event(
                    seq,
                    "output",
                    Some(json!({
                        "category": "stdout",
                        "output": new_output,
                    })),
                ));
            }
            self.output = output;
        }
    }
}
