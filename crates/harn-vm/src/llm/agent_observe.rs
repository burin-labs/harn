//! LLM call observability: retry logic, transcript dumps, span annotation,
//! and the `observed_llm_call` wrapper extracted from `agent.rs`.
//!
//! # Transcript log shape
//!
//! Writes go to `$HARN_LLM_TRANSCRIPT_DIR/llm_transcript.jsonl`, one JSON
//! object per line, append-only. Consumers replay the events in order to
//! reconstruct the model's context at any iteration.
//!
//! Event types:
//!
//! - `system_prompt` `{content, hash}` — emitted once when a new system
//!   prompt takes effect. Dedup'd via a rolling hash so consecutive
//!   identical prompts are not re-emitted.
//! - `tool_schemas` `{schemas, hash}` — same shape for the tool schema
//!   list; each request re-uses the last-emitted set.
//! - `message` `{role, content, iteration?}` — single message appended to
//!   the visible conversation. Emitted every time a message lands in the
//!   transcript (user task, nudge, assistant reply, tool result, host
//!   push).
//! - `provider_call_request` `{call_id, iteration, model, provider,
//!   tool_format, max_tokens, temperature, tool_choice}` — slim metadata
//!   for a single model call. No `messages`, `system`, or `tool_schemas`
//!   fields; those are reconstructable from prior events.
//! - `provider_call_response` `{call_id, iteration, model, text,
//!   tool_calls, input_tokens, output_tokens, cache_*, thinking,
//!   response_ms}` — slim response metadata.
//! - `interpreted_response` `{call_id, iteration, tool_format, prose,
//!   tool_calls, tool_parse_errors}` — post-parse view of the last
//!   assistant turn.
//!
//! To reconstruct the prompt sent at `call_id=X`, replay events in order
//! and track the last `system_prompt`, the last `tool_schemas`, and every
//! `message` up to (but not including) the matching `provider_call_request`.

use std::cell::RefCell;
use std::rc::Rc;

use crate::value::VmError;

use super::api::{vm_call_llm_full_streaming, vm_call_llm_full_streaming_offthread, DeltaSender};
use super::trace::{trace_llm_call, LlmTraceEntry};

use super::agent_tools::next_call_id;

thread_local! {
    /// Last-emitted hash for the current transcript's system prompt and
    /// tool schemas. Used to dedup identical payloads across turns so we
    /// write them once per stage instead of once per request. Cleared on
    /// stage boundaries via `reset_transcript_dedup()`.
    static LAST_SYSTEM_PROMPT_HASH: RefCell<Option<u64>> = const { RefCell::new(None) };
    static LAST_TOOL_SCHEMAS_HASH: RefCell<Option<u64>> = const { RefCell::new(None) };
    /// Current iteration index for any `message` events emitted outside
    /// the main LLM request path (e.g. nudges appended before the first
    /// call, tool results between iterations). Set at the top of each
    /// iteration and cleared on loop exit.
    static CURRENT_ITERATION: RefCell<Option<usize>> = const { RefCell::new(None) };
}

fn hash_str(value: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

fn hash_json(value: &serde_json::Value) -> u64 {
    // Dedup only needs intra-process stability; built-in key ordering is fine.
    let encoded = serde_json::to_string(value).unwrap_or_default();
    hash_str(&encoded)
}

/// Clear the dedup state. Call at the start of a new stage so the first
/// turn always emits system_prompt and tool_schemas events.
pub(crate) fn reset_transcript_dedup() {
    LAST_SYSTEM_PROMPT_HASH.with(|cell| *cell.borrow_mut() = None);
    LAST_TOOL_SCHEMAS_HASH.with(|cell| *cell.borrow_mut() = None);
    CURRENT_ITERATION.with(|cell| *cell.borrow_mut() = None);
}

/// Record the iteration index that applies to any `message` events
/// emitted until the next call. Message events emitted before any
/// iteration has started carry `iteration: null`.
pub(crate) fn set_current_iteration(iteration: Option<usize>) {
    CURRENT_ITERATION.with(|cell| *cell.borrow_mut() = iteration);
}

fn current_iteration() -> Option<usize> {
    CURRENT_ITERATION.with(|cell| *cell.borrow())
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
        VmError::CategorizedError { category, .. } => return category.is_transient(),
        VmError::Thrown(crate::value::VmValue::String(s)) => s.as_ref(),
        VmError::Runtime(s) => s.as_str(),
        _ => return false,
    };
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
    if let Some(num_str) = value.split_whitespace().next() {
        if let Ok(secs) = num_str.parse::<f64>() {
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

/// Write the full LLM request payload to a JSONL transcript file.
pub(super) fn append_llm_transcript_entry(entry: &serde_json::Value) {
    let dir = match std::env::var("HARN_LLM_TRANSCRIPT_DIR") {
        Ok(d) if !d.is_empty() => d,
        _ => return,
    };
    let _ = std::fs::create_dir_all(&dir);
    let path = format!("{dir}/llm_transcript.jsonl");
    if let Ok(line) = serde_json::to_string(&entry) {
        use std::io::Write;
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&path)
        {
            let _ = writeln!(f, "{line}");
        }
    }
}

/// Emit a `message` event for an assistant/user/tool message that was just
/// appended to the visible transcript. One row per message keeps the log
/// append-only: reconstructing the prompt at turn N is a replay, not a
/// snapshot diff.
pub(crate) fn emit_message_event_with_iteration(
    message: &serde_json::Value,
    iteration: Option<usize>,
) {
    let role = message
        .get("role")
        .and_then(|v| v.as_str())
        .unwrap_or("unknown");
    append_llm_transcript_entry(&serde_json::json!({
        "type": "message",
        "timestamp": chrono_now(),
        "span_id": crate::tracing::current_span_id(),
        "iteration": iteration,
        "role": role,
        "content": message.get("content").cloned().unwrap_or(serde_json::Value::Null),
        "tool_calls": message.get("tool_calls").cloned(),
        "tool_call_id": message.get("tool_call_id").cloned(),
        "name": message.get("name").cloned(),
    }));
}

/// Emit a `message` event using the thread-local current iteration.
/// Preferred entry point for the agent loop; for tests or other callers
/// that need an explicit iteration, use `emit_message_event_with_iteration`.
pub(crate) fn emit_message_event(message: &serde_json::Value) {
    emit_message_event_with_iteration(message, current_iteration());
}

fn emit_system_prompt_if_changed(system: Option<&str>) {
    let content = system.unwrap_or("");
    let current = hash_str(content);
    let changed = LAST_SYSTEM_PROMPT_HASH.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.as_ref() == Some(&current) {
            false
        } else {
            *slot = Some(current);
            true
        }
    });
    if !changed {
        return;
    }
    append_llm_transcript_entry(&serde_json::json!({
        "type": "system_prompt",
        "timestamp": chrono_now(),
        "span_id": crate::tracing::current_span_id(),
        "hash": current,
        "content": content,
    }));
}

fn emit_tool_schemas_if_changed(schemas: &[crate::llm::tools::ToolSchema]) {
    let value = serde_json::to_value(schemas).unwrap_or(serde_json::Value::Null);
    let current = hash_json(&value);
    let changed = LAST_TOOL_SCHEMAS_HASH.with(|cell| {
        let mut slot = cell.borrow_mut();
        if slot.as_ref() == Some(&current) {
            false
        } else {
            *slot = Some(current);
            true
        }
    });
    if !changed {
        return;
    }
    append_llm_transcript_entry(&serde_json::json!({
        "type": "tool_schemas",
        "timestamp": chrono_now(),
        "span_id": crate::tracing::current_span_id(),
        "hash": current,
        "schemas": value,
    }));
}

pub(super) fn dump_llm_request(
    iteration: usize,
    call_id: &str,
    tool_format: &str,
    opts: &super::api::LlmCallOptions,
) {
    // Emit system prompt + schemas as dedup'd events so they don't
    // repeat on every request.
    emit_system_prompt_if_changed(opts.system.as_deref());
    let tool_schemas =
        crate::llm::tools::collect_tool_schemas(opts.tools.as_ref(), opts.native_tools.as_deref());
    emit_tool_schemas_if_changed(&tool_schemas);

    append_llm_transcript_entry(&serde_json::json!({
        "type": "provider_call_request",
        "iteration": iteration,
        "call_id": call_id,
        "span_id": crate::tracing::current_span_id(),
        "timestamp": chrono_now(),
        "model": opts.model,
        "provider": opts.provider,
        "max_tokens": opts.max_tokens,
        "temperature": opts.temperature,
        "tool_choice": opts.tool_choice,
        "tool_format": tool_format,
        "native_tool_count": opts.native_tools.as_ref().map(|tools| tools.len()).unwrap_or(0),
        "message_count": opts.messages.len(),
    }));
}

pub(super) fn dump_llm_response(
    iteration: usize,
    call_id: &str,
    result: &super::api::LlmResult,
    response_ms: u64,
) {
    append_llm_transcript_entry(&serde_json::json!({
        "type": "provider_call_response",
        "iteration": iteration,
        "call_id": call_id,
        "span_id": crate::tracing::current_span_id(),
        "timestamp": chrono_now(),
        "model": result.model,
        "text": result.text,
        "tool_calls": result.tool_calls,
        "input_tokens": result.input_tokens,
        "output_tokens": result.output_tokens,
        "cache_read_tokens": result.cache_read_tokens,
        "cache_write_tokens": result.cache_write_tokens,
        // Explicit bool for easy cache-regression spotting in tailed logs.
        "cache_hit": result.cache_read_tokens > 0,
        "thinking": result.thinking,
        "response_ms": response_ms,
    }));
}

pub(super) fn dump_llm_interpreted_response(
    iteration: usize,
    call_id: &str,
    tool_format: &str,
    prose: &str,
    tool_calls: &[serde_json::Value],
    tool_parse_errors: &[String],
) {
    append_llm_transcript_entry(&serde_json::json!({
        "type": "interpreted_response",
        "iteration": iteration,
        "call_id": call_id,
        "span_id": crate::tracing::current_span_id(),
        "timestamp": chrono_now(),
        "tool_format": tool_format,
        "prose": prose,
        "tool_calls": tool_calls,
        "tool_parse_errors": tool_parse_errors,
    }));
}

pub(super) fn annotate_current_span(metadata: &[(&str, serde_json::Value)]) {
    let Some(span_id) = crate::tracing::current_span_id() else {
        return;
    };
    for (key, value) in metadata {
        crate::tracing::span_set_metadata(span_id, key, value.clone());
    }
}

pub(super) fn chrono_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}", now.as_secs(), now.subsec_millis())
}

/// Create an unbounded channel and spawn a local task that forwards text
/// deltas to `bridge.send_call_progress()`.
pub(super) fn spawn_progress_forwarder(
    bridge: &Rc<crate::bridge::HostBridge>,
    call_id: String,
    user_visible: bool,
) -> DeltaSender {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let bridge = bridge.clone();
    tokio::task::spawn_local(async move {
        let mut token_count: u64 = 0;
        while let Some(delta) = rx.recv().await {
            token_count += 1;
            bridge.send_call_progress(&call_id, &delta, token_count, user_visible);
        }
    });
    tx
}

/// Configuration for LLM call retries.
pub(crate) struct LlmRetryConfig {
    /// Maximum number of retries for transient errors (429, 5xx, connection).
    pub retries: usize,
    /// Base backoff in milliseconds between retries.
    pub backoff_ms: u64,
}

impl Default for LlmRetryConfig {
    fn default() -> Self {
        Self {
            retries: 0,
            backoff_ms: 2000,
        }
    }
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
    bridge: Option<&Rc<crate::bridge::HostBridge>>,
    retry_config: &LlmRetryConfig,
    iteration: Option<usize>,
    user_visible: bool,
    offthread: bool,
) -> Result<super::api::LlmResult, VmError> {
    let effective_tool_format = tool_format
        .map(str::to_string)
        .or_else(|| {
            std::env::var("HARN_AGENT_TOOL_FORMAT")
                .ok()
                .filter(|value| !value.trim().is_empty())
        })
        .unwrap_or_else(|| crate::llm_config::default_tool_format(&opts.model, &opts.provider));
    let mut attempt = 0usize;
    loop {
        super::rate_limit::acquire_permit(&opts.provider).await;

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
        ];
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

        let start = std::time::Instant::now();
        let llm_result = if let Some(b) = bridge {
            let delta_tx = spawn_progress_forwarder(b, call_id.clone(), user_visible);
            if offthread {
                vm_call_llm_full_streaming_offthread(opts, delta_tx).await
            } else {
                vm_call_llm_full_streaming(opts, delta_tx).await
            }
        } else if offthread {
            let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            vm_call_llm_full_streaming_offthread(opts, delta_tx).await
        } else {
            super::api::vm_call_llm_full(opts).await
        };
        let duration_ms = start.elapsed().as_millis() as u64;

        match llm_result {
            Ok(result) => {
                annotate_current_span(&[
                    ("status", serde_json::json!("ok")),
                    ("input_tokens", serde_json::json!(result.input_tokens)),
                    ("output_tokens", serde_json::json!(result.output_tokens)),
                ]);
                dump_llm_response(iteration.unwrap_or(0), &call_id, &result, duration_ms);
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
                        }),
                    );
                }
                trace_llm_call(LlmTraceEntry {
                    model: result.model.clone(),
                    input_tokens: result.input_tokens,
                    output_tokens: result.output_tokens,
                    duration_ms,
                });
                super::trace::emit_agent_event(super::trace::AgentTraceEvent::LlmCall {
                    call_id: call_id.clone(),
                    model: result.model.clone(),
                    input_tokens: result.input_tokens,
                    output_tokens: result.output_tokens,
                    cache_tokens: result.cache_read_tokens,
                    duration_ms,
                    iteration: iteration.unwrap_or(0),
                });
                return Ok(result);
            }
            Err(error) => {
                let retryable = is_retryable_llm_error(&error);
                let can_retry = retryable && attempt < retry_config.retries;
                let status = if can_retry {
                    "retrying"
                } else if retryable {
                    "retries_exhausted"
                } else {
                    "error"
                };
                annotate_current_span(&[
                    ("status", serde_json::json!(status)),
                    ("error", serde_json::json!(error.to_string())),
                    ("retryable", serde_json::json!(retryable)),
                    ("attempt", serde_json::json!(attempt)),
                ]);
                if let Some(b) = bridge {
                    b.send_call_end(
                        &call_id,
                        "llm",
                        "llm_call",
                        duration_ms,
                        status,
                        serde_json::json!({
                            "error": error.to_string(),
                            "retryable": retryable,
                            "attempt": attempt,
                            "user_visible": user_visible,
                        }),
                    );
                }
                if !can_retry {
                    return Err(error);
                }
                attempt += 1;
                let backoff = extract_retry_after_ms(&error)
                    .unwrap_or(retry_config.backoff_ms * (1 << attempt.min(4)) as u64);
                crate::events::log_warn(
                    "llm",
                    &format!(
                        "LLM call failed ({}), retrying in {}ms (attempt {}/{})",
                        error, backoff, attempt, retry_config.retries
                    ),
                );
                tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
            }
        }
    }
}

#[cfg(test)]
mod retry_tests {
    use super::*;
    use crate::value::{ErrorCategory, VmError, VmValue};
    use std::rc::Rc;

    fn thrown(s: &str) -> VmError {
        VmError::Thrown(VmValue::String(Rc::from(s)))
    }

    fn categorized(msg: &str, category: ErrorCategory) -> VmError {
        VmError::CategorizedError {
            message: msg.to_string(),
            category,
        }
    }

    #[test]
    fn categorized_overloaded_is_retryable() {
        assert!(is_retryable_llm_error(&categorized(
            "upstream overloaded",
            ErrorCategory::Overloaded
        )));
    }

    #[test]
    fn categorized_server_error_is_retryable() {
        assert!(is_retryable_llm_error(&categorized(
            "500 internal",
            ErrorCategory::ServerError
        )));
    }

    #[test]
    fn categorized_transient_network_is_retryable() {
        assert!(is_retryable_llm_error(&categorized(
            "reset",
            ErrorCategory::TransientNetwork
        )));
    }

    #[test]
    fn categorized_auth_not_retryable() {
        assert!(!is_retryable_llm_error(&categorized(
            "invalid key",
            ErrorCategory::Auth
        )));
    }

    #[test]
    fn http_503_is_retryable_via_classifier() {
        assert!(is_retryable_llm_error(&thrown(
            "HTTP 503 Service Unavailable"
        )));
    }

    #[test]
    fn http_504_is_retryable() {
        assert!(is_retryable_llm_error(&thrown("HTTP 504 Gateway Timeout")));
    }

    #[test]
    fn http_529_is_retryable() {
        assert!(is_retryable_llm_error(&thrown("HTTP 529 overloaded_error")));
    }

    #[test]
    fn bad_gateway_string_is_retryable() {
        assert!(is_retryable_llm_error(&thrown("bad gateway response")));
    }

    #[test]
    fn service_unavailable_string_is_retryable() {
        assert!(is_retryable_llm_error(&thrown("service unavailable")));
    }

    #[test]
    fn auth_error_not_retryable() {
        assert!(!is_retryable_llm_error(&thrown("HTTP 401 Unauthorized")));
    }

    #[test]
    fn retry_after_integer_seconds() {
        assert_eq!(parse_retry_after("err: retry-after: 5"), Some(5_000));
    }

    #[test]
    fn retry_after_fractional_seconds() {
        assert_eq!(parse_retry_after("retry-after: 2.5"), Some(2_500));
    }

    #[test]
    fn retry_after_clamped_to_cap() {
        assert_eq!(parse_retry_after("retry-after: 600"), Some(60_000));
    }

    #[test]
    fn retry_after_http_date_past_is_zero() {
        let past = "retry-after: Mon, 01 Jan 1990 00:00:00 GMT";
        assert_eq!(parse_retry_after(past), Some(0));
    }

    #[test]
    fn retry_after_missing_returns_none() {
        assert_eq!(parse_retry_after("nothing here"), None);
    }

    #[test]
    fn retry_after_malformed_returns_none() {
        assert_eq!(parse_retry_after("retry-after: soon-ish"), None);
    }
}
