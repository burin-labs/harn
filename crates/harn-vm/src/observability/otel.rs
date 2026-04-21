use std::collections::BTreeMap;
#[cfg(feature = "otel")]
use std::collections::HashMap;
use std::fs::{self, OpenOptions};
use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

#[cfg(feature = "otel")]
use sha2::{Digest, Sha256};
#[cfg(feature = "otel")]
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::fmt::MakeWriter;
use tracing_subscriber::layer::SubscriberExt;
#[cfg(feature = "otel")]
use tracing_subscriber::Layer as _;
use tracing_subscriber::{filter::LevelFilter, EnvFilter};

use crate::TraceId;

pub const OTEL_PARENT_SPAN_ID_HEADER: &str = "otel_parent_span_id";
pub const OTEL_TRACEPARENT_HEADER: &str = "traceparent";
pub const OTEL_TRACESTATE_HEADER: &str = "tracestate";

static OBSERVABILITY_INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LogFormat {
    Text,
    Pretty,
    Json,
}

#[derive(Clone, Debug)]
pub struct OrchestratorObservabilityConfig {
    pub log_format: LogFormat,
    pub state_dir: Option<PathBuf>,
}

pub struct ObservabilityGuard {
    #[cfg(feature = "otel")]
    tracer_provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
}

impl ObservabilityGuard {
    pub fn install_orchestrator_subscriber_from_env() -> Result<Self, String> {
        Self::install_orchestrator_subscriber(OrchestratorObservabilityConfig {
            log_format: LogFormat::Text,
            state_dir: None,
        })
    }

    pub fn install_orchestrator_subscriber(
        config: OrchestratorObservabilityConfig,
    ) -> Result<Self, String> {
        if OBSERVABILITY_INIT.get().is_some() {
            return Ok(Self {
                #[cfg(feature = "otel")]
                tracer_provider: None,
            });
        }

        #[cfg(feature = "otel")]
        {
            if let Some(provider) = build_tracer_provider_from_env()? {
                use opentelemetry::trace::TracerProvider as _;

                let writer = log_writer(&config)?;
                match config.log_format {
                    LogFormat::Json => {
                        let tracer = provider.tracer("harn.orchestrator");
                        let telemetry = tracing_opentelemetry::layer()
                            .with_tracer(tracer)
                            .with_filter(filter_fn(|metadata| {
                                metadata.is_span() && metadata.target().starts_with("harn")
                            }));
                        let subscriber = tracing_subscriber::registry()
                            .with(env_filter())
                            .with(
                                tracing_subscriber::fmt::layer()
                                    .json()
                                    .flatten_event(true)
                                    .with_current_span(true)
                                    .with_writer(writer),
                            )
                            .with(telemetry);
                        tracing::subscriber::set_global_default(subscriber).map_err(|error| {
                            format!("failed to install global tracing subscriber: {error}")
                        })?;
                    }
                    LogFormat::Pretty => {
                        let tracer = provider.tracer("harn.orchestrator");
                        let telemetry = tracing_opentelemetry::layer()
                            .with_tracer(tracer)
                            .with_filter(filter_fn(|metadata| {
                                metadata.is_span() && metadata.target().starts_with("harn")
                            }));
                        let subscriber = tracing_subscriber::registry()
                            .with(env_filter())
                            .with(
                                tracing_subscriber::fmt::layer()
                                    .pretty()
                                    .with_writer(writer),
                            )
                            .with(telemetry);
                        tracing::subscriber::set_global_default(subscriber).map_err(|error| {
                            format!("failed to install global tracing subscriber: {error}")
                        })?;
                    }
                    LogFormat::Text => {
                        let tracer = provider.tracer("harn.orchestrator");
                        let telemetry = tracing_opentelemetry::layer()
                            .with_tracer(tracer)
                            .with_filter(filter_fn(|metadata| {
                                metadata.is_span() && metadata.target().starts_with("harn")
                            }));
                        let subscriber = tracing_subscriber::registry()
                            .with(env_filter())
                            .with(
                                tracing_subscriber::fmt::layer()
                                    .compact()
                                    .with_target(false)
                                    .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
                                    .with_writer(writer),
                            )
                            .with(telemetry);
                        tracing::subscriber::set_global_default(subscriber).map_err(|error| {
                            format!("failed to install global tracing subscriber: {error}")
                        })?;
                    }
                }
                let _ = OBSERVABILITY_INIT.set(());
                return Ok(Self {
                    tracer_provider: Some(provider),
                });
            }
        }

        #[cfg(not(feature = "otel"))]
        if std::env::var("HARN_OTEL_ENDPOINT")
            .ok()
            .filter(|value| !value.trim().is_empty())
            .is_some()
        {
            return Err(
                "HARN_OTEL_ENDPOINT is set, but this build was compiled without the `otel` feature"
                    .to_string(),
            );
        }

        let writer = log_writer(&config)?;
        match config.log_format {
            LogFormat::Json => {
                let subscriber = tracing_subscriber::registry().with(env_filter()).with(
                    tracing_subscriber::fmt::layer()
                        .json()
                        .flatten_event(true)
                        .with_current_span(true)
                        .with_writer(writer),
                );
                tracing::subscriber::set_global_default(subscriber).map_err(|error| {
                    format!("failed to install global tracing subscriber: {error}")
                })?;
            }
            LogFormat::Pretty => {
                let subscriber = tracing_subscriber::registry().with(env_filter()).with(
                    tracing_subscriber::fmt::layer()
                        .pretty()
                        .with_writer(writer),
                );
                tracing::subscriber::set_global_default(subscriber).map_err(|error| {
                    format!("failed to install global tracing subscriber: {error}")
                })?;
            }
            LogFormat::Text => {
                let subscriber = tracing_subscriber::registry().with(env_filter()).with(
                    tracing_subscriber::fmt::layer()
                        .compact()
                        .with_target(false)
                        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
                        .with_writer(writer),
                );
                tracing::subscriber::set_global_default(subscriber).map_err(|error| {
                    format!("failed to install global tracing subscriber: {error}")
                })?;
            }
        }
        let _ = OBSERVABILITY_INIT.set(());
        Ok(Self {
            #[cfg(feature = "otel")]
            tracer_provider: None,
        })
    }

    #[cfg_attr(not(feature = "otel"), allow(unused_mut))]
    pub fn shutdown(mut self) -> Result<(), String> {
        #[cfg(feature = "otel")]
        if let Some(provider) = self.tracer_provider.take() {
            provider
                .force_flush()
                .map_err(|error| format!("failed to flush OTel spans: {error}"))?;
            provider
                .shutdown()
                .map_err(|error| format!("failed to shut down OTel tracer provider: {error}"))?;
        }
        Ok(())
    }
}

fn env_filter() -> EnvFilter {
    EnvFilter::builder()
        .with_default_directive(LevelFilter::INFO.into())
        .from_env_lossy()
}

fn log_writer(config: &OrchestratorObservabilityConfig) -> Result<OrchestratorLogWriter, String> {
    let file = if let Some(state_dir) = config.state_dir.as_ref() {
        let log_dir = state_dir.join("logs");
        fs::create_dir_all(&log_dir).map_err(|error| {
            format!(
                "failed to create orchestrator log dir {}: {error}",
                log_dir.display()
            )
        })?;
        Some(Arc::new(Mutex::new(RotatingFile::open(
            log_dir.join("orchestrator.log"),
        )?)))
    } else {
        None
    };
    Ok(OrchestratorLogWriter {
        format: config.log_format,
        file,
    })
}

#[derive(Clone)]
struct OrchestratorLogWriter {
    format: LogFormat,
    file: Option<Arc<Mutex<RotatingFile>>>,
}

impl<'a> MakeWriter<'a> for OrchestratorLogWriter {
    type Writer = OrchestratorLogLineWriter;

    fn make_writer(&'a self) -> Self::Writer {
        OrchestratorLogLineWriter {
            format: self.format,
            file: self.file.clone(),
        }
    }
}

struct OrchestratorLogLineWriter {
    format: LogFormat,
    file: Option<Arc<Mutex<RotatingFile>>>,
}

impl Write for OrchestratorLogLineWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.format {
            LogFormat::Json => io::stdout().write_all(buf)?,
            LogFormat::Text | LogFormat::Pretty => io::stderr().write_all(buf)?,
        }
        if let Some(file) = self.file.as_ref() {
            file.lock()
                .expect("orchestrator log file poisoned")
                .write_all(buf)?;
        }
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        match self.format {
            LogFormat::Json => io::stdout().flush()?,
            LogFormat::Text | LogFormat::Pretty => io::stderr().flush()?,
        }
        if let Some(file) = self.file.as_ref() {
            file.lock()
                .expect("orchestrator log file poisoned")
                .flush()?;
        }
        Ok(())
    }
}

struct RotatingFile {
    path: PathBuf,
    file: fs::File,
    bytes_written: u64,
}

impl RotatingFile {
    const MAX_BYTES: u64 = 10 * 1024 * 1024;

    fn open(path: PathBuf) -> Result<Self, String> {
        let bytes_written = fs::metadata(&path)
            .map(|metadata| metadata.len())
            .unwrap_or(0);
        let file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
            .map_err(|error| {
                format!(
                    "failed to open orchestrator log {}: {error}",
                    path.display()
                )
            })?;
        Ok(Self {
            path,
            file,
            bytes_written,
        })
    }

    fn rotate_if_needed(&mut self, next_write_bytes: usize) -> io::Result<()> {
        if self.bytes_written + next_write_bytes as u64 <= Self::MAX_BYTES {
            return Ok(());
        }
        self.file.flush()?;
        let rotated = self.path.with_extension("log.1");
        let _ = fs::remove_file(&rotated);
        if self.path.exists() {
            fs::rename(&self.path, rotated)?;
        }
        self.file = OpenOptions::new()
            .create(true)
            .append(true)
            .open(&self.path)?;
        self.bytes_written = 0;
        Ok(())
    }
}

impl Write for RotatingFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.rotate_if_needed(buf.len())?;
        let written = self.file.write(buf)?;
        self.bytes_written += written as u64;
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.file.flush()
    }
}

impl Drop for ObservabilityGuard {
    fn drop(&mut self) {
        // Best-effort flush + shutdown so span batches are delivered even when
        // the caller exits via panic or early return without calling
        // `shutdown()` explicitly. Ignore errors — there's nothing to recover
        // to during teardown.
        #[cfg(feature = "otel")]
        if let Some(provider) = self.tracer_provider.take() {
            let _ = provider.force_flush();
            let _ = provider.shutdown();
        }
    }
}

#[cfg(feature = "otel")]
pub fn set_span_parent(
    span: &tracing::Span,
    trace_id: &TraceId,
    parent_span_id: Option<&str>,
) -> Result<(), String> {
    use opentelemetry::trace::TraceContextExt as _;
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    let context = opentelemetry::Context::current()
        .with_remote_span_context(span_context(trace_id, parent_span_id));
    span.set_parent(context)
        .map_err(|error| format!("failed to attach OTel parent context: {error}"))
}

#[cfg(not(feature = "otel"))]
pub fn set_span_parent(
    _span: &tracing::Span,
    _trace_id: &TraceId,
    _parent_span_id: Option<&str>,
) -> Result<(), String> {
    Ok(())
}

#[cfg(feature = "otel")]
pub fn current_span_id_hex(span: &tracing::Span) -> Option<String> {
    use opentelemetry::trace::TraceContextExt as _;
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    let context = span.context();
    let binding = context.span();
    let span_context = binding.span_context();
    span_context
        .is_valid()
        .then(|| span_context.span_id().to_string())
}

#[cfg(not(feature = "otel"))]
pub fn current_span_id_hex(_span: &tracing::Span) -> Option<String> {
    None
}

#[cfg(feature = "otel")]
pub fn inject_current_context_headers(
    span: &tracing::Span,
    headers: &mut BTreeMap<String, String>,
) -> Result<(), String> {
    use opentelemetry::propagation::{Injector, TextMapPropagator as _};
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    struct HeaderInjector<'a>(&'a mut BTreeMap<String, String>);

    impl Injector for HeaderInjector<'_> {
        fn set(&mut self, key: &str, value: String) {
            self.0.insert(key.to_string(), value);
        }
    }

    let propagator = opentelemetry_sdk::propagation::TraceContextPropagator::new();
    propagator.inject_context(&span.context(), &mut HeaderInjector(headers));
    Ok(())
}

#[cfg(not(feature = "otel"))]
pub fn inject_current_context_headers(
    _span: &tracing::Span,
    _headers: &mut BTreeMap<String, String>,
) -> Result<(), String> {
    Ok(())
}

#[cfg(feature = "otel")]
pub fn set_span_parent_from_headers(
    span: &tracing::Span,
    headers: &BTreeMap<String, String>,
    trace_id: &TraceId,
    fallback_parent_span_id: Option<&str>,
) -> Result<(), String> {
    use opentelemetry::propagation::{Extractor, TextMapPropagator as _};
    use opentelemetry::trace::TraceContextExt as _;
    use tracing_opentelemetry::OpenTelemetrySpanExt as _;

    struct HeaderExtractor<'a>(&'a BTreeMap<String, String>);

    impl Extractor for HeaderExtractor<'_> {
        fn get(&self, key: &str) -> Option<&str> {
            self.0.get(key).map(String::as_str)
        }

        fn keys(&self) -> Vec<&str> {
            self.0.keys().map(String::as_str).collect()
        }
    }

    let propagator = opentelemetry_sdk::propagation::TraceContextPropagator::new();
    let context = propagator.extract(&HeaderExtractor(headers));
    let binding = context.span();
    let span_context = binding.span_context();
    if span_context.is_valid() {
        return span
            .set_parent(context)
            .map_err(|error| format!("failed to attach OTel parent context: {error}"));
    }
    set_span_parent(span, trace_id, fallback_parent_span_id)
}

#[cfg(not(feature = "otel"))]
pub fn set_span_parent_from_headers(
    _span: &tracing::Span,
    _headers: &BTreeMap<String, String>,
    _trace_id: &TraceId,
    _fallback_parent_span_id: Option<&str>,
) -> Result<(), String> {
    Ok(())
}

#[cfg(feature = "otel")]
fn build_tracer_provider_from_env(
) -> Result<Option<opentelemetry_sdk::trace::SdkTracerProvider>, String> {
    use opentelemetry::global;
    use opentelemetry_otlp::{Protocol, WithExportConfig as _, WithHttpConfig as _};
    use opentelemetry_sdk::runtime;
    use opentelemetry_sdk::trace::span_processor_with_async_runtime::BatchSpanProcessor;
    use opentelemetry_sdk::Resource;

    let Some(raw_endpoint) = std::env::var("HARN_OTEL_ENDPOINT")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
    else {
        return Ok(None);
    };

    let endpoint = normalize_otlp_traces_endpoint(&raw_endpoint);
    let service_name = std::env::var("HARN_OTEL_SERVICE_NAME")
        .ok()
        .map(|value| value.trim().to_string())
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| "harn-orchestrator".to_string());
    let headers = parse_headers(&std::env::var("HARN_OTEL_HEADERS").unwrap_or_default());

    let exporter = opentelemetry_otlp::SpanExporter::builder()
        .with_http()
        .with_http_client(
            reqwest::Client::builder()
                .build()
                .map_err(|error| format!("failed to build OTLP HTTP client: {error}"))?,
        )
        .with_protocol(Protocol::HttpJson)
        .with_endpoint(endpoint)
        .with_headers(headers)
        .build()
        .map_err(|error| format!("failed to build OTel span exporter: {error}"))?;

    let batch = BatchSpanProcessor::builder(exporter, runtime::Tokio).build();
    let provider = opentelemetry_sdk::trace::SdkTracerProvider::builder()
        .with_resource(Resource::builder().with_service_name(service_name).build())
        .with_span_processor(batch)
        .build();
    global::set_tracer_provider(provider.clone());
    Ok(Some(provider))
}

#[cfg(feature = "otel")]
fn span_context(
    trace_id: &TraceId,
    parent_span_id: Option<&str>,
) -> opentelemetry::trace::SpanContext {
    use opentelemetry::trace::{SpanContext, SpanId, TraceFlags, TraceState};

    let trace_id = otel_trace_id(trace_id);
    let span_id = parent_span_id
        .and_then(|value| SpanId::from_hex(value).ok())
        .filter(|value| *value != SpanId::INVALID)
        .unwrap_or_else(|| hashed_span_id(trace_id.to_string().as_bytes()));

    SpanContext::new(
        trace_id,
        span_id,
        TraceFlags::SAMPLED,
        true,
        TraceState::default(),
    )
}

#[cfg(feature = "otel")]
fn otel_trace_id(trace_id: &TraceId) -> opentelemetry::trace::TraceId {
    use opentelemetry::trace::TraceId as OtelTraceId;

    let normalized = trace_id
        .0
        .strip_prefix("trace_")
        .unwrap_or(trace_id.0.as_str())
        .replace('-', "");
    if let Ok(trace_id) = OtelTraceId::from_hex(&normalized) {
        if trace_id != OtelTraceId::INVALID {
            return trace_id;
        }
    }
    hashed_trace_id(trace_id.0.as_bytes())
}

#[cfg(feature = "otel")]
fn hashed_trace_id(input: &[u8]) -> opentelemetry::trace::TraceId {
    let digest = Sha256::digest(input);
    let mut bytes = [0_u8; 16];
    bytes.copy_from_slice(&digest[..16]);
    opentelemetry::trace::TraceId::from_bytes(bytes)
}

#[cfg(feature = "otel")]
fn hashed_span_id(input: &[u8]) -> opentelemetry::trace::SpanId {
    let digest = Sha256::digest(input);
    let mut bytes = [0_u8; 8];
    bytes.copy_from_slice(&digest[..8]);
    if bytes.iter().all(|byte| *byte == 0) {
        bytes[7] = 1;
    }
    opentelemetry::trace::SpanId::from_bytes(bytes)
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

#[cfg(feature = "otel")]
fn parse_headers(raw: &str) -> HashMap<String, String> {
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

#[cfg(all(test, feature = "otel"))]
mod tests {
    use super::*;

    #[test]
    fn normalizes_trace_endpoint_suffix() {
        assert_eq!(
            normalize_otlp_traces_endpoint("http://127.0.0.1:4318"),
            "http://127.0.0.1:4318/v1/traces"
        );
        assert_eq!(
            normalize_otlp_traces_endpoint("http://127.0.0.1:4318/v1/traces"),
            "http://127.0.0.1:4318/v1/traces"
        );
    }

    #[test]
    fn parses_header_lists() {
        let headers = parse_headers("authorization=Bearer token,x-tenant-id=tenant-123;trace=true");
        assert_eq!(
            headers.get("authorization"),
            Some(&"Bearer token".to_string())
        );
        assert_eq!(headers.get("x-tenant-id"), Some(&"tenant-123".to_string()));
        assert_eq!(headers.get("trace"), Some(&"true".to_string()));
    }
}
