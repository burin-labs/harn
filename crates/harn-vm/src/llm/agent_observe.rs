//! LLM call observability: retry logic, transcript dumps, span annotation,
//! and the `observed_llm_call` wrapper extracted from `agent.rs`.

use std::rc::Rc;

use crate::value::VmError;

use super::api::{vm_call_llm_full_streaming, vm_call_llm_full_streaming_offthread, DeltaSender};
use super::trace::{trace_llm_call, LlmTraceEntry};

use super::agent_tools::next_call_id;

// ---------------------------------------------------------------------------
// Retryable error classification
// ---------------------------------------------------------------------------

/// Classify whether a VmError from an LLM call is transient and worth retrying.
pub(super) fn is_retryable_llm_error(err: &VmError) -> bool {
    let msg = match err {
        VmError::Thrown(crate::value::VmValue::String(s)) => s.to_lowercase(),
        VmError::CategorizedError { category, .. } => {
            return matches!(
                category,
                crate::value::ErrorCategory::RateLimit | crate::value::ErrorCategory::Timeout
            );
        }
        VmError::Runtime(s) => s.to_lowercase(),
        _ => return false,
    };
    msg.contains("http 429")
        || msg.contains("http 500")
        || msg.contains("http 502")
        || msg.contains("http 503")
        || msg.contains("http 529")
        || msg.contains("overloaded")
        || msg.contains("rate limit")
        || msg.contains("too many requests")
        || msg.contains("stream error")
        || msg.contains("connection")
        || msg.contains("timed out")
        || msg.contains("timeout")
        || msg.contains("delivered no content")
        || msg.contains("eof")
}

/// Extract retry-after delay from error message if present.
pub(super) fn extract_retry_after_ms(err: &VmError) -> Option<u64> {
    let msg = match err {
        VmError::Thrown(crate::value::VmValue::String(s)) => s.as_ref(),
        VmError::Runtime(s) => s.as_str(),
        _ => return None,
    };
    let lower = msg.to_lowercase();
    if let Some(pos) = lower.find("retry-after:") {
        let after = &msg[pos + "retry-after:".len()..];
        let trimmed = after.trim_start();
        if let Some(num_str) = trimmed.split_whitespace().next() {
            if let Ok(secs) = num_str.parse::<f64>() as Result<f64, _> {
                return Some((secs * 1000.0) as u64);
            }
        }
    }
    None
}

// ---------------------------------------------------------------------------
// Transcript dump helpers (HARN_LLM_TRANSCRIPT_DIR)
// ---------------------------------------------------------------------------

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

pub(super) fn dump_llm_request(
    iteration: usize,
    call_id: &str,
    tool_format: &str,
    opts: &super::api::LlmCallOptions,
) {
    let tool_schemas =
        crate::llm::tools::collect_tool_schemas(opts.tools.as_ref(), opts.native_tools.as_deref());
    append_llm_transcript_entry(&serde_json::json!({
        "type": "request",
        "iteration": iteration,
        "call_id": call_id,
        "span_id": crate::tracing::current_span_id(),
        "timestamp": chrono_now(),
        "model": opts.model,
        "provider": opts.provider,
        "system": opts.system,
        "messages": opts.messages,
        "max_tokens": opts.max_tokens,
        "temperature": opts.temperature,
        "tool_choice": opts.tool_choice,
        "tool_schemas": tool_schemas,
        "tool_format": tool_format,
        "native_tool_count": opts.native_tools.as_ref().map(|tools| tools.len()).unwrap_or(0),
    }));
}

pub(super) fn dump_llm_response(
    iteration: usize,
    call_id: &str,
    result: &super::api::LlmResult,
    response_ms: u64,
) {
    append_llm_transcript_entry(&serde_json::json!({
        "type": "response",
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

// ---------------------------------------------------------------------------
// Progress forwarding
// ---------------------------------------------------------------------------

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

// ---------------------------------------------------------------------------
// LLM retry config
// ---------------------------------------------------------------------------

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
        // Rate limit: yield until the provider's RPM window has capacity.
        super::rate_limit::acquire_permit(&opts.provider).await;

        let call_id = next_call_id();
        let prompt_chars: usize = opts
            .messages
            .iter()
            .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
            .map(|s| s.len())
            .sum();

        // Span annotation
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

        // Bridge: call_start notification
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

        // Transcript dump (enabled by HARN_LLM_TRANSCRIPT_DIR)
        dump_llm_request(
            iteration.unwrap_or(0),
            &call_id,
            &effective_tool_format,
            opts,
        );

        // Execute the LLM call
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
