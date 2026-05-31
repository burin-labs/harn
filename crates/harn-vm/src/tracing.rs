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
use std::time::{Instant, SystemTime, UNIX_EPOCH};

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
    /// `pool.submit()` boundary — accepted, rejected, or queued (PL-06).
    PoolSubmit,
    /// Pool worker picks the task out of the queue (PL-06). Links back to
    /// the originating `PoolSubmit` span across the async boundary so
    /// queue dwell time can be reconstructed from a single trace.
    PoolDequeue,
    /// `emit_channel(...)` boundary — opened at `emit_channel`, closed
    /// after the durable append + trigger fan-out finishes (CH-06 / #1877).
    ChannelEmit,
    /// Channel-source trigger match boundary — opened at trigger fan-out
    /// just before the handler is invoked, closed once dispatch finishes.
    /// Links back to the originating `ChannelEmit` span (multi-link for
    /// batched / aggregated triggers).
    ChannelMatch,
    /// Script-opened user timing span via `std/timing`. Modeled as an
    /// OTel INTERNAL span — distinct from `FnCall` so OTel exporters and
    /// `harn run --profile-json` do not confuse them with LLM/tool work.
    UserTiming,
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
            Self::PoolSubmit => "pool_submit",
            Self::PoolDequeue => "pool_dequeue",
            Self::ChannelEmit => "channel_emit",
            Self::ChannelMatch => "channel_match",
            Self::UserTiming => "user_timing",
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

/// One sub-phase annotation attached to a span. Modeled after OTel span
/// events: a named checkpoint with optional structured attributes that
/// piggy-backs on the enclosing span rather than allocating a new one.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize, PartialEq, Eq)]
#[serde(default)]
pub struct SpanEvent {
    pub name: String,
    /// Wall-clock time of the event in milliseconds since the UNIX epoch.
    pub time_unix_ms: u64,
    /// Monotonic offset from the parent span's start, in milliseconds.
    pub offset_ms: u64,
    #[serde(skip_serializing_if = "BTreeMap::is_empty")]
    pub attributes: BTreeMap<String, serde_json::Value>,
}

/// A completed tracing span.
#[derive(Debug, Clone)]
pub struct Span {
    pub trace_id: String,
    pub span_id: u64,
    pub parent_id: Option<u64>,
    pub kind: SpanKind,
    pub name: String,
    /// Monotonic offset from the collector's epoch, in milliseconds.
    pub start_ms: u64,
    /// Wall-clock start in milliseconds since the UNIX epoch. Recorded
    /// once at `start` for external correlation; duration is always
    /// derived from the monotonic clock, not from wall-clock end - start.
    pub start_unix_ms: u64,
    pub duration_ms: u64,
    pub metadata: BTreeMap<String, serde_json::Value>,
    pub links: Vec<SpanLink>,
    pub events: Vec<SpanEvent>,
}

/// An in-flight span (not yet completed).
struct OpenSpan {
    trace_id: String,
    span_id: u64,
    parent_id: Option<u64>,
    kind: SpanKind,
    name: String,
    started_at: Instant,
    /// Mock-monotonic snapshot at start, captured only when a
    /// `clock_mock` override was active. Pairs with the closing snapshot
    /// to compute deterministic durations under `mock_time(...)`.
    started_at_mock_mono_ms: Option<u64>,
    start_unix_ms: u64,
    metadata: BTreeMap<String, serde_json::Value>,
    links: Vec<SpanLink>,
    events: Vec<SpanEvent>,
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
        let started_at_mock_mono_ms = mock_monotonic_ms();
        let start_unix_ms = wall_clock_ms();

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
                started_at_mock_mono_ms,
                start_unix_ms,
                metadata: BTreeMap::new(),
                links,
                events: Vec::new(),
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

    /// Append a sub-phase annotation to an open span. Returns `true` if
    /// the event was attached; `false` if `span_id` does not match any
    /// open span (already closed or never opened).
    pub fn record_event(
        &mut self,
        span_id: u64,
        name: String,
        attributes: BTreeMap<String, serde_json::Value>,
    ) -> bool {
        let Some(span) = self.open.get_mut(&span_id) else {
            return false;
        };
        let offset_ms = match (span.started_at_mock_mono_ms, mock_monotonic_ms()) {
            (Some(start), Some(now)) => now.saturating_sub(start),
            _ => span.started_at.elapsed().as_millis() as u64,
        };
        span.events.push(SpanEvent {
            name,
            time_unix_ms: wall_clock_ms(),
            offset_ms,
            attributes,
        });
        true
    }

    /// Read the wall-clock start of an open span.
    pub fn open_start_unix_ms(&self, span_id: u64) -> Option<u64> {
        self.open.get(&span_id).map(|span| span.start_unix_ms)
    }

    /// End a span. Moves it from open to completed and returns the
    /// finalized span so callers (e.g. `std/timing`) can read its
    /// `duration_ms` directly without re-scanning `take_spans()`.
    pub fn end(&mut self, span_id: u64) -> Option<Span> {
        let span = self.open.remove(&span_id)?;
        let start_ms = span.started_at.duration_since(self.epoch).as_millis() as u64;
        let duration_ms = match (span.started_at_mock_mono_ms, mock_monotonic_ms()) {
            (Some(start), Some(end)) => end.saturating_sub(start),
            _ => span.started_at.elapsed().as_millis() as u64,
        };

        let mut end_meta = span.metadata.clone();
        end_meta.insert(
            "duration_ms".to_string(),
            serde_json::Value::Number(serde_json::Number::from(duration_ms)),
        );
        crate::events::emit_span_end(span_id, end_meta);

        let completed = Span {
            trace_id: span.trace_id,
            span_id: span.span_id,
            parent_id: span.parent_id,
            kind: span.kind,
            name: span.name,
            start_ms,
            start_unix_ms: span.start_unix_ms,
            duration_ms,
            metadata: span.metadata,
            links: span.links,
            events: span.events,
        };
        self.completed.push(completed.clone());

        if let Some(pos) = self.active_stack.iter().rposition(|&id| id == span_id) {
            self.active_stack.remove(pos);
        }
        Some(completed)
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

/// Best-effort wall-clock millis since the UNIX epoch. Honors an active
/// `clock_mock` override so spans recorded inside `mock_time(...)` blocks
/// align with the rest of the runtime's clock reads; returns 0 only if
/// the host clock is behind the epoch (e.g. unusual sandbox shims).
fn wall_clock_ms() -> u64 {
    if let Some(mock) = crate::clock_mock::active_mock_clock() {
        return mock.now_wall_ms() as u64;
    }
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_millis() as u64)
        .unwrap_or(0)
}

/// Mock-aware monotonic snapshot. Returns `Some(ms)` when a
/// `clock_mock` override is active, `None` otherwise. Span lifecycle
/// pairs the start/end snapshots so durations recorded under
/// `mock_time(...)` reflect `advance_time(...)` instead of real
/// wall-clock progress; spans without an active mock at start fall
/// through to the standard `Instant::elapsed` path on close.
fn mock_monotonic_ms() -> Option<u64> {
    crate::clock_mock::active_mock_clock().map(|mock| mock.now_monotonic_ms() as u64)
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

/// End a span (no-op if span_id is 0). Returns the finalized span when
/// the id was a live open span.
pub fn span_end(span_id: u64) -> Option<Span> {
    if span_id == 0 {
        return None;
    }
    COLLECTOR.with(|c| c.borrow_mut().end(span_id))
}

/// Start a user-timing span. Unlike [`span_start`], this always records
/// regardless of [`is_tracing_enabled`] — `std/timing` callers depend on
/// the returned `duration_ms` to function as a primitive replacement for
/// hand-rolled `now_ms()` subtraction.
pub fn span_start_user_timing(
    name: String,
    attrs: BTreeMap<String, serde_json::Value>,
) -> (u64, String, Option<u64>, u64) {
    COLLECTOR.with(|c| {
        let mut c = c.borrow_mut();
        let id = c.start(SpanKind::UserTiming, name);
        for (key, value) in attrs {
            c.set_metadata(id, &key, value);
        }
        let parent = c.open.get(&id).and_then(|span| span.parent_id);
        let trace_id = c
            .open
            .get(&id)
            .map(|span| span.trace_id.clone())
            .unwrap_or_default();
        let start_unix_ms = c.open_start_unix_ms(id).unwrap_or(0);
        (id, trace_id, parent, start_unix_ms)
    })
}

/// Record a sub-phase event on an open span. No-op when `span_id` is 0
/// or already closed; returns whether the event was attached so callers
/// can surface no-op feedback.
pub fn span_record_event(
    span_id: u64,
    name: String,
    attributes: BTreeMap<String, serde_json::Value>,
) -> bool {
    if span_id == 0 {
        return false;
    }
    COLLECTOR.with(|c| c.borrow_mut().record_event(span_id, name, attributes))
}

/// Attach metadata to an open span. No-op when `span_id` is 0 or
/// already closed.
pub fn span_attach_metadata(span_id: u64, key: &str, value: serde_json::Value) {
    if span_id == 0 {
        return;
    }
    COLLECTOR.with(|c| c.borrow_mut().set_metadata(span_id, key, value));
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
        VmValue::String(std::sync::Arc::from(span.trace_id.as_str())),
    );
    d.insert("span_id".into(), VmValue::Int(span.span_id as i64));
    d.insert(
        "parent_id".into(),
        span.parent_id
            .map(|id| VmValue::Int(id as i64))
            .unwrap_or(VmValue::Nil),
    );
    d.insert(
        "kind".into(),
        VmValue::String(std::sync::Arc::from(span.kind.as_str())),
    );
    d.insert(
        "name".into(),
        VmValue::String(std::sync::Arc::from(span.name.as_str())),
    );
    d.insert("start_ms".into(), VmValue::Int(span.start_ms as i64));
    d.insert(
        "start_unix_ms".into(),
        VmValue::Int(span.start_unix_ms as i64),
    );
    d.insert("duration_ms".into(), VmValue::Int(span.duration_ms as i64));

    if !span.metadata.is_empty() {
        let meta: BTreeMap<String, VmValue> = span
            .metadata
            .iter()
            .map(|(k, v)| (k.clone(), crate::stdlib::json_to_vm_value(v)))
            .collect();
        d.insert("metadata".into(), VmValue::Dict(std::sync::Arc::new(meta)));
    }
    if !span.links.is_empty() {
        d.insert(
            "links".into(),
            crate::stdlib::json_to_vm_value(&serde_json::json!(span.links)),
        );
    }
    if !span.events.is_empty() {
        d.insert(
            "events".into(),
            crate::stdlib::json_to_vm_value(&serde_json::json!(span.events)),
        );
    }

    VmValue::Dict(std::sync::Arc::new(d))
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
        assert!(span_end(id).is_none());
    }

    #[test]
    fn test_user_timing_records_when_tracing_disabled() {
        // UserTiming is the substrate behind `std/timing`. Script
        // callers depend on a real `duration_ms` even when global VM
        // tracing is off, so the collector must always record this
        // kind.
        set_tracing_enabled(false);
        reset_tracing();
        let mut attrs = BTreeMap::new();
        attrs.insert("phase".into(), serde_json::json!("warmup"));
        let (id, trace_id, parent, start_unix_ms) =
            span_start_user_timing("script.work".into(), attrs);
        assert!(id != 0);
        assert!(!trace_id.is_empty());
        assert_eq!(parent, None);
        assert!(start_unix_ms > 0);

        assert!(span_record_event(id, "checkpoint".into(), BTreeMap::new()));

        let closed = span_end(id).expect("user timing always records");
        assert_eq!(closed.kind, SpanKind::UserTiming);
        assert_eq!(closed.events.len(), 1);
        assert_eq!(closed.events[0].name, "checkpoint");
        assert_eq!(closed.metadata["phase"], serde_json::json!("warmup"));

        // The recorded user_timing span survives in the collector
        // snapshot so `trace_spans()` / `harn run --profile-json`
        // surface it alongside the other VM-emitted spans.
        let snapshot = peek_spans();
        assert!(snapshot
            .iter()
            .any(|span| span.kind == SpanKind::UserTiming && span.name == "script.work"));
    }

    #[test]
    fn test_span_event_offset_is_monotonic() {
        let mut c = SpanCollector::new();
        let id = c.start(SpanKind::UserTiming, "outer".into());
        assert!(c.record_event(id, "before".into(), BTreeMap::new()));
        // Wait a beat so the offsets diverge on real Instant clocks.
        std::thread::sleep(std::time::Duration::from_millis(2));
        assert!(c.record_event(id, "after".into(), BTreeMap::new()));
        let closed = c.end(id).expect("open span");
        assert_eq!(closed.events.len(), 2);
        assert!(closed.events[1].offset_ms >= closed.events[0].offset_ms);
    }
}
