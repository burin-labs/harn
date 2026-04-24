use std::cell::RefCell;

/// A single LLM call trace entry.
#[derive(Debug, Clone)]
pub struct LlmTraceEntry {
    pub model: String,
    pub input_tokens: i64,
    pub output_tokens: i64,
    pub duration_ms: u64,
}

thread_local! {
    static LLM_TRACE: RefCell<Vec<LlmTraceEntry>> = const { RefCell::new(Vec::new()) };
    static LLM_TRACING_ENABLED: RefCell<bool> = const { RefCell::new(false) };
}

/// Enable LLM tracing for the current thread.
pub fn enable_tracing() {
    LLM_TRACING_ENABLED.with(|v| *v.borrow_mut() = true);
}

/// Get and clear the trace log.
pub fn take_trace() -> Vec<LlmTraceEntry> {
    LLM_TRACE.with(|v| std::mem::take(&mut *v.borrow_mut()))
}

/// Clone the current trace log without consuming it.
pub fn peek_trace() -> Vec<LlmTraceEntry> {
    LLM_TRACE.with(|v| v.borrow().clone())
}

/// Summarize trace usage without consuming entries.
pub fn peek_trace_summary() -> (i64, i64, i64, i64) {
    LLM_TRACE.with(|v| {
        let entries = v.borrow();
        let mut input = 0i64;
        let mut output = 0i64;
        let mut duration = 0i64;
        let count = entries.len() as i64;
        for e in entries.iter() {
            input += e.input_tokens;
            output += e.output_tokens;
            duration += e.duration_ms as i64;
        }
        (input, output, duration, count)
    })
}

/// Reset thread-local trace state. Call between test runs.
pub(crate) fn reset_trace_state() {
    LLM_TRACE.with(|v| v.borrow_mut().clear());
    LLM_TRACING_ENABLED.with(|v| *v.borrow_mut() = false);
}

pub(crate) fn trace_llm_call(entry: LlmTraceEntry) {
    LLM_TRACING_ENABLED.with(|enabled| {
        if *enabled.borrow() {
            LLM_TRACE.with(|v| v.borrow_mut().push(entry));
        }
    });
}

/// Fine-grained event emitted during agent loop execution. Captures tool
/// calls, LLM calls, interventions, compaction, and phase changes so
/// downstream consumers (portal, IDE hosts, cloud runners) can display
/// execution traces without reconstructing them from raw JSON.
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentTraceEvent {
    LlmCall {
        call_id: String,
        model: String,
        input_tokens: i64,
        output_tokens: i64,
        cache_tokens: i64,
        duration_ms: u64,
        iteration: usize,
    },
    ToolExecution {
        tool_name: String,
        tool_use_id: String,
        duration_ms: u64,
        status: String,
        classification: String,
        iteration: usize,
    },
    ToolRejected {
        tool_name: String,
        reason: String,
        iteration: usize,
    },
    LoopIntervention {
        tool_name: String,
        kind: String,
        count: usize,
        iteration: usize,
    },
    ContextCompaction {
        archived_messages: usize,
        new_summary_len: usize,
        iteration: usize,
    },
    PhaseChange {
        from_phase: String,
        to_phase: String,
        iteration: usize,
    },
    LoopComplete {
        status: String,
        iterations: usize,
        total_duration_ms: u64,
        tools_used: Vec<String>,
        successful_tools: Vec<String>,
    },
    /// Emitted when `llm_call` re-prompts the model after the previous
    /// response failed `output_schema` validation. One event per retry;
    /// `attempt` counts retries (the initial call is attempt 0 and
    /// produces no event; the first retry emits `attempt: 1`).
    ///
    /// The retry does **not** persist the invalid response — the
    /// original messages are replayed with a single appended user-role
    /// correction that cites the validation errors and schema. That
    /// correction text is surfaced here as `correction_prompt` so
    /// transcripts show both why the retry happened and what was sent.
    SchemaRetry {
        attempt: usize,
        errors: Vec<String>,
        nudge_used: bool,
        correction_prompt: String,
    },
    NativeToolFallback {
        iteration: usize,
        accepted: bool,
        policy: String,
        fallback_index: usize,
        tool_call_count: usize,
    },
    EmptyCompletionRetry {
        iteration: usize,
        attempt: usize,
        error: String,
    },
}

thread_local! {
    static AGENT_TRACE: RefCell<Vec<AgentTraceEvent>> = const { RefCell::new(Vec::new()) };
}

/// Emit an agent trace event.
pub(crate) fn emit_agent_event(event: AgentTraceEvent) {
    AGENT_TRACE.with(|v| v.borrow_mut().push(event));
}

/// Get and clear the agent trace log.
pub fn take_agent_trace() -> Vec<AgentTraceEvent> {
    AGENT_TRACE.with(|v| std::mem::take(&mut *v.borrow_mut()))
}

/// Clone the current agent trace log without consuming it.
pub fn peek_agent_trace() -> Vec<AgentTraceEvent> {
    AGENT_TRACE.with(|v| v.borrow().clone())
}

/// Produce a rolled-up summary of agent trace events as JSON.
pub fn agent_trace_summary() -> serde_json::Value {
    AGENT_TRACE.with(|v| {
        let events = v.borrow();
        let mut llm_calls = 0usize;
        let mut tool_executions = 0usize;
        let mut tool_rejections = 0usize;
        let mut interventions = 0usize;
        let mut compactions = 0usize;
        let mut native_text_tool_fallbacks = 0usize;
        let mut native_text_tool_fallback_rejections = 0usize;
        let mut empty_completion_retries = 0usize;
        let mut total_input_tokens = 0i64;
        let mut total_output_tokens = 0i64;
        let mut total_llm_duration_ms = 0u64;
        let mut total_tool_duration_ms = 0u64;
        let mut tools_used: Vec<String> = Vec::new();
        let mut status = "unknown".to_string();
        let mut iterations = 0usize;
        let mut total_duration_ms = 0u64;

        for event in events.iter() {
            match event {
                AgentTraceEvent::LlmCall {
                    input_tokens,
                    output_tokens,
                    duration_ms,
                    ..
                } => {
                    llm_calls += 1;
                    total_input_tokens += input_tokens;
                    total_output_tokens += output_tokens;
                    total_llm_duration_ms += duration_ms;
                }
                AgentTraceEvent::ToolExecution {
                    tool_name,
                    duration_ms,
                    ..
                } => {
                    tool_executions += 1;
                    total_tool_duration_ms += duration_ms;
                    if !tools_used.contains(tool_name) {
                        tools_used.push(tool_name.clone());
                    }
                }
                AgentTraceEvent::ToolRejected { .. } => {
                    tool_rejections += 1;
                }
                AgentTraceEvent::LoopIntervention { .. } => {
                    interventions += 1;
                }
                AgentTraceEvent::ContextCompaction { .. } => {
                    compactions += 1;
                }
                AgentTraceEvent::PhaseChange { .. } => {}
                AgentTraceEvent::LoopComplete {
                    status: s,
                    iterations: i,
                    total_duration_ms: d,
                    ..
                } => {
                    status = s.clone();
                    iterations = *i;
                    total_duration_ms = *d;
                }
                AgentTraceEvent::SchemaRetry { .. } => {}
                AgentTraceEvent::NativeToolFallback { accepted, .. } => {
                    native_text_tool_fallbacks += 1;
                    if !accepted {
                        native_text_tool_fallback_rejections += 1;
                    }
                }
                AgentTraceEvent::EmptyCompletionRetry { .. } => {
                    empty_completion_retries += 1;
                }
            }
        }

        serde_json::json!({
            "status": status,
            "iterations": iterations,
            "total_duration_ms": total_duration_ms,
            "llm_calls": llm_calls,
            "tool_executions": tool_executions,
            "tool_rejections": tool_rejections,
            "interventions": interventions,
            "compactions": compactions,
            "native_text_tool_fallbacks": native_text_tool_fallbacks,
            "native_text_tool_fallback_rejections": native_text_tool_fallback_rejections,
            "empty_completion_retries": empty_completion_retries,
            "total_input_tokens": total_input_tokens,
            "total_output_tokens": total_output_tokens,
            "total_llm_duration_ms": total_llm_duration_ms,
            "total_tool_duration_ms": total_tool_duration_ms,
            "tools_used": tools_used,
        })
    })
}

/// Reset agent trace state. Call between test runs.
pub(crate) fn reset_agent_trace_state() {
    AGENT_TRACE.with(|v| v.borrow_mut().clear());
}
