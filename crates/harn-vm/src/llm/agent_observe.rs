//! LLM call observability: retry logic, transcript dumps, span annotation,
//! and the `observed_llm_call` wrapper extracted from `agent.rs`.
//!
//! # Transcript log shape
//!
//! Writes go to `$HARN_LLM_TRANSCRIPT_DIR/llm_transcript.jsonl`, one JSON
//! object per line, append-only. Consumers replay the events in order to
//! reconstruct the model's context at any iteration.
//!
//! Every event also carries a `type` discriminator, a `timestamp` (Unix
//! `secs.millis`), and a `span_id` (the current tracing span, may be
//! null) — these common fields are omitted from the per-event field lists
//! below.
//!
//! Event types:
//!
//! - `system_prompt` `{content, hash, content_hash}` — deduped prompt;
//!   `content_hash` is stable redacted `blake3:`.
//! - `tool_schemas` `{schemas, hash, content_hash}` — deduped schemas with a
//!   stable redacted `blake3:` content hash.
//! - `message` `{role, content, iteration?}` — single message appended to
//!   the visible conversation. Emitted every time a message lands in the
//!   transcript (user task, nudge, assistant reply, tool result, host
//!   push).
//! - `routing_decision` `{call_id, iteration, policy, requested_quality,
//!   selected_provider, selected_model, fallback_chain, alternatives}` —
//!   emitted once before `provider_call_request` whenever a routing
//!   decision was attached to the call (model/provider selection,
//!   fallback chain, and the considered alternatives).
//! - `provider_call_request` core `{call_id, iteration, model, provider,
//!   max_tokens, temperature, tool_choice, tool_format, context_token_breakdown}` —
//!   slim metadata for a single model call.
//!   No `messages`, `system`, or `tool_schemas` fields; those are reconstructable.
//!   `served_context` carries stable redacted prompt/schema/tool hashes.
//!   Set `HARN_LLM_TRANSCRIPT_VERBOSE=1` to include a `request_snapshot`
//!   object with the exact system prompt, message list, and tool schemas
//!   attached to each request for debugging provider-context issues.
//!   Set `HARN_LLM_TRANSCRIPT_RAW=1` to persist redacted, exact provider
//!   request/response sidecars under `raw-provider/` and emit
//!   `provider_raw_capture` pointer events for extraction-drop debugging.
//! - `provider_call_response` core `{call_id, iteration, model, provider,
//!   text, tool_calls, parsed_tool_calls, input_tokens, output_tokens,
//!   response_ms}`. `tool_calls` is the provider-native tool-call array
//!   (empty for text-format local models); `parsed_tool_calls` is the
//!   merged view (native when present, otherwise the calls parsed out of
//!   the inline tagged `<tool_call>` blocks in `text`) so the record is
//!   self-describing for text-format runs. `raw_tool_calls` is present only
//!   when the provider supplied native object receipts before normalization;
//!   dispatch continues to use `tool_calls`. Also carries diagnostics
//!   `{cost_usd, cache_* (cache_read_tokens, cache_write_tokens,
//!   cache_hit_ratio, cache_savings_usd,
//!   cache_hit), thinking, thinking_summary, provider_telemetry,
//!   structural_experiment}`.
//! - `interpreted_response` `{call_id, iteration, tool_format, prose,
//!   tool_calls, tool_parse_errors}` — post-parse view of the last
//!   assistant turn.
//!
//! To reconstruct the prompt sent at `call_id=X`, replay events in order
//! and track the last `system_prompt`, the last `tool_schemas`, and every
//! `message` up to (but not including) the matching `provider_call_request`.

use std::future::Future;
use std::path::PathBuf;
use std::sync::Arc;

use crate::event_log::EventLog;
use crate::value::{VmError, VmValue};

use super::api::{
    vm_call_llm_full_single_route, vm_call_llm_full_streaming_offthread_single_route,
    vm_call_llm_full_streaming_single_route, DeltaSender,
};
use super::trace::{trace_llm_call, LlmTraceEntry};
use super::{api, cost, first_token, rate_limit, resolved_dispatch, routing, trace};

use super::agent_tools::next_call_id;

mod raw_tool_receipts;
mod served_context_receipts;
mod transcript_ambient;

use transcript_ambient::{current_transcript_dir, system_prompt_changed, tool_schemas_changed};
pub(crate) use transcript_ambient::{
    pop_llm_transcript_dir, push_llm_transcript_dir, swap_llm_transcript_ambient,
    LlmTranscriptAmbient,
};

tokio::task_local! {
    static RAW_PROVIDER_CAPTURE_CONTEXT: RawProviderCaptureContext;
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct RawProviderCaptureContext {
    pub(crate) call_id: String,
    pub(crate) iteration: usize,
    transcript_dir: Option<String>,
}

impl RawProviderCaptureContext {
    fn new(call_id: String, iteration: usize) -> Self {
        Self {
            call_id,
            iteration,
            transcript_dir: current_transcript_dir(),
        }
    }
}

pub(crate) async fn with_raw_provider_capture_context<F>(
    context: RawProviderCaptureContext,
    future: F,
) -> F::Output
where
    F: Future,
{
    RAW_PROVIDER_CAPTURE_CONTEXT.scope(context, future).await
}

pub(crate) fn current_raw_provider_capture_context() -> Option<RawProviderCaptureContext> {
    RAW_PROVIDER_CAPTURE_CONTEXT.try_with(Clone::clone).ok()
}

fn hash_str(value: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn hash_json(value: &serde_json::Value) -> u64 {
    hash_str(&crate::canonical_json::to_string(value))
}

fn env_flag_enabled(name: &str) -> bool {
    std::env::var(name)
        .ok()
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on" | "full")
        })
        .unwrap_or(false)
}

fn verbose_llm_transcript_enabled() -> bool {
    env_flag_enabled("HARN_LLM_TRANSCRIPT_VERBOSE")
}

fn raw_llm_transcript_enabled() -> bool {
    env_flag_enabled("HARN_LLM_TRANSCRIPT_RAW")
}

pub(crate) fn raw_provider_capture_enabled(context: Option<&RawProviderCaptureContext>) -> bool {
    context
        .and_then(|context| context.transcript_dir.as_ref())
        .is_some()
        && raw_llm_transcript_enabled()
}

pub(crate) fn persist_raw_provider_request(
    context: Option<&RawProviderCaptureContext>,
    provider: &str,
    model: &str,
    wire_dialect: &str,
    attempt: Option<usize>,
    body: &serde_json::Value,
) -> Option<String> {
    let context = context?;
    if !raw_provider_capture_enabled(Some(context)) {
        return None;
    }
    let envelope = serde_json::json!({
        "schema_version": "harn.llm.raw_provider_request.v1",
        "kind": "request",
        "captured_at": chrono_now(),
        "call_id": context.call_id,
        "iteration": context.iteration,
        "attempt": attempt,
        "provider": provider,
        "model": model,
        "wire_dialect": wire_dialect,
        "body": body,
    });
    write_raw_provider_sidecar(context, "request", provider, model, attempt, envelope)
}

pub(crate) fn persist_raw_provider_response(
    context: Option<&RawProviderCaptureContext>,
    provider: &str,
    model: &str,
    transport: &str,
    attempt: Option<usize>,
    status: u16,
    content_type: Option<&str>,
    body_text: &str,
) -> Option<String> {
    let context = context?;
    if !raw_provider_capture_enabled(Some(context)) {
        return None;
    }
    let parsed_json = serde_json::from_str::<serde_json::Value>(body_text).ok();
    let envelope = serde_json::json!({
        "schema_version": "harn.llm.raw_provider_response.v1",
        "kind": "response",
        "captured_at": chrono_now(),
        "call_id": context.call_id,
        "iteration": context.iteration,
        "attempt": attempt,
        "provider": provider,
        "model": model,
        "transport": transport,
        "status": status,
        "content_type": content_type,
        "body_text": body_text,
        "body_json": parsed_json,
    });
    write_raw_provider_sidecar(
        context,
        &format!("response-{transport}"),
        provider,
        model,
        attempt,
        envelope,
    )
}

fn write_raw_provider_sidecar(
    context: &RawProviderCaptureContext,
    suffix: &str,
    provider: &str,
    model: &str,
    attempt: Option<usize>,
    mut envelope: serde_json::Value,
) -> Option<String> {
    crate::redact::current_policy().redact_json_in_place(&mut envelope);
    let dir = context.transcript_dir.as_deref()?;
    let raw_dir = PathBuf::from(&dir).join("raw-provider");
    std::fs::create_dir_all(&raw_dir).ok()?;
    let call_id = raw_provider_file_id(&context.call_id);
    let attempt_part = attempt
        .map(|attempt| format!("-attempt-{attempt}"))
        .unwrap_or_default();
    let filename = format!("{call_id}{attempt_part}-{suffix}.json");
    let relative_path = format!("raw-provider/{filename}");
    let path = raw_dir.join(filename);
    let encoded = serde_json::to_vec_pretty(&envelope).ok()?;
    static RAW_PROVIDER_WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    {
        let _guard = RAW_PROVIDER_WRITE_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let mut file = std::fs::OpenOptions::new()
            .create(true)
            .write(true)
            .truncate(true)
            .open(path)
            .ok()?;
        use std::io::Write;
        file.write_all(&encoded).ok()?;
        file.write_all(b"\n").ok()?;
    }
    append_llm_transcript_entry_to_dir(
        &serde_json::json!({
            "type": "provider_raw_capture",
            "timestamp": chrono_now(),
            "span_id": crate::tracing::current_span_id(),
            "call_id": context.call_id,
            "iteration": context.iteration,
            "attempt": attempt,
            "provider": provider,
            "model": model,
            "capture": suffix,
            "path": relative_path,
        }),
        Some(dir),
    );
    Some(relative_path)
}

fn raw_provider_file_id(value: &str) -> String {
    let sanitized: String = value
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || matches!(ch, '-' | '_' | '.') {
                ch
            } else {
                '_'
            }
        })
        .collect();
    if sanitized.is_empty() {
        "call".to_string()
    } else {
        sanitized
    }
}

/// Classify whether a VmError from an LLM call is transient and worth
/// retrying.
///
/// Priority:
/// 1. `CategorizedError` → consult `ErrorCategory::is_transient()` for the
///    authoritative, structured answer.
/// 2. `Thrown(String)` / `Runtime(String)` → first try to *derive* a
///    category via the shared `classify_error_message` machinery (so
///    HTTP-status patterns and well-known provider identifiers stay in
///    one place), then fall back to a small substring list for error
///    shapes that don't carry a status code (network failure phrases).
pub(super) fn is_retryable_llm_error(err: &VmError) -> bool {
    use crate::value::{classify_error_message, ErrorCategory};
    let msg = match err {
        VmError::CategorizedError { category, message } => {
            let llm_info = crate::llm::api::classify_llm_error(category.clone(), message);
            return if llm_info.reason == crate::llm::api::LlmErrorReason::Unknown {
                category.is_transient()
            } else {
                llm_info.kind == crate::llm::api::LlmErrorKind::Transient
            };
        }
        VmError::Thrown(crate::value::VmValue::Dict(d)) => {
            if let Some(kind) = d.get("kind").map(|v| v.display()) {
                return kind == "transient";
            }
            if let Some(category) = d.get("category").map(|v| v.display()) {
                return ErrorCategory::parse(&category).is_transient();
            }
            return false;
        }
        VmError::Thrown(crate::value::VmValue::String(s)) => s.as_ref(),
        VmError::Runtime(s) => s.as_str(),
        _ => return false,
    };
    let category = classify_error_message(msg);
    let llm_info = crate::llm::api::classify_llm_error(category, msg);
    if llm_info.kind == crate::llm::api::LlmErrorKind::Transient {
        return true;
    }
    if llm_info.reason != crate::llm::api::LlmErrorReason::Unknown {
        return false;
    }
    let derived = classify_error_message(msg);
    if derived != ErrorCategory::Generic {
        return derived.is_transient();
    }
    // Fallback for retryable shapes that don't carry a status code.
    let lower = msg.to_lowercase();
    lower.contains("too many requests")
        || lower.contains("rate limit")
        || lower.contains("overloaded")
        || lower.contains("service unavailable")
        || lower.contains("bad gateway")
        || lower.contains("gateway timeout")
        || lower.contains("timed out")
        || lower.contains("timeout")
        || lower.contains("delivered no content")
        || lower.contains("eof")
}

/// Whether an LLM-call failure is a transport-level *network* failure
/// (connection refused/reset, DNS failure, dropped link, request timeout).
/// Feeds the per-route circuit breaker together with
/// [`is_overloaded_llm_error`].
///
/// Deliberately excludes `RateLimit` (429): a 429 means the link is healthy and
/// the provider is throttling us; that is handled by the rate limiter's cooldown
/// and Retry-After, not by the breaker. Generic `ServerError` (500/502) is
/// likewise the provider's fault on a reachable link and must not trip the
/// breaker either; provider *overload* (529/503) is the one server-side class
/// that does, via the separate overload predicate.
pub(super) fn is_network_failure_llm_error(err: &VmError) -> bool {
    let (category, message) = match err {
        VmError::CategorizedError { category, message } => (category.clone(), message.clone()),
        VmError::Thrown(crate::value::VmValue::String(s)) => {
            (crate::value::classify_error_message(s), s.to_string())
        }
        VmError::Runtime(s) => (crate::value::classify_error_message(s), s.clone()),
        _ => return false,
    };
    let reason = crate::llm::api::classify_llm_error(category, &message).reason;
    matches!(
        reason,
        crate::llm::api::LlmErrorReason::NetworkError | crate::llm::api::LlmErrorReason::Timeout
    )
}

/// Whether an LLM-call failure says the provider itself is shedding load
/// (HTTP 529 / 503, Anthropic `overloaded_error`). Distinct from a 429 — the
/// client hasn't exceeded a quota — and from a generic 500/502, which is a
/// single-request server fault. Overload is a provider-wide condition, so it
/// feeds BOTH the per-route breaker (fail fast instead of burning the retry
/// budget) and the shared cooldown (N parallel agents back off together
/// instead of stampeding the overloaded provider).
pub(super) fn is_overloaded_llm_error(err: &VmError) -> bool {
    crate::value::error_to_category(err) == crate::value::ErrorCategory::Overloaded
}

/// L0 detection: classify an LLM-call failure into a rate-governor throttle
/// signal from the STRUCTURED error category (never a raw log string), reusing
/// the same category logic the cooldown/breaker seams above use so the governor
/// agrees with the runtime's own routing. Returns `None` for non-throttle
/// failures (network/auth/context/generic 5xx). A 429 → `RateLimit429`; a
/// provider overload (529/503/`overloaded_error`) → `Overloaded`. The
/// empty-under-load signal is detected separately (on the `Ok` empty path)
/// because it is not a thrown error.
mod detector;
mod provider_errors;
mod transcript_observability;

use detector::*;
use provider_errors::*;
use transcript_observability::*;
pub(crate) use transcript_observability::{append_llm_observability_entry, record_template_render};
/// Extract retry-after delay from an error message if present.
///
/// Supports both forms defined by RFC 7231 §7.1.3:
/// - delta-seconds (integer or fractional)
/// - HTTP-date (IMF-fixdate)
///
/// Returns `None` if no recognizable `retry-after:` header is embedded.
/// HTTP-date values in the past are normalized to 0 ms. Values above
/// `60_000` ms are clamped — callers combine the hint with their own
/// exponential backoff rather than honoring huge provider-requested
/// sleeps verbatim.
pub(super) fn extract_retry_after_ms(err: &VmError) -> Option<u64> {
    let msg = match err {
        VmError::Thrown(crate::value::VmValue::String(s)) => s.as_ref(),
        VmError::Thrown(crate::value::VmValue::Dict(d)) => {
            return d.get("retry_after_ms").and_then(|v| match v {
                crate::value::VmValue::Int(ms) if *ms >= 0 => Some(*ms as u64),
                _ => None,
            });
        }
        VmError::CategorizedError { message, .. } => message.as_str(),
        VmError::Runtime(s) => s.as_str(),
        _ => return None,
    };
    parse_retry_after(msg)
}

/// Parse the value of a `retry-after:` header embedded anywhere in `msg`.
///
/// Exposed for unit tests; the public entry point is
/// `extract_retry_after_ms`.
pub(crate) fn parse_retry_after(msg: &str) -> Option<u64> {
    const MAX_MS: u64 = 60_000;
    let lower = msg.to_lowercase();
    let pos = lower.find("retry-after:")?;
    let after = &msg[pos + "retry-after:".len()..];
    // End at CRLF so we don't grab a neighboring header.
    let end = after.find(['\r', '\n']).unwrap_or(after.len());
    let value = after[..end].trim();
    if value.is_empty() {
        return None;
    }
    let numeric_prefix = value
        .chars()
        .take_while(|ch| ch.is_ascii_digit() || *ch == '.')
        .collect::<String>();
    if !numeric_prefix.is_empty() {
        if let Ok(secs) = numeric_prefix.parse::<f64>() {
            if !secs.is_finite() || secs < 0.0 {
                return Some(0);
            }
            let ms = (secs * 1000.0) as u64;
            return Some(ms.min(MAX_MS));
        }
    }
    if let Ok(target) = httpdate::parse_http_date(value) {
        let now = std::time::SystemTime::now();
        let delta = target
            .duration_since(now)
            .map(|d| d.as_millis() as u64)
            .unwrap_or(0);
        return Some(delta.min(MAX_MS));
    }
    None
}

/// Effective retry budget for zero-token empty completions:
/// [`EMPTY_COMPLETION_BUILTIN_RETRIES`] for provider-shaped routes (including
/// the crate-internal `fake` provider, which exists to simulate them). The
/// user-facing `mock` provider replays scripted turns — a built-in silent
/// retry would consume turns conformance tests rely on — so it gets none.
fn empty_completion_retry_budget(provider: &str) -> usize {
    if crate::llm::providers::MockProvider::should_intercept(provider) {
        0
    } else {
        EMPTY_COMPLETION_BUILTIN_RETRIES
    }
}

fn llm_retry_backoff_ms(error: &VmError, attempt: usize, provider: &str) -> u64 {
    if crate::llm::providers::MockProvider::should_intercept(provider) {
        return 0;
    }
    // Honor an explicit provider Retry-After, but still add a small jitter on
    // top so concurrent same-key callers (eval --concurrency K plus a coexisting
    // session) that all received the same Retry-After do not resume in lockstep
    // and re-stampede the provider the instant the window opens.
    match extract_retry_after_ms(error) {
        Some(retry_after_ms) => retry_after_ms.saturating_add(retry_after_jitter_ms()),
        None => base_retry_backoff_ms(attempt),
    }
}

/// Equal-jitter exponential backoff base shared by the error-retry and
/// empty-completion retry paths (no `retry-after` hint available on the latter).
///
/// AWS "equal jitter": `wait = ceil/2 + rand(0, ceil/2)`, where
/// `ceil = backoff_ms * 2^min(attempt, 4)`. Keeping the lower half fixed avoids
/// the near-zero waits that pure "full jitter" can produce, while randomizing
/// the upper half desynchronizes retries across concurrent same-key processes
/// (avoids the thundering herd that the old zero-jitter `ceil` produced).
fn base_retry_backoff_ms(attempt: usize) -> u64 {
    let ceil = DEFAULT_LLM_CALL_BACKOFF_MS.saturating_mul(1 << attempt.min(4));
    equal_jitter_ms(ceil, &mut rand::rng())
}

/// Pure equal-jitter computation, seamed on an injectable RNG so tests can
/// assert the `[ceil/2, ceil]` bounds and desynchronization deterministically.
fn equal_jitter_ms<R: rand::RngExt>(ceil: u64, rng: &mut R) -> u64 {
    let half = ceil / 2;
    if half == 0 {
        return ceil;
    }
    // `ceil/2` fixed lower half + `rand(0, ceil/2)` upper half → `[ceil/2, ceil]`.
    half + rand_range_inclusive(half, rng)
}

/// Small additive jitter (0..=backoff base) layered on top of a provider
/// Retry-After so identical Retry-After values do not resume in lockstep.
fn retry_after_jitter_ms() -> u64 {
    rand_range_inclusive(DEFAULT_LLM_CALL_BACKOFF_MS, &mut rand::rng())
}

/// `rand(0, max)` inclusive, seamed on an injectable RNG. Centralizes the
/// half-open-to-inclusive conversion (`random_range` is exclusive of its end).
fn rand_range_inclusive<R: rand::RngExt>(max: u64, rng: &mut R) -> u64 {
    rng.random_range(0..max.saturating_add(1))
}

/// Rewrite a native-tool-format request onto the text channel for a retry,
/// without rebuilding the whole request from scratch. Mirrors the established
/// "text-channel request" shape (see the Ollama raw-generate test in `api.rs`):
/// drop the provider-native tool payload, force `Text` output, and clear the
/// structured-output mirrors so the transport serves a plain chat completion
/// the model answers in content. The agent loop's text-tool parser then reads
/// the calls back out of the assistant text.
///
/// This is the wire-level half of the runtime tool_format fallback. It does NOT
/// re-render the system prompt's tool exemplar (that lives in the pipeline), so
/// the goal is strictly to stop a native-channel failure from hard-failing or
/// parse-looping the call — letting the model produce *parseable* output on a
/// working channel — not to guarantee identical guidance to a text-pinned run.
fn degrade_options_to_text_channel(
    opts: &super::api::LlmCallOptions,
) -> super::api::LlmCallOptions {
    let mut degraded = opts.clone();
    degraded.native_tools = None;
    degraded.output_format = super::api::OutputFormat::Text;
    degraded.response_format = None;
    degraded.json_schema = None;
    degraded
}

fn degrade_options_to_non_streaming_transport(
    opts: &super::api::LlmCallOptions,
) -> super::api::LlmCallOptions {
    let mut degraded = opts.clone();
    degraded.stream = false;
    degraded
}

// ---------------------------------------------------------------------------
// observed_llm_call — shared single-LLM-call wrapper with full observability
// ---------------------------------------------------------------------------

/// Make one LLM call with full observability: call-id generation, bridge
/// notifications (call_start / call_progress / call_end), span annotation,
/// retry with exponential backoff, and tracing.
pub(crate) async fn observed_llm_call(
    opts: &super::api::LlmCallOptions,
    tool_format: Option<&str>,
    bridge: Option<&Arc<crate::bridge::HostBridge>>,
    iteration: Option<usize>,
    user_visible: bool,
    offthread: bool,
    streaming_detector: Option<StreamingDetectorContext>,
    delta_sink: Option<DeltaSender>,
) -> Result<super::api::LlmResult, VmError> {
    let _in_flight_guard = super::call::InFlightLlmCallGuard::enter(opts);
    let mut effective_tool_format = tool_format
        .map(str::to_string)
        .or_else(|| {
            std::env::var("HARN_AGENT_TOOL_FORMAT")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| crate::llm_config::default_tool_format(&opts.model, &opts.provider));
    // Working request. Starts as the caller's `opts` (zero-copy) and is only
    // cloned when the runtime tool_format fallback degrades a native-channel
    // request to text mid-retry (see the `Err` arm below). Once degraded, the
    // degraded copy persists for every remaining attempt this call makes.
    let mut working: std::borrow::Cow<'_, super::api::LlmCallOptions> =
        std::borrow::Cow::Borrowed(opts);
    let mut degraded_to_text = false;
    let mut degraded_stream_transport = false;
    let mut attempt = 0usize;
    // How many empty-completion flakes this call retried through. Distinct from
    // `attempt` (which also counts transient-error and tool-format-degrade
    // retries) so the `resolved_dispatch` record can tell a clean serve from a
    // serve that RECOVERED from an empty flake — the exact recovered-vs-terminal
    // distinction the escalation guard hinges on.
    let mut empty_completion_retries = 0usize;
    loop {
        let opts: &super::api::LlmCallOptions = working.as_ref();
        // Network-only circuit breaker: if this route has seen sustained
        // NetworkError/Timeout failures, fail fast instead of burning the retry
        // budget against a dead link (laptop disconnect / DNS failure). 429s do
        // NOT trip this — they are handled by the rate-limiter cooldown below.
        super::rate_limit::check_network_breaker_for_llm_call(opts)?;

        let rate_limit_permit = super::rate_limit::acquire_permit_for_llm_call(opts).await?;

        // Rate governor (Layer 1, behind the `llm.rate_governor` flag; a byte-
        // identical no-op when off). Retries WAIT behind the governor instead of
        // blind-firing: an AIMD-shrunk concurrency limit, a full token bucket, or
        // an OPEN circuit all resolve to a bounded back-off here rather than
        // another hammer at a throttled provider. `gate` reserves an in-flight
        // slot on `Proceed`; the outcome below releases it exactly once.
        let governor_org_key = crate::llm::rate_governor::org_key_id(&opts.api_key);
        let governor_est_tokens = governor_estimated_tokens(opts);
        let governor_reserved =
            await_governor_admission(&opts.provider, &governor_org_key, governor_est_tokens).await;
        let provider_was_throttled_during_call =
            crate::llm::rate_governor::provider_already_throttled(
                &opts.provider,
                &governor_org_key,
            );

        let call_id = next_call_id();
        let prompt_chars: usize = opts
            .messages
            .iter()
            .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
            .map(|s| s.len())
            .sum();

        let mut span_meta = vec![
            ("call_id", serde_json::json!(call_id.clone())),
            ("model", serde_json::json!(opts.model.clone())),
            ("provider", serde_json::json!(opts.provider.clone())),
            ("prompt_chars", serde_json::json!(prompt_chars)),
            (
                "route_policy",
                serde_json::json!(opts.route_policy.as_label()),
            ),
            (
                "fallback_chain",
                serde_json::json!(opts.fallback_chain.clone()),
            ),
        ];
        if let Some(decision) = opts.routing_decision.as_ref() {
            span_meta.push(("routing_decision", serde_json::json!(decision)));
        }
        if let Some(iter) = iteration {
            span_meta.push(("iteration", serde_json::json!(iter)));
            span_meta.push(("llm_attempt", serde_json::json!(attempt)));
        }
        annotate_current_span(&span_meta);

        let mut call_start_meta =
            serde_json::json!({"model": opts.model, "prompt_chars": prompt_chars});
        call_start_meta["stream_publicly"] =
            serde_json::json!(opts.response_format.as_deref() != Some("json"));
        call_start_meta["user_visible"] = serde_json::json!(user_visible);
        if let Some(iter) = iteration {
            call_start_meta["iteration"] = serde_json::json!(iter);
            call_start_meta["llm_attempt"] = serde_json::json!(attempt);
        }
        if let Some(b) = bridge {
            b.send_call_start(&call_id, "llm", "llm_call", call_start_meta);
        }

        dump_llm_request(
            iteration.unwrap_or(0),
            &call_id,
            &effective_tool_format,
            opts,
        );

        let first_token = super::first_token::FirstTokenTimer::for_current_span();
        let start = std::time::Instant::now();
        // The streaming detector runs once per LLM call. Move the
        // context into whichever forwarder we end up spawning so the
        // detector finalizes when the stream closes (or never spawns
        // if this call is non-streamed and there's nothing to listen
        // to).
        let detector_ctx = streaming_detector
            .as_ref()
            .map(|c| StreamingDetectorContext {
                session_id: c.session_id.clone(),
                known_tools: c.known_tools.clone(),
            });
        let raw_capture_context =
            RawProviderCaptureContext::new(call_id.clone(), iteration.unwrap_or(0));
        let llm_result = with_raw_provider_capture_context(raw_capture_context, async {
            if let Some(b) = bridge {
                let delta_tx = spawn_progress_forwarder(
                    b,
                    call_id.clone(),
                    user_visible,
                    detector_ctx,
                    first_token,
                );
                let delta_tx = match delta_sink.clone() {
                    Some(sink) => tee_delta_sender(vec![delta_tx, sink]),
                    None => delta_tx,
                };
                if offthread {
                    vm_call_llm_full_streaming_offthread_single_route(opts, delta_tx).await
                } else {
                    vm_call_llm_full_streaming_single_route(opts, delta_tx).await
                }
            } else if offthread {
                let delta_tx = match detector_ctx {
                    Some(ctx) => {
                        let detector_tx = spawn_detector_only_forwarder(ctx, first_token);
                        match delta_sink.clone() {
                            Some(sink) => tee_delta_sender(vec![detector_tx, sink]),
                            None => detector_tx,
                        }
                    }
                    None if let Some(sink) = delta_sink.clone() => sink,
                    None => {
                        let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<String>();
                        tx
                    }
                };
                vm_call_llm_full_streaming_offthread_single_route(opts, delta_tx).await
            } else if let Some(sink) = delta_sink.clone() {
                let delta_tx = match detector_ctx {
                    Some(ctx) => tee_delta_sender(vec![
                        spawn_detector_only_forwarder(ctx, first_token),
                        sink,
                    ]),
                    None => sink,
                };
                vm_call_llm_full_streaming_single_route(opts, delta_tx).await
            } else if let Some(ctx) = detector_ctx {
                let delta_tx = spawn_detector_only_forwarder(ctx, first_token);
                vm_call_llm_full_streaming_single_route(opts, delta_tx).await
            } else {
                vm_call_llm_full_single_route(opts).await
            }
        })
        .await;
        drop(rate_limit_permit);
        let duration_ms = start.elapsed().as_millis() as u64;
        let attempt_count = attempt + 1;

        // Release the governor slot reserved above and drive AIMD + the circuit
        // (no-op when the flag is off). Runs once per gated attempt, BEFORE the
        // arms below branch into retry/return/error, so the slot accounting can
        // never leak. Detection (L0) also emits a `provider_throttle` record on
        // a throttle signal. `empty_under_load` fires only when this provider is
        // already throttled, so a lone empty completion is not misclassified.
        record_governor_call_outcome(
            &opts.provider,
            &governor_org_key,
            governor_reserved,
            &llm_result,
        );

        match llm_result {
            Ok(result) => {
                // An unproductive "success" the loop has no action to run on —
                // either a zero-token empty completion (provider stall) or an
                // errored-but-actionless turn (`stop_reason == "error"` that
                // narrated an intended tool call but emitted none). Both are
                // provider hiccups, not answers: retry within the
                // empty-completion budget rather than advancing the loop on a
                // broken turn (which would only reply with a generic
                // no-progress nag). Once the budget is exhausted, surface a
                // failover-eligible `provider_exhausted` error so routing can
                // move to the next route. Actionless non-empty errors retain
                // their throttle gate because they can be model behavior rather
                // than a dead serving route.
                if is_retryable_unproductive_completion(&result)
                    && attempt < empty_completion_retry_budget(&opts.provider)
                {
                    let errored_actionless = is_errored_actionless_completion(&result);
                    annotate_current_span(&[
                        ("status", serde_json::json!("retrying")),
                        ("retry_reason", serde_json::json!("empty_completion")),
                        ("attempt", serde_json::json!(attempt)),
                    ]);
                    let detail = if errored_actionless {
                        format!(
                            "provider {} model {} ended with a provider error (stop_reason=error) and emitted no tool call (the intended action went only to the reasoning channel)",
                            opts.provider, opts.model
                        )
                    } else {
                        format!(
                            "provider {} model {} returned a zero-token empty completion (no content, thinking, or tool calls)",
                            opts.provider, opts.model
                        )
                    };
                    let retry_reason = if errored_actionless {
                        UnproductiveCompletionReason::UnproductiveCompletion
                    } else {
                        UnproductiveCompletionReason::EmptyGeneration
                    };
                    emit_empty_completion_retry(
                        iteration.unwrap_or(0),
                        attempt + 1,
                        opts,
                        retry_reason,
                        duration_ms,
                        &detail,
                    );
                    if let Some(b) = bridge {
                        b.send_call_end(
                            &call_id,
                            "llm",
                            "llm_call",
                            duration_ms,
                            "retrying",
                            serde_json::json!({
                                "error": detail,
                                "retryable": true,
                                "attempt": attempt,
                                "user_visible": user_visible,
                            }),
                        );
                    }
                    attempt += 1;
                    empty_completion_retries += 1;
                    let backoff =
                        if crate::llm::providers::MockProvider::should_intercept(&opts.provider) {
                            0
                        } else {
                            base_retry_backoff_ms(attempt)
                        };
                    crate::events::log_warn(
                        "llm",
                        &format!("{detail}; retrying in {backoff}ms (attempt {attempt})"),
                    );
                    if backoff > 0 {
                        crate::clock_mock::sleep(std::time::Duration::from_millis(backoff)).await;
                    }
                    continue;
                }
                let provider_under_throttle = provider_was_throttled_during_call
                    || crate::llm::rate_governor::provider_already_throttled(
                        &opts.provider,
                        &governor_org_key,
                    );
                if let Some(error) = terminal_unproductive_completion_failure(
                    opts,
                    &result,
                    provider_under_throttle,
                    attempt_count,
                    duration_ms,
                ) {
                    let category = crate::value::error_to_category(&error);
                    let message = error.to_string();
                    let classified = super::api::classify_llm_error(category.clone(), &message);
                    let status = "retries_exhausted";
                    annotate_current_span(&[
                        ("status", serde_json::json!(status)),
                        ("error", serde_json::json!(message.as_str())),
                        ("retryable", serde_json::json!(false)),
                        ("failover_eligible", serde_json::json!(true)),
                        ("attempt", serde_json::json!(attempt)),
                    ]);
                    dump_llm_response(
                        iteration.unwrap_or(0),
                        &call_id,
                        &result,
                        duration_ms,
                        opts.applied_structural_experiment.as_ref(),
                        opts.tools.as_ref(),
                    );
                    append_provider_call_error_observability(ProviderCallErrorObservation {
                        iteration: iteration.unwrap_or(0),
                        call_id: &call_id,
                        attempt: attempt_count,
                        status,
                        opts,
                        category: &category,
                        classified: &classified,
                        message: &message,
                        retryable: false,
                        failover_eligible: true,
                        attempt_count: Some(attempt_count),
                    });
                    dump_resolved_dispatch(
                        iteration.unwrap_or(0),
                        &call_id,
                        opts,
                        &effective_tool_format,
                        &super::resolved_dispatch::DispatchOutcome::from_error_message(&message),
                    );
                    if let Some(b) = bridge {
                        b.send_call_end(
                            &call_id,
                            "llm",
                            "llm_call",
                            duration_ms,
                            status,
                            serde_json::json!({
                                "error": message,
                                "retryable": false,
                                "failover_eligible": true,
                                "attempt": attempt,
                                "user_visible": user_visible,
                            }),
                        );
                    }
                    if let Some(metrics) = crate::active_metrics_registry() {
                        metrics.record_llm_call(
                            &result.provider,
                            &result.model,
                            status,
                            result.priced_cost_usd().unwrap_or(0.0),
                        );
                    }
                    return Err(error);
                }
                let usage = crate::tracing::LlmCallUsage {
                    model: result.model.clone(),
                    provider: result.provider.clone(),
                    input_tokens: result.input_tokens,
                    output_tokens: result.output_tokens,
                    cache_read_tokens: result.cache_read_tokens,
                    cache_write_tokens: result.cache_write_tokens,
                    cost_usd: result.priced_cost_usd(),
                };
                annotate_current_span(&[("status", serde_json::json!("ok"))]);
                annotate_current_span(&usage.metadata_pairs());
                dump_llm_response(
                    iteration.unwrap_or(0),
                    &call_id,
                    &result,
                    duration_ms,
                    opts.applied_structural_experiment.as_ref(),
                    opts.tools.as_ref(),
                );
                dump_resolved_dispatch(
                    iteration.unwrap_or(0),
                    &call_id,
                    opts,
                    &effective_tool_format,
                    &super::resolved_dispatch::DispatchOutcome::from_result(
                        &result,
                        empty_completion_retries,
                    ),
                );
                annotate_current_span(&[(
                    "structural_experiment",
                    opts.applied_structural_experiment
                        .as_ref()
                        .map(serde_json::to_value)
                        .transpose()
                        .unwrap_or(None)
                        .unwrap_or(serde_json::Value::Null),
                )]);
                if let Some(b) = bridge {
                    b.send_call_end(
                        &call_id,
                        "llm",
                        "llm_call",
                        duration_ms,
                        "ok",
                        serde_json::json!({
                            "model": result.model,
                            "input_tokens": result.input_tokens,
                            "output_tokens": result.output_tokens,
                            "user_visible": user_visible,
                            "structural_experiment": opts.applied_structural_experiment.as_ref(),
                        }),
                    );
                }
                trace_llm_call(LlmTraceEntry {
                    model: result.model.clone(),
                    input_tokens: result.input_tokens,
                    output_tokens: result.output_tokens,
                    duration_ms,
                });
                if let Some(metrics) = crate::active_metrics_registry() {
                    metrics.record_llm_call(
                        &result.provider,
                        &result.model,
                        "succeeded",
                        result.priced_cost_usd().unwrap_or(0.0),
                    );
                    if result.cache_read_tokens > 0 {
                        metrics.record_llm_cache_hit(&result.provider);
                    }
                }
                super::trace::emit_agent_event(super::trace::AgentTraceEvent::LlmCall {
                    call_id: call_id.clone(),
                    model: result.model.clone(),
                    input_tokens: result.input_tokens,
                    output_tokens: result.output_tokens,
                    cache_tokens: result.cache_read_tokens,
                    duration_ms,
                    iteration: iteration.unwrap_or(0),
                });
                // A terminal unproductive completion (a zero-token empty or a
                // billed-noncommittal turn that survived the built-in
                // empty-completion retry budget) is served-but-useless. It must
                // NOT close the breaker as if the route answered — that reset is
                // exactly what let the same throttled/empty lane be re-dispatched
                // every turn, storming 18-43x per trial. Feed the always-on
                // unproductive-completion streak instead so a route that keeps
                // empty-completing trips `circuit_open` fast (governor-independent,
                // and works for a single-provider model harn#4023's failover
                // cannot rescue). A genuinely answering turn closes the breaker.
                if is_retryable_unproductive_completion(&result)
                    && !crate::llm::providers::is_internal_simulator(&opts.provider)
                {
                    let reason = if is_empty_unproductive_completion(&result) {
                        UnproductiveCompletionReason::EmptyGeneration
                    } else {
                        UnproductiveCompletionReason::UnproductiveCompletion
                    };
                    super::rate_limit::observe_unproductive_completion_for_llm_call(
                        opts,
                        reason.as_str(),
                    );
                } else {
                    super::rate_limit::observe_network_outcome_for_llm_call(opts, false);
                }
                return Ok(result);
            }
            Err(error) => {
                let category = crate::value::error_to_category(&error);
                let message = error.to_string();
                let classified = super::api::classify_llm_error(category.clone(), &message);
                // Shared cooldown: 429 Retry-After, plus 529/503 overload
                // (with a default window when the provider sent no header) so
                // sibling agents on the same route back off together.
                super::rate_limit::observe_retry_after_for_llm_call(
                    opts,
                    shared_cooldown_ms_for_llm_error(&error),
                );
                // A *thrown* empty completion is neither a serve nor a network
                // failure: it must NOT reset the breaker as a success (that
                // reset is what let a dead empty-completing lane be re-dispatched
                // every turn). A terminal one feeds the unproductive-completion
                // streak below; a still-retryable one touches the breaker not at
                // all. Everything else feeds transport-level network failures
                // (connection/DNS/timeout) AND provider overload (529/503) —
                // never 429 (rate limit) or generic 5xx (single-request server
                // fault on a reachable, healthy link).
                let empty_completion_reason = empty_completion_retry_reason(&error);
                let empty_completion_error = empty_completion_reason.is_some();
                if !empty_completion_error {
                    super::rate_limit::observe_network_outcome_for_llm_call(
                        opts,
                        is_network_failure_llm_error(&error) || is_overloaded_llm_error(&error),
                    );
                }
                let retryable = is_retryable_llm_error(&error);
                // A *thrown* unproductive completion (zero-token empty, or the
                // billed-noncommittal contract violation whose tool call went
                // only to the reasoning channel) is retried within the same
                // bounded empty-completion budget the `Ok` arm uses — which
                // floors at 1 for real providers even when the caller's
                // transient-retry budget is 0. This unifies the thrown shape
                // onto the existing empty-completion retry path rather than
                // hard-breaking the loop as a silent `provider_error`; it does
                // NOT retry ordinary transient errors (those stay fail-fast;
                // compose `with_retry` for policy). Once the budget is
                // exhausted the loud thrown error (which names the
                // `upstream contract violation`) is surfaced unchanged, so the
                // eval layer can still classify it as infra, not capability.
                let empty_completion_retry = empty_completion_error
                    && attempt < empty_completion_retry_budget(&opts.provider);
                // Runtime tool_format fallback: a native-channel request whose
                // failure fingerprint says the provider's native tool-call
                // channel itself is broken for this route cannot be rescued by
                // retrying native — every retry re-feeds the same broken channel.
                // Degrade ONCE to the text channel instead and retry there, so the
                // call yields parseable output rather than hard-failing or
                // parse-looping. Two broken-channel signatures qualify, both keyed
                // on the failure SIGNATURE (never a model name) and both only when
                // the request actually carried provider-native tools:
                //
                // 1. **Server-side parser choke** (5xx/EOF + tool-parser
                //    fingerprint): the documented Ollama 500 / EOF leak, or any
                //    serving stack that 500s/EOFs on the native assumption.
                //    Detected by [`is_native_tool_channel_failure`] (the #3500
                //    mechanism).
                // 2. **Billed-noncommittal vanishing call** (the canonical
                //    cheap-model signature): the upstream finished cleanly, billed
                //    output tokens, and emitted ZERO `tool_calls` — it serialized
                //    the action only onto a private reasoning channel or returned
                //    an empty committed message. Detected deterministically one
                //    layer down by [`super::api::is_billed_noncommittal_completion`]
                //    and thrown as `billed_noncommittal_completion_error`, matched
                //    here by [`is_billed_noncommittal_throw`]. Before this, that
                //    throw routed onto the bounded SAME-CHANNEL empty-completion
                //    retry, which just re-fed the broken native channel until the
                //    budget drained, then surfaced — never degrading. A native
                //    channel that vanishes once vanishes again; the right move is
                //    the same degrade-to-text as case 1, so the model can produce
                //    a parseable call on the text channel that the gate already
                //    guarantees this route can carry.
                let native_tool_channel_degrade = !degraded_to_text
                    && crate::llm_config::tool_format_channel(&effective_tool_format)
                        == Some(crate::llm_config::ToolFormatChannel::Native)
                    && opts.native_tools.is_some()
                    && (is_native_tool_channel_failure(&error)
                        || is_billed_noncommittal_throw(&error));
                let stream_transport_degrade = !degraded_stream_transport
                    && !native_tool_channel_degrade
                    && is_stream_transport_failure(&error)
                    && can_degrade_stream_transport(opts);
                // Transient errors are fail-fast here: retry policy is composed
                // in Harn via `with_retry` / routing policies, never a hidden
                // in-call budget. Only the bounded provider-hiccup recoveries
                // (empty completion, one-shot channel/transport degrades) loop.
                let can_retry = empty_completion_retry
                    || native_tool_channel_degrade
                    || stream_transport_degrade;
                let status = if can_retry {
                    "retrying"
                } else if empty_completion_error {
                    "retries_exhausted"
                } else {
                    "error"
                };
                let event_retryable = can_retry || (status == "error" && retryable);
                annotate_current_span(&[
                    ("status", serde_json::json!(status)),
                    ("error", serde_json::json!(message.as_str())),
                    ("retryable", serde_json::json!(event_retryable)),
                    ("attempt", serde_json::json!(attempt)),
                ]);
                append_provider_call_error_observability(ProviderCallErrorObservation {
                    iteration: iteration.unwrap_or(0),
                    call_id: &call_id,
                    attempt: attempt_count,
                    status,
                    opts,
                    category: &category,
                    classified: &classified,
                    message: &message,
                    retryable: event_retryable,
                    failover_eligible: false,
                    attempt_count: None,
                });
                if let Some(b) = bridge {
                    b.send_call_end(
                        &call_id,
                        "llm",
                        "llm_call",
                        duration_ms,
                        status,
                        serde_json::json!({
                            "error": error.to_string(),
                            "retryable": event_retryable,
                            "attempt": attempt,
                            "user_visible": user_visible,
                        }),
                    );
                }
                if !can_retry {
                    let surfaced_error = if empty_completion_error
                        && !crate::llm::providers::is_internal_simulator(&opts.provider)
                    {
                        Some(provider_exhausted_error(
                            opts,
                            empty_completion_reason.expect("empty reason accompanies empty error"),
                            attempt_count,
                            Some(duration_ms),
                            message.clone(),
                        ))
                    } else {
                        None
                    };
                    if empty_completion_error
                        && !crate::llm::providers::is_internal_simulator(&opts.provider)
                    {
                        // A thrown empty completion that exhausted its retry
                        // budget is terminal-unproductive: feed the always-on
                        // unproductive-completion streak so a route that keeps
                        // throwing empties trips `circuit_open` fast instead of
                        // being re-escalated into every turn (the storm harn#4023
                        // cannot stop for a single-provider model). Mirrors the
                        // `Ok`-arm terminal-empty feed so both empty shapes bound
                        // identically.
                        super::rate_limit::observe_unproductive_completion_for_llm_call(
                            opts,
                            empty_completion_reason
                                .expect("terminal empty has a classified reason")
                                .as_str(),
                        );
                    }
                    if let Some(metrics) = crate::active_metrics_registry() {
                        metrics.record_llm_call(&opts.provider, &opts.model, status, 0.0);
                    }
                    // Terminal failure: emit the self-contained dispatch record
                    // with the error-derived outcome so a consumer sees the
                    // resolved route AND why it failed without joining events or
                    // re-parsing the error string. Retryable attempts do NOT
                    // emit (the retry/recovery is the story there); only the
                    // surfaced terminal error does.
                    dump_resolved_dispatch(
                        iteration.unwrap_or(0),
                        &call_id,
                        opts,
                        &effective_tool_format,
                        &super::resolved_dispatch::DispatchOutcome::from_error_message(&message),
                    );
                    return Err(surfaced_error.unwrap_or(error));
                }
                if empty_completion_error {
                    // This thrown empty completion is being retried (we passed
                    // the `!can_retry` gate). Count it so a subsequent serve is
                    // recorded as transient-recovered, not a clean serve.
                    empty_completion_retries += 1;
                    emit_empty_completion_retry(
                        iteration.unwrap_or(0),
                        attempt + 1,
                        opts,
                        empty_completion_reason.expect("empty retry has a classified reason"),
                        duration_ms,
                        &error.to_string(),
                    );
                }
                // Apply the runtime tool_format degrade for the next attempt:
                // swap the working request to its text-channel form, flip the
                // effective format reported to telemetry, and record why. The
                // clone severs the shared borrow of `working` so it can be
                // reassigned; `opts` is not used again before the loop restarts.
                let degraded_options =
                    native_tool_channel_degrade.then(|| degrade_options_to_text_channel(opts));
                let stream_degraded_options = stream_transport_degrade
                    .then(|| degrade_options_to_non_streaming_transport(opts));
                attempt += 1;
                let backoff = llm_retry_backoff_ms(&error, attempt, &opts.provider);
                crate::events::log_warn(
                    "llm",
                    &format!(
                        "LLM call failed ({error}), retrying in {backoff}ms (attempt {attempt})"
                    ),
                );
                if let Some(degraded) = degraded_options {
                    let detail = format!(
                        "provider {} model {} native tool channel failed (server-side tool-call \
                         parser 500/EOF: {error}); degrading tool_format native -> json and \
                         retrying on the text channel",
                        degraded.provider, degraded.model
                    );
                    crate::events::log_warn("llm", &detail);
                    append_llm_observability_entry(
                        "tool_format_degrade",
                        serde_json::Map::from_iter([
                            (
                                "iteration".to_string(),
                                serde_json::json!(iteration.unwrap_or(0)),
                            ),
                            ("attempt".to_string(), serde_json::json!(attempt)),
                            ("provider".to_string(), serde_json::json!(degraded.provider)),
                            ("model".to_string(), serde_json::json!(degraded.model)),
                            ("from".to_string(), serde_json::json!("native")),
                            ("to".to_string(), serde_json::json!("json")),
                            ("error".to_string(), serde_json::json!(error.to_string())),
                        ]),
                    );
                    annotate_current_span(&[
                        ("tool_format_degrade", serde_json::json!(true)),
                        ("tool_format_degrade_from", serde_json::json!("native")),
                        ("tool_format_degrade_to", serde_json::json!("json")),
                    ]);
                    effective_tool_format = "json".to_string();
                    degraded_to_text = true;
                    working = std::borrow::Cow::Owned(degraded);
                }
                if let Some(degraded) = stream_degraded_options {
                    let detail = format!(
                        "provider {} model {} streaming transport failed ({error}); degrading \
                         stream=true -> false and retrying through request/response transport",
                        degraded.provider, degraded.model
                    );
                    crate::events::log_warn("llm", &detail);
                    append_llm_observability_entry(
                        "stream_transport_degrade",
                        serde_json::Map::from_iter([
                            (
                                "iteration".to_string(),
                                serde_json::json!(iteration.unwrap_or(0)),
                            ),
                            ("attempt".to_string(), serde_json::json!(attempt)),
                            ("provider".to_string(), serde_json::json!(degraded.provider)),
                            ("model".to_string(), serde_json::json!(degraded.model)),
                            ("from".to_string(), serde_json::json!(true)),
                            ("to".to_string(), serde_json::json!(false)),
                            ("error".to_string(), serde_json::json!(error.to_string())),
                        ]),
                    );
                    annotate_current_span(&[
                        ("stream_transport_degrade", serde_json::json!(true)),
                        ("stream_transport_degrade_from", serde_json::json!(true)),
                        ("stream_transport_degrade_to", serde_json::json!(false)),
                    ]);
                    degraded_stream_transport = true;
                    working = std::borrow::Cow::Owned(degraded);
                }
                if backoff > 0 {
                    crate::clock_mock::sleep(std::time::Duration::from_millis(backoff)).await;
                }
            }
        }
    }
}

#[cfg(test)]
#[path = "agent_observe_cost_tests.rs"]
mod cost_tests;

#[cfg(test)]
mod empty_completion_retry_tests;
#[cfg(test)]
mod retry_tests;
#[cfg(test)]
mod streaming_detector_tests;
