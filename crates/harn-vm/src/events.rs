//! Structured event emission for observability.
//!
//! Provides an `EventSink` trait and a thread-local sink registry so that the
//! VM (and especially the LLM layer) can emit structured log and span events
//! instead of raw `eprintln!` calls.  Consumers register one or more sinks;
//! the default `StderrSink` preserves backward-compatible stderr output.

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
    active_spans:
        std::cell::RefCell<std::collections::HashMap<u64, opentelemetry_sdk::trace::Span>>,
}

#[cfg(feature = "otel")]
impl OtelSink {
    /// Create a new OTel sink. Reads OTLP configuration from the
    /// environment (endpoint, service name, headers). Errors when the
    /// span exporter fails to initialise — a missing endpoint is **not**
    /// an error; the exporter falls back to OpenTelemetry's default
    /// (`http://localhost:4318/v1/traces`). Callers that want
    /// presence-of-endpoint to gate registration should use
    /// [`install_otel_sink_from_env`].
    pub fn new() -> Result<Self, String> {
        use opentelemetry::global;
        use opentelemetry_otlp::{
            Protocol, SpanExporter, WithExportConfig as _, WithHttpConfig as _,
        };
        use opentelemetry_sdk::runtime;
        use opentelemetry_sdk::trace::span_processor_with_async_runtime::BatchSpanProcessor;
        use opentelemetry_sdk::trace::SdkTracerProvider;
        use opentelemetry_sdk::Resource;

        let endpoint = otel_endpoint_from_env();
        let headers = otel_headers_from_env();
        let service_name = otel_service_name_from_env();

        // opentelemetry-otlp does not pull in any default HTTP client
        // because the `reqwest-rustls` feature only opts the dep in —
        // the exporter still requires an explicit client. Reuse the
        // same reqwest configuration as the orchestrator-side
        // provider so both surfaces hit the collector with identical
        // TLS + connection-pool behaviour.
        let http_client = reqwest::Client::builder()
            .build()
            .map_err(|error| format!("failed to build OTLP HTTP client: {error}"))?;

        let mut exporter_builder = SpanExporter::builder()
            .with_http()
            .with_http_client(http_client)
            .with_protocol(Protocol::HttpJson)
            .with_headers(headers);
        if let Some(endpoint) = endpoint.as_deref() {
            exporter_builder =
                exporter_builder.with_endpoint(normalize_otlp_traces_endpoint(endpoint));
        }
        let exporter = exporter_builder
            .build()
            .map_err(|e| format!("OTel span exporter init failed: {e}"))?;

        // Drive the batch processor on the current Tokio runtime so
        // the exporter's reqwest client can reach the network. The
        // default SDK processor spawns its own thread, which has no
        // Tokio reactor and panics on the first send — we need the
        // async-runtime variant for the same reason the orchestrator
        // path uses it.
        let provider = SdkTracerProvider::builder()
            .with_resource(Resource::builder().with_service_name(service_name).build())
            .with_span_processor(BatchSpanProcessor::builder(exporter, runtime::Tokio).build())
            .build();

        global::set_tracer_provider(provider.clone());

        Ok(Self {
            provider,
            active_spans: std::cell::RefCell::new(std::collections::HashMap::new()),
        })
    }
}

/// The host spawns a fresh `harn` child per session, so the only
/// reliable way to opt into local trace export is via environment
/// variables read at startup. Prefer the Harn-specific variable so a
/// caller that points an unrelated process at an OTLP collector via
/// the shared OpenTelemetry variable doesn't accidentally enable Harn
/// emission too.
#[cfg(feature = "otel")]
fn otel_endpoint_from_env() -> Option<String> {
    for name in ["HARN_OTEL_ENDPOINT", "OTEL_EXPORTER_OTLP_ENDPOINT"] {
        if let Ok(value) = std::env::var(name) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }
    None
}

#[cfg(feature = "otel")]
fn otel_service_name_from_env() -> String {
    for name in ["HARN_OTEL_SERVICE_NAME", "OTEL_SERVICE_NAME"] {
        if let Ok(value) = std::env::var(name) {
            let trimmed = value.trim();
            if !trimmed.is_empty() {
                return trimmed.to_string();
            }
        }
    }
    "harn".to_string()
}

#[cfg(feature = "otel")]
fn otel_headers_from_env() -> std::collections::HashMap<String, String> {
    let raw = std::env::var("HARN_OTEL_HEADERS")
        .ok()
        .or_else(|| std::env::var("OTEL_EXPORTER_OTLP_HEADERS").ok())
        .unwrap_or_default();
    raw.split([',', '\n', ';'])
        .map(str::trim)
        .filter(|segment| !segment.is_empty())
        .filter_map(|segment| {
            let (name, value) = segment
                .split_once('=')
                .or_else(|| segment.split_once(':'))?;
            let name = name.trim();
            let value = value.trim();
            if name.is_empty() || value.is_empty() {
                return None;
            }
            Some((name.to_string(), value.to_string()))
        })
        .collect()
}

#[cfg(feature = "otel")]
fn normalize_otlp_traces_endpoint(endpoint: &str) -> String {
    let trimmed = endpoint.trim_end_matches('/');
    if trimmed.ends_with("/v1/traces") {
        trimmed.to_string()
    } else {
        format!("{trimmed}/v1/traces")
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
    "harn.kind",
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
fn otel_span_end_attributes(
    metadata: &BTreeMap<String, serde_json::Value>,
) -> Vec<(String, String)> {
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
    if otel_endpoint_from_env().is_none() {
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
            // OTel span attributes are the fourth sink covered by the
            // OA-06 token-redaction policy (transcripts, audit
            // receipts, OTel, and system reminders). The helper also
            // bounds attribute-key cardinality by folding unknown
            // metadata into `harn.meta_json`.
            for (key, redacted) in otel_span_end_attributes(metadata) {
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
                .extend(otel_span_end_attributes(metadata));
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

    #[cfg(feature = "otel")]
    mod otel_env {
        use super::super::*;
        use std::sync::{Mutex, MutexGuard, OnceLock};

        /// Serializes env-mutating tests in this module. Crate-wide
        /// `crate::llm::env_lock()` is reserved for LLM env scopes; a
        /// dedicated lock here keeps these tests independent.
        fn lock() -> MutexGuard<'static, ()> {
            static LOCK: OnceLock<Mutex<()>> = OnceLock::new();
            LOCK.get_or_init(|| Mutex::new(()))
                .lock()
                .expect("otel env lock")
        }

        /// RAII guard for a single env var. Saves the prior value on
        /// construction and restores it on Drop so parallel tests in
        /// the same process don't leak state.
        struct ScopedEnvVar {
            key: &'static str,
            previous: Option<String>,
        }

        impl ScopedEnvVar {
            fn set(key: &'static str, value: &str) -> Self {
                let previous = std::env::var(key).ok();
                // SAFETY: env mutation is serialized by the test-level
                // `lock()` above; no other thread inspects these
                // variables while a guard is alive.
                unsafe { std::env::set_var(key, value) };
                Self { key, previous }
            }

            fn remove(key: &'static str) -> Self {
                let previous = std::env::var(key).ok();
                // SAFETY: see `set` above.
                unsafe { std::env::remove_var(key) };
                Self { key, previous }
            }
        }

        impl Drop for ScopedEnvVar {
            fn drop(&mut self) {
                // SAFETY: see `set` above. Restoration happens while the
                // test still holds the module lock.
                match &self.previous {
                    Some(value) => unsafe { std::env::set_var(self.key, value) },
                    None => unsafe { std::env::remove_var(self.key) },
                }
            }
        }

        #[test]
        fn install_returns_false_when_endpoint_unset() {
            let _guard = lock();
            let _endpoint = ScopedEnvVar::remove("HARN_OTEL_ENDPOINT");
            let _standard = ScopedEnvVar::remove("OTEL_EXPORTER_OTLP_ENDPOINT");

            let installed = install_otel_sink_from_env()
                .expect("install must not error when endpoint is unset");
            assert!(!installed, "expected no sink registration without endpoint");
        }

        #[test]
        fn endpoint_helper_prefers_harn_variable() {
            let _guard = lock();
            let _harn = ScopedEnvVar::set("HARN_OTEL_ENDPOINT", "http://harn.example.test:4318");
            let _standard = ScopedEnvVar::set(
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                "http://generic.example.test:4318",
            );

            assert_eq!(
                otel_endpoint_from_env().as_deref(),
                Some("http://harn.example.test:4318"),
            );
        }

        #[test]
        fn endpoint_helper_falls_back_to_standard_variable() {
            let _guard = lock();
            let _harn = ScopedEnvVar::remove("HARN_OTEL_ENDPOINT");
            let _standard = ScopedEnvVar::set(
                "OTEL_EXPORTER_OTLP_ENDPOINT",
                "http://generic.example.test:4318",
            );

            assert_eq!(
                otel_endpoint_from_env().as_deref(),
                Some("http://generic.example.test:4318"),
            );
        }

        #[test]
        fn endpoint_helper_ignores_whitespace_only_values() {
            let _guard = lock();
            let _harn = ScopedEnvVar::set("HARN_OTEL_ENDPOINT", "   ");
            let _standard = ScopedEnvVar::remove("OTEL_EXPORTER_OTLP_ENDPOINT");

            assert!(otel_endpoint_from_env().is_none());
        }

        #[test]
        fn service_name_helper_layers_defaults() {
            let _guard = lock();
            let _harn = ScopedEnvVar::remove("HARN_OTEL_SERVICE_NAME");
            let _standard = ScopedEnvVar::remove("OTEL_SERVICE_NAME");
            assert_eq!(otel_service_name_from_env(), "harn");

            let _standard = ScopedEnvVar::set("OTEL_SERVICE_NAME", "editor");
            assert_eq!(otel_service_name_from_env(), "editor");

            let _harn = ScopedEnvVar::set("HARN_OTEL_SERVICE_NAME", "burin-tui");
            assert_eq!(otel_service_name_from_env(), "burin-tui");
        }

        #[test]
        fn headers_helper_parses_comma_separated_pairs() {
            let _guard = lock();
            let _harn = ScopedEnvVar::set(
                "HARN_OTEL_HEADERS",
                "x-honeycomb-team=abc123, x-other=val ,blank=",
            );

            let headers = otel_headers_from_env();
            assert_eq!(
                headers.get("x-honeycomb-team").map(String::as_str),
                Some("abc123"),
            );
            assert_eq!(headers.get("x-other").map(String::as_str), Some("val"));
            assert!(
                !headers.contains_key("blank"),
                "empty values must be dropped to match the orchestrator helper",
            );
        }

        #[test]
        fn normalize_endpoint_appends_traces_path_when_missing() {
            assert_eq!(
                normalize_otlp_traces_endpoint("http://localhost:4318"),
                "http://localhost:4318/v1/traces",
            );
            assert_eq!(
                normalize_otlp_traces_endpoint("http://localhost:4318/"),
                "http://localhost:4318/v1/traces",
            );
            assert_eq!(
                normalize_otlp_traces_endpoint("http://localhost:4318/v1/traces"),
                "http://localhost:4318/v1/traces",
            );
            assert_eq!(
                normalize_otlp_traces_endpoint("http://localhost:4318/v1/traces/"),
                "http://localhost:4318/v1/traces",
            );
        }
    }

    #[cfg(not(feature = "otel"))]
    #[test]
    fn install_otel_sink_returns_ok_false_on_non_otel_builds() {
        let installed = install_otel_sink_from_env().expect("non-otel stub never errors");
        assert!(!installed);
    }
}
