use std::cell::RefCell;

// =============================================================================
// LLM trace log (thread-local for async-safe access)
// =============================================================================

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
