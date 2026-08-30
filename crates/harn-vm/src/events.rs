//! Structured event emission for observability.
//!
//! Provides an `EventSink` trait and a thread-local sink registry so that the
//! VM (and especially the LLM layer) can emit structured log and span events
//! instead of raw `eprintln!` calls. Consumers register one or more sinks.

use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

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

/// Trait for receiving structured events from the VM.
pub trait EventSink {
    fn emit_log(&self, event: &LogEvent);
    fn emit_span_start(&self, event: &SpanEvent);
    fn emit_span_end(&self, span_id: u64, metadata: &BTreeMap<String, serde_json::Value>);
}

/// Default sink that writes formatted output to stderr.
pub struct StderrSink;

impl EventSink for StderrSink {
    fn emit_log(&self, event: &LogEvent) {
        if !stderr_level_enabled(event.level) {
            return;
        }
        let level_str = match event.level {
            EventLevel::Trace => "TRACE",
            EventLevel::Debug => "DEBUG",
            EventLevel::Info => "INFO",
            EventLevel::Warn => "WARN",
            EventLevel::Error => "ERROR",
        };
        // "[harn]" prefix for warn/error is relied on by downstream
        // tooling and tests that parse stderr.
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

    fn emit_span_end(&self, _span_id: u64, _metadata: &BTreeMap<String, serde_json::Value>) {}
}

fn stderr_level_enabled(level: EventLevel) -> bool {
    let threshold = std::env::var("HARN_LOG_LEVEL")
        .ok()
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_else(|| "info".to_string());
    let min_level = match threshold.as_str() {
        "trace" => EventLevel::Trace,
        "debug" => EventLevel::Debug,
        "info" | "" => EventLevel::Info,
        "warn" | "warning" => EventLevel::Warn,
        "error" => EventLevel::Error,
        "off" | "none" | "silent" => return false,
        _ => EventLevel::Info,
    };
    event_level_rank(level) >= event_level_rank(min_level)
}

fn event_level_rank(level: EventLevel) -> u8 {
    match level {
        EventLevel::Trace => 0,
        EventLevel::Debug => 1,
        EventLevel::Info => 2,
        EventLevel::Warn => 3,
        EventLevel::Error => 4,
    }
}

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

    fn emit_span_end(&self, _span_id: u64, _metadata: &BTreeMap<String, serde_json::Value>) {}
}

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

/// Log at Debug level with metadata.
pub fn log_debug_meta(
    category: &str,
    message: &str,
    metadata: BTreeMap<String, serde_json::Value>,
) {
    emit_log(EventLevel::Debug, category, message, metadata);
}

/// Log at Warn level with metadata.
pub fn log_warn_meta(category: &str, message: &str, metadata: BTreeMap<String, serde_json::Value>) {
    emit_log(EventLevel::Warn, category, message, metadata);
}

/// OpenTelemetry exporter sink. Requires the `otel` feature flag.
/// Forwards Harn log events and span lifecycle to OTLP collectors.
///
/// Active spans are stored keyed by Harn's `span_id` so that
/// `emit_span_end` can close the correct OTel span.
#[cfg(feature = "otel")]
pub struct OtelSink {
    provider: opentelemetry_sdk::trace::SdkTracerProvider,
    active_spans: std::cell::RefCell<std::collections::HashMap<u64, opentelemetry::Context>>,
}

#[cfg(feature = "otel")]
impl OtelSink {
    /// Create a new OTel sink. Reads OTLP configuration from the
    /// environment (endpoint, service name, headers). Errors when the
    /// span exporter fails to initialise or no endpoint is configured.
    /// Callers should normally use [`install_otel_sink_from_env`], which
    /// treats a missing endpoint as a disabled exporter.
    pub fn new() -> Result<Self, String> {
        let provider = crate::observability::otel::build_tracer_provider_from_env("harn")?
            .ok_or_else(|| "OTel span exporter is not configured".to_string())?;

        Ok(Self {
            provider,
            active_spans: std::cell::RefCell::new(std::collections::HashMap::new()),
        })
    }
}

/// Stable OTel span-end attribute keys exported as top-level attributes.
///
/// Keep this list narrow: span metadata can be script-controlled, and OTel
/// backends charge or index by attribute key. Runtime code that needs another
/// top-level key should add an exact `harn.*` key here rather than letting
/// arbitrary metadata become a new exported attribute name.
#[cfg(feature = "otel")]
const ALLOWED_SPAN_ATTR_KEYS: &[&str] = &[
    "harn.duration_ms",
    "harn.error",
    "harn.error.kind",
    "harn.execution.id",
    "harn.kind",
    "harn.parent_span_id",
    "harn.span_id",
    "harn.status",
];

/// Runtime-owned low-cardinality attribute namespaces.
///
/// These prefixes are for stable schema families, not dynamic suffixes such as
/// run ids, file paths, or UUIDs. Metadata outside this exact-key/prefix
/// allowlist is folded into `harn.meta_json` so OTel sees one bounded key.
#[cfg(feature = "otel")]
const ALLOWED_SPAN_ATTR_PREFIXES: &[&str] = &[
    "harn.cost.",
    "harn.lifecycle.",
    "harn.llm.",
    "harn.step.",
    "harn.timing.",
    "harn.token.",
    "harn.tool.",
    "harn.worker.",
];

#[cfg(feature = "otel")]
fn is_low_cardinality_attr_key(key: &str) -> bool {
    ALLOWED_SPAN_ATTR_KEYS.contains(&key)
        || ALLOWED_SPAN_ATTR_PREFIXES
            .iter()
            .any(|prefix| key.starts_with(prefix))
}

#[cfg(feature = "otel")]
fn otel_span_attributes(metadata: &BTreeMap<String, serde_json::Value>) -> Vec<(String, String)> {
    let policy = crate::redact::current_policy();
    let mut attributes = Vec::new();
    let mut meta_json = BTreeMap::new();

    for (key, value) in metadata {
        if is_low_cardinality_attr_key(key) {
            let raw = format!("{value}");
            let redacted = policy.redact_string(&raw).into_owned();
            attributes.push((key.clone(), redacted));
        } else {
            meta_json.insert(key.clone(), value.clone());
        }
    }

    if !meta_json.is_empty() {
        let raw = serde_json::to_string(&meta_json).unwrap_or_else(|_| "{}".to_string());
        let redacted = policy.redact_string(&raw).into_owned();
        attributes.push(("harn.meta_json".to_string(), redacted));
    }

    attributes
}

/// Idempotency guard for [`install_otel_sink_from_env`]. The first
/// caller wins; later ones become no-ops returning `Ok(false)`. The
/// stored provider keeps the batch processor's runtime alive — and
/// gives [`shutdown_otel_sink`] a handle to flush before the host
/// tokio runtime exits.
#[cfg(feature = "otel")]
static OTEL_PROVIDER: std::sync::OnceLock<
    std::sync::Mutex<Option<opentelemetry_sdk::trace::SdkTracerProvider>>,
> = std::sync::OnceLock::new();

/// Register an [`OtelSink`] into the thread-local event sink chain when
/// the environment is configured for OTLP export.
///
/// Returns `Ok(true)` when a sink was installed, `Ok(false)` when no
/// OTLP endpoint is configured (or when a sink has already been
/// installed for this process), and `Err` when the exporter failed to
/// initialise.
///
/// The exporter activates iff at least one of `HARN_OTEL_ENDPOINT` or
/// the standard `OTEL_EXPORTER_OTLP_ENDPOINT` is non-empty. Service
/// name comes from `HARN_OTEL_SERVICE_NAME` → `OTEL_SERVICE_NAME` →
/// `"harn"` (in that order). Headers come from `HARN_OTEL_HEADERS` →
/// `OTEL_EXPORTER_OTLP_HEADERS` (comma/semicolon-separated
/// `name=value` pairs).
///
/// Hosts (`harn run`, `harn serve acp`, and other embedders) should
/// call this once near process startup so any spans emitted during the
/// session land at the configured collector.
#[cfg(feature = "otel")]
pub fn install_otel_sink_from_env() -> Result<bool, String> {
    if crate::observability::otel::otel_endpoint_from_env().is_none() {
        return Ok(false);
    }
    let provider_slot = OTEL_PROVIDER.get_or_init(|| std::sync::Mutex::new(None));
    {
        let guard = provider_slot.lock().expect("otel provider mutex poisoned");
        if guard.is_some() {
            // A sink was already installed in this process. Don't
            // double-register; the existing one will keep emitting.
            return Ok(false);
        }
    }
    let sink = OtelSink::new()?;
    let provider = sink.provider.clone();
    add_event_sink(Rc::new(sink));
    provider_slot
        .lock()
        .expect("otel provider mutex poisoned")
        .replace(provider);
    Ok(true)
}

/// Flush and tear down the auto-registered OTel sink. Hosts that
/// shut down their tokio runtime before process exit must call this
/// while the runtime is still alive — `BatchSpanProcessor` needs a
/// reactor to drain queued exports, and the [`Drop`] impl on
/// [`OtelSink`] otherwise runs after the runtime is gone. Safe to
/// call when no sink was installed.
///
/// Returns `Ok(true)` when a provider was flushed, `Ok(false)` when
/// none was installed, and `Err` when the SDK reported an export or
/// shutdown error. Errors are advisory — long-running hosts should
/// log and continue.
#[cfg(feature = "otel")]
pub fn shutdown_otel_sink() -> Result<bool, String> {
    let Some(slot) = OTEL_PROVIDER.get() else {
        return Ok(false);
    };
    let provider = {
        let mut guard = slot.lock().expect("otel provider mutex poisoned");
        guard.take()
    };
    let Some(provider) = provider else {
        return Ok(false);
    };
    provider
        .force_flush()
        .map_err(|error| format!("OTel force_flush failed: {error}"))?;
    provider
        .shutdown()
        .map_err(|error| format!("OTel shutdown failed: {error}"))?;
    Ok(true)
}

/// No-op stub for builds compiled without the `otel` feature. Returns
/// `Ok(false)` so call sites can use the same code path on either
/// build.
#[cfg(not(feature = "otel"))]
pub fn install_otel_sink_from_env() -> Result<bool, String> {
    Ok(false)
}

#[cfg(not(feature = "otel"))]
pub fn shutdown_otel_sink() -> Result<bool, String> {
    Ok(false)
}

#[cfg(feature = "otel")]
impl EventSink for OtelSink {
    fn emit_log(&self, event: &LogEvent) {
        use opentelemetry::trace::{Tracer, TracerProvider};
        let tracer = self.provider.tracer("harn");
        // Apply the unified redaction policy to attribute values
        // before they leave the process. The active policy includes
        // the OAuth token catalog (HARN-OAU-001) so a Bearer header
        // or provider token that snuck into a log message gets
        // scrubbed before it lands at the OTel collector.
        let policy = crate::redact::current_policy();
        // Log events are zero-duration spans — start and immediately drop.
        let _span = tracer
            .span_builder(format!("log.{}", event.category))
            .with_attributes(vec![
                opentelemetry::KeyValue::new("level", format!("{:?}", event.level)),
                opentelemetry::KeyValue::new(
                    "message",
                    policy.redact_string(&event.message).into_owned(),
                ),
                opentelemetry::KeyValue::new("category", event.category.clone()),
            ])
            .start(&tracer);
    }

    fn emit_span_start(&self, event: &SpanEvent) {
        use opentelemetry::trace::{TraceContextExt, Tracer, TracerProvider};
        let tracer = self.provider.tracer("harn");
        let parent_context = event
            .parent_id
            .and_then(|parent_id| self.active_spans.borrow().get(&parent_id).cloned())
            .unwrap_or_default();
        let mut attributes = vec![
            opentelemetry::KeyValue::new("harn.span_id", event.span_id as i64),
            opentelemetry::KeyValue::new("harn.kind", event.kind.clone()),
        ];
        if let Some(parent_id) = event.parent_id {
            attributes.push(opentelemetry::KeyValue::new(
                "harn.parent_span_id",
                parent_id as i64,
            ));
        }
        attributes.extend(
            otel_span_attributes(&event.metadata)
                .into_iter()
                .map(|(key, value)| opentelemetry::KeyValue::new(key, value)),
        );
        let span = tracer
            .span_builder(event.name.clone())
            .with_attributes(attributes)
            .start_with_context(&tracer, &parent_context);
        self.active_spans
            .borrow_mut()
            .insert(event.span_id, parent_context.with_span(span));
    }

    fn emit_span_end(&self, span_id: u64, metadata: &BTreeMap<String, serde_json::Value>) {
        use opentelemetry::trace::TraceContextExt;
        if let Some(context) = self.active_spans.borrow_mut().remove(&span_id) {
            let span = context.span();
            // OTel span attributes are the fourth sink covered by the
            // OA-06 token-redaction policy (transcripts, audit
            // receipts, OTel, and system reminders). The helper also
            // bounds attribute-key cardinality by folding unknown
            // metadata into `harn.meta_json`.
            for (key, redacted) in otel_span_attributes(metadata) {
                span.set_attribute(opentelemetry::KeyValue::new(key.clone(), redacted));
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

    #[cfg(feature = "otel")]
    #[derive(Default)]
    struct SpanAttrCollectorSink {
        attrs: RefCell<Vec<(String, String)>>,
    }

    #[cfg(feature = "otel")]
    impl EventSink for SpanAttrCollectorSink {
        fn emit_log(&self, _event: &LogEvent) {}

        fn emit_span_start(&self, _event: &SpanEvent) {}

        fn emit_span_end(&self, _span_id: u64, metadata: &BTreeMap<String, serde_json::Value>) {
            self.attrs
                .borrow_mut()
                .extend(otel_span_attributes(metadata));
        }
    }

    #[cfg(feature = "otel")]
    #[test]
    fn span_attr_keys_are_low_cardinality() {
        let sink = Rc::new(SpanAttrCollectorSink::default());
        clear_event_sinks();
        add_event_sink(sink.clone());

        let rogue_key = "request.550e8400-e29b-41d4-a716-446655440000";
        let mut metadata = BTreeMap::new();
        metadata.insert("harn.kind".to_string(), serde_json::json!("llm_call"));
        metadata.insert(rogue_key.to_string(), serde_json::json!("rogue-value"));

        emit_span_end(42, metadata);
        reset_event_sinks();

        let attrs = sink.attrs.borrow();
        assert!(
            attrs
                .iter()
                .any(|(key, value)| key == "harn.kind" && value.contains("llm_call")),
            "allowlisted harn.kind should remain a top-level OTel attribute: {attrs:?}",
        );
        assert!(
            !attrs.iter().any(|(key, _)| key == rogue_key),
            "rogue metadata key must not become a top-level OTel attribute: {attrs:?}",
        );
        let (_, meta_json) = attrs
            .iter()
            .find(|(key, _)| key == "harn.meta_json")
            .expect("rogue metadata should be folded into harn.meta_json");
        let blob: serde_json::Value =
            serde_json::from_str(meta_json).expect("harn.meta_json should stay JSON");
        assert_eq!(blob[rogue_key], serde_json::json!("rogue-value"));
    }

    #[cfg(not(feature = "otel"))]
    #[test]
    fn install_otel_sink_returns_ok_false_on_non_otel_builds() {
        let installed = install_otel_sink_from_env().expect("non-otel stub never errors");
        assert!(!installed);
    }
}
