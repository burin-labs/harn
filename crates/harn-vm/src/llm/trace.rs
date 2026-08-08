use std::cell::RefCell;

/// A single LLM call trace entry.
#[derive(Debug, Clone)]
pub struct LlmTraceEntry {
    pub model: String,
    /// Provider that served the call. Carried alongside `model` because
    /// catalog pricing resolves on the (provider, model) pair, and a trace
    /// summary that priced by model alone would silently misprice every model
    /// served by more than one provider.
    pub provider: String,
    /// Canonical per-call accounting. Traces carry the ledger rather than a
    /// second token/cost shape so cache and accelerated-tier pricing cannot be
    /// lost or recomputed differently by reporting consumers.
    pub usage: super::usage::LlmUsage,
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
            input += e.usage.input_tokens;
            output += e.usage.output_tokens;
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

/// The loop and tool facts an agent session already records durably.
///
/// Tool activity and loop completion never reached the trace event log:
/// `ToolExecution`, `ToolRejected`, `LoopIntervention`, `PhaseChange`, and
/// `LoopComplete` were declared here but no code ever emitted them, so every
/// summary reported `tool_executions: 0`, `tools_used: []`, and
/// `status: "unknown"` even for runs whose transcript held the calls (#5997).
///
/// The fix is not a second set of events to keep in step with the transcript.
/// The session that produces `result.tools` and `result.llm` is already the
/// canonical owner of these facts, so the summary reads them from there.
#[derive(Debug, Clone, Default)]
pub struct AgentLoopFacts {
    pub status: String,
    pub iterations: usize,
    /// Wall time for the whole loop, when something canonical measured it.
    /// `None` serializes as null rather than zero: no agent session records a
    /// loop duration today, and a zero would read as an instantaneous run.
    pub total_duration_ms: Option<u64>,
    pub tool_executions: usize,
    pub tool_rejections: usize,
    /// Distinct tools that ran, in first-use order.
    pub tools_used: Vec<String>,
}

/// Fine-grained event emitted during agent loop execution. Captures LLM
/// calls, provider retries, compaction, and typed checkpoints so downstream
/// consumers (portal, IDE hosts, cloud runners) can display execution traces
/// without reconstructing them from raw JSON.
///
/// Tool and loop-lifecycle facts deliberately do NOT live here; see
/// [`AgentLoopFacts`].
#[derive(Debug, Clone, serde::Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AgentTraceEvent {
    LlmCall {
        call_id: String,
        model: String,
        #[serde(flatten)]
        usage: super::usage::LlmUsage,
        duration_ms: u64,
        iteration: usize,
    },
    ContextCompaction {
        archived_messages: usize,
        new_summary_len: usize,
        iteration: usize,
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
    /// Emitted when `llm_call` aborts a streaming provider response
    /// because the partial JSON content can no longer satisfy
    /// `output_schema`. `chunks_consumed` counts text-delta chunks seen
    /// before the abort; `provider` / `model` track the route that fired
    /// so cost dashboards can attribute the savings.
    SchemaStreamAborted {
        provider: String,
        model: String,
        reason: String,
        path: String,
        chunks_consumed: usize,
    },
    TypedCheckpoint {
        name: String,
        status: String,
        checkpoint_attempts: usize,
        llm_attempts: usize,
        error_category: Option<String>,
        errors: Vec<String>,
        repaired: bool,
        final_accepted: bool,
        raw_text: String,
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
        provider: String,
        model: String,
        reason: String,
        duration_ms: u64,
        error: String,
    },
    /// Emitted when a `models:`/`ladder:` model ladder advances from one rung
    /// to the next because the current rung hit a transport-class failure
    /// (connection/timeout/429/5xx/circuit_open). Schema-validation failures
    /// never emit this — they re-ask the SAME rung's model. `from_index` is
    /// the 0-based ladder position that failed; `category` is the failover
    /// error category that drove the advance.
    ModelsAdvance {
        from_index: usize,
        from_model: String,
        to_model: String,
        category: String,
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
///
/// The loop and tool fields report `loop_facts: "unavailable"`, because this
/// entry point has no session to read them from. A caller that holds the
/// session — the terminal agent result — must use
/// [`agent_trace_summary_with_loop`] instead, or it will publish zeros beside
/// a transcript that recorded real tool calls.
pub fn agent_trace_summary() -> serde_json::Value {
    agent_trace_summary_inner(None)
}

/// Produce the summary with loop and tool counters taken from the canonical
/// session state rather than from trace events, which never carried them.
pub fn agent_trace_summary_with_loop(facts: &AgentLoopFacts) -> serde_json::Value {
    agent_trace_summary_inner(Some(facts))
}

fn agent_trace_summary_inner(facts: Option<&AgentLoopFacts>) -> serde_json::Value {
    AGENT_TRACE.with(|v| {
        let events = v.borrow();
        let mut llm_calls = 0usize;
        let mut compactions = 0usize;
        let mut native_text_tool_fallbacks = 0usize;
        let mut native_text_tool_fallback_rejections = 0usize;
        let mut empty_completion_retries = 0usize;
        let mut models_advances = 0usize;
        let mut schema_stream_aborts = 0usize;
        let mut typed_checkpoints = 0usize;
        let mut typed_checkpoint_failures = 0usize;
        let mut total_input_tokens = 0i64;
        let mut total_output_tokens = 0i64;
        let mut total_llm_duration_ms = 0u64;

        let default_facts = AgentLoopFacts::default();
        let loop_facts = facts.unwrap_or(&default_facts);
        let status = if facts.is_some() && !loop_facts.status.is_empty() {
            loop_facts.status.clone()
        } else {
            "unknown".to_string()
        };
        let loop_facts_source = if facts.is_some() {
            "observed"
        } else {
            "unavailable"
        };

        for event in events.iter() {
            match event {
                AgentTraceEvent::LlmCall {
                    usage, duration_ms, ..
                } => {
                    llm_calls += 1;
                    total_input_tokens += usage.input_tokens;
                    total_output_tokens += usage.output_tokens;
                    total_llm_duration_ms += duration_ms;
                }
                AgentTraceEvent::ContextCompaction { .. } => {
                    compactions += 1;
                }
                AgentTraceEvent::SchemaRetry { .. } => {}
                AgentTraceEvent::SchemaStreamAborted { .. } => {
                    schema_stream_aborts += 1;
                }
                AgentTraceEvent::TypedCheckpoint { final_accepted, .. } => {
                    typed_checkpoints += 1;
                    if !final_accepted {
                        typed_checkpoint_failures += 1;
                    }
                }
                AgentTraceEvent::NativeToolFallback { accepted, .. } => {
                    native_text_tool_fallbacks += 1;
                    if !accepted {
                        native_text_tool_fallback_rejections += 1;
                    }
                }
                AgentTraceEvent::EmptyCompletionRetry { .. } => {
                    empty_completion_retries += 1;
                }
                AgentTraceEvent::ModelsAdvance { .. } => {
                    models_advances += 1;
                }
            }
        }

        serde_json::json!({
            // Loop and tool facts come from the session, not from these
            // events. `loop_facts` says whether a caller supplied them: an
            // "unavailable" summary is reporting the absence of a session,
            // not an agent that used no tools.
            "loop_facts": loop_facts_source,
            "status": status,
            "iterations": loop_facts.iterations,
            "total_duration_ms": loop_facts.total_duration_ms,
            "tool_executions": loop_facts.tool_executions,
            "tool_rejections": loop_facts.tool_rejections,
            "tools_used": loop_facts.tools_used,
            // Every provider call, including schema retries, empty-completion
            // retries, and model-ladder advances. `result.llm` counts only the
            // accepted result of each agent turn, so the two legitimately
            // differ; `token_scope` names which is which.
            "token_scope": "every_provider_call",
            "llm_calls": llm_calls,
            "compactions": compactions,
            "native_text_tool_fallbacks": native_text_tool_fallbacks,
            "native_text_tool_fallback_rejections": native_text_tool_fallback_rejections,
            "empty_completion_retries": empty_completion_retries,
            "models_advances": models_advances,
            "schema_stream_aborts": schema_stream_aborts,
            "typed_checkpoints": typed_checkpoints,
            "typed_checkpoint_failures": typed_checkpoint_failures,
            "total_input_tokens": total_input_tokens,
            "total_output_tokens": total_output_tokens,
            "total_llm_duration_ms": total_llm_duration_ms,
        })
    })
}

/// Reset agent trace state. Call between test runs.
pub(crate) fn reset_agent_trace_state() {
    AGENT_TRACE.with(|v| v.borrow_mut().clear());
}
