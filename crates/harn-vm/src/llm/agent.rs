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
    build_tool_calling_contract_prompt, build_tool_result_message, handle_tool_locally,
    normalize_tool_args, parse_text_tool_calls_with_tools,
};
use super::trace::{trace_llm_call, LlmTraceEntry};

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
                message: format!("tool not available without host bridge: {tool_name}"),
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
    let tool_contract_prompt = if has_tools {
        Some(build_tool_calling_contract_prompt(
            tools_val,
            native_tools_for_prompt.as_deref(),
            &tool_format,
            tool_format == "text",
        ))
    } else {
        None
    };

    let persistent_system_prompt = if persistent {
        Some(format!(
            "\n\nIMPORTANT: You MUST keep working until the task is complete. \
             Do NOT stop to explain or summarize — take action with tools. \
             When the requested work is complete and your verification has succeeded, \
             stop immediately and output {done_sentinel} on its own line. \
             Do not make additional tool calls after a passing verification result unless \
             you still have concrete evidence that the task is incomplete or failing."
        ))
    } else {
        None
    };
    let mut visible_messages = opts.messages.clone();
    let mut recorded_messages = opts.messages.clone();

    let mut total_text = String::new();
    let mut consecutive_text_only = 0usize;
    // Count turns in a row where the model emitted malformed tool calls.
    // When the model also emits the DONE sentinel in such a turn, we give it
    // exactly one recovery attempt before honoring DONE, so a model that
    // genuinely cannot produce valid syntax does not loop until timeout.
    let mut consecutive_parse_error_turns = 0usize;
    const MAX_PARSE_ERROR_RECOVERY_TURNS: usize = 1;
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
        opts.messages = call_messages;
        opts.system = call_system;
        let start = std::time::Instant::now();
        let result = if let Some(bridge) = bridge.as_ref() {
            let mut llm_attempt = 0usize;
            loop {
                let llm_call_id = next_call_id();
                let prompt_chars: usize = opts
                    .messages
                    .iter()
                    .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
                    .map(|s| s.len())
                    .sum();
                annotate_current_span(&[
                    ("call_id", serde_json::json!(llm_call_id.clone())),
                    ("iteration", serde_json::json!(iteration)),
                    ("model", serde_json::json!(opts.model.clone())),
                    ("provider", serde_json::json!(opts.provider.clone())),
                    ("prompt_chars", serde_json::json!(prompt_chars)),
                ]);
                bridge.send_call_start(
                    &llm_call_id,
                    "llm",
                    "llm_call",
                    serde_json::json!({
                        "model": opts.model,
                        "prompt_chars": prompt_chars,
                        "iteration": iteration,
                        "llm_attempt": llm_attempt,
                    }),
                );
                dump_llm_request(iteration, &llm_call_id, opts);
                let delta_tx = spawn_progress_forwarder(bridge, llm_call_id.clone());
                let llm_result = vm_call_llm_full_streaming(opts, delta_tx).await;
                let llm_duration = start.elapsed().as_millis() as u64;
                match llm_result {
                    Ok(result) => {
                        annotate_current_span(&[
                            ("status", serde_json::json!("ok")),
                            ("input_tokens", serde_json::json!(result.input_tokens)),
                            ("output_tokens", serde_json::json!(result.output_tokens)),
                        ]);
                        dump_llm_response(iteration, &llm_call_id, &result, llm_duration);
                        bridge.send_call_end(
                            &llm_call_id,
                            "llm",
                            "llm_call",
                            llm_duration,
                            "ok",
                            serde_json::json!({
                                "model": result.model,
                                "input_tokens": result.input_tokens,
                                "output_tokens": result.output_tokens,
                            }),
                        );
                        break result;
                    }
                    Err(error) => {
                        let retryable = is_retryable_llm_error(&error);
                        let can_retry = retryable && llm_attempt < config.llm_retries;
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
                            ("attempt", serde_json::json!(llm_attempt)),
                        ]);
                        bridge.send_call_end(
                            &llm_call_id,
                            "llm",
                            "llm_call",
                            llm_duration,
                            status,
                            serde_json::json!({
                                "error": error.to_string(),
                                "retryable": retryable,
                                "attempt": llm_attempt,
                            }),
                        );
                        if !can_retry {
                            return Err(error);
                        }
                        llm_attempt += 1;
                        let backoff = extract_retry_after_ms(&error)
                            .unwrap_or(config.llm_backoff_ms * (1 << llm_attempt.min(4)) as u64);
                        eprintln!(
                            "[harn] LLM call failed ({}), retrying in {}ms (attempt {}/{})",
                            error, backoff, llm_attempt, config.llm_retries
                        );
                        tokio::time::sleep(std::time::Duration::from_millis(backoff)).await;
                    }
                }
            }
        } else {
            super::api::vm_call_llm_full(opts).await?
        };

        trace_llm_call(LlmTraceEntry {
            model: result.model.clone(),
            input_tokens: result.input_tokens,
            output_tokens: result.output_tokens,
            duration_ms: start.elapsed().as_millis() as u64,
        });

        let text = result.text.clone();
        total_text.push_str(&text);
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

        let mut tool_call_source = "none";
        let mut tool_parse_errors: Vec<String> = Vec::new();
        let tool_calls = if !result.tool_calls.is_empty() {
            tool_call_source = "native";
            result.tool_calls.clone()
        } else if has_tools {
            // Prefer provider-native tool calls when available, but keep text-call
            // parsing as a compatibility fallback. This lets workflows use
            // tool_format="native" without breaking providers or models that still
            // emit ```call blocks.
            let parse_result = parse_text_tool_calls_with_tools(&text, tools_val);
            if !parse_result.calls.is_empty() {
                tool_call_source = "text_fallback";
                if tool_format == "native" {
                    eprintln!(
                        "[harn] text_fallback_triggered: model emitted {} text call(s) in native mode",
                        parse_result.calls.len()
                    );
                }
            }
            tool_parse_errors = parse_result.errors;
            parse_result.calls
        } else {
            Vec::new()
        };
        if std::env::var("BURIN_TRACE_HARN_TOOL_PARSE").as_deref() == Ok("1") {
            eprintln!(
                "[harn-vm] tool_call_source={} count={} text_len={}",
                tool_call_source,
                tool_calls.len(),
                text.len()
            );
        }

        // Check done_sentinel on EVERY response, not just text-only ones.
        // If present alongside tool calls, we still process the tools (so their
        // results land in the conversation), but mark the loop to exit afterward.
        let sentinel_hit = persistent
            && (text.contains(&done_sentinel)
                || break_unless_phase
                    .as_deref()
                    .is_some_and(|phase| loop_state_requests_phase_change(&text, phase)));

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
                append_message_to_contexts(
                    &mut visible_messages,
                    &mut recorded_messages,
                    serde_json::json!({
                        "role": "assistant",
                        "content": text,
                    }),
                );
            }

            let mut observations = String::new();
            let mut tools_used_this_iter = Vec::new();
            let mut rejection_followups: Vec<String> = Vec::new();

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
                if std::env::var("BURIN_TRACE_HARN_TOOL_PARSE").as_deref() == Ok("1") {
                    eprintln!(
                        "[harn-vm] parsed_tool_call name={} args={}",
                        tool_name, tc["arguments"]
                    );
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
                            "<tool_result name=\"{tool_name}\">\n{result_text}\n</tool_result>\n\n"
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
                                "<tool_result name=\"{tool_name}\">\n{result_text}\n</tool_result>\n\n"
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
                                        "<tool_result name=\"{tool_name}\">\n{result_text}\n</tool_result>\n\n"
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

                // Prefer a pre-computed result from the parallel pre-fetch
                // pass above, when available. That pass runs read-only tool
                // executions concurrently via join_all before this sequential
                // loop begins, so the loop just consumes results here without
                // waiting on I/O. Falls through to the inline dispatch if no
                // cached result exists (non-read-only tools, or read-only
                // tools whose args were modified by a hook in this loop).
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

                let is_rejected = matches!(
                    &call_result,
                    Err(VmError::CategorizedError {
                        category: ErrorCategory::ToolRejected,
                        ..
                    })
                );
                let result_text = match &call_result {
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

                if is_rejected && !rejected_tools.contains(&tool_name.to_string()) {
                    rejected_tools.push(tool_name.to_string());
                }

                // Microcompaction: snip oversized tool outputs
                let result_text = if let Some(ref ac) = auto_compact {
                    crate::orchestration::microcompact_tool_output(
                        &result_text,
                        ac.tool_output_max_chars,
                    )
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
                        "<tool_result name=\"{tool_name}\">\n{result_text}\n</tool_result>\n\n"
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

            // Auto-compaction check after tool processing
            if let Some(ref ac) = auto_compact {
                let est = crate::orchestration::estimate_message_tokens(&visible_messages);
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

            if !tool_parse_errors.is_empty() {
                consecutive_parse_error_turns += 1;
            } else {
                consecutive_parse_error_turns = 0;
            }
            if sentinel_hit {
                // Parse errors take precedence over DONE for exactly one
                // recovery attempt. This lets a model that mistyped `:` vs `=`
                // fix itself, but prevents an open-ended loop when the model
                // cannot produce valid syntax at all.
                if !tool_parse_errors.is_empty()
                    && consecutive_parse_error_turns <= MAX_PARSE_ERROR_RECOVERY_TURNS
                {
                    eprintln!(
                        "[harn] DONE sentinel ignored for one recovery attempt ({} parse error(s) this turn)",
                        tool_parse_errors.len()
                    );
                } else {
                    if !tool_parse_errors.is_empty() {
                        eprintln!(
                            "[harn] DONE sentinel honored despite {} parse error(s) — recovery budget ({}) exhausted",
                            tool_parse_errors.len(),
                            MAX_PARSE_ERROR_RECOVERY_TURNS
                        );
                    }
                    break;
                }
            }
            continue;
        }

        // If the model attempted tool calls but parsing failed, replace the
        // raw malformed assistant text with a compact placeholder before
        // replaying history. Otherwise the next iteration sees its own
        // broken syntax and mutates it further (observed self-poison loop).
        let assistant_content_for_history = if !tool_parse_errors.is_empty() {
            compact_malformed_assistant_turn(tool_parse_errors.len())
        } else {
            text.clone()
        };
        append_message_to_contexts(
            &mut visible_messages,
            &mut recorded_messages,
            serde_json::json!({
                "role": "assistant",
                "content": assistant_content_for_history,
            }),
        );

        // Track parse-error streaks for the bounded-recovery policy used by
        // the sentinel check below.
        if !tool_parse_errors.is_empty() {
            consecutive_parse_error_turns += 1;
        } else {
            consecutive_parse_error_turns = 0;
        }
        // Sentinel check for text-only responses (no tool calls). Parse errors
        // earn exactly one recovery attempt before DONE is honored — a model
        // that cannot emit valid syntax after one diagnostic is not going to
        // recover, and looping until timeout is strictly worse than exiting
        // with whatever partial state already exists.
        if sentinel_hit {
            if !tool_parse_errors.is_empty()
                && consecutive_parse_error_turns <= MAX_PARSE_ERROR_RECOVERY_TURNS
            {
                eprintln!(
                    "[harn] DONE sentinel ignored for one recovery attempt ({} parse error(s) this turn)",
                    tool_parse_errors.len()
                );
            } else {
                if !tool_parse_errors.is_empty() {
                    eprintln!(
                        "[harn] DONE sentinel honored despite {} parse error(s) — recovery budget ({}) exhausted",
                        tool_parse_errors.len(),
                        MAX_PARSE_ERROR_RECOVERY_TURNS
                    );
                }
                break;
            }
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

        let nudge = custom_nudge.clone().unwrap_or_else(|| {
            if consecutive_text_only == 1 {
                if tool_format == "native" {
                    "You must use tools to complete this task. Start with the best available tool."
                        .to_string()
                } else {
                    "You must use tools to complete this task. Respond with a real ```call block, not a prose description of the tool you intend to use.\nExample:\n```call\ntool_name(param=\"value\")\n```"
                        .to_string()
                }
            } else if consecutive_text_only <= 3 {
                if tool_format == "native" {
                    "STOP explaining and USE TOOLS NOW. Include a concrete tool call."
                        .to_string()
                } else {
                    "STOP explaining and USE TOOLS NOW. A plain-English plan is a failure here. Reply with one or more actual ```call blocks only.\nExample:\n```call\ntool_name(param=\"value\")\n```"
                        .to_string()
                }
            } else {
                "FINAL WARNING: call a tool now or the task will fail.".to_string()
            }
        });
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
        "text": total_text,
        "visible_text": total_text,
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
            let max_nudges = opt_int(&options, "max_nudges").unwrap_or(3) as usize;
            let custom_nudge = opt_str(&options, "nudge");
            let tool_retries = opt_int(&options, "tool_retries").unwrap_or(0) as usize;
            let tool_backoff_ms = opt_int(&options, "tool_backoff_ms").unwrap_or(1000) as u64;
            let tool_format =
                opt_str(&options, "tool_format").unwrap_or_else(|| "text".to_string());
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
                if let Some(callback) = options.as_ref().and_then(|o| o.get("compact_callback")) {
                    ac.custom_compactor = Some(callback.clone());
                    if !options
                        .as_ref()
                        .is_some_and(|o| o.contains_key("compact_strategy"))
                    {
                        ac.compact_strategy = crate::orchestration::CompactStrategy::Custom;
                    }
                }
                // Adapt the compact threshold to the provider's actual max
                // context window if it can be discovered. This prevents the
                // "server silently truncates the prompt" failure mode where
                // the agent loses older turns without knowing, which we hit
                // with vLLM at 32K against the default 80K threshold.
                {
                    let probe_opts = extract_llm_options(&args)?;
                    crate::llm::api::adapt_auto_compact_to_provider(
                        &mut ac,
                        user_specified_threshold,
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
                    llm_retries: opt_int(&options, "llm_retries").unwrap_or(2) as usize,
                    llm_backoff_ms: opt_int(&options, "llm_backoff_ms").unwrap_or(2000) as u64,
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
    use super::api::vm_build_llm_result;
    use super::helpers::extract_json;
    use crate::stdlib::json_to_vm_value;

    let b = bridge;
    vm.register_async_builtin("llm_call", move |args| {
        let bridge = b.clone();
        async move {
            let opts = extract_llm_options(&args)?;

            let call_id = next_call_id();
            let prompt_chars: usize = opts
                .messages
                .iter()
                .filter_map(|m| m.get("content").and_then(|c| c.as_str()))
                .map(|s| s.len())
                .sum();
            annotate_current_span(&[
                ("call_id", serde_json::json!(call_id.clone())),
                ("model", serde_json::json!(opts.model.clone())),
                ("provider", serde_json::json!(opts.provider.clone())),
                ("prompt_chars", serde_json::json!(prompt_chars)),
            ]);
            bridge.send_call_start(
                &call_id,
                "llm",
                "llm_call",
                serde_json::json!({"model": opts.model, "prompt_chars": prompt_chars}),
            );

            let start = std::time::Instant::now();
            let delta_tx = spawn_progress_forwarder(&bridge, call_id.clone());
            let llm_result = vm_call_llm_full_streaming_offthread(&opts, delta_tx).await;
            let duration_ms = start.elapsed().as_millis() as u64;
            let result = match llm_result {
                Ok(r) => r,
                Err(e) => {
                    annotate_current_span(&[
                        ("status", serde_json::json!("error")),
                        ("error", serde_json::json!(e.to_string())),
                    ]);
                    bridge.send_call_end(
                        &call_id,
                        "llm",
                        "llm_call",
                        duration_ms,
                        "error",
                        serde_json::json!({"error": e.to_string()}),
                    );
                    return Err(e);
                }
            };

            trace_llm_call(LlmTraceEntry {
                model: result.model.clone(),
                input_tokens: result.input_tokens,
                output_tokens: result.output_tokens,
                duration_ms,
            });
            annotate_current_span(&[
                ("status", serde_json::json!("ok")),
                ("input_tokens", serde_json::json!(result.input_tokens)),
                ("output_tokens", serde_json::json!(result.output_tokens)),
            ]);

            bridge.send_call_end(
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

            let mut transcript_messages = opts.messages.clone();
            transcript_messages.push(build_assistant_response_message(
                &result.text,
                &result.blocks,
                &result.tool_calls,
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

            // Always return dict (breaking change: no more plain string)
            if opts.response_format.as_deref() == Some("json") {
                let json_str = extract_json(&result.text);
                let parsed = serde_json::from_str::<serde_json::Value>(json_str)
                    .ok()
                    .map(|jv| json_to_vm_value(&jv));
                return Ok(vm_build_llm_result(&result, parsed, Some(transcript)));
            }

            Ok(vm_build_llm_result(&result, None, Some(transcript)))
        }
    });
}

#[cfg(test)]
mod tests {
    use super::{compact_malformed_assistant_turn, loop_state_requests_phase_change};

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
        // The message must not echo any of the broken tool-call syntax that
        // triggered the elision; it is a fixed template.
        assert!(!msg.contains("```call"));
        assert!(!msg.contains("<<'EOF'"));

        let msg_plural = compact_malformed_assistant_turn(3);
        assert!(msg_plural.contains("3 malformed tool calls"));
    }
}
