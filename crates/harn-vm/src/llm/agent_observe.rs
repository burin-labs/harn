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
//! - `system_prompt` `{content, hash}` — emitted once when a new system
//!   prompt takes effect. Dedup'd via a rolling hash so consecutive
//!   identical prompts are not re-emitted.
//! - `tool_schemas` `{schemas, hash}` — same shape for the tool schema
//!   list; each request re-uses the last-emitted set.
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
//!   max_tokens, temperature, tool_choice, tool_format}` — slim metadata
//!   for a single model call. No `messages`, `system`, or `tool_schemas`
//!   fields; those are reconstructable from prior events. Also carries
//!   diagnostics `{thinking, native_tool_count, message_count,
//!   structural_experiment, route_policy, fallback_chain,
//!   routing_decision}`.
//!   Set `HARN_LLM_TRANSCRIPT_VERBOSE=1` to include a `request_snapshot`
//!   object with the exact system prompt, message list, and tool schemas
//!   attached to each request for debugging provider-context issues.
//! - `provider_call_response` core `{call_id, iteration, model, provider,
//!   text, tool_calls, parsed_tool_calls, input_tokens, output_tokens,
//!   response_ms}`. `tool_calls` is the provider-native tool-call array
//!   (empty for text-format local models); `parsed_tool_calls` is the
//!   merged view (native when present, otherwise the calls parsed out of
//!   the inline tagged `<tool_call>` blocks in `text`) so the record is
//!   self-describing for text-format runs. Also carries diagnostics
//!   `{cost_usd, cache_* (cache_read_tokens, cache_write_tokens,
//!   cache_creation_input_tokens, cache_hit_ratio, cache_savings_usd,
//!   cache_hit), thinking, thinking_summary, provider_telemetry,
//!   structural_experiment}`.
//! - `interpreted_response` `{call_id, iteration, tool_format, prose,
//!   tool_calls, tool_parse_errors}` — post-parse view of the last
//!   assistant turn.
//!
//! To reconstruct the prompt sent at `call_id=X`, replay events in order
//! and track the last `system_prompt`, the last `tool_schemas`, and every
//! `message` up to (but not including) the matching `provider_call_request`.

use std::cell::RefCell;
use std::sync::Arc;

use crate::event_log::EventLog;
use crate::value::VmError;

use super::api::{vm_call_llm_full_streaming, vm_call_llm_full_streaming_offthread, DeltaSender};
use super::trace::{trace_llm_call, LlmTraceEntry};

use super::agent_tools::next_call_id;

thread_local! {
    /// Last-emitted hash for the current transcript's system prompt and
    /// tool schemas. Used to dedup identical payloads across turns so we
    /// write them once per stage instead of once per request.
    static LAST_SYSTEM_PROMPT_HASH: RefCell<Option<u64>> = const { RefCell::new(None) };
    static LAST_TOOL_SCHEMAS_HASH: RefCell<Option<u64>> = const { RefCell::new(None) };
    static TRANSCRIPT_DIR_STACK: RefCell<Vec<String>> = const { RefCell::new(Vec::new()) };
}

fn reset_transcript_dedup() {
    LAST_SYSTEM_PROMPT_HASH.with(|hash| *hash.borrow_mut() = None);
    LAST_TOOL_SCHEMAS_HASH.with(|hash| *hash.borrow_mut() = None);
}

pub(super) fn push_llm_transcript_dir(dir: &str) {
    if dir.trim().is_empty() {
        return;
    }
    TRANSCRIPT_DIR_STACK.with(|stack| stack.borrow_mut().push(dir.to_string()));
    reset_transcript_dedup();
}

pub(super) fn pop_llm_transcript_dir() {
    TRANSCRIPT_DIR_STACK.with(|stack| {
        stack.borrow_mut().pop();
    });
    reset_transcript_dedup();
}

fn current_transcript_dir() -> Option<String> {
    let stacked = TRANSCRIPT_DIR_STACK.with(|stack| stack.borrow().last().cloned());
    if stacked.is_some() {
        return stacked;
    }
    std::env::var("HARN_LLM_TRANSCRIPT_DIR")
        .ok()
        .filter(|d| !d.is_empty())
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

fn verbose_llm_transcript_enabled() -> bool {
    std::env::var("HARN_LLM_TRANSCRIPT_VERBOSE")
        .ok()
        .map(|value| {
            let normalized = value.trim().to_ascii_lowercase();
            matches!(normalized.as_str(), "1" | "true" | "yes" | "on" | "full")
        })
        .unwrap_or(false)
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

fn is_empty_completion_retry_error(err: &VmError) -> bool {
    let msg = match err {
        VmError::Thrown(crate::value::VmValue::String(s)) => s.as_ref(),
        VmError::CategorizedError { message, .. } => message.as_str(),
        VmError::Runtime(s) => s.as_str(),
        _ => return false,
    };
    let lower = msg.to_lowercase();
    lower.contains("completion_tokens=") && lower.contains("delivered no content")
}

/// A wire-level "success" that carries nothing at all: zero output tokens, no
/// text, no thinking, no tool calls, and no server-side tool-search activity.
/// Observed live (OpenRouter): a provider stall that ends with an empty 200
/// flows back into the agent loop as an empty assistant turn the loop has to
/// burn an iteration recovering from. Treated as a transient provider hiccup
/// and retried in [`observed_llm_call`].
///
/// Token-cap truncations (`stop_reason` length/max_tokens) are excluded — a
/// deterministic cap would just re-truncate on every retry, mirroring the
/// `done_reason == "length"` carve-out on the Ollama NDJSON path.
fn is_zero_token_empty_completion(result: &super::api::LlmResult) -> bool {
    let truncated = matches!(
        result
            .stop_reason
            .as_deref()
            .map(str::to_ascii_lowercase)
            .as_deref(),
        Some("length" | "max_tokens")
    );
    let has_tool_search_block = result.blocks.iter().any(|block| {
        matches!(
            block.get("type").and_then(|value| value.as_str()),
            Some("tool_search_query") | Some("tool_search_result")
        )
    });
    result.output_tokens == 0
        && result.text.is_empty()
        && result.tool_calls.is_empty()
        && result.thinking.as_deref().unwrap_or("").is_empty()
        && !truncated
        && !has_tool_search_block
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

/// Write the full LLM request payload to a JSONL transcript file.
///
/// Holds a process-wide mutex around the open + write so concurrent
/// transcript emitters (parallel tests, multi-tenant agent loops on the
/// same VM) never produce a torn line. POSIX `O_APPEND` only guarantees
/// atomicity for writes ≤ `PIPE_BUF` (512 bytes on macOS), and
/// `provider_call_request` events comfortably exceed that — without
/// this lock, two simultaneous `writeln!` calls on different `File`
/// handles for the same path can interleave their bytes mid-line and
/// produce invalid JSON that downstream readers (and tests) silently
/// drop.
pub(super) fn append_llm_transcript_entry(entry: &serde_json::Value) {
    let mut redacted = entry.clone();
    crate::redact::current_policy().redact_json_in_place(&mut redacted);
    forward_transcript_run_events(&redacted);
    append_llm_transcript_event_log(&redacted);
    let Some(dir) = current_transcript_dir() else {
        return;
    };
    let _ = std::fs::create_dir_all(&dir);
    let path = format!("{dir}/llm_transcript.jsonl");
    let Ok(line) = serde_json::to_string(&redacted) else {
        return;
    };
    static WRITE_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    let _guard = WRITE_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    if let Ok(mut f) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
    {
        use std::io::Write;
        let _ = f.write_all(line.as_bytes());
        let _ = f.write_all(b"\n");
    }
}

/// Fan transcript entries out to the run-events sink (`harn run
/// --json`). [`RunEvent::Transcript`] mirrors the raw entry; tool
/// calls and tool results carried inside the transcript stream are
/// also surfaced as their own [`RunEvent::ToolCall`] /
/// [`RunEvent::ToolResult`] variants so consumers don't have to
/// re-parse the transcript shape.
///
/// `tool_call` events are emitted once per logical call, keyed off
/// `interpreted_response` (the post-parse view that resolves the final
/// tool selection). Earlier-stage entries (`provider_call_response`)
/// still appear as `transcript` events for replay, but their
/// `tool_calls` arrays are not promoted to avoid duplicate
/// `tool_call` events for the same `call_id`.
fn forward_transcript_run_events(entry: &serde_json::Value) {
    if !crate::run_events::sink_active() {
        return;
    }
    let kind = entry
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("transcript_event")
        .to_string();
    let agent_id = entry
        .get("agent_id")
        .and_then(|value| value.as_str())
        .map(str::to_string);

    if kind == "interpreted_response" {
        if let Some(calls) = entry.get("tool_calls").and_then(|value| value.as_array()) {
            for call in calls {
                let name = call
                    .get("name")
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                let id = call
                    .get("id")
                    .or_else(|| call.get("call_id"))
                    .and_then(|value| value.as_str())
                    .unwrap_or("")
                    .to_string();
                let args = call
                    .get("arguments")
                    .or_else(|| call.get("args"))
                    .cloned()
                    .unwrap_or(serde_json::Value::Null);
                crate::run_events::emit(crate::run_events::RunEvent::ToolCall {
                    call_id: id,
                    name,
                    args,
                    started_at: chrono_now(),
                });
            }
        }
    }

    if kind == "tool_result" {
        let call_id = entry
            .get("call_id")
            .or_else(|| entry.get("id"))
            .and_then(|value| value.as_str())
            .unwrap_or("")
            .to_string();
        let ok = entry
            .get("ok")
            .and_then(|value| value.as_bool())
            .unwrap_or(true);
        let result = entry
            .get("result")
            .or_else(|| entry.get("content"))
            .cloned()
            .unwrap_or(serde_json::Value::Null);
        crate::run_events::emit(crate::run_events::RunEvent::ToolResult {
            call_id,
            ok,
            result,
        });
    }

    crate::run_events::emit(crate::run_events::RunEvent::Transcript {
        agent_id,
        kind,
        payload: entry.clone(),
    });
}

fn append_llm_transcript_event_log(entry: &serde_json::Value) {
    let Some(log) = crate::event_log::active_event_log() else {
        return;
    };
    let topic = crate::event_log::Topic::new("agent.transcript.llm")
        .expect("static transcript topic should be valid");
    let kind = entry
        .get("type")
        .and_then(|value| value.as_str())
        .unwrap_or("transcript_event")
        .to_string();
    let mut headers = std::collections::BTreeMap::new();
    if let Some(span_id) = entry.get("span_id").and_then(|value| value.as_u64()) {
        headers.insert("span_id".to_string(), span_id.to_string());
    }
    if let Some(context) = crate::triggers::dispatcher::current_dispatch_context() {
        headers.insert("trigger_id".to_string(), context.binding_id.clone());
        headers.insert(
            "binding_key".to_string(),
            format!("{}@v{}", context.binding_id, context.binding_version),
        );
        headers.insert("event_id".to_string(), context.trigger_event.id.0.clone());
        headers.insert(
            "trace_id".to_string(),
            context.trigger_event.trace_id.0.clone(),
        );
        headers.insert("pipeline".to_string(), context.binding_id);
        headers.insert("action".to_string(), context.action);
        if let Some(tenant_id) = context.trigger_event.tenant_id {
            headers.insert("tenant_id".to_string(), tenant_id.0);
        }
    }
    let event = crate::event_log::LogEvent::new(kind, entry.clone()).with_headers(headers);
    // Append synchronously. Earlier this fire-and-forget `handle.spawn`ed the
    // append on the ambient tokio runtime, but the agent loop and the test
    // runner drive their runtime with `LocalSet::run_until`, which stops
    // polling once the driving future resolves. Detached append tasks were
    // therefore never polled to completion: each stranded task pinned its
    // transcript-sized `LogEvent` payload plus an `Arc<AnyEventLog>` clone for
    // the lifetime of the runtime — across an entire `harn test --parallel`
    // worker, that accumulated ~one transcript per test and OOM'd CI (#2660).
    //
    // None of the event-log backends actually yield to the tokio reactor on
    // `append` (memory = `Mutex`, sqlite = blocking `Mutex<Connection>`, file =
    // blocking fs), so a private `futures::executor::block_on` runs the append
    // to completion on this thread without touching the ambient runtime. This
    // is the same path the non-runtime branch already used.
    let _ = futures::executor::block_on(log.append(&topic, event));
}

/// Record a `template.render` transcript event for a `render()` /
/// `render_prompt()` call that resolved under an LLM-aware frame.
/// Captures the active LLM identity + capability snapshot plus the
/// branch trace produced during rendering. Replay determinism is
/// guaranteed by the renderer itself; this function is purely a
/// serializer.
pub fn record_template_render(
    template_uri: &str,
    template_revision_hash: &str,
    ctx: &crate::stdlib::template::LlmRenderContext,
    trace: &[crate::stdlib::template::BranchDecision],
    rendered_bytes: usize,
) {
    let branches = trace
        .iter()
        .map(|decision| {
            let mut entry = serde_json::Map::new();
            entry.insert(
                "kind".to_string(),
                serde_json::Value::String(decision.kind.as_str().to_string()),
            );
            entry.insert(
                "template_uri".to_string(),
                serde_json::Value::String(decision.template_uri.clone()),
            );
            entry.insert("line".to_string(), serde_json::json!(decision.line));
            entry.insert("col".to_string(), serde_json::json!(decision.col));
            entry.insert(
                "branch_id".to_string(),
                serde_json::Value::String(decision.branch_id.clone()),
            );
            if let Some(label) = decision.branch_label.as_ref() {
                entry.insert(
                    "branch_label".to_string(),
                    serde_json::Value::String(label.clone()),
                );
            }
            serde_json::Value::Object(entry)
        })
        .collect::<Vec<_>>();
    let llm = serde_json::json!({
        "provider": ctx.provider,
        "model": ctx.model,
        "family": ctx.family,
        "capabilities": vm_value_to_json(&ctx.capabilities),
    });
    let mut fields = serde_json::Map::new();
    fields.insert(
        "template_uri".to_string(),
        serde_json::Value::String(template_uri.to_string()),
    );
    fields.insert(
        "template_revision_hash".to_string(),
        serde_json::Value::String(template_revision_hash.to_string()),
    );
    fields.insert("llm".to_string(), llm);
    fields.insert("branches".to_string(), serde_json::Value::Array(branches));
    fields.insert(
        "rendered_bytes".to_string(),
        serde_json::json!(rendered_bytes),
    );
    append_llm_observability_entry("template.render", fields);
}

fn vm_value_to_json(value: &crate::value::VmValue) -> serde_json::Value {
    use crate::value::VmValue;
    match value {
        VmValue::Nil => serde_json::Value::Null,
        VmValue::Bool(b) => serde_json::Value::Bool(*b),
        VmValue::Int(n) => serde_json::json!(*n),
        VmValue::Float(f) => serde_json::json!(*f),
        VmValue::String(s) => serde_json::Value::String(s.to_string()),
        VmValue::List(items) => {
            serde_json::Value::Array(items.iter().map(vm_value_to_json).collect())
        }
        VmValue::Dict(d) => serde_json::Value::Object(
            d.iter()
                .map(|(k, v)| (k.clone(), vm_value_to_json(v)))
                .collect(),
        ),
        other => serde_json::Value::String(other.display()),
    }
}

pub(crate) fn append_llm_observability_entry(
    event_type: &str,
    mut fields: serde_json::Map<String, serde_json::Value>,
) {
    fields.insert("type".to_string(), serde_json::json!(event_type));
    fields
        .entry("timestamp".to_string())
        .or_insert_with(|| serde_json::json!(chrono_now()));
    fields
        .entry("span_id".to_string())
        .or_insert_with(|| serde_json::json!(crate::tracing::current_span_id()));
    append_llm_transcript_entry(&serde_json::Value::Object(fields));
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

    let structural_experiment = opts
        .applied_structural_experiment
        .as_ref()
        .map(serde_json::to_value)
        .transpose()
        .unwrap_or(None)
        .unwrap_or(serde_json::Value::Null);
    if let Some(decision) = opts.routing_decision.as_ref() {
        append_llm_transcript_entry(&serde_json::json!({
            "type": "routing_decision",
            "iteration": iteration,
            "call_id": call_id,
            "span_id": crate::tracing::current_span_id(),
            "timestamp": chrono_now(),
            "policy": decision.policy.clone(),
            "requested_quality": decision.requested_quality.clone(),
            "selected_provider": decision.selected_provider.clone(),
            "selected_model": decision.selected_model.clone(),
            "fallback_chain": opts.fallback_chain.clone(),
            "alternatives": decision.alternatives.clone(),
        }));
    }
    let mut request_event = serde_json::json!({
        "type": "provider_call_request",
        "iteration": iteration,
        "call_id": call_id,
        "span_id": crate::tracing::current_span_id(),
        "timestamp": chrono_now(),
        "model": opts.model,
        "provider": opts.provider,
        "max_tokens": opts.max_tokens,
        "temperature": opts.temperature,
        "thinking": match &opts.thinking {
            super::api::ThinkingConfig::Disabled => serde_json::json!({
                "mode": "disabled",
                "enabled": false,
                "budget_tokens": serde_json::Value::Null,
            }),
            super::api::ThinkingConfig::Enabled { budget_tokens } => serde_json::json!({
                "mode": "enabled",
                "enabled": true,
                "budget_tokens": budget_tokens,
            }),
            super::api::ThinkingConfig::Adaptive => serde_json::json!({
                "mode": "adaptive",
                "enabled": true,
                "budget_tokens": serde_json::Value::Null,
            }),
            super::api::ThinkingConfig::Effort { level } => serde_json::json!({
                "mode": "effort",
                "level": level.as_str(),
                "enabled": *level != super::api::ReasoningEffort::None,
                "budget_tokens": serde_json::Value::Null,
            }),
        },
        "tool_choice": opts.tool_choice,
        "tool_format": tool_format,
        "native_tool_count": opts.native_tools.as_ref().map(|tools| tools.len()).unwrap_or(0),
        "message_count": opts.messages.len(),
        "structural_experiment": structural_experiment,
        "route_policy": opts.route_policy.as_label(),
        "fallback_chain": opts.fallback_chain.clone(),
        "routing_decision": opts.routing_decision.clone(),
    });
    if verbose_llm_transcript_enabled() {
        request_event["request_snapshot"] = serde_json::json!({
            "system": opts.system,
            "messages": opts.messages,
            "tool_schemas": tool_schemas,
            "native_tools": opts.native_tools,
        });
    }
    append_llm_transcript_entry(&request_event);
}

/// Compute the merged (native OR text-parsed) tool calls for the
/// observability response record. Mirrors the merge in
/// `crate::llm::api::result::vm_build_llm_result` (provider-native calls
/// take precedence; otherwise fall back to the calls parsed out of the
/// inline tagged `<tool_call>` blocks in `result.text`, resolved against
/// the same `tools` registry the request used so unknown-name calls are
/// not dropped). By the time the result reaches this function `text` has
/// already been canonicalized from any `[[CALL]]` wire form back to
/// `<tool_call>`, so the tagged parser sees the calls.
///
/// Observability-only: this is read off the existing `result` + `tools`
/// and does not flow back into the request-construction / history path, so
/// the model's next-turn payload is byte-identical with or without this
/// call.
fn merged_tool_calls_for_observability(
    result: &super::api::LlmResult,
    tools: Option<&crate::value::VmValue>,
) -> Vec<serde_json::Value> {
    if !result.tool_calls.is_empty() {
        return result.tool_calls.clone();
    }
    crate::llm::tools::parse_text_tool_calls_with_tools(&result.text, tools).calls
}

pub(super) fn dump_llm_response(
    iteration: usize,
    call_id: &str,
    result: &super::api::LlmResult,
    response_ms: u64,
    structural_experiment: Option<&crate::llm::structural_experiments::AppliedStructuralExperiment>,
    tools: Option<&crate::value::VmValue>,
) {
    let structural_experiment = structural_experiment
        .map(serde_json::to_value)
        .transpose()
        .unwrap_or(None)
        .unwrap_or(serde_json::Value::Null);
    let telemetry = serde_json::to_value(&result.telemetry).unwrap_or(serde_json::Value::Null);
    let parsed_tool_calls = merged_tool_calls_for_observability(result, tools);
    append_llm_transcript_entry(&serde_json::json!({
        "type": "provider_call_response",
        "iteration": iteration,
        "call_id": call_id,
        "span_id": crate::tracing::current_span_id(),
        "timestamp": chrono_now(),
        "provider": result.provider,
        "model": result.model,
        "text": result.text,
        "tool_calls": result.tool_calls,
        // Observability-only merged view: provider-native calls when present,
        // otherwise the calls parsed out of the inline tagged `<tool_call>`
        // blocks in `text`. Text-format local models (llamacpp/qwen3.6) carry
        // their calls only inline, so `tool_calls` (native) is empty for them;
        // this sidecar makes the response record self-describing. Distinct from
        // `tool_calls` so consumers can tell native vs. text-parsed apart. This
        // does NOT touch the request-construction / history path — the model's
        // next-turn payload is unchanged.
        "parsed_tool_calls": parsed_tool_calls,
        "input_tokens": result.input_tokens,
        "output_tokens": result.output_tokens,
        "cost_usd": crate::llm::cost::calculate_cost_for_provider(
            &result.provider,
            &result.model,
            result.input_tokens,
            result.output_tokens,
        ),
        "cache_read_tokens": result.cache_read_tokens,
        "cache_write_tokens": result.cache_write_tokens,
        "cache_creation_input_tokens": result.cache_write_tokens,
        "cache_hit_ratio": crate::llm::cost::cache_hit_ratio(
            result.input_tokens,
            result.cache_read_tokens,
            result.cache_write_tokens,
        ),
        "cache_savings_usd": crate::llm::cost::cache_savings_usd_for_provider(
            &result.provider,
            &result.model,
            result.cache_read_tokens,
            result.cache_write_tokens,
        ),
        // Explicit bool for easy cache-regression spotting in tailed logs.
        "cache_hit": result.cache_read_tokens > 0,
        "thinking": result.thinking,
        "thinking_summary": result.thinking_summary,
        "response_ms": response_ms,
        // Server-side runtime telemetry (Ollama timings, llama.cpp prefill /
        // decode breakdown, etc.). Empty for providers that report nothing.
        "provider_telemetry": telemetry,
        "structural_experiment": structural_experiment,
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

/// Inputs required to wire a streaming candidate detector (harn#692)
/// into a delta-forwarding task. When supplied, the detector consumes
/// each text delta in parallel with the bridge progress notifier and
/// emits `AgentEvent::ToolCall { parsing: true, .. }` /
/// `AgentEvent::ToolCallUpdate { parsing: false, .. }` events through
/// the global session sink registry so ACP clients render an in-flight
/// chip while the model is still writing the args.
pub(crate) struct StreamingDetectorContext {
    pub session_id: String,
    pub known_tools: std::collections::BTreeSet<String>,
}

/// Create an unbounded channel and spawn a local task that forwards text
/// deltas to `bridge.send_call_progress()`. When `detector_ctx` is
/// `Some`, the same task also drives a streaming text-tool-call
/// candidate detector — emitting candidate-start / promoted / aborted
/// events via the global session sink registry as the buffer grows
/// (harn#692).
pub(super) fn spawn_progress_forwarder(
    bridge: &Arc<crate::bridge::HostBridge>,
    call_id: String,
    user_visible: bool,
    detector_ctx: Option<StreamingDetectorContext>,
    mut first_token: super::first_token::FirstTokenTimer,
) -> DeltaSender {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let bridge = bridge.clone();
    let mut detector = detector_ctx.map(|ctx| {
        crate::llm::tools::StreamingToolCallDetector::new(ctx.session_id, ctx.known_tools)
    });
    tokio::task::spawn_local(async move {
        let mut token_count: u64 = 0;
        while let Some(delta) = rx.recv().await {
            first_token.observe_delta();
            token_count += 1;
            bridge.send_call_progress(&call_id, &delta, token_count, user_visible);
            if let Some(d) = detector.as_mut() {
                for event in d.push(&delta) {
                    crate::agent_events::emit_event(&event);
                }
            }
        }
        if let Some(mut d) = detector {
            for event in d.finalize() {
                crate::agent_events::emit_event(&event);
            }
        }
    });
    tx
}

/// No-bridge twin of `spawn_progress_forwarder`. Drives only the
/// streaming candidate detector — the deltas are otherwise discarded
/// (the bridge progress channel is the only consumer, and we don't have
/// one). Used so non-bridge callers (offthread VM, CLI loops without an
/// attached host) still see candidate events when they have a
/// `StreamingDetectorContext`.
pub(super) fn spawn_detector_only_forwarder(
    detector_ctx: StreamingDetectorContext,
    first_token: super::first_token::FirstTokenTimer,
) -> DeltaSender {
    let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    tokio::task::spawn_local(run_detector_loop(detector_ctx, rx, first_token));
    tx
}

/// Inner loop driving a [`StreamingToolCallDetector`] from a delta
/// channel. Pulled out of `spawn_detector_only_forwarder` so tests can
/// drive the same logic deterministically (await directly) without
/// depending on `spawn_local` task scheduling.
///
/// `sink` is the function each emitted event flows through. Production
/// passes `crate::agent_events::emit_event` so events fan out through
/// the global session-keyed sink registry. Tests pass a closure that
/// captures into a local buffer — sidestepping the global registry,
/// which other tests in this binary mutate via `reset_all_sinks` and
/// can race the per-session install.
#[cfg(test)]
async fn run_detector_loop_with_sink(
    detector_ctx: StreamingDetectorContext,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    mut sink: impl FnMut(&crate::agent_events::AgentEvent),
) {
    let mut detector = crate::llm::tools::StreamingToolCallDetector::new(
        detector_ctx.session_id,
        detector_ctx.known_tools,
    );
    while let Some(delta) = rx.recv().await {
        for event in detector.push(&delta) {
            sink(&event);
        }
    }
    for event in detector.finalize() {
        sink(&event);
    }
}

/// Production wrapper: forwards every detector event through the global
/// session sink registry so ACP / external sinks see them.
async fn run_detector_loop(
    detector_ctx: StreamingDetectorContext,
    mut rx: tokio::sync::mpsc::UnboundedReceiver<String>,
    mut first_token: super::first_token::FirstTokenTimer,
) {
    let mut detector = crate::llm::tools::StreamingToolCallDetector::new(
        detector_ctx.session_id,
        detector_ctx.known_tools,
    );
    while let Some(delta) = rx.recv().await {
        first_token.observe_delta();
        for event in detector.push(&delta) {
            crate::agent_events::emit_event(&event);
        }
    }
    for event in detector.finalize() {
        crate::agent_events::emit_event(&event);
    }
}

/// Configuration for LLM call retries.
pub(crate) const DEFAULT_LLM_CALL_RETRIES: usize = 0;
pub(crate) const DEFAULT_LLM_CALL_BACKOFF_MS: u64 = 250;

/// Built-in retry budget for zero-token empty completions. Applies even when
/// the caller's transient-retry budget is 0 (the fail-fast `llm_call`
/// default), mirroring the transport's unconditional single retry for the
/// Ollama empty-content parser bug: an empty 200 is clearly a provider
/// hiccup, and most live callers (e.g. the Burin agent loop) retry only on
/// *errors*, so an empty Ok would otherwise sail through untouched.
const EMPTY_COMPLETION_BUILTIN_RETRIES: usize = 1;

/// Effective retry budget for zero-token empty completions: the caller's
/// transient budget, floored at [`EMPTY_COMPLETION_BUILTIN_RETRIES`] for real
/// providers. Deterministic in-process providers (mock/fake) replay scripted
/// turns — a built-in silent retry would consume turns tests rely on — so
/// they only honor an explicit `llm_retries` opt-in.
fn empty_completion_retry_budget(retry_config: &LlmRetryConfig, provider: &str) -> usize {
    if crate::llm::providers::MockProvider::should_intercept(provider)
        || crate::llm::fake::FakeLlmProvider::should_intercept(provider)
    {
        retry_config.retries
    } else {
        retry_config.retries.max(EMPTY_COMPLETION_BUILTIN_RETRIES)
    }
}

pub(crate) struct LlmRetryConfig {
    /// Maximum number of retries for transient errors (429, 5xx, connection).
    pub retries: usize,
    /// Base backoff in milliseconds between retries.
    pub backoff_ms: u64,
}

impl Default for LlmRetryConfig {
    fn default() -> Self {
        Self {
            retries: DEFAULT_LLM_CALL_RETRIES,
            backoff_ms: DEFAULT_LLM_CALL_BACKOFF_MS,
        }
    }
}

fn llm_retry_backoff_ms(
    error: &VmError,
    retry_config: &LlmRetryConfig,
    attempt: usize,
    provider: &str,
) -> u64 {
    if crate::llm::providers::MockProvider::should_intercept(provider) {
        return 0;
    }
    extract_retry_after_ms(error).unwrap_or_else(|| base_retry_backoff_ms(retry_config, attempt))
}

/// Exponential backoff base shared by the error-retry and empty-completion
/// retry paths (no `retry-after` hint available on the latter).
fn base_retry_backoff_ms(retry_config: &LlmRetryConfig, attempt: usize) -> u64 {
    retry_config.backoff_ms.saturating_mul(1 << attempt.min(4))
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
    retry_config: &LlmRetryConfig,
    iteration: Option<usize>,
    user_visible: bool,
    offthread: bool,
    streaming_detector: Option<StreamingDetectorContext>,
) -> Result<super::api::LlmResult, VmError> {
    let _in_flight_guard = super::call::InFlightLlmCallGuard::enter(opts);
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
        let rate_limit_permit = super::rate_limit::acquire_permit_for_llm_call(opts).await?;

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
        let llm_result = if let Some(b) = bridge {
            let delta_tx = spawn_progress_forwarder(
                b,
                call_id.clone(),
                user_visible,
                detector_ctx,
                first_token,
            );
            if offthread {
                vm_call_llm_full_streaming_offthread(opts, delta_tx).await
            } else {
                vm_call_llm_full_streaming(opts, delta_tx).await
            }
        } else if offthread {
            let delta_tx = match detector_ctx {
                Some(ctx) => spawn_detector_only_forwarder(ctx, first_token),
                None => {
                    let (tx, _rx) = tokio::sync::mpsc::unbounded_channel::<String>();
                    tx
                }
            };
            vm_call_llm_full_streaming_offthread(opts, delta_tx).await
        } else {
            super::api::vm_call_llm_full(opts).await
        };
        drop(rate_limit_permit);
        let duration_ms = start.elapsed().as_millis() as u64;

        match llm_result {
            Ok(result) => {
                // Zero-token empty "success" (no content, no thinking, no tool
                // calls): a provider hiccup, not an answer. Retry within the
                // empty-completion budget; once exhausted, fall through and
                // return the result unchanged so callers see today's shape
                // rather than a novel error.
                if is_zero_token_empty_completion(&result)
                    && attempt < empty_completion_retry_budget(retry_config, &opts.provider)
                {
                    annotate_current_span(&[
                        ("status", serde_json::json!("retrying")),
                        ("retry_reason", serde_json::json!("empty_completion")),
                        ("attempt", serde_json::json!(attempt)),
                    ]);
                    let detail = format!(
                        "provider {} model {} returned a zero-token empty completion (no content, thinking, or tool calls)",
                        opts.provider, opts.model
                    );
                    append_llm_observability_entry(
                        "empty_completion_retry",
                        serde_json::Map::from_iter([
                            (
                                "iteration".to_string(),
                                serde_json::json!(iteration.unwrap_or(0)),
                            ),
                            ("attempt".to_string(), serde_json::json!(attempt + 1)),
                            ("provider".to_string(), serde_json::json!(opts.provider)),
                            ("model".to_string(), serde_json::json!(opts.model)),
                            ("error".to_string(), serde_json::json!(detail.clone())),
                        ]),
                    );
                    super::trace::emit_agent_event(
                        super::trace::AgentTraceEvent::EmptyCompletionRetry {
                            iteration: iteration.unwrap_or(0),
                            attempt: attempt + 1,
                            error: detail.clone(),
                        },
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
                    let backoff =
                        if crate::llm::providers::MockProvider::should_intercept(&opts.provider) {
                            0
                        } else {
                            base_retry_backoff_ms(retry_config, attempt)
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
                annotate_current_span(&[
                    ("status", serde_json::json!("ok")),
                    ("input_tokens", serde_json::json!(result.input_tokens)),
                    ("output_tokens", serde_json::json!(result.output_tokens)),
                    (
                        "cost_usd",
                        serde_json::json!(crate::llm::cost::calculate_cost_for_provider(
                            &result.provider,
                            &result.model,
                            result.input_tokens,
                            result.output_tokens,
                        )),
                    ),
                ]);
                dump_llm_response(
                    iteration.unwrap_or(0),
                    &call_id,
                    &result,
                    duration_ms,
                    opts.applied_structural_experiment.as_ref(),
                    opts.tools.as_ref(),
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
                        super::cost::calculate_cost_for_provider(
                            &result.provider,
                            &result.model,
                            result.input_tokens,
                            result.output_tokens,
                        ),
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
                return Ok(result);
            }
            Err(error) => {
                let category = crate::value::error_to_category(&error);
                let message = error.to_string();
                let classified = super::api::classify_llm_error(category.clone(), &message);
                if classified.reason == super::api::LlmErrorReason::RateLimit {
                    if let Some(retry_after_ms) = extract_retry_after_ms(&error) {
                        super::rate_limit::observe_retry_after_for_llm_call(opts, retry_after_ms);
                    }
                }
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
                    ("error", serde_json::json!(message.clone())),
                    ("retryable", serde_json::json!(retryable)),
                    ("attempt", serde_json::json!(attempt)),
                ]);
                append_llm_observability_entry(
                    "provider_call_error",
                    serde_json::Map::from_iter([
                        (
                            "iteration".to_string(),
                            serde_json::json!(iteration.unwrap_or(0)),
                        ),
                        ("call_id".to_string(), serde_json::json!(call_id.clone())),
                        ("attempt".to_string(), serde_json::json!(attempt)),
                        ("status".to_string(), serde_json::json!(status)),
                        ("provider".to_string(), serde_json::json!(opts.provider)),
                        ("model".to_string(), serde_json::json!(opts.model)),
                        ("category".to_string(), serde_json::json!(category.as_str())),
                        (
                            "kind".to_string(),
                            serde_json::json!(classified.kind.as_str()),
                        ),
                        (
                            "reason".to_string(),
                            serde_json::json!(classified.reason.as_str()),
                        ),
                        ("message".to_string(), serde_json::json!(message.clone())),
                        ("retryable".to_string(), serde_json::json!(retryable)),
                    ]),
                );
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
                    if let Some(metrics) = crate::active_metrics_registry() {
                        metrics.record_llm_call(&opts.provider, &opts.model, status, 0.0);
                    }
                    return Err(error);
                }
                if is_empty_completion_retry_error(&error) {
                    append_llm_observability_entry(
                        "empty_completion_retry",
                        serde_json::Map::from_iter([
                            (
                                "iteration".to_string(),
                                serde_json::json!(iteration.unwrap_or(0)),
                            ),
                            ("attempt".to_string(), serde_json::json!(attempt + 1)),
                            ("provider".to_string(), serde_json::json!(opts.provider)),
                            ("model".to_string(), serde_json::json!(opts.model)),
                            ("error".to_string(), serde_json::json!(error.to_string())),
                        ]),
                    );
                    super::trace::emit_agent_event(
                        super::trace::AgentTraceEvent::EmptyCompletionRetry {
                            iteration: iteration.unwrap_or(0),
                            attempt: attempt + 1,
                            error: error.to_string(),
                        },
                    );
                }
                attempt += 1;
                let backoff = llm_retry_backoff_ms(&error, retry_config, attempt, &opts.provider);
                crate::events::log_warn(
                    "llm",
                    &format!(
                        "LLM call failed ({}), retrying in {}ms (attempt {}/{})",
                        error, backoff, attempt, retry_config.retries
                    ),
                );
                if backoff > 0 {
                    crate::clock_mock::sleep(std::time::Duration::from_millis(backoff)).await;
                }
            }
        }
    }
}

#[cfg(test)]
mod retry_tests {
    use super::*;
    use crate::value::{ErrorCategory, VmError, VmValue};

    fn thrown(s: &str) -> VmError {
        VmError::Thrown(VmValue::String(std::sync::Arc::from(s)))
    }

    fn categorized(msg: &str, category: ErrorCategory) -> VmError {
        VmError::CategorizedError {
            message: msg.to_string(),
            category,
        }
    }

    #[test]
    fn template_render_event_round_trips_through_jsonl() {
        use crate::stdlib::template::{
            render_template_to_string, LlmRenderContext, LlmRenderContextGuard,
        };
        let dir = tempfile::tempdir().expect("tempdir");
        push_llm_transcript_dir(dir.path().to_str().expect("utf8"));
        {
            let _ctx = LlmRenderContextGuard::enter(LlmRenderContext::resolve(
                "anthropic",
                "claude-opus-4-7",
            ));
            let rendered = render_template_to_string(
                "{{ if llm.capabilities.native_tools }}native{{ else }}text{{ end }}\
                 {{ section \"task\" }}b{{ endsection }}",
                None,
                None,
                None,
            )
            .expect("render");
            assert!(rendered.contains("native"));
            assert!(rendered.contains("<task>"));
        }
        pop_llm_transcript_dir();
        let transcript = std::fs::read_to_string(dir.path().join("llm_transcript.jsonl"))
            .expect("read transcript");
        let line = transcript
            .lines()
            .find(|line| line.contains("\"template.render\""))
            .expect("template.render event present");
        let event: serde_json::Value = serde_json::from_str(line).expect("parse event");
        assert_eq!(event["type"], "template.render");
        assert_eq!(event["llm"]["provider"], "anthropic");
        assert_eq!(event["llm"]["family"], "anthropic-claude");
        assert_eq!(event["llm"]["capabilities"]["native_tools"], true);
        let branches = event["branches"].as_array().expect("branches array");
        let if_branch = branches
            .iter()
            .find(|b| b["kind"] == "if")
            .expect("if branch present");
        assert_eq!(if_branch["branch_id"], "if");
        let section_branch = branches
            .iter()
            .find(|b| b["kind"] == "section")
            .expect("section branch present");
        assert_eq!(section_branch["branch_id"], "xml");
        assert_eq!(section_branch["branch_label"], "task");
    }

    // Fix B regression: for text-format local models (llamacpp/qwen3.6) the
    // tool calls live only inline as `<tool_call>...</tool_call>` in the
    // assistant content — the provider-native `result.tool_calls` array is
    // EMPTY. The `provider_call_response` observability record used to carry
    // only that native array, so the JSONL transcript was not self-describing
    // for text-format runs. The record now also carries a `parsed_tool_calls`
    // sidecar holding the merged (native OR text-parsed) view.
    //
    // Critically this is OBSERVABILITY ONLY: the request-construction / history
    // path keys off `native_tool_calls` (the native-only list from
    // `vm_build_llm_result`, consumed by
    // `agent_session_host::assistant_message_from_llm_result`), which the test
    // also asserts stays empty for a text-format result — so the model's
    // next-turn payload is unchanged.
    #[test]
    fn response_record_exposes_text_parsed_calls_without_touching_history() {
        use super::super::api::{vm_build_llm_result, LlmResult, ProviderTelemetry};
        use crate::value::VmValue;

        // Minimal tool registry so the tagged parser resolves the `run` name
        // (mirrors the `tools` registry the request used).
        fn run_tool_registry() -> VmValue {
            let dict = |pairs: &[(&str, VmValue)]| -> VmValue {
                VmValue::Dict(std::sync::Arc::new(
                    pairs
                        .iter()
                        .map(|(k, v)| ((*k).to_string(), v.clone()))
                        .collect(),
                ))
            };
            let s = |v: &str| VmValue::String(std::sync::Arc::from(v));
            let run_tool = dict(&[
                ("name", s("run")),
                ("description", s("Run a shell command.")),
                (
                    "parameters",
                    dict(&[(
                        "command",
                        dict(&[("type", s("string")), ("description", s("Shell command."))]),
                    )]),
                ),
            ]);
            dict(&[("tools", VmValue::List(std::sync::Arc::new(vec![run_tool])))])
        }

        // A text-format completion: native tool_calls EMPTY, the call lives
        // inline as a canonical `<tool_call>` block (already canonicalized from
        // any `[[CALL]]` wire form by the time the result reaches here).
        let text = "<tool_call>\nrun({ command: \"ls\" })\n</tool_call>";
        let result = LlmResult {
            served_fast: false,
            text: text.to_string(),
            tool_calls: Vec::new(),
            input_tokens: 12,
            output_tokens: 5,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cache_supported: true,
            model: "qwen3.6".to_string(),
            provider: "llamacpp".to_string(),
            thinking: None,
            thinking_summary: None,
            stop_reason: Some("stop".to_string()),
            blocks: Vec::new(),
            logprobs: Vec::new(),
            telemetry: ProviderTelemetry::default(),
        };
        let tools = run_tool_registry();

        // 1. Observability path: the response record now exposes the parsed
        //    call via the new sidecar, while native `tool_calls` stays empty.
        let dir = tempfile::tempdir().expect("tempdir");
        push_llm_transcript_dir(dir.path().to_str().expect("utf8"));
        dump_llm_response(0, "call-textfmt", &result, 42, None, Some(&tools));
        pop_llm_transcript_dir();

        let transcript = std::fs::read_to_string(dir.path().join("llm_transcript.jsonl"))
            .expect("read transcript");
        let line = transcript
            .lines()
            .find(|line| line.contains("\"provider_call_response\""))
            .expect("provider_call_response event present");
        let event: serde_json::Value = serde_json::from_str(line).expect("parse event");

        let native = event["tool_calls"].as_array().expect("tool_calls array");
        assert!(
            native.is_empty(),
            "native tool_calls must remain empty for a text-format result, got: {native:?}"
        );
        let parsed = event["parsed_tool_calls"]
            .as_array()
            .expect("parsed_tool_calls array");
        assert_eq!(
            parsed.len(),
            1,
            "the text-parsed call must surface in the sidecar, got: {parsed:?}"
        );
        assert_eq!(parsed[0]["name"], "run");

        // 2. Request-construction / history path is UNCHANGED: the value that
        //    feeds the assistant history envelope is `native_tool_calls`, which
        //    stays empty (native-only) for a text-format result. The merged
        //    `tool_calls` carries the call for unified-view callers, but the
        //    history-feeding native list does not.
        let vm_result = vm_build_llm_result(&result, None, None, Some(&tools));
        let VmValue::Dict(ref dict) = vm_result else {
            panic!("vm_build_llm_result must return a dict");
        };
        let native_history = dict
            .get("native_tool_calls")
            .expect("native_tool_calls present");
        match native_history {
            VmValue::List(items) => assert!(
                items.is_empty(),
                "native_tool_calls (history-feeding list) must stay empty for a \
                 text-format result, got: {items:?}"
            ),
            other => panic!("native_tool_calls must be a list, got {other:?}"),
        }
        // The merged `tool_calls` (unified view) does carry the call — proving
        // the sidecar mirrors the same merge the result builder already does,
        // not a divergent computation.
        let merged_history = dict.get("tool_calls").expect("tool_calls present");
        match merged_history {
            VmValue::List(items) => assert_eq!(
                items.len(),
                1,
                "merged tool_calls (unified view) should carry the text-parsed call"
            ),
            other => panic!("tool_calls must be a list, got {other:?}"),
        }
    }

    #[test]
    fn transcript_dir_option_overrides_env_until_popped() {
        push_llm_transcript_dir("/tmp/harn-transcript-a");
        assert_eq!(
            current_transcript_dir().as_deref(),
            Some("/tmp/harn-transcript-a")
        );
        push_llm_transcript_dir("/tmp/harn-transcript-b");
        assert_eq!(
            current_transcript_dir().as_deref(),
            Some("/tmp/harn-transcript-b")
        );
        pop_llm_transcript_dir();
        assert_eq!(
            current_transcript_dir().as_deref(),
            Some("/tmp/harn-transcript-a")
        );
        pop_llm_transcript_dir();
    }

    // Regression for #2660. `append_llm_transcript_event_log` used to
    // `handle.spawn` the event-log append as a detached task. The agent loop
    // and the test runner drive their tokio runtime with
    // `LocalSet::run_until`, which stops polling the moment the driving future
    // resolves — so those detached appends were never run to completion. Each
    // stranded task pinned a transcript-sized payload plus an
    // `Arc<AnyEventLog>` clone for the lifetime of the runtime, leaking ~one
    // transcript per test across a `harn test --parallel` worker until CI
    // OOM'd. The append must therefore complete synchronously: the entry has
    // to be readable from the log the instant the producing future resolves,
    // without any further runtime polling.
    #[test]
    fn transcript_event_is_appended_synchronously_under_run_until() {
        use crate::event_log::{
            install_memory_for_current_thread, reset_active_event_log, EventLog, Topic,
        };

        reset_active_event_log();
        let log = install_memory_for_current_thread(128);
        let topic = Topic::new("agent.transcript.llm").expect("static topic");

        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime");
        let local = tokio::task::LocalSet::new();
        runtime.block_on(local.run_until(async {
            // Emit a transcript entry from inside the run_until future, exactly
            // as the agent loop does. With the old detached-spawn path the
            // append task is left scheduled-but-unpolled when this future
            // resolves; with the synchronous path it has already landed.
            append_llm_transcript_entry(&serde_json::json!({
                "type": "provider_call_request",
                "iteration": 0,
                "marker": "regression-2660",
            }));
        }));

        // The driving future has resolved and we are no longer polling the
        // runtime. The event must already be in the log — proving the append
        // ran synchronously rather than on a stranded detached task.
        let latest = futures::executor::block_on(log.latest(&topic))
            .expect("latest query")
            .expect("transcript event must be present immediately after run_until resolves");
        assert_eq!(latest, 1, "exactly one transcript event should be recorded");

        reset_active_event_log();
    }

    #[test]
    fn mock_provider_retry_backoff_is_zero() {
        let config = LlmRetryConfig {
            retries: 1,
            backoff_ms: 2000,
        };
        assert_eq!(
            llm_retry_backoff_ms(&thrown("HTTP 503"), &config, 1, "mock"),
            0
        );
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
    fn llm_error_kind_dict_gates_retry() {
        let transient = VmError::Thrown(VmValue::Dict(std::sync::Arc::new(
            std::collections::BTreeMap::from([
                (
                    "kind".to_string(),
                    VmValue::String(std::sync::Arc::from("transient")),
                ),
                (
                    "reason".to_string(),
                    VmValue::String(std::sync::Arc::from("network_error")),
                ),
            ]),
        )));
        assert!(is_retryable_llm_error(&transient));

        let terminal = VmError::Thrown(VmValue::Dict(std::sync::Arc::new(
            std::collections::BTreeMap::from([
                (
                    "kind".to_string(),
                    VmValue::String(std::sync::Arc::from("terminal")),
                ),
                (
                    "reason".to_string(),
                    VmValue::String(std::sync::Arc::from("context_overflow")),
                ),
            ]),
        )));
        assert!(!is_retryable_llm_error(&terminal));
    }

    #[test]
    fn context_overflow_message_is_not_retryable() {
        assert!(!is_retryable_llm_error(&thrown(
            "local HTTP 400 Bad Request [context_overflow]: prompt is too long"
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
    fn retry_after_seconds_with_provider_message_punctuation() {
        let msg = "cerebras HTTP 429 Too Many Requests [rate_limited]: Tokens per minute limit exceeded (type: too_many_tokens_error, code: token_quota_exceeded) (retry-after: 60))";
        assert_eq!(parse_retry_after(msg), Some(60_000));
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

#[cfg(test)]
mod empty_completion_retry_tests {
    //! Zero-token empty-completion retry coverage. A provider stall can end
    //! with an empty HTTP 200 (observed live on OpenRouter: 133s hang,
    //! `output_tokens=0`), which is not an error at the wire level —
    //! `observed_llm_call` must treat it as a transient hiccup and retry,
    //! and must return the empty result unchanged once the budget is spent.
    //! Driven through `FakeLlmProvider` (an empty scripted stream produces
    //! exactly the zero-token empty shape) so the full retry loop runs
    //! without network I/O.

    use super::*;
    use crate::llm::fake::{
        install_fake_llm_script, FakeLlmEvent, FakeLlmScript, FakeLlmTurn, FakeStopReason,
    };
    use crate::llm::trace::{peek_agent_trace, reset_agent_trace_state, AgentTraceEvent};

    fn fake_opts() -> crate::llm::api::LlmCallOptions {
        let mut opts = crate::llm::api::options::base_opts("fake");
        opts.model = "fake-stream".to_string();
        opts.native_tools = None;
        opts.tools = None;
        opts.tool_choice = None;
        opts.provider_overrides = None;
        opts
    }

    fn current_thread_runtime() -> tokio::runtime::Runtime {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("runtime")
    }

    fn empty_turn() -> FakeLlmTurn {
        FakeLlmTurn::stream(vec![FakeLlmEvent::Done(FakeStopReason::EndTurn)])
    }

    fn retry_config(retries: usize) -> LlmRetryConfig {
        LlmRetryConfig {
            retries,
            backoff_ms: 0,
        }
    }

    #[test]
    fn empty_completion_retries_then_succeeds_on_second_attempt() {
        current_thread_runtime().block_on(async {
            reset_agent_trace_state();
            let _guard = install_fake_llm_script(FakeLlmScript::new().push(empty_turn()).push(
                FakeLlmTurn::stream(vec![
                    FakeLlmEvent::Token("recovered".into()),
                    FakeLlmEvent::Done(FakeStopReason::EndTurn),
                ]),
            ));
            let result = observed_llm_call(
                &fake_opts(),
                None,
                None,
                &retry_config(1),
                None,
                false,
                false,
                None,
            )
            .await
            .expect("empty completion retry should recover");
            assert_eq!(result.text, "recovered");

            let retries: Vec<usize> = peek_agent_trace()
                .iter()
                .filter_map(|event| match event {
                    AgentTraceEvent::EmptyCompletionRetry { attempt, .. } => Some(*attempt),
                    _ => None,
                })
                .collect();
            assert_eq!(
                retries,
                vec![1],
                "expected exactly one EmptyCompletionRetry trace event"
            );
            reset_agent_trace_state();
            // _guard drop asserts both scripted turns were consumed.
        });
    }

    #[test]
    fn empty_completion_returns_result_unchanged_after_budget_exhausted() {
        current_thread_runtime().block_on(async {
            reset_agent_trace_state();
            let _guard =
                install_fake_llm_script(FakeLlmScript::new().push(empty_turn()).push(empty_turn()));
            let result = observed_llm_call(
                &fake_opts(),
                None,
                None,
                &retry_config(1),
                None,
                false,
                false,
                None,
            )
            .await
            .expect("exhausted empty-completion retries must return Ok, not a new error");
            assert!(result.text.is_empty());
            assert!(result.tool_calls.is_empty());
            assert_eq!(result.output_tokens, 0);
            reset_agent_trace_state();
        });
    }

    #[test]
    fn fake_provider_without_retry_budget_does_not_silently_retry() {
        // Mock/fake providers replay scripted turns, so the built-in
        // empty-completion floor must not apply to them — only an explicit
        // budget. One scripted turn, zero retries: the guard would panic on
        // drop if a hidden retry consumed a second turn.
        current_thread_runtime().block_on(async {
            let _guard = install_fake_llm_script(FakeLlmScript::new().push(empty_turn()));
            let result = observed_llm_call(
                &fake_opts(),
                None,
                None,
                &retry_config(0),
                None,
                false,
                false,
                None,
            )
            .await
            .expect("empty completion without budget returns as today");
            assert!(result.text.is_empty());
        });
    }

    #[test]
    fn builtin_empty_retry_budget_floors_real_providers_only() {
        let zero = retry_config(0);
        assert_eq!(empty_completion_retry_budget(&zero, "openrouter"), 1);
        assert_eq!(empty_completion_retry_budget(&zero, "fake"), 0);
        assert_eq!(empty_completion_retry_budget(&zero, "mock"), 0);
        let three = retry_config(3);
        assert_eq!(empty_completion_retry_budget(&three, "openrouter"), 3);
        assert_eq!(empty_completion_retry_budget(&three, "fake"), 3);
    }

    fn empty_result() -> crate::llm::api::LlmResult {
        crate::llm::api::LlmResult {
            text: String::new(),
            tool_calls: Vec::new(),
            input_tokens: 0,
            output_tokens: 0,
            cache_read_tokens: 0,
            cache_write_tokens: 0,
            cache_supported: true,
            model: "test-model".to_string(),
            provider: "openrouter".to_string(),
            thinking: None,
            thinking_summary: None,
            stop_reason: Some("stop".to_string()),
            served_fast: false,
            blocks: Vec::new(),
            logprobs: Vec::new(),
            telemetry: crate::llm::api::ProviderTelemetry::default(),
        }
    }

    #[test]
    fn zero_token_empty_completion_predicate_edges() {
        assert!(is_zero_token_empty_completion(&empty_result()));

        // Token-cap truncation is deterministic — not a retryable hiccup.
        let mut truncated = empty_result();
        truncated.stop_reason = Some("length".to_string());
        assert!(!is_zero_token_empty_completion(&truncated));
        let mut truncated_upper = empty_result();
        truncated_upper.stop_reason = Some("MAX_TOKENS".to_string());
        assert!(!is_zero_token_empty_completion(&truncated_upper));

        // Any delivered payload disqualifies.
        let mut with_text = empty_result();
        with_text.text = "hi".to_string();
        assert!(!is_zero_token_empty_completion(&with_text));
        let mut with_tokens = empty_result();
        with_tokens.output_tokens = 3;
        assert!(!is_zero_token_empty_completion(&with_tokens));
        let mut with_thinking = empty_result();
        with_thinking.thinking = Some("hmm".to_string());
        assert!(!is_zero_token_empty_completion(&with_thinking));
        let mut with_tool_call = empty_result();
        with_tool_call.tool_calls = vec![serde_json::json!({"id": "t1", "name": "look"})];
        assert!(!is_zero_token_empty_completion(&with_tool_call));
        let mut with_tool_search = empty_result();
        with_tool_search.blocks = vec![serde_json::json!({"type": "tool_search_query"})];
        assert!(!is_zero_token_empty_completion(&with_tool_search));
    }
}

#[cfg(test)]
mod streaming_detector_tests {
    //! Verify the streaming candidate detector glue (harn#692). The
    //! unit tests in `crate::llm::tools::parse::streaming` already cover
    //! the detector's state machine; these tests cover the loop body
    //! that pumps deltas through the detector and dispatches each event
    //! to a sink. Uses `run_detector_loop_with_sink` with a captured
    //! buffer so the test doesn't depend on the global session sink
    //! registry — other tests in this binary mutate the registry via
    //! `reset_all_sinks` and can race a per-session install otherwise.
    use std::cell::RefCell;
    use std::rc::Rc;

    use crate::agent_events::{AgentEvent, ToolCallStatus};

    use super::{run_detector_loop_with_sink, StreamingDetectorContext};

    /// Pipe `chunks` through `run_detector_loop_with_sink`, await its
    /// completion, and return the captured events in arrival order.
    async fn drive(session_id: &str, known: &[&str], chunks: &[&str]) -> Vec<AgentEvent> {
        let captured: Rc<RefCell<Vec<AgentEvent>>> = Rc::new(RefCell::new(Vec::new()));
        let sink_buf = captured.clone();
        let known_tools = known.iter().map(|s| (*s).to_string()).collect();
        let (tx, rx) = tokio::sync::mpsc::unbounded_channel::<String>();
        for chunk in chunks {
            tx.send((*chunk).to_string()).expect("send delta");
        }
        drop(tx);
        run_detector_loop_with_sink(
            StreamingDetectorContext {
                session_id: session_id.to_string(),
                known_tools,
            },
            rx,
            move |event| sink_buf.borrow_mut().push(event.clone()),
        )
        .await;
        let events = captured.borrow().clone();
        events
    }

    #[tokio::test(flavor = "current_thread")]
    async fn detector_loop_emits_start_and_promoted_through_sink() {
        let events = drive(
            "session-stream-promote",
            &["read"],
            &["read({ path: \"a.md\" })"],
        )
        .await;
        assert_eq!(
            events.len(),
            2,
            "expected start + promoted, got: {events:#?}"
        );
        match &events[0] {
            AgentEvent::ToolCall {
                parsing,
                tool_name,
                status,
                ..
            } => {
                assert_eq!(*parsing, Some(true));
                assert_eq!(tool_name, "read");
                assert_eq!(*status, ToolCallStatus::Pending);
            }
            other => panic!("expected ToolCall, got {other:?}"),
        }
        match &events[1] {
            AgentEvent::ToolCallUpdate {
                parsing,
                status,
                error_category,
                ..
            } => {
                assert_eq!(*parsing, Some(false));
                assert_eq!(*status, ToolCallStatus::Pending);
                assert!(error_category.is_none());
            }
            other => panic!("expected ToolCallUpdate, got {other:?}"),
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn detector_loop_finalizes_unclosed_tagged_block_as_aborted() {
        let events = drive(
            "session-stream-abort",
            &["run"],
            &["<tool_call>\nrun({ command: \"ls\""],
        )
        .await;
        assert_eq!(events.len(), 2, "events={events:#?}");
        match &events[1] {
            AgentEvent::ToolCallUpdate {
                status,
                error_category,
                parsing,
                ..
            } => {
                assert_eq!(*status, ToolCallStatus::Failed);
                assert_eq!(
                    *error_category,
                    Some(crate::agent_events::ToolCallErrorCategory::ParseAborted)
                );
                assert_eq!(*parsing, Some(false));
            }
            other => panic!("expected ToolCallUpdate, got {other:?}"),
        }
    }
}
