use std::collections::HashMap;
use std::rc::Rc;

use serde::Deserialize;

use crate::value::{ErrorCategory, VmClosure, VmError, VmValue};
use crate::vm::Vm;

use super::api::{vm_call_llm_full_streaming, vm_call_llm_full_streaming_offthread, DeltaSender};
use super::helpers::{
    extract_llm_options, opt_bool, opt_int, opt_str, transcript_event, transcript_to_vm_with_events,
};
use super::tools::{
    build_assistant_response_message, build_assistant_tool_message,
    build_tool_calling_contract_prompt, build_tool_result_message, collect_tool_schemas,
    handle_tool_locally, normalize_tool_args, parse_text_tool_calls_with_tools, validate_tool_args,
};
use super::trace::{trace_llm_call, LlmTraceEntry};

// ---------------------------------------------------------------------------
// Tool loop detection
// ---------------------------------------------------------------------------
// Tracks repeated tool calls with identical arguments and results to detect
// stuck loops.  When the model calls the same tool with the same args and
// gets the same result N times in a row, it's stuck.  We intervene with
// increasingly forceful messages to redirect it.

/// Hash a serde_json::Value deterministically for dedup purposes.
fn stable_hash(val: &serde_json::Value) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    let canonical = serde_json::to_string(val).unwrap_or_default();
    canonical.hash(&mut hasher);
    hasher.finish()
}

fn stable_hash_str(s: &str) -> u64 {
    use std::hash::{Hash, Hasher};
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    s.hash(&mut hasher);
    hasher.finish()
}

struct ToolCallTracker {
    /// (tool_name, args_hash) -> (consecutive_count, last_result_hash)
    entries: HashMap<(String, u64), (usize, u64)>,
    /// Thresholds for intervention tiers
    warn_threshold: usize,
    block_threshold: usize,
    skip_threshold: usize,
}

/// What the tracker recommends for a given tool call.
enum LoopIntervention {
    /// No loop detected — proceed normally.
    Proceed,
    /// Warn: append a redirection hint after the tool result.
    Warn { count: usize },
    /// Block: replace the result with a hard redirect, still execute to track.
    Block { count: usize },
    /// Skip: do not execute, inject a skip message.
    Skip { count: usize },
}

impl ToolCallTracker {
    fn new(warn: usize, block: usize, skip: usize) -> Self {
        Self {
            entries: HashMap::new(),
            warn_threshold: warn,
            block_threshold: block,
            skip_threshold: skip,
        }
    }

    /// Check if a tool call is a repeated loop.  Call BEFORE execution.
    fn check(&self, tool_name: &str, args_hash: u64) -> LoopIntervention {
        let key = (tool_name.to_string(), args_hash);
        if let Some(&(count, _result_hash)) = self.entries.get(&key) {
            // count is how many times we've ALREADY seen this exact call
            // with an identical result.  The thresholds apply to the
            // upcoming call number (count + 1).
            let next = count + 1;
            if next >= self.skip_threshold {
                return LoopIntervention::Skip { count: next };
            }
            if next >= self.block_threshold {
                return LoopIntervention::Block { count: next };
            }
            if next >= self.warn_threshold {
                return LoopIntervention::Warn { count: next };
            }
        }
        LoopIntervention::Proceed
    }

    /// Record a tool call result.  Returns the intervention to apply
    /// based on whether the result is new or identical to the last one.
    fn record(&mut self, tool_name: &str, args_hash: u64, result_hash: u64) -> LoopIntervention {
        let key = (tool_name.to_string(), args_hash);
        if let Some(entry) = self.entries.get_mut(&key) {
            if entry.1 == result_hash {
                // Same args, same result — stuck loop
                entry.0 += 1;
                let count = entry.0;
                if count >= self.skip_threshold {
                    return LoopIntervention::Skip { count };
                }
                if count >= self.block_threshold {
                    return LoopIntervention::Block { count };
                }
                if count >= self.warn_threshold {
                    return LoopIntervention::Warn { count };
                }
            } else {
                // Same args, different result — progress was made, reset
                entry.0 = 1;
                entry.1 = result_hash;
            }
        } else {
            self.entries.insert(key, (1, result_hash));
        }
        LoopIntervention::Proceed
    }
}

fn loop_intervention_message(
    tool_name: &str,
    result_text: &str,
    intervention: &LoopIntervention,
) -> Option<String> {
    match intervention {
        LoopIntervention::Proceed => None,
        LoopIntervention::Warn { count, .. } => {
            let first_line = result_text.lines().next().unwrap_or("(empty)");
            Some(format!(
                "\n[LOOP DETECTED] This exact {tool_name}() call has produced the same result {count} times. \
                 The result says: \"{first_line}\". \
                 Try a DIFFERENT tool or DIFFERENT parameters."
            ))
        }
        LoopIntervention::Block { count } => Some(format!(
            "BLOCKED: {tool_name}() has failed {count} times identically. \
                 You MUST use a different approach. \
                 Read the Available tools section and pick a different strategy."
        )),
        LoopIntervention::Skip { count } => Some(format!(
            "BLOCKED: {tool_name}() was NOT executed (repeated {count} times identically). \
                 This call will not be executed again with these arguments. \
                 You MUST change your approach NOW."
        )),
    }
}

fn next_call_id() -> String {
    uuid::Uuid::now_v7().to_string()
}

/// Tools that have no side effects and can be safely dispatched concurrently
/// within a single assistant turn. These cover exploration/research calls
/// that weaker coding models batch heavily during their ground phase; the
/// data we collected across 201 eval runs shows 62.7% of all tool
/// invocations fall into this set, so parallelizing it is the single
/// largest latency lever we have on the tool-dispatch side.
fn is_read_only_tool(name: &str) -> bool {
    matches!(
        name,
        "read"
            | "read_file"
            | "lookup"
            | "search"
            | "outline"
            | "list_directory"
            | "list_templates"
            | "get_template"
            | "web_search"
            | "web_fetch"
    )
}

/// Dispatch a single tool invocation to its execution backend (local
/// handler, Harn-defined closure, or host bridge), with the same retry
/// semantics the sequential loop has historically used. Extracted so both
/// the sequential loop and the parallel pre-fetch pass can share a single
/// source of truth for tool execution.
async fn dispatch_tool_execution(
    tool_name: &str,
    tool_args: &serde_json::Value,
    tools_val: Option<&VmValue>,
    bridge: Option<&Rc<crate::bridge::HostBridge>>,
    tool_retries: usize,
    tool_backoff_ms: u64,
) -> Result<serde_json::Value, VmError> {
    let mut attempt = 0usize;
    loop {
        let result = if let Some(local_result) = handle_tool_locally(tool_name, tool_args) {
            Ok(serde_json::Value::String(local_result))
        } else if let Some(handler) = find_tool_handler(tools_val, tool_name) {
            // Harn-defined tool handler — invoke via a freshly-cloned child
            // VM. The clone is lightweight (Arc/Rc-shared state) and gives
            // this caller its own execution context so multiple concurrent
            // handler invocations can run in parallel without fighting
            // over a shared stack slot.
            let Some(mut vm) = crate::vm::clone_async_builtin_child_vm() else {
                return Err(VmError::CategorizedError {
                    message: format!(
                        "tool '{tool_name}' is Harn-owned but no child VM context was available"
                    ),
                    category: ErrorCategory::ToolRejected,
                });
            };
            let args_vm = crate::stdlib::json_to_vm_value(tool_args);
            match vm.call_closure_pub(&handler, &[args_vm], &[]).await {
                Ok(val) => Ok(serde_json::Value::String(val.display())),
                Err(e) => Ok(serde_json::Value::String(format!("Error: {e}"))),
            }
        } else if let Some(bridge) = bridge {
            bridge
                .call(
                    "builtin_call",
                    serde_json::json!({
                        "name": tool_name,
                        "args": [tool_args],
                    }),
                )
                .await
        } else {
            Err(VmError::CategorizedError {
                message: format!(
                    "Tool '{}' is not available in the current environment. \
                     Use only the tools listed in the tool-calling contract.",
                    tool_name
                ),
                category: ErrorCategory::ToolRejected,
            })
        };
        match &result {
            Ok(_) => break result,
            Err(VmError::CategorizedError {
                category: ErrorCategory::ToolRejected,
                ..
            }) => break result,
            Err(_) if attempt < tool_retries => {
                attempt += 1;
                let delay = tool_backoff_ms * (1u64 << attempt.min(5));
                tokio::time::sleep(tokio::time::Duration::from_millis(delay)).await;
            }
            Err(_) => break result,
        }
    }
}

/// Look up the Harn-defined handler closure for a tool, if any.
/// Returns `None` if the tool has no handler (handler is Nil) or isn't found.
fn find_tool_handler(tools_val: Option<&VmValue>, tool_name: &str) -> Option<Rc<VmClosure>> {
    let dict = tools_val?.as_dict()?;
    let tools_list = match dict.get("tools") {
        Some(VmValue::List(l)) => l,
        _ => return None,
    };
    for tool in tools_list.iter() {
        let entry: &std::collections::BTreeMap<String, VmValue> = match tool {
            VmValue::Dict(d) => d,
            _ => continue,
        };
        let name = match entry.get("name") {
            Some(v) => v.display(),
            None => continue,
        };
        if name == tool_name {
            if let Some(VmValue::Closure(c)) = entry.get("handler") {
                return Some(Rc::clone(c));
            }
            return None;
        }
    }
    None
}

thread_local! {
    static CURRENT_HOST_BRIDGE: std::cell::RefCell<Option<Rc<crate::bridge::HostBridge>>> = const { std::cell::RefCell::new(None) };
}

#[derive(Clone, Debug)]
pub struct AgentLoopConfig {
    pub persistent: bool,
    pub max_iterations: usize,
    pub max_nudges: usize,
    pub nudge: Option<String>,
    pub done_sentinel: Option<String>,
    pub break_unless_phase: Option<String>,
    pub tool_retries: usize,
    pub tool_backoff_ms: u64,
    pub tool_format: String,
    /// Auto-compaction config. When set, the agent loop automatically compacts
    /// the transcript when estimated tokens exceed the threshold.
    pub auto_compact: Option<crate::orchestration::AutoCompactConfig>,
    /// Optional per-turn callback that can rewrite the prompt-visible messages
    /// and/or effective system prompt without mutating the recorded transcript.
    pub context_callback: Option<VmValue>,
    /// Capability policy scoped to this agent loop.
    pub policy: Option<crate::orchestration::CapabilityPolicy>,
    /// Daemon mode: agent stays alive waiting for user messages instead of
    /// terminating after the LLM produces text without tool calls.
    pub daemon: bool,
    /// LLM call retry count for transient errors (429, 5xx, connection).
    pub llm_retries: usize,
    /// Base backoff in milliseconds between LLM retries.
    pub llm_backoff_ms: u64,
    /// When true, the done sentinel is only honoured if the last `run()` tool
    /// call returned exit code 0.  If the model emits the sentinel without a
    /// passing verification, the loop injects a corrective and continues.
    pub exit_when_verified: bool,
    /// Tool loop detection thresholds.  When the same tool+args produces
    /// the same result N consecutive times, the loop intervenes:
    ///   warn (default 2):  append a redirection hint
    ///   block (default 3): replace result with hard redirect
    ///   skip (default 4):  don't execute, inject skip message
    /// Set all to 0 to disable loop detection.
    pub loop_detect_warn: usize,
    pub loop_detect_block: usize,
    pub loop_detect_skip: usize,
    /// Optional few-shot examples injected into the tool-calling contract
    /// prompt, shown to the model before the tool schema listing. Provided
    /// by the pipeline (.harn) — Harn itself has no hardcoded tool names.
    pub tool_examples: Option<String>,
    /// Optional Harn closure called after each tool-calling turn completes.
    /// Receives a dict with turn metadata (tool_names, tool_count, iteration,
    /// consecutive_single_tool_turns). If it returns a non-empty string, that
    /// string is injected as a user message before the next LLM call.
    pub post_turn_callback: Option<VmValue>,
}

/// Classify whether a VmError from an LLM call is transient and worth retrying.
fn is_retryable_llm_error(err: &VmError) -> bool {
    let msg = match err {
        VmError::Thrown(VmValue::String(s)) => s.to_lowercase(),
        VmError::CategorizedError { category, .. } => {
            return matches!(
                category,
                crate::value::ErrorCategory::RateLimit | crate::value::ErrorCategory::Timeout
            );
        }
        VmError::Runtime(s) => s.to_lowercase(),
        _ => return false,
    };
    // Retryable HTTP status codes
    msg.contains("http 429")
        || msg.contains("http 500")
        || msg.contains("http 502")
        || msg.contains("http 503")
        || msg.contains("http 529")
        || msg.contains("overloaded")
        || msg.contains("rate limit")
        || msg.contains("too many requests")
        // Connection/timeout errors
        || msg.contains("stream error")
        || msg.contains("connection")
        || msg.contains("timed out")
        || msg.contains("timeout")
        // Ollama transient issues
        || msg.contains("delivered no content")
        || msg.contains("eof")
}

/// Extract retry-after delay from error message if present (e.g. "retry-after: 5").
fn extract_retry_after_ms(err: &VmError) -> Option<u64> {
    let msg = match err {
        VmError::Thrown(VmValue::String(s)) => s.as_ref(),
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

fn loop_state_requests_phase_change(text: &str, current_phase: &str) -> bool {
    if current_phase.trim().is_empty() {
        return false;
    }

    let current_phase = current_phase.trim();
    let mut last_next_phase: Option<&str> = None;
    for line in text.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("next_phase:") {
            let phase = rest.trim();
            if !phase.is_empty() {
                last_next_phase = Some(phase);
            }
        }
    }

    last_next_phase.is_some_and(|phase| phase != current_phase)
}

/// Write the full LLM request payload to a JSONL transcript file.
/// Enabled by setting HARN_LLM_TRANSCRIPT_DIR to a directory path.
fn dump_llm_request(iteration: usize, call_id: &str, opts: &super::api::LlmCallOptions) {
    let dir = match std::env::var("HARN_LLM_TRANSCRIPT_DIR") {
        Ok(d) if !d.is_empty() => d,
        _ => return,
    };
    let _ = std::fs::create_dir_all(&dir);
    let path = format!("{dir}/llm_transcript.jsonl");
    let tool_schemas =
        crate::llm::tools::collect_tool_schemas(opts.tools.as_ref(), opts.native_tools.as_deref());
    let entry = serde_json::json!({
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
        "tool_schemas": tool_schemas,
        "tool_format": std::env::var("HARN_AGENT_TOOL_FORMAT").unwrap_or_else(|_| "text".to_string()),
    });
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

fn dump_llm_response(
    iteration: usize,
    call_id: &str,
    result: &super::api::LlmResult,
    response_ms: u64,
) {
    let dir = match std::env::var("HARN_LLM_TRANSCRIPT_DIR") {
        Ok(d) if !d.is_empty() => d,
        _ => return,
    };
    let path = format!("{dir}/llm_transcript.jsonl");
    let entry = serde_json::json!({
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
    });
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

fn annotate_current_span(metadata: &[(&str, serde_json::Value)]) {
    let Some(span_id) = crate::tracing::current_span_id() else {
        return;
    };
    for (key, value) in metadata {
        crate::tracing::span_set_metadata(span_id, key, value.clone());
    }
}

fn chrono_now() -> String {
    let now = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:03}", now.as_secs(), now.subsec_millis())
}

pub(crate) fn install_current_host_bridge(bridge: Rc<crate::bridge::HostBridge>) {
    CURRENT_HOST_BRIDGE.with(|slot| {
        *slot.borrow_mut() = Some(bridge);
    });
}

pub(crate) fn current_host_bridge() -> Option<Rc<crate::bridge::HostBridge>> {
    CURRENT_HOST_BRIDGE.with(|slot| slot.borrow().clone())
}

fn classify_tool_mutation(tool_name: &str) -> String {
    crate::orchestration::current_tool_mutation_classification(tool_name)
}

fn declared_paths(tool_name: &str, tool_args: &serde_json::Value) -> Vec<String> {
    crate::orchestration::current_tool_declared_paths(tool_name, tool_args)
}

async fn inject_queued_user_messages(
    bridge: Option<&Rc<crate::bridge::HostBridge>>,
    messages: &mut Vec<serde_json::Value>,
    checkpoint: crate::bridge::DeliveryCheckpoint,
) -> Result<Vec<crate::bridge::QueuedUserMessage>, VmError> {
    let Some(bridge) = bridge else {
        return Ok(Vec::new());
    };
    let queued = bridge.take_queued_user_messages_for(checkpoint).await;
    for message in &queued {
        messages.push(serde_json::json!({
            "role": "user",
            "content": message.content.clone(),
        }));
    }
    Ok(queued)
}

fn append_message_to_contexts(
    visible_messages: &mut Vec<serde_json::Value>,
    recorded_messages: &mut Vec<serde_json::Value>,
    message: serde_json::Value,
) {
    visible_messages.push(message.clone());
    recorded_messages.push(message);
}

/// Replacement text for an assistant turn whose ```call blocks all failed to
/// parse. Feeding the raw malformed text back into the next request causes the
/// model to mutate its own broken syntax (observed self-poison loop in
/// local-gemma4 eval 2026-04-05). We keep the parse-error diagnostic as the
/// user-role reply, but elide the broken assistant content from history.
fn compact_malformed_assistant_turn(error_count: usize) -> String {
    let plural = if error_count == 1 { "" } else { "s" };
    format!(
        "<assistant turn elided: produced {error_count} malformed tool call{plural} \
         (see parse error below). Emit a corrected call in the next turn.>"
    )
}

fn append_host_messages_to_recorded(
    recorded_messages: &mut Vec<serde_json::Value>,
    queued_messages: &[crate::bridge::QueuedUserMessage],
) {
    for message in queued_messages {
        recorded_messages.push(serde_json::json!({
            "role": "user",
            "content": message.content.clone(),
        }));
    }
}

fn build_agent_system_prompt(
    base_system: Option<&str>,
    tool_prompt: Option<&str>,
    persistent_prompt: Option<&str>,
) -> Option<String> {
    let mut parts = Vec::new();
    if let Some(base) = base_system {
        let trimmed = base.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    if let Some(tool_prompt) = tool_prompt {
        let trimmed = tool_prompt.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    if let Some(persistent_prompt) = persistent_prompt {
        let trimmed = persistent_prompt.trim();
        if !trimmed.is_empty() {
            parts.push(trimmed.to_string());
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

#[derive(Debug, Deserialize)]
struct AgentContextCallbackResponse {
    #[serde(default)]
    messages: Option<Vec<serde_json::Value>>,
    #[serde(default)]
    system: Option<String>,
}

fn message_content_text(message: &serde_json::Value) -> Option<String> {
    let content = message.get("content")?;
    if let Some(text) = content.as_str() {
        return Some(text.to_string());
    }
    let items = content.as_array()?;
    let mut parts: Vec<String> = Vec::new();
    for item in items {
        let item_type = item
            .get("type")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        match item_type {
            "text" => {
                if let Some(text) = item.get("text").and_then(|value| value.as_str()) {
                    if !text.is_empty() {
                        parts.push(text.to_string());
                    }
                }
            }
            "tool_result" => {
                if let Some(text) = item.get("content").and_then(|value| value.as_str()) {
                    if !text.is_empty() {
                        parts.push(text.to_string());
                    }
                }
            }
            _ => {}
        }
    }
    if parts.is_empty() {
        None
    } else {
        Some(parts.join("\n\n"))
    }
}

fn latest_message_text(messages: &[serde_json::Value], role: &str) -> Option<String> {
    messages
        .iter()
        .rev()
        .find(|message| message.get("role").and_then(|value| value.as_str()) == Some(role))
        .and_then(message_content_text)
}

fn latest_tool_result_text(messages: &[serde_json::Value]) -> Option<String> {
    for message in messages.iter().rev() {
        let role = message
            .get("role")
            .and_then(|value| value.as_str())
            .unwrap_or("");
        if role == "tool" {
            if let Some(text) = message_content_text(message) {
                if !text.is_empty() {
                    return Some(text);
                }
            }
            continue;
        }
        if role != "user" {
            continue;
        }
        let Some(items) = message.get("content").and_then(|value| value.as_array()) else {
            continue;
        };
        for item in items {
            if item.get("type").and_then(|value| value.as_str()) != Some("tool_result") {
                continue;
            }
            if let Some(text) = item.get("content").and_then(|value| value.as_str()) {
                if !text.is_empty() {
                    return Some(text.to_string());
                }
            }
        }
    }
    None
}

fn recent_message_tail(messages: &[serde_json::Value], count: usize) -> Vec<serde_json::Value> {
    let len = messages.len();
    let start = len.saturating_sub(count);
    messages[start..].to_vec()
}

async fn apply_agent_context_callback(
    callback: &VmValue,
    iteration: usize,
    system: Option<&str>,
    visible_messages: &[serde_json::Value],
    recorded_messages: &[serde_json::Value],
) -> Result<(Vec<serde_json::Value>, Option<String>), VmError> {
    let Some(VmValue::Closure(closure)) = Some(callback.clone()) else {
        return Err(VmError::Runtime(
            "context_callback must be a closure".to_string(),
        ));
    };
    let payload = serde_json::json!({
        "iteration": iteration,
        "system": system,
        "messages": visible_messages,
        "visible_messages": visible_messages,
        "recorded_messages": recorded_messages,
        "recent_visible_messages": recent_message_tail(visible_messages, 8),
        "recent_recorded_messages": recent_message_tail(recorded_messages, 12),
        "latest_visible_user_message": latest_message_text(visible_messages, "user"),
        "latest_visible_assistant_message": latest_message_text(visible_messages, "assistant"),
        "latest_recorded_user_message": latest_message_text(recorded_messages, "user"),
        "latest_recorded_assistant_message": latest_message_text(recorded_messages, "assistant"),
        "latest_tool_result": latest_tool_result_text(visible_messages),
        "latest_recorded_tool_result": latest_tool_result_text(recorded_messages),
    });
    let mut vm = crate::vm::clone_async_builtin_child_vm().ok_or_else(|| {
        VmError::Runtime("context_callback requires an async builtin VM context".to_string())
    })?;
    let payload_vm = crate::stdlib::json_to_vm_value(&payload);
    let result = vm.call_closure_pub(&closure, &[payload_vm], &[]).await;
    let value = result?;
    match value {
        VmValue::Nil => Ok((visible_messages.to_vec(), system.map(|s| s.to_string()))),
        VmValue::List(list) => Ok((
            list.iter().map(crate::llm::helpers::vm_value_to_json).collect(),
            system.map(|s| s.to_string()),
        )),
        VmValue::Dict(_) => {
            let parsed: AgentContextCallbackResponse =
                serde_json::from_value(crate::llm::helpers::vm_value_to_json(&value)).map_err(
                    |error| {
                        VmError::Runtime(format!(
                            "context_callback returned an invalid response: {error}"
                        ))
                    },
                )?;
            Ok((
                parsed.messages.unwrap_or_else(|| visible_messages.to_vec()),
                parsed.system.or_else(|| system.map(|s| s.to_string())),
            ))
        }
        other => Err(VmError::Runtime(format!(
            "context_callback must return nil, a messages list, or a dict with optional messages/system fields; got {}",
            other.display()
        ))),
    }
}

pub(crate) fn agent_loop_result_from_llm(
    result: &super::api::LlmResult,
    opts: super::api::LlmCallOptions,
) -> serde_json::Value {
    let mut transcript_messages = opts.messages.clone();
    transcript_messages.push(build_assistant_response_message(
        &result.text,
        &result.blocks,
        &result.tool_calls,
        result.thinking.as_deref(),
        &opts.provider,
    ));
    let mut events = vec![transcript_event(
        "provider_payload",
        "assistant",
        "internal",
        "",
        Some(serde_json::json!({
            "model": result.model.clone(),
            "input_tokens": result.input_tokens,
            "output_tokens": result.output_tokens,
            "tool_calls": result.tool_calls.clone(),
        })),
    )];
    if let Some(thinking) = result.thinking.clone() {
        if !thinking.is_empty() {
            events.push(transcript_event(
                "private_reasoning",
                "assistant",
                "private",
                &thinking,
                None,
            ));
        }
    }
    serde_json::json!({
        "status": "done",
        "text": result.text,
        "visible_text": result.text,
        "private_reasoning": result.thinking,
        "iterations": 1,
        "duration_ms": 0,
        "tools_used": [],
        "transcript": super::helpers::vm_value_to_json(&transcript_to_vm_with_events(
            opts.transcript_id,
            opts.transcript_summary,
            opts.transcript_metadata,
            &transcript_messages,
            events,
            Vec::new(),
            Some("active"),
        )),
    })
}

/// Create an unbounded channel and spawn a local task that forwards text
/// deltas to `bridge.send_call_progress()`.  Returns the sender half —
/// drop it when the LLM call is done to terminate the forwarding task.
fn spawn_progress_forwarder(
    bridge: &Rc<crate::bridge::HostBridge>,
    call_id: String,
) -> DeltaSender {
    let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel::<String>();
    let bridge = bridge.clone();
    tokio::task::spawn_local(async move {
        let mut token_count: u64 = 0;
        while let Some(delta) = rx.recv().await {
            token_count += 1;
            bridge.send_call_progress(&call_id, &delta, token_count);
        }
    });
    tx
}

// ---------------------------------------------------------------------------
// observed_llm_call — shared single-LLM-call wrapper with full observability
// ---------------------------------------------------------------------------
// Both `llm_call` and `agent_loop` previously duplicated call-id generation,
// bridge notifications, span annotation, retry logic, and tracing.  This
// function is the single source of truth for "make one LLM call with all
// production instrumentation."

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

/// Make one LLM call with full observability: call-id generation, bridge
/// notifications (call_start / call_progress / call_end), span annotation,
/// retry with exponential backoff, and tracing.  Returns the raw `LlmResult`.
///
/// - `bridge`: when present, sends call_start/call_end notifications and
///   streams token-by-token deltas to the host via a progress forwarder.
/// - `retry_config`: controls transient-error retry behavior.
/// - `iteration`: optional loop iteration index for span context (used by
///   `agent_loop`; `llm_call` passes `None`).
/// - `offthread`: when true, runs provider I/O on Tokio's multithreaded
///   scheduler via `vm_call_llm_full_streaming_offthread`. Use this for
///   standalone calls that must not block the VM's LocalSet.
pub(crate) async fn observed_llm_call(
    opts: &super::api::LlmCallOptions,
    bridge: Option<&Rc<crate::bridge::HostBridge>>,
    retry_config: &LlmRetryConfig,
    iteration: Option<usize>,
    offthread: bool,
) -> Result<super::api::LlmResult, VmError> {
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
        if let Some(iter) = iteration {
            call_start_meta["iteration"] = serde_json::json!(iter);
            call_start_meta["llm_attempt"] = serde_json::json!(attempt);
        }
        if let Some(b) = bridge {
            b.send_call_start(&call_id, "llm", "llm_call", call_start_meta);
        }

        // Transcript dump (enabled by HARN_LLM_TRANSCRIPT_DIR)
        dump_llm_request(iteration.unwrap_or(0), &call_id, opts);

        // Execute the LLM call
        let start = std::time::Instant::now();
        let llm_result = if let Some(b) = bridge {
            let delta_tx = spawn_progress_forwarder(b, call_id.clone());
            if offthread {
                vm_call_llm_full_streaming_offthread(opts, delta_tx).await
            } else {
                vm_call_llm_full_streaming(opts, delta_tx).await
            }
        } else if offthread {
            // No bridge but offthread requested — still use streaming with a
            // discarding receiver so the call runs on the multithreaded scheduler.
            let (delta_tx, _delta_rx) = tokio::sync::mpsc::unbounded_channel::<String>();
            vm_call_llm_full_streaming_offthread(opts, delta_tx).await
        } else {
            super::api::vm_call_llm_full(opts).await
        };
        let duration_ms = start.elapsed().as_millis() as u64;

        match llm_result {
            Ok(result) => {
                // Success: annotate span, dump response, notify bridge, trace
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

pub async fn run_agent_loop_internal(
    opts: &mut super::api::LlmCallOptions,
    config: AgentLoopConfig,
) -> Result<serde_json::Value, VmError> {
    struct ExecutionPolicyGuard {
        active: bool,
    }

    impl Drop for ExecutionPolicyGuard {
        fn drop(&mut self) {
            if self.active {
                crate::orchestration::pop_execution_policy();
            }
        }
    }

    let bridge = current_host_bridge();
    let max_iterations = config.max_iterations;
    let persistent = config.persistent;
    let max_nudges = config.max_nudges;
    let custom_nudge = config.nudge;
    let done_sentinel = config
        .done_sentinel
        .clone()
        .unwrap_or_else(|| "##DONE##".to_string());
    let break_unless_phase = config.break_unless_phase.clone();
    let tool_retries = config.tool_retries;
    let tool_backoff_ms = config.tool_backoff_ms;
    let tool_format = config.tool_format;
    let context_callback = config.context_callback.clone();

    let auto_compact = config.auto_compact.clone();
    let daemon = config.daemon;
    let exit_when_verified = config.exit_when_verified;
    let mut last_run_exit_code: Option<i32> = None;

    // Tool loop detection — catches stuck loops where the model calls the
    // same tool with the same args and gets the same result repeatedly.
    let loop_detect_enabled = config.loop_detect_warn > 0;
    let mut loop_tracker = ToolCallTracker::new(
        config.loop_detect_warn,
        config.loop_detect_block,
        config.loop_detect_skip,
    );

    // Push per-agent policy if configured
    if let Some(ref policy) = config.policy {
        crate::orchestration::push_execution_policy(policy.clone());
    }
    let _policy_guard = ExecutionPolicyGuard {
        active: config.policy.is_some(),
    };

    let tools_owned = opts.tools.clone();
    let tools_val = tools_owned.as_ref();
    let native_tools_for_prompt = opts.native_tools.clone();
    let rendered_schemas =
        crate::llm::tools::collect_tool_schemas(tools_val, native_tools_for_prompt.as_deref());
    let has_tools = !rendered_schemas.is_empty();
    let base_system = opts.system.clone();

    if has_tools && tool_format != "native" {
        opts.native_tools = None;
    }
    let tool_examples = config.tool_examples.clone();
    let tool_contract_prompt = if has_tools {
        Some(build_tool_calling_contract_prompt(
            tools_val,
            native_tools_for_prompt.as_deref(),
            &tool_format,
            tool_format == "text",
            tool_examples.as_deref(),
        ))
    } else {
        None
    };

    let persistent_system_prompt = if persistent {
        if exit_when_verified {
            // When exit_when_verified is set, the harness enforces that the
            // done sentinel is only honoured after a passing run(). The
            // system prompt only needs a brief reminder, not a long rule.
            Some(format!(
                "\n\nKeep working until the task is complete. Take action with tools — \
                 do not stop to explain. Output {done_sentinel} when done."
            ))
        } else {
            Some(format!(
                "\n\nIMPORTANT: You MUST keep working until the task is complete. \
                 Do NOT stop to explain or summarize — take action with tools. \
                 When the requested work is complete, output {done_sentinel} on its own line."
            ))
        }
    } else {
        None
    };
    let mut visible_messages = opts.messages.clone();
    let mut recorded_messages = opts.messages.clone();

    // `total_text` is the concatenation of every iteration's assistant text.
    // It is the raw transcript the reflector/meta-analysis callers want to
    // see end-to-end. It is NOT suitable as "the agent's answer" because
    // exploration turns and tool-call expressions from mid-run bleed into
    // it. Callers wanting "what the user should see" should use the
    // `visible_text` field instead, which is the LAST iteration's text
    // alone — see the Ok(json!) block at the end of this function.
    let mut total_text = String::new();
    let mut last_iteration_text = String::new();
    let mut consecutive_text_only = 0usize;
    let mut consecutive_single_tool_turns = 0usize;
    let mut all_tools_used: Vec<String> = Vec::new();
    let mut rejected_tools: Vec<String> = Vec::new();
    let mut deferred_user_messages: Vec<String> = Vec::new();
    let mut total_iterations = 0usize;
    let mut final_status = "done";
    let mut transcript_summary = opts.transcript_summary.clone();
    let loop_start = std::time::Instant::now();
    let mut transcript_events = Vec::new();
    let mut idle_backoff_ms = 100u64;

    for iteration in 0..max_iterations {
        total_iterations = iteration + 1;
        let immediate_messages = inject_queued_user_messages(
            bridge.as_ref(),
            &mut visible_messages,
            crate::bridge::DeliveryCheckpoint::InterruptImmediate,
        )
        .await?;
        append_host_messages_to_recorded(&mut recorded_messages, &immediate_messages);
        for message in &immediate_messages {
            transcript_events.push(transcript_event(
                "host_input",
                "user",
                "public",
                &message.content,
                Some(serde_json::json!({"delivery": format!("{:?}", message.mode)})),
            ));
        }
        if !immediate_messages.is_empty() {
            consecutive_text_only = 0;
            idle_backoff_ms = 100;
        }
        let default_system = build_agent_system_prompt(
            base_system.as_deref(),
            tool_contract_prompt.as_deref(),
            persistent_system_prompt.as_deref(),
        );
        let (call_messages, call_system) = if let Some(callback) = context_callback.as_ref() {
            apply_agent_context_callback(
                callback,
                iteration,
                default_system.as_deref(),
                &visible_messages,
                &recorded_messages,
            )
            .await?
        } else {
            (visible_messages.clone(), default_system)
        };
        crate::llm::api::debug_log_message_shapes(
            &format!("agent iteration={iteration} preflight"),
            &call_messages,
        );
        opts.messages = call_messages;
        opts.system = call_system;
        let result = observed_llm_call(
            opts,
            bridge.as_ref(),
            &LlmRetryConfig {
                retries: config.llm_retries,
                backoff_ms: config.llm_backoff_ms,
            },
            Some(iteration),
            false, // agent_loop runs on the local set, not offthread
        )
        .await?;

        let text = result.text.clone();
        total_text.push_str(&text);
        // `last_iteration_text` is assigned below AFTER the tool-call parser
        // runs, so it holds the prose (calls stripped) rather than the raw
        // text. For the native-tool-call and no-tools branches we fall back
        // to the raw text a few lines down.
        transcript_events.push(transcript_event(
            "provider_payload",
            "assistant",
            "internal",
            "",
            Some(serde_json::json!({
                "model": result.model,
                "input_tokens": result.input_tokens,
                "output_tokens": result.output_tokens,
                "tool_calls": result.tool_calls,
                "tool_calling_mode": tool_format.clone(),
            })),
        ));
        if let Some(thinking) = result.thinking.clone() {
            if !thinking.is_empty() {
                transcript_events.push(transcript_event(
                    "private_reasoning",
                    "assistant",
                    "private",
                    &thinking,
                    None,
                ));
            }
        }

        let mut tool_parse_errors: Vec<String> = Vec::new();
        // `text_prose` is the model's text with any fenceless TS tool-call
        // expressions excised. When the parser doesn't run (native-format
        // path or no-tools path), it falls back to the raw text verbatim.
        let mut text_prose = text.clone();
        let tool_calls = if !result.tool_calls.is_empty() {
            result.tool_calls.clone()
        } else if has_tools {
            // Prefer provider-native tool calls when available, but keep text-call
            // parsing as a compatibility fallback. This lets workflows use
            // tool_format="native" without breaking providers or models that still
            // emit ```call blocks.
            let parse_result = parse_text_tool_calls_with_tools(&text, tools_val);
            if !parse_result.calls.is_empty() && tool_format == "native" {
                crate::events::log_info(
                    "llm.tool",
                    &format!(
                        "text_fallback_triggered: model emitted {} text call(s) in native mode",
                        parse_result.calls.len()
                    ),
                );
            }
            tool_parse_errors = parse_result.errors;
            text_prose = parse_result.prose;
            let calls = parse_result.calls;

            // When the parser found tool-call-looking text but couldn't
            // parse it, inject the specific parse error into the conversation
            // so the model knows what to fix (e.g. unescaped backtick inside
            // a template literal).  Without this, the generic nudge message
            // gives the model no signal about *what* was wrong, causing it to
            // retry the same broken format 5-7 times.
            if calls.is_empty() && !tool_parse_errors.is_empty() {
                let error_summary = tool_parse_errors
                    .iter()
                    .take(2)
                    .cloned()
                    .collect::<Vec<_>>()
                    .join("; ");
                crate::events::log_warn(
                    "llm.tool",
                    &format!(
                        "{} tool-call parse error(s): {}",
                        tool_parse_errors.len(),
                        &error_summary[..error_summary.len().min(200)]
                    ),
                );
                let feedback = format!(
                    "Your tool call could not be parsed: {error_summary}\n\n\
                     Use heredoc syntax for multiline content — it requires NO escaping:\n\
                     edit({{\n\
                         action: \"create\",\n\
                         path: \"...\",\n\
                         content: <<EOF\n\
                     package main\n\
                     // backticks, quotes, backslashes — all fine inside heredoc\n\
                     EOF\n\
                     }})\n\n\
                     Do NOT use backtick template literals for code that contains \
                     backtick characters (Go raw strings, Rust raw strings, shell). \
                     Heredoc avoids all escaping issues."
                );
                append_message_to_contexts(
                    &mut visible_messages,
                    &mut recorded_messages,
                    serde_json::json!({"role": "user", "content": feedback}),
                );
            }
            calls
        } else {
            Vec::new()
        };
        // Surface the prose (not the raw text) to callers that read
        // `last_iteration_text` / `visible_text`. Tool call expressions are
        // structured data in `tool_calls`, not something the user should
        // see as the agent's "answer". This also means conversation history
        // will carry the prose, so future iterations don't see their own
        // prior call syntax as narration.
        last_iteration_text = text_prose.clone();

        // Check done_sentinel on EVERY response, not just text-only ones.
        // If present alongside tool calls, we still process the tools (so their
        // results land in the conversation), but mark the loop to exit afterward.
        let sentinel_in_text = text.contains(&done_sentinel);
        let phase_change = break_unless_phase
            .as_deref()
            .is_some_and(|phase| loop_state_requests_phase_change(&text, phase));
        // When exit_when_verified is set, the sentinel is only honoured if the
        // last run() tool call returned exit code 0.  This prevents premature
        // exit when the model claims it's done but verification hasn't passed.
        let verified = !exit_when_verified || last_run_exit_code == Some(0);
        // Guard: the model must have made at least one tool call before the
        // done sentinel is honoured.  This prevents premature exits where the
        // model describes a plan and emits ##DONE## without actually acting.
        let has_acted = !all_tools_used.is_empty() || !tool_calls.is_empty();
        let sentinel_hit =
            persistent && ((sentinel_in_text && verified && has_acted) || phase_change);

        // If the model emitted the sentinel but verification hasn't passed,
        // inject a corrective so the model knows it must keep going.
        if sentinel_in_text && !verified && persistent {
            let code_str = last_run_exit_code.map_or("none".to_string(), |c| c.to_string());
            let corrective = format!(
                "You emitted the done sentinel but verification has not passed \
                 (last run exit code: {code_str}). The loop will continue. \
                 Run the verification command and fix any failures before finishing."
            );
            visible_messages.push(serde_json::json!({
                "role": "user",
                "content": corrective
            }));
            recorded_messages.push(serde_json::json!({
                "role": "user",
                "content": corrective
            }));
        }
        // If the model emitted the sentinel without having made any tool
        // calls, it's trying to declare done without doing any work.
        if sentinel_in_text && !has_acted && persistent && has_tools {
            let corrective = "You emitted the done sentinel without making any tool calls. \
                 You MUST use tools to complete the task — read source files, \
                 create test files, and run verification before finishing. \
                 Start by using lookup() or read() to explore the codebase."
                .to_string();
            visible_messages.push(serde_json::json!({
                "role": "user",
                "content": corrective
            }));
            recorded_messages.push(serde_json::json!({
                "role": "user",
                "content": corrective
            }));
        }

        if !tool_calls.is_empty() {
            consecutive_text_only = 0;
            idle_backoff_ms = 100;
            if tool_format == "native" {
                append_message_to_contexts(
                    &mut visible_messages,
                    &mut recorded_messages,
                    build_assistant_tool_message(&text, &tool_calls, &opts.provider),
                );
            } else {
                // When some calls parsed but others didn't, replace the raw
                // assistant text with a compact summary so the next iteration
                // cannot see (and mutate) the malformed call blocks. Tool
                // results for the successful calls are appended below, so the
                // model still has their outcomes.
                // In the clean case we append the model's PROSE (tool-call
                // expressions stripped). Tool calls are structured data —
                // replaying them as narration would make the next turn see
                // them as both invocations AND narration commentary, which
                // is exactly the "visible_text leaked lookup()/read() into
                // the refined prompt" bug we saw in the rewriter.
                let assistant_content_for_history = if tool_parse_errors.is_empty() {
                    text_prose.clone()
                } else {
                    format!(
                        "<assistant turn partially elided: {} tool call(s) executed successfully \
                         ({}), {} malformed tool call(s) rejected. \
                         See tool results and parse errors that follow.>",
                        tool_calls.len(),
                        tool_calls
                            .iter()
                            .filter_map(|c| c.get("name").and_then(|n| n.as_str()))
                            .collect::<Vec<_>>()
                            .join(", "),
                        tool_parse_errors.len(),
                    )
                };
                append_message_to_contexts(
                    &mut visible_messages,
                    &mut recorded_messages,
                    serde_json::json!({
                        "role": "assistant",
                        "content": assistant_content_for_history,
                    }),
                );
            }

            let mut observations = String::new();
            let mut tools_used_this_iter = Vec::new();
            let mut rejection_followups: Vec<String> = Vec::new();
            let tool_schemas = collect_tool_schemas(tools_val, opts.native_tools.as_deref());

            // Parallel dispatch for read-only exploration batches. When the
            // leading run of tool calls in this assistant response are all in
            // the read-only set (read, lookup, search, outline, list_templates,
            // get_template, web_search, web_fetch), we concurrently pre-fetch
            // their execution results via join_all. This covers two cases:
            //   (a) all tools read-only — entire turn runs in parallel latency
            //   (b) mixed turn starting with reads — the read-only prefix
            //       runs in parallel, then sequential dispatch handles the
            //       non-read-only tail (edit, run, etc.) as before.
            //
            // The sequential loop still runs for ALL bookkeeping (policy
            // checks, hooks, transcript events, observation appending,
            // post-hooks, ordering) — only the actual tool-execution step is
            // parallelized, and only for tools whose index is in the cache.
            // Any hook denial or arg mutation falls through to the sequential
            // path, which safely recomputes that single call.
            let ro_prefix_len: usize = tool_calls
                .iter()
                .position(|tc| !is_read_only_tool(tc["name"].as_str().unwrap_or("")))
                .unwrap_or(tool_calls.len());
            let parallel_indices: Vec<usize> = if ro_prefix_len >= 2 {
                (0..ro_prefix_len).collect()
            } else {
                Vec::new()
            };
            let mut parallel_results: std::collections::HashMap<
                usize,
                Result<serde_json::Value, VmError>,
            > = std::collections::HashMap::new();
            if !parallel_indices.is_empty() {
                // Build futures for each read-only execution. We use the raw
                // tool_args here (pre-hook); if a hook would modify or deny,
                // the sequential loop will still run its full checks and
                // choose to either reuse our result (if hooks are Allow with
                // no modifications) or recompute. This is safe because we
                // only cache results for read-only tools which have no side
                // effects — re-running them is at worst wasted work.
                use futures::future::join_all;
                let futures = parallel_indices.iter().map(|&idx| {
                    let tc = tool_calls[idx].clone();
                    let tool_name = tc["name"].as_str().unwrap_or("").to_string();
                    let tool_args = normalize_tool_args(&tool_name, &tc["arguments"]);
                    let tool_retries_local = tool_retries;
                    let tool_backoff_ms_local = tool_backoff_ms;
                    let bridge_local = bridge.clone();
                    let tools_val_local = tools_val.cloned();
                    async move {
                        dispatch_tool_execution(
                            &tool_name,
                            &tool_args,
                            tools_val_local.as_ref(),
                            bridge_local.as_ref(),
                            tool_retries_local,
                            tool_backoff_ms_local,
                        )
                        .await
                    }
                });
                let joined: Vec<Result<serde_json::Value, VmError>> = join_all(futures).await;
                for (i, idx) in parallel_indices.iter().enumerate() {
                    parallel_results.insert(*idx, joined[i].clone());
                }
            }

            for (tc_index, tc) in tool_calls.iter().enumerate() {
                let tool_id = tc["id"].as_str().unwrap_or("");
                let tool_name = tc["name"].as_str().unwrap_or("");
                let mut tool_args = normalize_tool_args(tool_name, &tc["arguments"]);

                // Detect malformed JSON arguments that the provider returned
                // (marked with __parse_error sentinel during response parsing).
                if let Some(parse_err) = tool_args.get("__parse_error").and_then(|v| v.as_str()) {
                    let result_text = format!("ERROR: {parse_err}");
                    transcript_events.push(transcript_event(
                        "tool_execution",
                        "tool",
                        "internal",
                        &result_text,
                        Some(serde_json::json!({
                            "tool_name": tool_name,
                            "tool_use_id": tool_id,
                            "rejected": true,
                        })),
                    ));
                    if tool_format == "native" {
                        append_message_to_contexts(
                            &mut visible_messages,
                            &mut recorded_messages,
                            build_tool_result_message(tool_id, &result_text, &opts.provider),
                        );
                    } else {
                        observations.push_str(&format!(
                            "[result of {tool_name}]\n{result_text}\n[end of {tool_name} result]\n\n"
                        ));
                    }
                    continue;
                }

                let policy_result = crate::orchestration::enforce_current_policy_for_tool(
                    tool_name,
                )
                .and_then(|_| {
                    crate::orchestration::enforce_tool_arg_constraints(
                        &crate::orchestration::current_execution_policy().unwrap_or_default(),
                        tool_name,
                        &tool_args,
                    )
                });
                if let Err(error) = policy_result {
                    let result_text = format!(
                        "REJECTED: {}. Use one of the declared tools exactly as named and put extra fields inside that tool's arguments.",
                        error
                    );
                    if !rejected_tools.contains(&tool_name.to_string()) {
                        rejected_tools.push(tool_name.to_string());
                    }
                    transcript_events.push(transcript_event(
                        "tool_execution",
                        "tool",
                        "internal",
                        &result_text,
                        Some(serde_json::json!({
                            "tool_name": tool_name,
                            "tool_use_id": tool_id,
                            "rejected": true,
                            "arguments": tool_args.clone(),
                        })),
                    ));
                    if tool_format == "native" {
                        append_message_to_contexts(
                            &mut visible_messages,
                            &mut recorded_messages,
                            build_tool_result_message(tool_id, &result_text, &opts.provider),
                        );
                    } else {
                        observations.push_str(&format!(
                            "[result of {tool_name}]\n{result_text}\n[end of {tool_name} result]\n\n"
                        ));
                    }
                    continue;
                }

                // PreToolUse hooks: in-process hooks first, then bridge gate
                match crate::orchestration::run_pre_tool_hooks(tool_name, &tool_args) {
                    crate::orchestration::PreToolAction::Allow => {}
                    crate::orchestration::PreToolAction::Deny(reason) => {
                        let result_text = format!("REJECTED by hook: {reason}");
                        if !rejected_tools.contains(&tool_name.to_string()) {
                            rejected_tools.push(tool_name.to_string());
                        }
                        transcript_events.push(transcript_event(
                            "tool_execution", "tool", "internal", &result_text,
                            Some(serde_json::json!({"tool_name": tool_name, "tool_use_id": tool_id, "rejected": true})),
                        ));
                        if tool_format == "native" {
                            append_message_to_contexts(
                                &mut visible_messages,
                                &mut recorded_messages,
                                build_tool_result_message(tool_id, &result_text, &opts.provider),
                            );
                        } else {
                            observations.push_str(&format!(
                                "[result of {tool_name}]\n{result_text}\n[end of {tool_name} result]\n\n"
                            ));
                        }
                        continue;
                    }
                    crate::orchestration::PreToolAction::Modify(new_args) => {
                        tool_args = new_args;
                    }
                }

                // Bridge-level PreToolUse gate: host can allow/deny/modify
                if let Some(bridge) = bridge.as_ref() {
                    let mutation = crate::orchestration::current_mutation_session();
                    let mutation_classification = classify_tool_mutation(tool_name);
                    if let Ok(response) = bridge
                        .call(
                            "tool/pre_use",
                            serde_json::json!({
                                "tool_name": tool_name,
                                "tool_use_id": tool_id,
                                "args": tool_args,
                                "mutation": {
                                    "classification": mutation_classification,
                                    "session": mutation,
                                    "declared_paths": declared_paths(tool_name, &tool_args),
                                },
                            }),
                        )
                        .await
                    {
                        let action = response
                            .get("action")
                            .and_then(|v| v.as_str())
                            .unwrap_or("allow");
                        match action {
                            "deny" => {
                                let reason = response
                                    .get("reason")
                                    .and_then(|v| v.as_str())
                                    .unwrap_or("denied by host");
                                let result_text = format!("REJECTED by host: {reason}");
                                rejection_followups.push(format!(
                                    "The previous tool call was rejected by the host. Treat this as a hard instruction and follow it exactly now.\n{reason}"
                                ));
                                if !rejected_tools.contains(&tool_name.to_string()) {
                                    rejected_tools.push(tool_name.to_string());
                                }
                                transcript_events.push(transcript_event(
                                    "tool_execution", "tool", "internal", &result_text,
                                    Some(serde_json::json!({"tool_name": tool_name, "tool_use_id": tool_id, "rejected": true})),
                                ));
                                if tool_format == "native" {
                                    append_message_to_contexts(
                                        &mut visible_messages,
                                        &mut recorded_messages,
                                        build_tool_result_message(
                                            tool_id,
                                            &result_text,
                                            &opts.provider,
                                        ),
                                    );
                                } else {
                                    observations.push_str(&format!(
                                        "[result of {tool_name}]\n{result_text}\n[end of {tool_name} result]\n\n"
                                    ));
                                }
                                continue;
                            }
                            "modify" => {
                                if let Some(new_args) = response.get("args") {
                                    tool_args = new_args.clone();
                                }
                            }
                            _ => {} // "allow" or anything else — proceed
                        }
                    }
                    // If the bridge call fails (host doesn't implement tool/pre_use),
                    // we silently proceed — the host can opt in to this protocol.
                }
                // Validate required parameters before dispatch so the LLM gets
                // a clear error instead of a cryptic handler failure.
                if let Err(msg) = validate_tool_args(tool_name, &tool_args, &tool_schemas) {
                    let result_text = format!("ERROR: {msg}");
                    transcript_events.push(transcript_event(
                        "tool_execution",
                        "tool",
                        "internal",
                        &result_text,
                        Some(serde_json::json!({
                            "tool_name": tool_name,
                            "tool_use_id": tool_id,
                            "rejected": true,
                            "arguments": tool_args.clone(),
                        })),
                    ));
                    if tool_format == "native" {
                        append_message_to_contexts(
                            &mut visible_messages,
                            &mut recorded_messages,
                            build_tool_result_message(tool_id, &result_text, &opts.provider),
                        );
                    } else {
                        observations.push_str(&format!(
                            "[result of {tool_name}]\n{result_text}\n[end of {tool_name} result]\n\n"
                        ));
                    }
                    continue;
                }

                transcript_events.push(transcript_event(
                    "tool_intent",
                    "assistant",
                    "internal",
                    tool_name,
                    Some(
                        serde_json::json!({"arguments": tool_args.clone(), "tool_use_id": tool_id}),
                    ),
                ));
                tools_used_this_iter.push(tool_name.to_string());
                let mutation_classification = classify_tool_mutation(tool_name);
                let declared_paths_current = declared_paths(tool_name, &tool_args);
                let tool_started_at = std::time::Instant::now();
                let tool_call_id = if tool_id.is_empty() {
                    format!("tool-iter-{iteration}-{}", tools_used_this_iter.len())
                } else {
                    format!("tool-{tool_id}")
                };
                let tool_span_id = crate::tracing::span_start(
                    crate::tracing::SpanKind::ToolCall,
                    tool_name.to_string(),
                );
                crate::tracing::span_set_metadata(
                    tool_span_id,
                    "tool_name",
                    serde_json::json!(tool_name),
                );
                crate::tracing::span_set_metadata(
                    tool_span_id,
                    "tool_use_id",
                    serde_json::json!(tool_id),
                );
                crate::tracing::span_set_metadata(
                    tool_span_id,
                    "call_id",
                    serde_json::json!(tool_call_id.clone()),
                );
                crate::tracing::span_set_metadata(
                    tool_span_id,
                    "iteration",
                    serde_json::json!(iteration),
                );
                crate::tracing::span_set_metadata(
                    tool_span_id,
                    "classification",
                    serde_json::json!(mutation_classification.clone()),
                );
                crate::tracing::span_set_metadata(
                    tool_span_id,
                    "declared_paths",
                    serde_json::json!(declared_paths_current.clone()),
                );
                if let Some(bridge) = bridge.as_ref() {
                    bridge.send_call_start(
                        &tool_call_id,
                        "tool",
                        tool_name,
                        serde_json::json!({
                            "tool_name": tool_name,
                            "tool_use_id": tool_id,
                            "iteration": iteration,
                            "classification": mutation_classification,
                            "declared_paths": declared_paths_current,
                        }),
                    );
                }

                // Tool loop detection: check BEFORE dispatch whether this
                // exact call has been stuck in a loop.
                let args_hash = if loop_detect_enabled {
                    stable_hash(&tool_args)
                } else {
                    0
                };
                if loop_detect_enabled {
                    if let LoopIntervention::Skip { count } =
                        loop_tracker.check(tool_name, args_hash)
                    {
                        // Skip execution entirely — the model is stuck.
                        let skip_msg = loop_intervention_message(
                            tool_name,
                            "",
                            &LoopIntervention::Skip { count },
                        )
                        .unwrap_or_default();
                        transcript_events.push(transcript_event(
                            "tool_execution",
                            "tool",
                            "internal",
                            &skip_msg,
                            Some(serde_json::json!({
                                "tool_name": tool_name,
                                "tool_use_id": tool_id,
                                "loop_skipped": true,
                                "repeat_count": count,
                            })),
                        ));
                        if tool_format == "native" {
                            append_message_to_contexts(
                                &mut visible_messages,
                                &mut recorded_messages,
                                build_tool_result_message(tool_id, &skip_msg, &opts.provider),
                            );
                        } else {
                            observations.push_str(&format!(
                                "[result of {tool_name}]\n{skip_msg}\n[end of {tool_name} result]\n\n"
                            ));
                        }
                        crate::tracing::span_end(tool_span_id);
                        if let Some(bridge) = bridge.as_ref() {
                            bridge.send_call_end(
                                &tool_call_id,
                                "tool",
                                tool_name,
                                0,
                                "loop_skipped",
                                serde_json::json!({"loop_skipped": true, "repeat_count": count}),
                            );
                        }
                        continue;
                    }
                }

                // Tool replay: if replay mode is active, try to use a
                // recorded fixture instead of executing the tool.
                let replay_hit = if crate::llm::mock::get_tool_recording_mode()
                    == crate::llm::mock::ToolRecordingMode::Replay
                {
                    crate::llm::mock::find_tool_replay_fixture(tool_name, &tool_args)
                } else {
                    None
                };

                let (is_rejected, result_text) = if let Some(fixture) = replay_hit {
                    (fixture.is_rejected, fixture.result.clone())
                } else {
                    // Prefer a pre-computed result from the parallel pre-fetch
                    // pass above, when available.
                    let call_result = if let Some(cached) = parallel_results.remove(&tc_index) {
                        cached
                    } else {
                        dispatch_tool_execution(
                            tool_name,
                            &tool_args,
                            tools_val,
                            bridge.as_ref(),
                            tool_retries,
                            tool_backoff_ms,
                        )
                        .await
                    };

                    let rejected = matches!(
                        &call_result,
                        Err(VmError::CategorizedError {
                            category: ErrorCategory::ToolRejected,
                            ..
                        })
                    );
                    let text = match &call_result {
                        Ok(val) => {
                            if let Some(text) = val.as_str() {
                                text.to_string()
                            } else if val.is_null() {
                                "(no output)".to_string()
                            } else {
                                serde_json::to_string_pretty(val).unwrap_or_default()
                            }
                        }
                        Err(VmError::CategorizedError {
                            message,
                            category: ErrorCategory::ToolRejected,
                        }) => {
                            format!("REJECTED: {message} Do not retry this tool.")
                        }
                        Err(error) => format!("Error: {error}"),
                    };
                    (rejected, text)
                };

                if is_rejected && !rejected_tools.contains(&tool_name.to_string()) {
                    rejected_tools.push(tool_name.to_string());
                }

                // Track run() exit codes for verification-gated exit.
                // The host bridge formats run results with "exit_code=N" or
                // "Command succeeded"/"Command failed" markers.
                if exit_when_verified && tool_name == "run" {
                    if result_text.contains("exit_code=0")
                        || result_text.contains("Command succeeded")
                        || result_text.contains("success=true")
                    {
                        last_run_exit_code = Some(0);
                    } else if result_text.contains("Command failed")
                        || result_text.contains("success=false")
                        || result_text.contains("exit_code=")
                    {
                        last_run_exit_code = Some(1);
                    }
                }

                // Microcompaction: compress oversized tool outputs
                let result_text = if let Some(ref ac) = auto_compact {
                    if result_text.len() > ac.tool_output_max_chars {
                        if let Some(ref cb) = ac.compress_callback {
                            crate::orchestration::invoke_compress_callback(
                                cb,
                                tool_name,
                                &result_text,
                                ac.tool_output_max_chars,
                            )
                            .await
                        } else {
                            crate::orchestration::microcompact_tool_output(
                                &result_text,
                                ac.tool_output_max_chars,
                            )
                        }
                    } else {
                        result_text
                    }
                } else {
                    result_text
                };
                crate::tracing::span_set_metadata(
                    tool_span_id,
                    "status",
                    serde_json::json!(if is_rejected { "rejected" } else { "ok" }),
                );
                crate::tracing::span_set_metadata(
                    tool_span_id,
                    "result_chars",
                    serde_json::json!(result_text.len()),
                );

                // PostToolUse hooks (in-process)
                let result_text =
                    crate::orchestration::run_post_tool_hooks(tool_name, &result_text);

                // Bridge-level PostToolUse gate: host can inspect/modify result
                let result_text = if let Some(bridge) = bridge.as_ref() {
                    let mutation = crate::orchestration::current_mutation_session();
                    if let Ok(response) = bridge
                        .call(
                            "tool/post_use",
                            serde_json::json!({
                                "tool_name": tool_name,
                                "tool_use_id": tool_id,
                                "result": result_text,
                                "rejected": is_rejected,
                                "mutation": {
                                    "classification": classify_tool_mutation(tool_name),
                                    "session": mutation,
                                    "declared_paths": declared_paths(tool_name, &tool_args),
                                },
                            }),
                        )
                        .await
                    {
                        response
                            .get("result")
                            .and_then(|v| v.as_str())
                            .map(|s| s.to_string())
                            .unwrap_or(result_text)
                    } else {
                        result_text
                    }
                } else {
                    result_text
                };
                if let Some(bridge) = bridge.as_ref() {
                    bridge.send_call_end(
                        &tool_call_id,
                        "tool",
                        tool_name,
                        tool_started_at.elapsed().as_millis() as u64,
                        if is_rejected { "rejected" } else { "ok" },
                        serde_json::json!({
                            "tool_name": tool_name,
                            "tool_use_id": tool_id,
                            "iteration": iteration,
                            "classification": classify_tool_mutation(tool_name),
                            "declared_paths": declared_paths(tool_name, &tool_args),
                            "result_chars": result_text.len(),
                            "rejected": is_rejected,
                        }),
                    );
                }
                crate::tracing::span_end(tool_span_id);

                // Record tool call result if recording mode is active.
                if crate::llm::mock::get_tool_recording_mode()
                    == crate::llm::mock::ToolRecordingMode::Record
                {
                    crate::llm::mock::record_tool_call(crate::orchestration::ToolCallRecord {
                        tool_name: tool_name.to_string(),
                        tool_use_id: tool_call_id.clone(),
                        args_hash: crate::orchestration::tool_fixture_hash(tool_name, &tool_args),
                        result: result_text.clone(),
                        is_rejected,
                        duration_ms: tool_started_at.elapsed().as_millis() as u64,
                        iteration,
                        timestamp: crate::orchestration::now_rfc3339(),
                    });
                }

                // Tool loop detection: record the result and check for
                // repeated identical outcomes.  If we detect a loop,
                // append a redirection hint or replace the result.
                let result_text = if loop_detect_enabled && !is_rejected {
                    let result_hash = stable_hash_str(&result_text);
                    let intervention = loop_tracker.record(tool_name, args_hash, result_hash);
                    if let Some(msg) =
                        loop_intervention_message(tool_name, &result_text, &intervention)
                    {
                        match intervention {
                            LoopIntervention::Warn { .. } => {
                                // Append hint after the real result
                                format!("{result_text}{msg}")
                            }
                            LoopIntervention::Block { .. } => {
                                // Replace the result entirely with the redirect
                                msg
                            }
                            _ => result_text,
                        }
                    } else {
                        result_text
                    }
                } else {
                    result_text
                };

                transcript_events.push(transcript_event(
                    "tool_execution",
                    "tool",
                    "internal",
                    &result_text,
                    Some(serde_json::json!({
                        "tool_name": tool_name,
                        "tool_use_id": tool_id,
                        "rejected": is_rejected,
                    })),
                ));

                if tool_format == "native" {
                    append_message_to_contexts(
                        &mut visible_messages,
                        &mut recorded_messages,
                        build_tool_result_message(tool_id, &result_text, &opts.provider),
                    );
                } else {
                    observations.push_str(&format!(
                        "[result of {tool_name}]\n{result_text}\n[end of {tool_name} result]\n\n"
                    ));
                }
            }

            all_tools_used.extend(tools_used_this_iter);
            if tool_format != "native" && !observations.is_empty() {
                append_message_to_contexts(
                    &mut visible_messages,
                    &mut recorded_messages,
                    serde_json::json!({
                        "role": "user",
                        "content": observations.trim_end(),
                    }),
                );
            }
            if !rejection_followups.is_empty() {
                append_message_to_contexts(
                    &mut visible_messages,
                    &mut recorded_messages,
                    serde_json::json!({
                        "role": "user",
                        "content": rejection_followups.join("\n\n"),
                    }),
                );
            }
            let finish_step_messages = inject_queued_user_messages(
                bridge.as_ref(),
                &mut visible_messages,
                crate::bridge::DeliveryCheckpoint::AfterCurrentOperation,
            )
            .await?;
            append_host_messages_to_recorded(&mut recorded_messages, &finish_step_messages);
            for message in &finish_step_messages {
                transcript_events.push(transcript_event(
                    "host_input",
                    "user",
                    "public",
                    &message.content,
                    Some(serde_json::json!({"delivery": format!("{:?}", message.mode)})),
                ));
            }
            if !finish_step_messages.is_empty() {
                consecutive_text_only = 0;
            }

            // Post-turn callback: let the pipeline inspect each tool turn
            // and optionally inject a user message (e.g. batching hints,
            // progress tracking, adaptive instructions).
            if tool_calls.len() == 1 {
                consecutive_single_tool_turns += 1;
            } else {
                consecutive_single_tool_turns = 0;
            }
            if let Some(VmValue::Closure(ref closure)) = config.post_turn_callback {
                let tool_names: Vec<&str> = tool_calls
                    .iter()
                    .filter_map(|tc| tc["name"].as_str())
                    .collect();
                let session_has_edit = all_tools_used
                    .iter()
                    .any(|t| t == "edit" || t == "scaffold" || t == "create");
                let turn_info = serde_json::json!({
                    "tool_names": tool_names,
                    "tool_count": tool_calls.len(),
                    "iteration": iteration,
                    "consecutive_single_tool_turns": consecutive_single_tool_turns,
                    "session_has_edit": session_has_edit,
                });
                let cb_arg = crate::stdlib::json_to_vm_value(&turn_info);
                if let Ok(mut vm) = crate::vm::clone_async_builtin_child_vm()
                    .ok_or_else(|| VmError::Runtime("no VM context".into()))
                {
                    if let Ok(val) = vm.call_closure_pub(closure, &[cb_arg], &[]).await {
                        let msg = val.display();
                        let msg = msg.trim();
                        if !msg.is_empty() {
                            crate::events::log_debug(
                                "agent.post_turn",
                                &format!("iter={iteration} injecting nudge ({} chars)", msg.len()),
                            );
                            append_message_to_contexts(
                                &mut visible_messages,
                                &mut recorded_messages,
                                serde_json::json!({
                                    "role": "user",
                                    "content": msg,
                                }),
                            );
                            consecutive_single_tool_turns = 0;
                        }
                    }
                }
            }

            // Auto-compaction check after tool processing.
            // Include the system prompt + tool definitions in the estimate
            // since they consume context window alongside messages.
            if let Some(ref ac) = auto_compact {
                let mut est = crate::orchestration::estimate_message_tokens(&visible_messages);
                if let Some(ref sys) = opts.system {
                    est += sys.len() / 4;
                }
                if est > ac.token_threshold {
                    let mut compact_opts = opts.clone();
                    compact_opts.messages = visible_messages.clone();
                    if let Some(summary) = crate::orchestration::auto_compact_messages(
                        &mut visible_messages,
                        ac,
                        Some(&compact_opts),
                    )
                    .await?
                    {
                        let merged = match transcript_summary.take() {
                            Some(existing)
                                if !existing.trim().is_empty()
                                    && existing.trim() != summary.trim() =>
                            {
                                format!("{existing}\n\n{summary}")
                            }
                            Some(_) | None => summary,
                        };
                        transcript_summary = Some(merged);
                    }
                }
            }

            // Feed parse-error diagnostics back in the mixed case too, so the
            // model can correct its syntax in the next turn (mirrors the
            // text-only branch below). Without this, rejected calls would
            // silently disappear from the conversation.
            if !tool_parse_errors.is_empty() {
                let error_msg = tool_parse_errors.join("\n\n");
                append_message_to_contexts(
                    &mut visible_messages,
                    &mut recorded_messages,
                    serde_json::json!({
                        "role": "user",
                        "content": error_msg,
                    }),
                );
            }
            if sentinel_hit {
                if !tool_parse_errors.is_empty() {
                    crate::events::log_warn(
                        "llm.tool",
                        &format!(
                            "{} tool-call parse error(s) suppressed by sentinel: {}",
                            tool_parse_errors.len(),
                            tool_parse_errors.join("; ")
                        ),
                    );
                }
                break;
            }
            continue;
        }

        // If the model attempted tool calls but parsing failed, replace the
        // raw malformed assistant text with a compact placeholder before
        // replaying history. Otherwise the next iteration sees its own
        // broken syntax and mutates it further (observed self-poison loop).
        //
        // In the clean text-only case we still use `text_prose` (not `text`)
        // so that IF the model wrote prose AND a non-malformed call that
        // simply happened to have no downstream effect (e.g. a call whose
        // tool was later filtered), history carries only the prose — not
        // the call expression as accidental narration.
        let assistant_content_for_history = if !tool_parse_errors.is_empty() {
            compact_malformed_assistant_turn(tool_parse_errors.len())
        } else {
            text_prose.clone()
        };
        append_message_to_contexts(
            &mut visible_messages,
            &mut recorded_messages,
            serde_json::json!({
                "role": "assistant",
                "content": assistant_content_for_history,
            }),
        );

        // Sentinel check for text-only responses (no tool calls).
        if sentinel_hit {
            break;
        }

        // If the model attempted tool calls but parsing failed, send diagnostics
        // back so it can fix its syntax instead of being silently nudged.
        if !tool_parse_errors.is_empty() {
            let error_msg = tool_parse_errors.join("\n\n");
            append_message_to_contexts(
                &mut visible_messages,
                &mut recorded_messages,
                serde_json::json!({
                    "role": "user",
                    "content": error_msg,
                }),
            );
            tool_parse_errors.clear();
            consecutive_text_only = 0;
            continue;
        }

        // done_sentinel already checked before tool dispatch above;
        // this path only reached for text-only responses without sentinel.
        if !persistent && !daemon {
            break;
        }

        // Daemon mode: if no tool calls and agent is idle, notify host and
        // wait briefly for user messages before deciding to continue/exit.
        if daemon && !persistent {
            let Some(bridge) = bridge.as_ref() else {
                final_status = "idle";
                break;
            };
            loop {
                bridge.notify(
                    "agent/idle",
                    serde_json::json!({
                        "iteration": total_iterations,
                        "backoff_ms": idle_backoff_ms,
                    }),
                );
                tokio::time::sleep(tokio::time::Duration::from_millis(idle_backoff_ms)).await;
                let resumed = bridge.take_resume_signal();
                let idle_messages = inject_queued_user_messages(
                    Some(bridge),
                    &mut visible_messages,
                    crate::bridge::DeliveryCheckpoint::InterruptImmediate,
                )
                .await?;
                append_host_messages_to_recorded(&mut recorded_messages, &idle_messages);
                if resumed || !idle_messages.is_empty() {
                    for message in &idle_messages {
                        transcript_events.push(transcript_event(
                            "host_input",
                            "user",
                            "public",
                            &message.content,
                            Some(serde_json::json!({"delivery": format!("{:?}", message.mode)})),
                        ));
                    }
                    consecutive_text_only = 0;
                    idle_backoff_ms = 100;
                    break;
                }
                idle_backoff_ms = match idle_backoff_ms {
                    0..=100 => 500,
                    101..=500 => 1000,
                    1001..=1999 => 2000,
                    _ => 2000,
                };
            }
            continue;
        }

        let finish_step_messages = inject_queued_user_messages(
            bridge.as_ref(),
            &mut visible_messages,
            crate::bridge::DeliveryCheckpoint::AfterCurrentOperation,
        )
        .await?;
        append_host_messages_to_recorded(&mut recorded_messages, &finish_step_messages);
        for message in &finish_step_messages {
            transcript_events.push(transcript_event(
                "host_input",
                "user",
                "public",
                &message.content,
                Some(serde_json::json!({"delivery": format!("{:?}", message.mode)})),
            ));
        }
        if !finish_step_messages.is_empty() {
            consecutive_text_only = 0;
            idle_backoff_ms = 100;
            continue;
        }

        consecutive_text_only += 1;
        if consecutive_text_only > max_nudges {
            final_status = "stuck";
            break;
        }

        // Silent continuation for short prose: when the model emits a
        // short text-only response (< 150 tokens, typically "thinking"
        // statements like "Let me check..."), don't inject a nudge. Just
        // loop back — the model sees its own text as the last assistant
        // message and naturally continues to act. This avoids polluting
        // context with nudge messages and the "nudge → rephrase → nudge"
        // loop seen with chatty models.
        //
        let nudge = custom_nudge
            .clone()
            .unwrap_or_else(|| "Continue — use a tool call to make progress.".to_string());
        append_message_to_contexts(
            &mut visible_messages,
            &mut recorded_messages,
            serde_json::json!({
                "role": "user",
                "content": nudge,
            }),
        );
    }

    deferred_user_messages.extend(
        inject_queued_user_messages(
            bridge.as_ref(),
            &mut visible_messages,
            crate::bridge::DeliveryCheckpoint::EndOfInteraction,
        )
        .await?
        .into_iter()
        .map(|message| message.content),
    );

    Ok(serde_json::json!({
        "status": final_status,
        // `text` is the full accumulated transcript of every assistant turn.
        // Use this for meta-analysis that genuinely wants end-to-end history
        // (reflectors, auditors, transcript replay).
        "text": total_text,
        // `visible_text` is what an end user should see as the agent's answer:
        // the LAST iteration's assistant text, unwrapped of any exploration
        // turns or tool-call expressions from earlier iterations. This is
        // what rewriters, chat bubbles, subagent consumers, and phase-routing
        // logic should key off. It is intentionally different from `text`.
        "visible_text": last_iteration_text,
        "iterations": total_iterations,
        "duration_ms": loop_start.elapsed().as_millis() as i64,
        "tools_used": all_tools_used,
        "rejected_tools": rejected_tools,
        "tool_calling_mode": tool_format,
        "deferred_user_messages": deferred_user_messages,
        "transcript": super::helpers::vm_value_to_json(&transcript_to_vm_with_events(
            opts.transcript_id.clone(),
            transcript_summary,
            opts.transcript_metadata.clone(),
            &recorded_messages,
            transcript_events,
            Vec::new(),
            Some(if final_status == "done" { "active" } else { "paused" }),
        )),
    }))
}

/// Register a tool-aware `agent_loop` that uses a bridge for tool execution.
/// This overrides the native text-only agent_loop with one that can:
/// 1. Pass tool definitions to the LLM
/// 2. Execute tool calls via the bridge (delegated to host)
/// 3. Feed tool results back into the conversation
pub fn register_agent_loop_with_bridge(vm: &mut Vm, bridge: Rc<crate::bridge::HostBridge>) {
    let b = bridge;
    install_current_host_bridge(b.clone());
    vm.register_async_builtin("agent_loop", move |args| {
        let captured_bridge = b.clone();
        async move {
            std::mem::drop(captured_bridge);
            let options = args.get(2).and_then(|a| a.as_dict()).cloned();
            let max_iterations = opt_int(&options, "max_iterations").unwrap_or(50) as usize;
            let persistent = opt_bool(&options, "persistent");
            let max_nudges = opt_int(&options, "max_nudges").unwrap_or(8) as usize;
            let custom_nudge = opt_str(&options, "nudge");
            let tool_retries = opt_int(&options, "tool_retries").unwrap_or(0) as usize;
            let tool_backoff_ms = opt_int(&options, "tool_backoff_ms").unwrap_or(1000) as u64;
            let tool_format = opt_str(&options, "tool_format").unwrap_or_else(|| {
                // Auto-detect from model/provider alias config.
                let opts = extract_llm_options(&args).ok();
                let model = opts.as_ref().map(|o| o.model.as_str()).unwrap_or("");
                let provider = opts.as_ref().map(|o| o.provider.as_str()).unwrap_or("");
                crate::llm_config::default_tool_format(model, provider)
            });
            let done_sentinel = opt_str(&options, "done_sentinel");
            let break_unless_phase = opt_str(&options, "break_unless_phase");
            let context_callback = options
                .as_ref()
                .and_then(|o| {
                    o.get("context_callback")
                        .or_else(|| o.get("context_filter"))
                })
                .cloned();
            let daemon = opt_bool(&options, "daemon");
            let auto_compact = if opt_bool(&options, "auto_compact") {
                let mut ac = crate::orchestration::AutoCompactConfig::default();
                let user_specified_threshold = opt_int(&options, "compact_threshold").is_some();
                if let Some(v) = opt_int(&options, "compact_threshold") {
                    ac.token_threshold = v as usize;
                }
                if let Some(v) = opt_int(&options, "tool_output_max_chars") {
                    ac.tool_output_max_chars = v as usize;
                }
                if let Some(v) = opt_int(&options, "compact_keep_last") {
                    ac.keep_last = v as usize;
                }
                if let Some(strategy) = opt_str(&options, "compact_strategy") {
                    ac.compact_strategy = crate::orchestration::parse_compact_strategy(&strategy)?;
                }
                if let Some(v) = opt_int(&options, "hard_limit_tokens") {
                    ac.hard_limit_tokens = Some(v as usize);
                }
                if let Some(strategy) = opt_str(&options, "hard_limit_strategy") {
                    ac.hard_limit_strategy =
                        crate::orchestration::parse_compact_strategy(&strategy)?;
                }
                if let Some(callback) = options.as_ref().and_then(|o| o.get("mask_callback")) {
                    ac.mask_callback = Some(callback.clone());
                }
                if let Some(callback) = options.as_ref().and_then(|o| o.get("compact_callback")) {
                    ac.custom_compactor = Some(callback.clone());
                    if !options
                        .as_ref()
                        .is_some_and(|o| o.contains_key("compact_strategy"))
                    {
                        ac.compact_strategy = crate::orchestration::CompactStrategy::Custom;
                    }
                }
                if let Some(callback) = options.as_ref().and_then(|o| o.get("compress_callback")) {
                    ac.compress_callback = Some(callback.clone());
                }
                // Adapt both tier-1 and tier-2 thresholds to the provider's
                // actual context window. Tier-1 stays at the configured
                // value unless it would overflow; tier-2 (hard_limit) is
                // automatically set to 75% of max context when not user-specified.
                {
                    let probe_opts = extract_llm_options(&args)?;
                    let user_specified_hard_limit =
                        opt_int(&options, "hard_limit_tokens").is_some();
                    crate::llm::api::adapt_auto_compact_to_provider(
                        &mut ac,
                        user_specified_threshold,
                        user_specified_hard_limit,
                        &probe_opts.provider,
                        &probe_opts.model,
                        &probe_opts.api_key,
                    )
                    .await;
                }
                Some(ac)
            } else {
                None
            };
            // Parse per-agent policy from options dict
            let policy = options.as_ref().and_then(|o| o.get("policy")).map(|v| {
                let json = crate::llm::helpers::vm_value_to_json(v);
                serde_json::from_value::<crate::orchestration::CapabilityPolicy>(json)
                    .unwrap_or_default()
            });
            let mut opts = extract_llm_options(&args)?;
            let result = run_agent_loop_internal(
                &mut opts,
                AgentLoopConfig {
                    persistent,
                    max_iterations,
                    max_nudges,
                    nudge: custom_nudge,
                    done_sentinel,
                    break_unless_phase,
                    tool_retries,
                    tool_backoff_ms,
                    tool_format,
                    auto_compact,
                    context_callback,
                    policy,
                    daemon,
                    llm_retries: opt_int(&options, "llm_retries").unwrap_or(4) as usize,
                    llm_backoff_ms: opt_int(&options, "llm_backoff_ms").unwrap_or(2000) as u64,
                    exit_when_verified: opt_bool(&options, "exit_when_verified"),
                    loop_detect_warn: opt_int(&options, "loop_detect_warn").unwrap_or(2) as usize,
                    loop_detect_block: opt_int(&options, "loop_detect_block").unwrap_or(3) as usize,
                    loop_detect_skip: opt_int(&options, "loop_detect_skip").unwrap_or(4) as usize,
                    tool_examples: opt_str(&options, "tool_examples"),
                    post_turn_callback: options
                        .as_ref()
                        .and_then(|o| o.get("post_turn_callback"))
                        .cloned(),
                },
            )
            .await?;
            Ok(crate::stdlib::json_to_vm_value(&result))
        }
    });
}

/// Register a bridge-aware `llm_call` that emits call_start/call_end notifications.
/// This overrides the native llm_call with one that reports to the host for observability.
pub fn register_llm_call_with_bridge(vm: &mut Vm, bridge: Rc<crate::bridge::HostBridge>) {
    let b = bridge;
    vm.register_async_builtin("llm_call", move |args| {
        let bridge = b.clone();
        async move {
            let opts = extract_llm_options(&args)?;
            let options = args.get(2).and_then(|a| a.as_dict()).cloned();
            let retry_config = LlmRetryConfig {
                retries: opt_int(&options, "llm_retries").unwrap_or(0) as usize,
                backoff_ms: opt_int(&options, "llm_backoff_ms").unwrap_or(2000) as u64,
            };

            let result = observed_llm_call(
                &opts,
                Some(&bridge),
                &retry_config,
                None,
                true, // offthread: standalone call, avoid blocking the VM LocalSet
            )
            .await?;

            Ok(build_llm_call_result(&result, &opts))
        }
    });
}

/// Assemble the user-facing result dict for `llm_call` from a raw `LlmResult`.
/// Shared by both the bridge-aware and non-bridge registrations.
pub(crate) fn build_llm_call_result(
    result: &super::api::LlmResult,
    opts: &super::api::LlmCallOptions,
) -> VmValue {
    use super::api::vm_build_llm_result;
    use super::helpers::extract_json;
    use crate::stdlib::json_to_vm_value;

    let mut transcript_messages = opts.messages.clone();
    transcript_messages.push(build_assistant_response_message(
        &result.text,
        &result.blocks,
        &result.tool_calls,
        result.thinking.as_deref(),
        &opts.provider,
    ));
    let mut extra_events = vec![transcript_event(
        "provider_payload",
        "assistant",
        "internal",
        "",
        Some(serde_json::json!({
            "model": result.model.clone(),
            "input_tokens": result.input_tokens,
            "output_tokens": result.output_tokens,
            "tool_calls": result.tool_calls.clone(),
        })),
    )];
    if let Some(thinking) = result.thinking.clone() {
        if !thinking.is_empty() {
            extra_events.push(transcript_event(
                "private_reasoning",
                "assistant",
                "private",
                &thinking,
                None,
            ));
        }
    }
    let transcript = transcript_to_vm_with_events(
        opts.transcript_id.clone(),
        opts.transcript_summary.clone(),
        opts.transcript_metadata.clone(),
        &transcript_messages,
        extra_events,
        Vec::new(),
        Some("active"),
    );

    if opts.response_format.as_deref() == Some("json") {
        let json_str = extract_json(&result.text);
        let parsed = serde_json::from_str::<serde_json::Value>(json_str)
            .ok()
            .map(|jv| json_to_vm_value(&jv));
        return vm_build_llm_result(result, parsed, Some(transcript), opts.tools.as_ref());
    }

    vm_build_llm_result(result, None, Some(transcript), opts.tools.as_ref())
}

#[cfg(test)]
mod tests {
    use super::{
        compact_malformed_assistant_turn, extract_retry_after_ms, is_read_only_tool,
        loop_state_requests_phase_change,
    };
    use crate::value::{VmError, VmValue};
    use std::rc::Rc;

    #[test]
    fn detects_phase_change_from_latest_loop_state_footer() {
        let text = "First\n\n## LOOP_STATE\nphase: assess\nnext_phase: ground\n## END_LOOP_STATE\n\nSecond\n\n## LOOP_STATE\nphase: ground\nnext_phase: execute\n## END_LOOP_STATE";
        assert!(loop_state_requests_phase_change(text, "ground"));
        assert!(!loop_state_requests_phase_change(text, "execute"));
    }

    #[test]
    fn compact_malformed_assistant_turn_elides_raw_text() {
        let msg = compact_malformed_assistant_turn(1);
        assert!(msg.contains("1 malformed tool call"));
        assert!(msg.contains("elided"));
        assert!(!msg.contains("```call"));
        assert!(!msg.contains("<<'EOF'"));

        let msg_plural = compact_malformed_assistant_turn(3);
        assert!(msg_plural.contains("3 malformed tool calls"));
    }

    // ---- extract_retry_after_ms ----

    #[test]
    fn retry_after_from_runtime_error() {
        let err = VmError::Runtime("rate limited, retry-after: 5".to_string());
        assert_eq!(extract_retry_after_ms(&err), Some(5000));
    }

    #[test]
    fn retry_after_from_thrown_string() {
        let err = VmError::Thrown(VmValue::String(Rc::from(
            "HTTP 429 Retry-After: 2.5 seconds",
        )));
        assert_eq!(extract_retry_after_ms(&err), Some(2500));
    }

    #[test]
    fn retry_after_case_insensitive() {
        let err = VmError::Runtime("RETRY-AFTER: 10".to_string());
        assert_eq!(extract_retry_after_ms(&err), Some(10000));
    }

    #[test]
    fn retry_after_missing() {
        let err = VmError::Runtime("rate limited".to_string());
        assert_eq!(extract_retry_after_ms(&err), None);
    }

    #[test]
    fn retry_after_non_numeric() {
        let err = VmError::Runtime("retry-after: tomorrow".to_string());
        assert_eq!(extract_retry_after_ms(&err), None);
    }

    #[test]
    fn retry_after_at_end_of_message() {
        let err = VmError::Runtime("retry-after: 3".to_string());
        assert_eq!(extract_retry_after_ms(&err), Some(3000));
    }

    #[test]
    fn retry_after_fractional_seconds() {
        let err = VmError::Runtime("retry-after: 0.5".to_string());
        assert_eq!(extract_retry_after_ms(&err), Some(500));
    }

    #[test]
    fn retry_after_non_string_error() {
        let err = VmError::Thrown(VmValue::Int(42));
        assert_eq!(extract_retry_after_ms(&err), None);
    }

    #[test]
    fn retry_after_with_extra_whitespace() {
        let err = VmError::Runtime("retry-after:   7  ".to_string());
        assert_eq!(extract_retry_after_ms(&err), Some(7000));
    }

    // ---- is_read_only_tool ----

    #[test]
    fn read_only_tools_recognized() {
        assert!(is_read_only_tool("read"));
        assert!(is_read_only_tool("read_file"));
        assert!(is_read_only_tool("lookup"));
        assert!(is_read_only_tool("search"));
        assert!(is_read_only_tool("outline"));
        assert!(is_read_only_tool("list_directory"));
        assert!(is_read_only_tool("web_search"));
        assert!(is_read_only_tool("web_fetch"));
    }

    #[test]
    fn write_tools_not_read_only() {
        assert!(!is_read_only_tool("write"));
        assert!(!is_read_only_tool("edit"));
        assert!(!is_read_only_tool("delete"));
        assert!(!is_read_only_tool("exec"));
        assert!(!is_read_only_tool(""));
    }
}
