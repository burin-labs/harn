//! Pipeline Observability: structured tracing spans with parent/child relationships.
//!
//! When tracing is enabled (`vm.enable_tracing()`), the VM automatically emits
//! spans for pipeline execution, function calls, LLM calls, tool invocations,
//! imports, and async operations. Spans form a tree via parent_span_id.
//!
//! Access via builtins: `trace_spans()` returns all completed spans,
//! `trace_summary()` returns a formatted summary.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;
use std::time::Instant;

use crate::value::VmValue;

// =============================================================================
// Span types
// =============================================================================

/// The kind of operation a span represents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SpanKind {
    Pipeline,
    FnCall,
    LlmCall,
    ToolCall,
    Import,
    Parallel,
    Spawn,
}

impl SpanKind {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Pipeline => "pipeline",
            Self::FnCall => "fn_call",
            Self::LlmCall => "llm_call",
            Self::ToolCall => "tool_call",
            Self::Import => "import",
            Self::Parallel => "parallel",
            Self::Spawn => "spawn",
        }
    }
}

/// A completed tracing span.
#[derive(Debug, Clone)]
pub struct Span {
    pub span_id: u64,
    pub parent_id: Option<u64>,
    pub kind: SpanKind,
    pub name: String,
    pub start_ms: u64,
    pub duration_ms: u64,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

/// An in-flight span (not yet completed).
struct OpenSpan {
    span_id: u64,
    parent_id: Option<u64>,
    kind: SpanKind,
    name: String,
    started_at: Instant,
    metadata: BTreeMap<String, serde_json::Value>,
}

// =============================================================================
// Collector
// =============================================================================

/// Thread-local span collector. Accumulates completed spans and tracks the
/// active span stack for automatic parent assignment.
pub struct SpanCollector {
    next_id: u64,
    /// Stack of open span IDs — the top is the current active span.
    active_stack: Vec<u64>,
    /// Open (in-flight) spans keyed by ID.
    open: BTreeMap<u64, OpenSpan>,
    /// Completed spans in chronological order.
    completed: Vec<Span>,
    /// Epoch for relative timing.
    epoch: Instant,
}

impl Default for SpanCollector {
    fn default() -> Self {
        Self::new()
    }
}

impl SpanCollector {
    pub fn new() -> Self {
        Self {
            next_id: 1,
            active_stack: Vec::new(),
            open: BTreeMap::new(),
            completed: Vec::new(),
            epoch: Instant::now(),
        }
    }

    /// Start a new span. Returns the span ID.
    pub fn start(&mut self, kind: SpanKind, name: String) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let parent_id = self.active_stack.last().copied();
        let now = Instant::now();

        self.open.insert(
            id,
            OpenSpan {
                span_id: id,
                parent_id,
                kind,
                name,
                started_at: now,
                metadata: BTreeMap::new(),
            },
        );
        self.active_stack.push(id);
        id
    }

    /// Attach metadata to an open span.
    pub fn set_metadata(&mut self, span_id: u64, key: &str, value: serde_json::Value) {
        if let Some(span) = self.open.get_mut(&span_id) {
            span.metadata.insert(key.to_string(), value);
        }
    }

    /// End a span. Moves it from open to completed.
    pub fn end(&mut self, span_id: u64) {
        if let Some(span) = self.open.remove(&span_id) {
            let duration = span.started_at.elapsed();
            let start_ms = span
                .started_at
                .duration_since(self.epoch)
                .as_millis() as u64;

            self.completed.push(Span {
                span_id: span.span_id,
                parent_id: span.parent_id,
                kind: span.kind,
                name: span.name,
                start_ms,
                duration_ms: duration.as_millis() as u64,
                metadata: span.metadata,
            });

            // Remove from active stack
            if let Some(pos) = self.active_stack.iter().rposition(|&id| id == span_id) {
                self.active_stack.remove(pos);
            }
        }
    }

    /// Get the current active span ID (if any).
    pub fn current_span_id(&self) -> Option<u64> {
        self.active_stack.last().copied()
    }

    /// Take all completed spans (drains the collector).
    pub fn take_spans(&mut self) -> Vec<Span> {
        std::mem::take(&mut self.completed)
    }

    /// Peek at all completed spans (non-destructive).
    pub fn spans(&self) -> &[Span] {
        &self.completed
    }

    /// Reset the collector.
    pub fn reset(&mut self) {
        self.active_stack.clear();
        self.open.clear();
        self.completed.clear();
        self.next_id = 1;
        self.epoch = Instant::now();
    }
}

// =============================================================================
// Thread-local collector
// =============================================================================

thread_local! {
    static COLLECTOR: RefCell<SpanCollector> = RefCell::new(SpanCollector::new());
    static TRACING_ENABLED: RefCell<bool> = const { RefCell::new(false) };
}

/// Enable or disable VM tracing for the current thread.
pub fn set_tracing_enabled(enabled: bool) {
    TRACING_ENABLED.with(|e| *e.borrow_mut() = enabled);
    if enabled {
        COLLECTOR.with(|c| c.borrow_mut().reset());
    }
}

/// Check if tracing is enabled.
pub fn is_tracing_enabled() -> bool {
    TRACING_ENABLED.with(|e| *e.borrow())
}

/// Start a span (no-op if tracing disabled). Returns span ID or 0.
pub fn span_start(kind: SpanKind, name: String) -> u64 {
    if !is_tracing_enabled() {
        return 0;
    }
    COLLECTOR.with(|c| c.borrow_mut().start(kind, name))
}

/// Attach metadata to an open span (no-op if span_id is 0).
pub fn span_set_metadata(span_id: u64, key: &str, value: serde_json::Value) {
    if span_id == 0 {
        return;
    }
    COLLECTOR.with(|c| c.borrow_mut().set_metadata(span_id, key, value));
}

/// End a span (no-op if span_id is 0).
pub fn span_end(span_id: u64) {
    if span_id == 0 {
        return;
    }
    COLLECTOR.with(|c| c.borrow_mut().end(span_id));
}

/// Take all completed spans.
pub fn take_spans() -> Vec<Span> {
    COLLECTOR.with(|c| c.borrow_mut().take_spans())
}

/// Peek at completed spans (cloned).
pub fn peek_spans() -> Vec<Span> {
    COLLECTOR.with(|c| c.borrow().spans().to_vec())
}

/// Reset the tracing collector.
pub fn reset_tracing() {
    COLLECTOR.with(|c| c.borrow_mut().reset());
}

// =============================================================================
// VmValue conversion
// =============================================================================

/// Convert a span to a VmValue dict for user access.
pub fn span_to_vm_value(span: &Span) -> VmValue {
    let mut d = BTreeMap::new();
    d.insert("span_id".into(), VmValue::Int(span.span_id as i64));
    d.insert(
        "parent_id".into(),
        span.parent_id
            .map(|id| VmValue::Int(id as i64))
            .unwrap_or(VmValue::Nil),
    );
    d.insert(
        "kind".into(),
        VmValue::String(Rc::from(span.kind.as_str())),
    );
    d.insert("name".into(), VmValue::String(Rc::from(span.name.as_str())));
    d.insert("start_ms".into(), VmValue::Int(span.start_ms as i64));
    d.insert("duration_ms".into(), VmValue::Int(span.duration_ms as i64));

    if !span.metadata.is_empty() {
        let meta: BTreeMap<String, VmValue> = span
            .metadata
            .iter()
            .map(|(k, v)| (k.clone(), crate::stdlib::json_to_vm_value(v)))
            .collect();
        d.insert("metadata".into(), VmValue::Dict(Rc::new(meta)));
    }

    VmValue::Dict(Rc::new(d))
}

/// Generate a formatted summary of all spans.
pub fn format_summary() -> String {
    let spans = peek_spans();
    if spans.is_empty() {
        return "No spans recorded.".into();
    }

    let mut lines = Vec::new();
    let total_ms: u64 = spans
        .iter()
        .filter(|s| s.parent_id.is_none())
        .map(|s| s.duration_ms)
        .sum();

    lines.push(format!("Trace: {} spans, {total_ms}ms total", spans.len()));
    lines.push(String::new());

    // Build tree structure
    fn print_tree(
        spans: &[Span],
        parent_id: Option<u64>,
        depth: usize,
        lines: &mut Vec<String>,
    ) {
        let children: Vec<&Span> = spans
            .iter()
            .filter(|s| s.parent_id == parent_id)
            .collect();
        for span in children {
            let indent = "  ".repeat(depth);
            let meta_str = if span.metadata.is_empty() {
                String::new()
            } else {
                let parts: Vec<String> = span
                    .metadata
                    .iter()
                    .map(|(k, v)| format!("{k}={v}"))
                    .collect();
                format!(" ({})", parts.join(", "))
            };
            lines.push(format!(
                "{indent}{} {} {}ms{meta_str}",
                span.kind.as_str(),
                span.name,
                span.duration_ms,
            ));
            print_tree(spans, Some(span.span_id), depth + 1, lines);
        }
    }

    print_tree(&spans, None, 0, &mut lines);
    lines.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_span_collector_basic() {
        let mut c = SpanCollector::new();
        let id = c.start(SpanKind::Pipeline, "main".into());
        assert_eq!(id, 1);
        assert_eq!(c.current_span_id(), Some(1));
        c.end(id);
        assert_eq!(c.current_span_id(), None);
        assert_eq!(c.spans().len(), 1);
        assert_eq!(c.spans()[0].name, "main");
        assert_eq!(c.spans()[0].parent_id, None);
    }

    #[test]
    fn test_span_parent_child() {
        let mut c = SpanCollector::new();
        let parent = c.start(SpanKind::Pipeline, "main".into());
        let child = c.start(SpanKind::FnCall, "helper".into());
        c.end(child);
        c.end(parent);
        assert_eq!(c.spans().len(), 2);
        assert_eq!(c.spans()[0].parent_id, Some(parent));
        assert_eq!(c.spans()[1].parent_id, None);
    }

    #[test]
    fn test_span_metadata() {
        let mut c = SpanCollector::new();
        let id = c.start(SpanKind::LlmCall, "gpt-4".into());
        c.set_metadata(id, "tokens", serde_json::json!(100));
        c.end(id);
        assert_eq!(c.spans()[0].metadata["tokens"], serde_json::json!(100));
    }

    #[test]
    fn test_noop_when_disabled() {
        set_tracing_enabled(false);
        let id = span_start(SpanKind::Pipeline, "test".into());
        assert_eq!(id, 0);
        span_end(id); // should not panic
    }
}
