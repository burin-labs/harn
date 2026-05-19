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
    /// A `@step`-annotated function while its frame is on the call stack.
    Step,
    /// Host-side VM setup before user bytecode starts executing.
    VmSetup,
    /// Cooperative worker suspension while the durable checkpoint is written.
    Suspension,
    /// Worker resumption after a cooperative suspension.
    Resume,
    /// Pipeline drain / settlement phase.
    Drain,
    /// One drain settlement decision.
    DrainDecision,
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
            Self::Step => "step",
            Self::VmSetup => "vm_setup",
            Self::Suspension => "suspension",
            Self::Resume => "resume",
            Self::Drain => "drain",
            Self::DrainDecision => "drain_decision",
        }
    }
}

/// Link to a span that is causally related but not the parent.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SpanLink {
    pub trace_id: String,
    pub span_id: String,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, String>,
}

impl SpanLink {
    pub fn new(trace_id: impl Into<String>, span_id: impl Into<String>) -> Self {
        Self {
            trace_id: trace_id.into(),
            span_id: span_id.into(),
            attributes: BTreeMap::new(),
        }
    }

    pub fn with_attributes(mut self, attributes: BTreeMap<String, String>) -> Self {
        self.attributes = attributes;
        self
    }
}

/// A completed tracing span.
#[derive(Debug, Clone)]
pub struct Span {
    pub trace_id: String,
    pub span_id: u64,
    pub parent_id: Option<u64>,
    pub kind: SpanKind,
    pub name: String,
    pub start_ms: u64,
    pub duration_ms: u64,
    pub metadata: BTreeMap<String, serde_json::Value>,
    pub links: Vec<SpanLink>,
}

/// An in-flight span (not yet completed).
struct OpenSpan {
    trace_id: String,
    span_id: u64,
    parent_id: Option<u64>,
    kind: SpanKind,
    name: String,
    started_at: Instant,
    metadata: BTreeMap<String, serde_json::Value>,
    links: Vec<SpanLink>,
}

/// Thread-local span collector. Accumulates completed spans and tracks the
/// active span stack for automatic parent assignment.
pub struct SpanCollector {
    trace_id: String,
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
            trace_id: format!("trace_{}", uuid::Uuid::now_v7()),
            active_stack: Vec::new(),
            open: BTreeMap::new(),
            completed: Vec::new(),
            epoch: Instant::now(),
        }
    }

    /// Start a new span. Returns the span ID.
    pub fn start(&mut self, kind: SpanKind, name: String) -> u64 {
        let parent_id = self.active_stack.last().copied();
        self.start_with_parent(kind, name, Vec::new(), parent_id)
    }

    /// Start a new span with non-parent causal links. Returns the span ID.
    pub fn start_with_links(&mut self, kind: SpanKind, name: String, links: Vec<SpanLink>) -> u64 {
        let parent_id = self.active_stack.last().copied();
        self.start_with_parent(kind, name, links, parent_id)
    }

    /// Start a root span with non-parent causal links. Returns the span ID.
    pub fn start_detached_with_links(
        &mut self,
        kind: SpanKind,
        name: String,
        links: Vec<SpanLink>,
    ) -> u64 {
        self.start_with_parent(kind, name, links, None)
    }

    fn start_with_parent(
        &mut self,
        kind: SpanKind,
        name: String,
        links: Vec<SpanLink>,
        parent_id: Option<u64>,
    ) -> u64 {
        let id = self.next_id;
        self.next_id += 1;
        let now = Instant::now();

        let mut event_metadata = BTreeMap::new();
        if !links.is_empty() {
            event_metadata.insert("links".to_string(), serde_json::json!(links));
        }
        crate::events::emit_span_start(id, parent_id, &name, kind.as_str(), event_metadata);

        self.open.insert(
            id,
            OpenSpan {
                trace_id: self.trace_id.clone(),
                span_id: id,
                parent_id,
                kind,
                name,
                started_at: now,
                metadata: BTreeMap::new(),
                links,
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
            let start_ms = span.started_at.duration_since(self.epoch).as_millis() as u64;
            let duration_ms = duration.as_millis() as u64;

            let mut end_meta = span.metadata.clone();
            end_meta.insert(
                "duration_ms".to_string(),
                serde_json::Value::Number(serde_json::Number::from(duration_ms)),
            );
            crate::events::emit_span_end(span_id, end_meta);

            self.completed.push(Span {
                trace_id: span.trace_id,
                span_id: span.span_id,
                parent_id: span.parent_id,
                kind: span.kind,
                name: span.name,
                start_ms,
                duration_ms,
                metadata: span.metadata,
                links: span.links,
            });

            if let Some(pos) = self.active_stack.iter().rposition(|&id| id == span_id) {
                self.active_stack.remove(pos);
            }
        }
    }

    /// Get the current active span ID (if any).
    pub fn current_span_id(&self) -> Option<u64> {
        self.active_stack.last().copied()
    }

    /// Build a serializable link for an open span.
    pub fn span_link(&self, span_id: u64) -> Option<SpanLink> {
        self.open
            .get(&span_id)
            .map(|span| SpanLink::new(span.trace_id.clone(), span.span_id.to_string()))
    }

    /// Build a serializable link for the current active span.
    pub fn current_span_link(&self) -> Option<SpanLink> {
        self.current_span_id()
            .and_then(|span_id| self.span_link(span_id))
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
        self.trace_id = format!("trace_{}", uuid::Uuid::now_v7());
        self.epoch = Instant::now();
    }
}

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

/// Start a span with non-parent causal links (no-op if tracing disabled).
pub fn span_start_with_links(kind: SpanKind, name: String, links: Vec<SpanLink>) -> u64 {
    if !is_tracing_enabled() {
        return 0;
    }
    COLLECTOR.with(|c| c.borrow_mut().start_with_links(kind, name, links))
}

/// Start a root span with non-parent causal links (no-op if tracing disabled).
pub fn span_start_detached_with_links(kind: SpanKind, name: String, links: Vec<SpanLink>) -> u64 {
    if !is_tracing_enabled() {
        return 0;
    }
    COLLECTOR.with(|c| c.borrow_mut().start_detached_with_links(kind, name, links))
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

/// Get the currently active span id, if tracing is enabled and a span is open.
pub fn current_span_id() -> Option<u64> {
    if !is_tracing_enabled() {
        return None;
    }
    COLLECTOR.with(|c| c.borrow().current_span_id())
}

/// Return a link reference for an open span.
pub fn span_link(span_id: u64) -> Option<SpanLink> {
    if span_id == 0 || !is_tracing_enabled() {
        return None;
    }
    COLLECTOR.with(|c| c.borrow().span_link(span_id))
}

/// Return a link reference for the current active span.
pub fn current_span_link() -> Option<SpanLink> {
    if !is_tracing_enabled() {
        return None;
    }
    COLLECTOR.with(|c| c.borrow().current_span_link())
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

/// Convert a span to a VmValue dict for user access.
pub fn span_to_vm_value(span: &Span) -> VmValue {
    let mut d = BTreeMap::new();
    d.insert(
        "trace_id".into(),
        VmValue::String(Rc::from(span.trace_id.as_str())),
    );
    d.insert("span_id".into(), VmValue::Int(span.span_id as i64));
    d.insert(
        "parent_id".into(),
        span.parent_id
            .map(|id| VmValue::Int(id as i64))
            .unwrap_or(VmValue::Nil),
    );
    d.insert("kind".into(), VmValue::String(Rc::from(span.kind.as_str())));
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
    if !span.links.is_empty() {
        d.insert(
            "links".into(),
            crate::stdlib::json_to_vm_value(&serde_json::json!(span.links)),
        );
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

    fn print_tree(spans: &[Span], parent_id: Option<u64>, depth: usize, lines: &mut Vec<String>) {
        let children: Vec<&Span> = spans.iter().filter(|s| s.parent_id == parent_id).collect();
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
        assert!(c.span_link(id).is_some());
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
    fn test_span_links_are_preserved() {
        let mut c = SpanCollector::new();
        let parent = c.start(SpanKind::Suspension, "suspend worker".into());
        let link = c.span_link(parent).expect("link for open span");
        c.end(parent);

        let child = c.start_with_links(SpanKind::Resume, "resume worker".into(), vec![link]);
        c.end(child);

        assert_eq!(c.spans().len(), 2);
        assert_eq!(c.spans()[1].parent_id, None);
        assert_eq!(c.spans()[1].links.len(), 1);
        assert_eq!(c.spans()[1].links[0].span_id, parent.to_string());
    }

    #[test]
    fn test_detached_span_links_do_not_inherit_active_parent() {
        let mut c = SpanCollector::new();
        let pipeline = c.start(SpanKind::Pipeline, "pipeline".into());
        let link = c.span_link(pipeline).expect("pipeline link");
        let drain = c.start_detached_with_links(SpanKind::Drain, "drain".into(), vec![link]);
        c.end(drain);
        c.end(pipeline);

        let drain = c
            .spans()
            .iter()
            .find(|span| span.kind == SpanKind::Drain)
            .expect("drain span");
        assert_eq!(drain.parent_id, None);
        assert_eq!(drain.links.len(), 1);
        assert_eq!(drain.links[0].span_id, pipeline.to_string());
    }

    #[test]
    fn test_noop_when_disabled() {
        set_tracing_enabled(false);
        let id = span_start(SpanKind::Pipeline, "test".into());
        assert_eq!(id, 0);
        span_end(id);
    }
}
