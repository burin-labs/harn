mod breakpoints;
mod events;
pub(crate) mod state;
mod stepping;
mod variables;

#[cfg(test)]
mod tests;

pub use state::Debugger;

use serde_json::json;

use crate::protocol::*;
use state::ProgramState;

/// Recursively serialize a PromptSourceSpan parent chain into nested
/// JSON so IDEs can render a breadcrumb like `A → B → C` when a deep
/// render spanned three included files (#96). Returns null when the
/// span is None so `optional_chain` on the IDE side is cheap.
fn serialize_parent_chain(span: Option<&harn_vm::PromptSourceSpan>) -> serde_json::Value {
    match span {
        None => serde_json::Value::Null,
        Some(s) => json!({
            "templateUri": s.template_uri,
            "templateLine": s.template_line,
            "templateCol": s.template_col,
            "outputStart": s.output_start,
            "outputEnd": s.output_end,
            "kind": prompt_span_kind_label(s.kind),
            "boundValue": s.bound_value,
            "parentSpan": serialize_parent_chain(s.parent_span.as_deref()),
        }),
    }
}

fn prompt_span_kind_label(kind: harn_vm::PromptSpanKind) -> &'static str {
    match kind {
        harn_vm::PromptSpanKind::Text => "text",
        harn_vm::PromptSpanKind::Expr => "expr",
        harn_vm::PromptSpanKind::LegacyBareInterp => "legacy_bare",
        harn_vm::PromptSpanKind::If => "if",
        harn_vm::PromptSpanKind::ForIteration => "for_iteration",
        harn_vm::PromptSpanKind::Include => "include",
    }
}

impl Debugger {
    pub fn handle_message(&mut self, msg: DapMessage) -> Vec<DapResponse> {
        let command = msg.command.as_deref().unwrap_or("");
        match command {
            "initialize" => self.handle_initialize(&msg),
            "launch" => self.handle_launch(&msg),
            "setBreakpoints" => self.handle_set_breakpoints(&msg),
            "setFunctionBreakpoints" => self.handle_set_function_breakpoints(&msg),
            "configurationDone" => self.handle_configuration_done(&msg),
            "continue" => self.handle_continue(&msg),
            "next" => self.handle_next(&msg),
            "stepIn" => self.handle_step_in(&msg),
            "stepOut" => self.handle_step_out(&msg),
            "pause" => self.handle_pause(&msg),
            "threads" => self.handle_threads(&msg),
            "stackTrace" => self.handle_stack_trace(&msg),
            "scopes" => self.handle_scopes(&msg),
            "variables" => self.handle_variables(&msg),
            "evaluate" => self.handle_evaluate(&msg),
            "setVariable" => self.handle_set_variable(&msg),
            "setExpression" => self.handle_set_expression(&msg),
            "restartFrame" => self.handle_restart_frame(&msg),
            "setExceptionBreakpoints" => self.handle_set_exception_breakpoints(&msg),
            "disconnect" => self.handle_disconnect(&msg),
            "cancel" => self.handle_cancel(&msg),
            "completions" => self.handle_completions(&msg),
            "stepInTargets" => self.handle_step_in_targets(&msg),
            "harnPing" => self.handle_ping(&msg),
            "burin/promptProvenance" => self.handle_prompt_provenance(&msg),
            "burin/promptConsumers" => self.handle_prompt_consumers(&msg),
            _ => {
                vec![DapResponse::success(
                    self.next_seq(),
                    msg.seq,
                    command,
                    None,
                )]
            }
        }
    }

    fn handle_initialize(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let caps = Capabilities::default();
        let seq = self.next_seq();
        let response = DapResponse::success(seq, msg.seq, "initialize", Some(json!(caps)));

        let event_seq = self.next_seq();
        let event = DapResponse::event(event_seq, "initialized", None);

        vec![response, event]
    }

    fn handle_launch(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let mut responses = Vec::new();

        if let Some(args) = &msg.arguments {
            if let Some(program) = args.get("program").and_then(|p| p.as_str()) {
                self.source_path = Some(program.to_string());
                match std::fs::read_to_string(program) {
                    Ok(source) => {
                        self.source_content = Some(source.clone());
                        match self.compile_program(&source) {
                            Ok(()) => {
                                self.program_state = ProgramState::Running;
                            }
                            Err(e) => {
                                let seq = self.next_seq();
                                responses.push(DapResponse::event(
                                    seq,
                                    "output",
                                    Some(json!({
                                        "category": "stderr",
                                        "output": format!("Compilation error: {e}\n"),
                                    })),
                                ));
                            }
                        }
                    }
                    Err(e) => {
                        let seq = self.next_seq();
                        responses.push(DapResponse::event(
                            seq,
                            "output",
                            Some(json!({
                                "category": "stderr",
                                "output": format!("Failed to read {program}: {e}\n"),
                            })),
                        ));
                    }
                }
            }
        }

        let seq = self.next_seq();
        responses.push(DapResponse::success(seq, msg.seq, "launch", None));
        responses
    }

    fn handle_threads(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let seq = self.next_seq();
        let threads: Vec<_> = self
            .threads
            .iter()
            .map(|(id, name)| json!({ "id": *id as i64, "name": name }))
            .collect();
        vec![DapResponse::success(
            seq,
            msg.seq,
            "threads",
            Some(json!({ "threads": threads })),
        )]
    }

    fn handle_stack_trace(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let frames: Vec<StackFrame> = if let Some(vm) = &self.vm {
            vm.debug_stack_frames()
                .into_iter()
                .enumerate()
                .map(|(i, (name, line))| StackFrame {
                    id: (i + 1) as i64,
                    name,
                    line: line.max(1) as i64,
                    column: 1,
                    source: self.source_path.as_ref().map(|p| Source {
                        name: std::path::Path::new(p)
                            .file_name()
                            .map(|f| f.to_string_lossy().into_owned()),
                        path: Some(p.clone()),
                    }),
                })
                .collect()
        } else {
            vec![StackFrame {
                id: 1,
                name: "pipeline".to_string(),
                line: self.current_line.max(1),
                column: 1,
                source: self.source_path.as_ref().map(|p| Source {
                    name: std::path::Path::new(p)
                        .file_name()
                        .map(|f| f.to_string_lossy().into_owned()),
                    path: Some(p.clone()),
                }),
            }]
        };

        let total = frames.len();
        let seq = self.next_seq();
        vec![DapResponse::success(
            seq,
            msg.seq,
            "stackTrace",
            Some(json!({
                "stackFrames": frames,
                "totalFrames": total,
            })),
        )]
    }

    /// Lightweight liveness check the IDE pings us with periodically.
    /// Replies with the current run-state so the IDE can distinguish
    /// "wedged" from "actively stepping" without having to emit
    /// progress events (which we already do for active runs).
    fn handle_ping(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let seq = self.next_seq();
        let state_str = match self.program_state {
            ProgramState::NotStarted => "not_started",
            ProgramState::Running => "running",
            ProgramState::Stopped => "stopped",
            ProgramState::Terminated => "terminated",
        };
        vec![DapResponse::success(
            seq,
            msg.seq,
            "harnPing",
            Some(json!({
                "state": state_str,
                "running": self.running,
                "stopped": self.stopped,
            })),
        )]
    }

    /// Custom `burin/promptProvenance` request — map an output byte
    /// offset in a rendered prompt to the originating `.harn.prompt`
    /// source span. The IDE uses this to highlight the template range
    /// that produced the chunk the user clicked in the LLM transcript
    /// view. See burin-code issues #93 and #94 for the UX backing.
    fn handle_prompt_provenance(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let args = msg.arguments.as_ref();
        let prompt_id = args
            .and_then(|a| a.get("promptId"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let output_offset = args
            .and_then(|a| a.get("outputOffset"))
            .and_then(|v| v.as_u64())
            .unwrap_or(0) as usize;
        if prompt_id.is_empty() {
            return vec![self.dap_error(msg, "burin/promptProvenance", "missing 'promptId'")];
        }
        match harn_vm::lookup_prompt_span(&prompt_id, output_offset) {
            Some((template_uri, span)) => {
                let seq = self.next_seq();
                // Span-level templateUri carries the inner file; the
                // request's top-level `templateUri` is the root render.
                // When the span was rendered inside an include, the
                // `parentSpan` chain walks back through each level so
                // the IDE can render an `A → B → C` breadcrumb (#96).
                let effective_uri = if span.template_uri.is_empty() {
                    template_uri.clone()
                } else {
                    span.template_uri.clone()
                };
                vec![DapResponse::success(
                    seq,
                    msg.seq,
                    "burin/promptProvenance",
                    Some(json!({
                        "templateUri": effective_uri,
                        "rootTemplateUri": template_uri,
                        "templateLine": span.template_line,
                        "templateCol": span.template_col,
                        "outputStart": span.output_start,
                        "outputEnd": span.output_end,
                        "kind": prompt_span_kind_label(span.kind),
                        "boundValue": span.bound_value,
                        "parentSpan": serialize_parent_chain(span.parent_span.as_deref()),
                    })),
                )]
            }
            None => vec![self.dap_error(
                msg,
                "burin/promptProvenance",
                &format!(
                    "no span found for promptId '{prompt_id}' at outputOffset {output_offset}"
                ),
            )],
        }
    }

    /// Custom `burin/promptConsumers` request — given a template URI
    /// and a line range, return every registered render's spans that
    /// drew from that region. Powers the template → transcript
    /// "which runs used this helper?" navigation.
    fn handle_prompt_consumers(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let args = msg.arguments.as_ref();
        let template_uri = args
            .and_then(|a| a.get("templateUri"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let template_line_start = args
            .and_then(|a| a.get("templateLineStart"))
            .and_then(|v| v.as_u64())
            .unwrap_or(1) as usize;
        let template_line_end = args
            .and_then(|a| a.get("templateLineEnd"))
            .and_then(|v| v.as_u64())
            .unwrap_or(usize::MAX as u64) as usize;
        if template_uri.is_empty() {
            return vec![self.dap_error(msg, "burin/promptConsumers", "missing 'templateUri'")];
        }
        let consumers =
            harn_vm::lookup_prompt_consumers(&template_uri, template_line_start, template_line_end);
        let payload: Vec<_> = consumers
            .into_iter()
            .map(|(prompt_id, span)| {
                // eventIndices: every AgentEvent index where this
                // prompt_id was consumed by an LLM call (#106).
                // Empty vec when emission sites haven't yet wired
                // record_prompt_render_index — the IDE's jump-to-
                // next-render falls back to no-op cleanly.
                let event_indices = harn_vm::prompt_render_indices(&prompt_id);
                json!({
                    "promptId": prompt_id,
                    "templateLine": span.template_line,
                    "templateCol": span.template_col,
                    "outputStart": span.output_start,
                    "outputEnd": span.output_end,
                    "kind": prompt_span_kind_label(span.kind),
                    "eventIndices": event_indices,
                })
            })
            .collect();
        let seq = self.next_seq();
        vec![DapResponse::success(
            seq,
            msg.seq,
            "burin/promptConsumers",
            Some(json!({ "consumers": payload })),
        )]
    }

    /// DAP `completions` request (#109) — returns identifiers the
    /// unified evaluator can reach in the current frame. Union of
    /// the frame's scope (locals + captures + globals) and the
    /// registered builtin/async-builtin names. Optional `text`
    /// argument prefixes the filter so VS Code can show scoped
    /// results as the user types.
    fn handle_completions(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let text = msg
            .arguments
            .as_ref()
            .and_then(|a| a.get("text"))
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string();
        let frame_id = msg
            .arguments
            .as_ref()
            .and_then(|a| a.get("frameId"))
            .and_then(|v| v.as_i64())
            .unwrap_or(0) as usize;

        let targets: Vec<String> = self
            .vm
            .as_ref()
            .map(|vm| vm.identifiers_in_scope(frame_id))
            .unwrap_or_default();

        let prefix_lower = text.to_lowercase();
        let items: Vec<_> = targets
            .into_iter()
            .filter(|name| prefix_lower.is_empty() || name.to_lowercase().contains(&prefix_lower))
            .take(200)
            .map(|label| {
                json!({
                    "label": label,
                    "type": "function",
                })
            })
            .collect();

        let seq = self.next_seq();
        vec![DapResponse::success(
            seq,
            msg.seq,
            "completions",
            Some(json!({ "targets": items })),
        )]
    }

    /// DAP `stepInTargets` request (#112) — returns every callable on
    /// the current source line so the IDE can show a mini-menu when
    /// Step Into hits a multi-call expression like `foo(bar(x), baz(y))`.
    fn handle_step_in_targets(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        let frame_id = msg
            .arguments
            .as_ref()
            .and_then(|a| a.get("frameId"))
            .and_then(|v| v.as_i64())
            .unwrap_or(1);
        // Map DAP frame_id (1-based, innermost=N) onto our VM frames.
        // When we don't have a matching frame, fall back to the
        // top frame's current line.
        let line = self.current_line as u32;
        let targets: Vec<_> = self
            .vm
            .as_ref()
            .map(|vm| vm.call_sites_on_line(line))
            .unwrap_or_default()
            .into_iter()
            .enumerate()
            .map(|(idx, (ip, label))| {
                json!({
                    "id": (frame_id * 1_000_000) + (idx as i64) + 1,
                    "label": label,
                    "line": line as i64,
                    "instructionPointerReference": format!("ip:{ip}"),
                })
            })
            .collect();
        let seq = self.next_seq();
        vec![DapResponse::success(
            seq,
            msg.seq,
            "stepInTargets",
            Some(json!({ "targets": targets })),
        )]
    }

    /// DAP `invalidated` event (#110). Emitted when state the IDE is
    /// already showing becomes stale — feedback injection mutated a
    /// running variable, a capability policy swapped mid-run, etc.
    /// `areas` conventionally contains `variables` / `threads` /
    /// `stacks` so the client can scope its refetch.
    ///
    /// Marked allow(dead_code) because the emission-site hook
    /// (observing feedback-injection or capability-policy change
    /// events) lives outside this crate; the helper is published
    /// so host code composes cleanly without rolling its own event
    /// JSON.
    #[allow(dead_code)]
    pub fn invalidated_event(&mut self, areas: Vec<&str>) -> DapResponse {
        let seq = self.next_seq();
        DapResponse::event(
            seq,
            "invalidated",
            Some(json!({
                "areas": areas,
                "threadId": self.current_thread_id as i64,
            })),
        )
    }

    /// DAP `cancel` request (#108). The client supplies a
    /// `requestId` (a prior request's seq) or `progressId`; we look
    /// up any pending reverse host_call keyed on that seq and signal
    /// its waiter with a "cancelled" failure reply. Long-running
    /// llm_call round trips that block inside DapHostBridge::dispatch
    /// unwind promptly as a normal Harn exception, without tearing
    /// down the whole session. Missing-id / unknown-id is a no-op
    /// that still returns success — matching VS Code's behavior so
    /// the Stop pill never flashes an error.
    fn handle_cancel(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        if let Some(bridge) = &self.host_bridge {
            let request_id = msg
                .arguments
                .as_ref()
                .and_then(|a| a.get("requestId"))
                .and_then(|v| v.as_i64());
            if let Some(id) = request_id {
                bridge.cancel_pending(id, "cancel");
            }
        }
        let seq = self.next_seq();
        vec![DapResponse::success(seq, msg.seq, "cancel", None)]
    }

    fn handle_disconnect(&mut self, msg: &DapMessage) -> Vec<DapResponse> {
        // Stop the VM and release any reverse-request waiters. Without
        // the cancellation step, a host_call in flight via DapHostBridge
        // would block until its 60s timeout -- leaving the script (and
        // any tokio task driving step_execute) stuck for a minute after
        // the IDE walks away.
        self.running = false;
        if let Some(bridge) = &self.host_bridge {
            bridge.cancel_all_pending("disconnect");
        }
        self.vm = None;
        self.program_state = ProgramState::Terminated;
        let mut responses = Vec::new();
        self.end_progress(&mut responses);
        let seq = self.next_seq();
        responses.push(DapResponse::success(seq, msg.seq, "disconnect", None));
        responses
    }
}
