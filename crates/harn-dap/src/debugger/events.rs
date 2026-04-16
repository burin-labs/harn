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
