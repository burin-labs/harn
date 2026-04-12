//! Structured event emission for observability.
//!
//! Provides an `EventSink` trait and a thread-local sink registry so that the
//! VM (and especially the LLM layer) can emit structured log and span events
//! instead of raw `eprintln!` calls.  Consumers register one or more sinks;
//! the default `StderrSink` preserves backward-compatible stderr output.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

// =============================================================================
// Event types
// =============================================================================

/// Severity level for log events.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EventLevel {
    Trace,
    Debug,
    Info,
    Warn,
    Error,
}

/// A structured log event.
#[derive(Clone, Debug)]
pub struct LogEvent {
    pub level: EventLevel,
    pub category: String,
    pub message: String,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

/// A structured span event (start or end).
#[derive(Clone, Debug)]
pub struct SpanEvent {
    pub span_id: u64,
    pub parent_id: Option<u64>,
    pub name: String,
    pub kind: String,
    pub metadata: BTreeMap<String, serde_json::Value>,
}

// =============================================================================
// EventSink trait
// =============================================================================

/// Trait for receiving structured events from the VM.
pub trait EventSink {
    fn emit_log(&self, event: &LogEvent);
    fn emit_span_start(&self, event: &SpanEvent);
    fn emit_span_end(&self, span_id: u64, metadata: &BTreeMap<String, serde_json::Value>);
}

// =============================================================================
// StderrSink — default, backward-compatible
// =============================================================================

/// Default sink that writes formatted output to stderr (preserves current behavior).
pub struct StderrSink;

impl EventSink for StderrSink {
    fn emit_log(&self, event: &LogEvent) {
        let level_str = match event.level {
            EventLevel::Trace => "TRACE",
            EventLevel::Debug => "DEBUG",
            EventLevel::Info => "INFO",
            EventLevel::Warn => "WARN",
            EventLevel::Error => "ERROR",
        };
        // Preserve the existing "[harn]" prefix style for warn/error so
        // that downstream tooling and tests that parse stderr are unaffected.
        match event.level {
            EventLevel::Warn => {
                eprintln!("[harn] warning: {}", event.message);
            }
            EventLevel::Error => {
                eprintln!("[harn] error: {}", event.message);
            }
            _ => {
                eprintln!("[{level_str}] [{}] {}", event.category, event.message);
            }
        }
    }

    fn emit_span_start(&self, _event: &SpanEvent) {
        // Silent by default — spans are for observability backends.
    }

    fn emit_span_end(&self, _span_id: u64, _metadata: &BTreeMap<String, serde_json::Value>) {
        // Silent by default.
    }
}

// =============================================================================
// CollectorSink — for testing and inspection
// =============================================================================

/// A sink that collects events for later retrieval (testing, inspection).
pub struct CollectorSink {
    pub logs: RefCell<Vec<LogEvent>>,
    pub spans: RefCell<Vec<SpanEvent>>,
}

impl CollectorSink {
    pub fn new() -> Self {
        Self {
            logs: RefCell::new(Vec::new()),
            spans: RefCell::new(Vec::new()),
        }
    }
}

impl Default for CollectorSink {
    fn default() -> Self {
        Self::new()
    }
}

impl EventSink for CollectorSink {
    fn emit_log(&self, event: &LogEvent) {
        self.logs.borrow_mut().push(event.clone());
    }

    fn emit_span_start(&self, event: &SpanEvent) {
        self.spans.borrow_mut().push(event.clone());
    }

    fn emit_span_end(&self, _span_id: u64, _metadata: &BTreeMap<String, serde_json::Value>) {
        // Could store end events if needed; for now just track starts.
    }
}

// =============================================================================
// Thread-local sink registry
// =============================================================================

thread_local! {
    static EVENT_SINKS: RefCell<Vec<Rc<dyn EventSink>>> = RefCell::new(vec![Rc::new(StderrSink)]);
}

/// Register an additional event sink.
pub fn add_event_sink(sink: Rc<dyn EventSink>) {
    EVENT_SINKS.with(|sinks| sinks.borrow_mut().push(sink));
}

/// Remove all sinks (including the default `StderrSink`).
pub fn clear_event_sinks() {
    EVENT_SINKS.with(|sinks| sinks.borrow_mut().clear());
}

/// Reset sinks to just the default `StderrSink`.
pub fn reset_event_sinks() {
    EVENT_SINKS.with(|sinks| {
        let mut s = sinks.borrow_mut();
        s.clear();
        s.push(Rc::new(StderrSink));
    });
}

// =============================================================================
// Emission helpers
// =============================================================================

/// Emit a structured log event to all registered sinks.
pub fn emit_log(
    level: EventLevel,
    category: &str,
    message: &str,
    metadata: BTreeMap<String, serde_json::Value>,
) {
    let event = LogEvent {
        level,
        category: category.to_string(),
        message: message.to_string(),
        metadata,
    };
    EVENT_SINKS.with(|sinks| {
        for sink in sinks.borrow().iter() {
            sink.emit_log(&event);
        }
    });
}

/// Emit a span-start event to all registered sinks.
pub fn emit_span_start(
    span_id: u64,
    parent_id: Option<u64>,
    name: &str,
    kind: &str,
    metadata: BTreeMap<String, serde_json::Value>,
) {
    let event = SpanEvent {
        span_id,
        parent_id,
        name: name.to_string(),
        kind: kind.to_string(),
        metadata,
    };
    EVENT_SINKS.with(|sinks| {
        for sink in sinks.borrow().iter() {
            sink.emit_span_start(&event);
        }
    });
}

/// Emit a span-end event to all registered sinks.
pub fn emit_span_end(span_id: u64, metadata: BTreeMap<String, serde_json::Value>) {
    EVENT_SINKS.with(|sinks| {
        for sink in sinks.borrow().iter() {
            sink.emit_span_end(span_id, &metadata);
        }
    });
}

// =============================================================================
// Convenience functions
// =============================================================================

/// Log at Info level with no metadata.
pub fn log_info(category: &str, message: &str) {
    emit_log(EventLevel::Info, category, message, BTreeMap::new());
}

/// Log at Warn level with no metadata.
pub fn log_warn(category: &str, message: &str) {
    emit_log(EventLevel::Warn, category, message, BTreeMap::new());
}

/// Log at Error level with no metadata.
pub fn log_error(category: &str, message: &str) {
    emit_log(EventLevel::Error, category, message, BTreeMap::new());
}

/// Log at Debug level with no metadata.
pub fn log_debug(category: &str, message: &str) {
    emit_log(EventLevel::Debug, category, message, BTreeMap::new());
}

/// Log at Info level with metadata.
pub fn log_info_meta(category: &str, message: &str, metadata: BTreeMap<String, serde_json::Value>) {
    emit_log(EventLevel::Info, category, message, metadata);
}

/// Log at Warn level with metadata.
pub fn log_warn_meta(category: &str, message: &str, metadata: BTreeMap<String, serde_json::Value>) {
    emit_log(EventLevel::Warn, category, message, metadata);
}

// =============================================================================
// OTel stub (behind feature flag)
// =============================================================================

/// OpenTelemetry exporter sink. Requires the `otel` feature flag.
/// Forwards Harn log events and span lifecycle to OTLP collectors.
///
/// Active spans are stored keyed by Harn's `span_id` so that
/// `emit_span_end` can close the correct OTel span.
#[cfg(feature = "otel")]
pub struct OtelSink {
    provider: opentelemetry_sdk::trace::SdkTracerProvider,
    active_spans:
        std::cell::RefCell<std::collections::HashMap<u64, opentelemetry_sdk::trace::Span>>,
}

#[cfg(feature = "otel")]
impl OtelSink {
    /// Create a new OTel sink. Initialises the OTLP span exporter
    /// (default endpoint via OTEL_EXPORTER_OTLP_ENDPOINT, or localhost:4318).
    pub fn new() -> Result<Self, String> {
        use opentelemetry_otlp::SpanExporter;
        use opentelemetry_sdk::trace::SdkTracerProvider;

        let exporter = SpanExporter::builder()
            .with_http()
            .build()
            .map_err(|e| format!("OTel span exporter init failed: {e}"))?;

        let provider = SdkTracerProvider::builder()
            .with_batch_exporter(exporter)
            .build();

        opentelemetry::global::set_tracer_provider(provider.clone());

        Ok(Self {
            provider,
            active_spans: std::cell::RefCell::new(std::collections::HashMap::new()),
        })
    }
}

#[cfg(feature = "otel")]
impl EventSink for OtelSink {
    fn emit_log(&self, event: &LogEvent) {
        use opentelemetry::trace::{Tracer, TracerProvider};
        let tracer = self.provider.tracer("harn");
        // Log events are zero-duration spans — start and immediately drop.
        let _span = tracer
            .span_builder(format!("log.{}", event.category))
            .with_attributes(vec![
                opentelemetry::KeyValue::new("level", format!("{:?}", event.level)),
                opentelemetry::KeyValue::new("message", event.message.clone()),
                opentelemetry::KeyValue::new("category", event.category.clone()),
            ])
            .start(&tracer);
    }

    fn emit_span_start(&self, event: &SpanEvent) {
        use opentelemetry::trace::{Tracer, TracerProvider};
        let tracer = self.provider.tracer("harn");
        let span = tracer
            .span_builder(event.name.clone())
            .with_attributes(vec![
                opentelemetry::KeyValue::new("harn.span_id", event.span_id as i64),
                opentelemetry::KeyValue::new("harn.kind", event.kind.clone()),
            ])
            .start(&tracer);
        self.active_spans.borrow_mut().insert(event.span_id, span);
    }

    fn emit_span_end(&self, span_id: u64, metadata: &BTreeMap<String, serde_json::Value>) {
        use opentelemetry::trace::Span;
        if let Some(mut span) = self.active_spans.borrow_mut().remove(&span_id) {
            for (key, value) in metadata {
                span.set_attribute(opentelemetry::KeyValue::new(
                    key.clone(),
                    format!("{value}"),
                ));
            }
            span.end();
        }
    }
}

#[cfg(feature = "otel")]
impl Drop for OtelSink {
    fn drop(&mut self) {
        // End any spans that were never closed (abnormal shutdown).
        self.active_spans.borrow_mut().clear();
        let _ = self.provider.shutdown();
    }
}

// =============================================================================
// Tests
// =============================================================================

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_collector_sink_captures_logs() {
        let sink = Rc::new(CollectorSink::new());
        clear_event_sinks();
        add_event_sink(sink.clone());

        log_info("llm", "test message");
        log_warn("llm.cost", "cost warning");
        log_error("llm.agent", "agent error");

        let logs = sink.logs.borrow();
        assert_eq!(logs.len(), 3);
        assert_eq!(logs[0].level, EventLevel::Info);
        assert_eq!(logs[0].category, "llm");
        assert_eq!(logs[0].message, "test message");
        assert_eq!(logs[1].level, EventLevel::Warn);
        assert_eq!(logs[2].level, EventLevel::Error);

        // Restore default sinks for other tests.
        reset_event_sinks();
    }

    #[test]
    fn test_collector_sink_captures_spans() {
        let sink = Rc::new(CollectorSink::new());
        clear_event_sinks();
        add_event_sink(sink.clone());

        emit_span_start(1, None, "agent_loop", "llm_call", BTreeMap::new());
        emit_span_end(1, BTreeMap::new());

        let spans = sink.spans.borrow();
        assert_eq!(spans.len(), 1);
        assert_eq!(spans[0].span_id, 1);
        assert_eq!(spans[0].name, "agent_loop");

        reset_event_sinks();
    }

    #[test]
    fn test_stderr_sink_does_not_panic() {
        let sink = StderrSink;
        let event = LogEvent {
            level: EventLevel::Warn,
            category: "test".into(),
            message: "hello".into(),
            metadata: BTreeMap::new(),
        };
        sink.emit_log(&event);
        sink.emit_span_start(&SpanEvent {
            span_id: 1,
            parent_id: None,
            name: "x".into(),
            kind: "y".into(),
            metadata: BTreeMap::new(),
        });
        sink.emit_span_end(1, &BTreeMap::new());
    }

    #[test]
    fn test_multiple_sinks() {
        let a = Rc::new(CollectorSink::new());
        let b = Rc::new(CollectorSink::new());
        clear_event_sinks();
        add_event_sink(a.clone());
        add_event_sink(b.clone());

        log_debug("x", "msg");

        assert_eq!(a.logs.borrow().len(), 1);
        assert_eq!(b.logs.borrow().len(), 1);

        reset_event_sinks();
    }

    #[test]
    fn test_log_with_metadata() {
        let sink = Rc::new(CollectorSink::new());
        clear_event_sinks();
        add_event_sink(sink.clone());

        let mut meta = BTreeMap::new();
        meta.insert("tokens".into(), serde_json::json!(42));
        log_info_meta("llm", "token usage", meta);

        let logs = sink.logs.borrow();
        assert_eq!(logs[0].metadata["tokens"], serde_json::json!(42));

        reset_event_sinks();
    }
}
