#[cfg(feature = "otel")]
use std::collections::HashMap;

#[cfg(feature = "otel")]
use sha2::{Digest, Sha256};
use tracing::level_filters::LevelFilter;
#[cfg(feature = "otel")]
use tracing_subscriber::filter::filter_fn;
use tracing_subscriber::layer::SubscriberExt;
#[cfg(feature = "otel")]
use tracing_subscriber::Layer as _;

use crate::TraceId;

pub const OTEL_PARENT_SPAN_ID_HEADER: &str = "otel_parent_span_id";

static OBSERVABILITY_INIT: std::sync::OnceLock<()> = std::sync::OnceLock::new();

pub struct ObservabilityGuard {
    #[cfg(feature = "otel")]
    tracer_provider: Option<opentelemetry_sdk::trace::SdkTracerProvider>,
}

impl ObservabilityGuard {
    pub fn install_orchestrator_subscriber_from_env() -> Result<Self, String> {
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

                let tracer = provider.tracer("harn.orchestrator");
                let telemetry = tracing_opentelemetry::layer()
                    .with_tracer(tracer)
                    .with_filter(filter_fn(|metadata| {
                        metadata.is_span() && metadata.target().starts_with("harn")
                    }));
                let subscriber = tracing_subscriber::registry()
                    .with(LevelFilter::INFO)
                    .with(fmt_layer())
                    .with(telemetry);
                tracing::subscriber::set_global_default(subscriber).map_err(|error| {
                    format!("failed to install global tracing subscriber: {error}")
                })?;
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

        let subscriber = tracing_subscriber::registry()
            .with(LevelFilter::INFO)
            .with(fmt_layer());
        tracing::subscriber::set_global_default(subscriber)
            .map_err(|error| format!("failed to install global tracing subscriber: {error}"))?;
        let _ = OBSERVABILITY_INIT.set(());
        Ok(Self {
            #[cfg(feature = "otel")]
            tracer_provider: None,
        })
    }

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

fn fmt_layer<S>() -> impl tracing_subscriber::Layer<S> + Send + Sync
where
    S: tracing::Subscriber,
    for<'span> S: tracing_subscriber::registry::LookupSpan<'span>,
{
    tracing_subscriber::fmt::layer()
        .with_target(false)
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stderr()))
        .with_writer(std::io::stderr)
        .compact()
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
