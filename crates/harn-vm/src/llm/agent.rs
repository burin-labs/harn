use std::collections::HashMap;
use std::rc::Rc;

use serde::Deserialize;

use crate::value::{ErrorCategory, VmClosure, VmError, VmValue};
use crate::vm::Vm;

use super::api::{vm_call_llm_full_streaming, vm_call_llm_full_streaming_offthread, DeltaSender};
use super::daemon::{
    detect_watch_changes, load_snapshot, parse_daemon_loop_config, persist_snapshot, watch_state,
    DaemonLoopConfig, DaemonSnapshot,
};
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

fn denied_tool_result(tool_name: &str, reason: impl Into<String>) -> serde_json::Value {
    serde_json::json!({
        "error": "permission_denied",
        "tool": tool_name,
        "reason": reason.into(),
    })
}

fn render_tool_result(value: &serde_json::Value) -> String {
    if let Some(text) = value.as_str() {
        text.to_string()
    } else if value.is_null() {
        "(no output)".to_string()
    } else {
        serde_json::to_string_pretty(value).unwrap_or_default()
    }
}

fn is_denied_tool_result(value: &serde_json::Value) -> bool {
    value
        .get("error")
        .and_then(|error| error.as_str())
        .is_some_and(|error| error == "permission_denied")
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

fn normalize_native_tools_for_format(
    tool_format: &str,
    native_tools: Option<Vec<serde_json::Value>>,
) -> Option<Vec<serde_json::Value>> {
    if tool_format == "native" {
        native_tools
    } else {
        None
    }
}

fn normalize_tool_examples_for_format(
    tool_format: &str,
    tool_examples: Option<String>,
) -> Option<String> {
    if tool_format == "text" {
        tool_examples.and_then(|examples| {
            let trimmed = examples.trim();
            if trimmed.is_empty() {
                None
            } else {
                Some(trimmed.to_string())
            }
        })
    } else {
        None
    }
}

fn required_tool_choice_for_provider(provider: &str) -> serde_json::Value {
    if provider == "anthropic" {
        serde_json::json!({"type": "any"})
    } else {
        serde_json::json!("required")
    }
}

fn normalize_tool_choice_for_format(
    provider: &str,
    tool_format: &str,
    native_tools: Option<&[serde_json::Value]>,
    tool_choice: Option<serde_json::Value>,
    turn_policy: Option<&crate::orchestration::TurnPolicy>,
) -> Option<serde_json::Value> {
    if tool_format != "native" {
        return None;
    }
    if native_tools.is_none_or(|tools| tools.is_empty()) {
        return None;
    }
    if let Some(choice) = tool_choice {
        return Some(choice);
    }
    if turn_policy.is_some_and(|policy| policy.require_action_or_yield) {
        return Some(required_tool_choice_for_provider(provider));
    }
    None
}

fn native_protocol_violation_nudge(
    turn_policy: Option<&crate::orchestration::TurnPolicy>,
    saw_text_tool_calls: bool,
) -> String {
    let mut message = if saw_text_tool_calls {
        "This transcript is native-tool-only. Your previous response used handwritten tool-call text, which was not executed. Call an available tool through the provider tool channel instead of writing tool syntax in the assistant message.".to_string()
    } else {
        "This transcript is native-tool-only. Call an available tool through the provider tool channel now instead of replying with prose or bare code.".to_string()
    };
    if let Some(nudge) = action_turn_nudge("native", turn_policy, false) {
        message.push(' ');
        message.push_str(&nudge);
    }
    message
}

#[derive(Default)]
struct PostTurnDirective {
    message: Option<String>,
    stop: bool,
}

fn parse_post_turn_directive(value: &VmValue) -> PostTurnDirective {
    match value {
        VmValue::String(s) => {
            let trimmed = s.trim();
            if trimmed.is_empty() {
                PostTurnDirective::default()
            } else {
                PostTurnDirective {
                    message: Some(trimmed.to_string()),
                    stop: false,
                }
            }
        }
        VmValue::Bool(stop) => PostTurnDirective {
            message: None,
            stop: *stop,
        },
        VmValue::Dict(map) => {
            let message = map
                .get("message")
                .map(VmValue::display)
                .map(|msg| msg.trim().to_string())
                .filter(|msg| !msg.is_empty());
            let stop = matches!(map.get("stop"), Some(VmValue::Bool(true)));
            PostTurnDirective { message, stop }
        }
        _ => PostTurnDirective::default(),
    }
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
                Err(VmError::CategorizedError {
                    message,
                    category: ErrorCategory::ToolRejected,
                }) => Ok(denied_tool_result(tool_name, message)),
                Err(e) => Ok(serde_json::Value::String(format!("Error: {e}"))),
            }
        } else if let Some(bridge) = bridge {
            match bridge
                .call(
                    "builtin_call",
                    serde_json::json!({
                        "name": tool_name,
                        "args": [tool_args],
                    }),
                )
                .await
            {
                Err(VmError::CategorizedError {
                    message,
                    category: ErrorCategory::ToolRejected,
                }) => Ok(denied_tool_result(tool_name, message)),
                other => other,
            }
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
    /// Extended daemon lifecycle settings: persistence, timer wakes, and file watches.
    pub daemon_config: DaemonLoopConfig,
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
    /// Optional turn-shape constraints for action stages.
    pub turn_policy: Option<crate::orchestration::TurnPolicy>,
    /// Stop after a tool-calling turn that successfully used any of these
    /// tool names. The whole turn still completes, so multiple write calls can
    /// be batched in one response.
    pub stop_after_successful_tools: Option<Vec<String>>,
    /// If set, the loop returns `status = "failed"` unless at least one of
    /// these tool names completed successfully during the interaction. Lets
    /// pipelines declare "this stage has not done its job unless tool X
    /// actually ran" without having to inspect the returned `tools_used`
    /// list themselves.
    pub require_successful_tools: Option<Vec<String>>,
    /// Pre-execution hook: called with `{tool_name, args}` before each tool call.
    /// Must return a dict: `{allow: bool}` to allow/deny, optionally `{args: dict}`
    /// to modify arguments. Returning `{allow: false, reason: "..."}` denies the call.
    pub on_tool_call: Option<VmValue>,
    /// Post-execution hook: called with `{tool_name, result}` after each tool call.
    /// Returns a string to replace the result, or nil to keep it unchanged.
    pub on_tool_result: Option<VmValue>,
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

fn should_stop_after_successful_tools(
    tool_results: &[serde_json::Value],
    stop_tools: &[String],
) -> bool {
    has_successful_tools(tool_results, stop_tools)
}

fn has_successful_tools(tool_results: &[serde_json::Value], tool_names: &[String]) -> bool {
    tool_results
        .iter()
        .filter(|result| result["status"].as_str() == Some("ok"))
        .filter_map(|result| result["tool_name"].as_str())
        .any(|tool_name| tool_names.iter().any(|wanted| wanted == tool_name))
}

fn prose_char_len(text: &str) -> usize {
    text.trim().chars().count()
}

fn prose_exceeds_budget(
    prose: &str,
    turn_policy: Option<&crate::orchestration::TurnPolicy>,
) -> bool {
    let Some(limit) = turn_policy.and_then(|policy| policy.max_prose_chars) else {
        return false;
    };
    prose_char_len(prose) > limit
}

fn trim_prose_for_history(
    prose: &str,
    turn_policy: Option<&crate::orchestration::TurnPolicy>,
) -> String {
    let trimmed = prose.trim();
    let Some(limit) = turn_policy.and_then(|policy| policy.max_prose_chars) else {
        return trimmed.to_string();
    };
    let chars: Vec<char> = trimmed.chars().collect();
    if chars.len() <= limit {
        return trimmed.to_string();
    }
    let kept: String = chars.into_iter().take(limit).collect();
    format!("{kept}\n\n<assistant prose truncated by turn policy; keep prose brief and act>")
}

fn action_turn_nudge(
    tool_format: &str,
    turn_policy: Option<&crate::orchestration::TurnPolicy>,
    prose_too_long: bool,
) -> Option<String> {
    let policy = turn_policy?;
    if !policy.require_action_or_yield {
        return None;
    }
    let prose_clause = if let Some(limit) = policy.max_prose_chars {
        format!("Keep prose to at most {limit} visible characters, then")
    } else {
        "Keep prose brief, then".to_string()
    };
    let emphasis = if prose_too_long {
        " Your last response spent too much budget on prose."
    } else {
        ""
    };
    let completion_clause = if policy.allow_done_sentinel {
        "either call at least one tool, switch phase, or output the done sentinel if the task is genuinely complete."
    } else {
        "either call at least one tool or switch phase if the workflow allows it."
    };
    let mode_clause = if tool_format == "native" {
        " Use the provider tool channel only; handwritten tool-call text is invalid in this transcript."
    } else {
        ""
    };
    Some(format!(
        "{prose_clause} {completion_clause}{emphasis}{mode_clause}"
    ))
}

fn sentinel_without_action_nudge(
    tool_format: &str,
    turn_policy: Option<&crate::orchestration::TurnPolicy>,
) -> String {
    let mut message = if turn_policy.is_some_and(|policy| !policy.allow_done_sentinel) {
        "You emitted a done sentinel in a workflow-owned action stage. The task is not complete yet. Use an available tool now, or switch phase if the workflow allows it. Do not output a done sentinel in this stage.".to_string()
    } else {
        "You emitted the done sentinel without taking any tool action. The task is not complete yet. Use an available tool now, or switch phase if the workflow allows it. Do not output the done sentinel again until you have acted.".to_string()
    };
    if let Some(nudge) = action_turn_nudge(tool_format, turn_policy, false) {
        message.push(' ');
        message.push_str(&nudge);
    }
    message
}

/// Write the full LLM request payload to a JSONL transcript file.
/// Enabled by setting HARN_LLM_TRANSCRIPT_DIR to a directory path.
fn append_llm_transcript_entry(entry: &serde_json::Value) {
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

fn dump_llm_request(
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

fn dump_llm_response(
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

fn dump_llm_interpreted_response(
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

async fn maybe_auto_compact_agent_messages(
    opts: &super::api::LlmCallOptions,
    auto_compact: &Option<crate::orchestration::AutoCompactConfig>,
    visible_messages: &mut Vec<serde_json::Value>,
    transcript_summary: &mut Option<String>,
) -> Result<(), VmError> {
    if let Some(ac) = auto_compact {
        let approx_tokens = crate::orchestration::estimate_message_tokens(visible_messages);
        if approx_tokens >= ac.token_threshold {
            let mut compact_opts = opts.clone();
            compact_opts.messages = visible_messages.clone();
            if let Some(summary) = crate::orchestration::auto_compact_messages(
                visible_messages,
                ac,
                Some(&compact_opts),
            )
            .await?
            {
                let merged = match transcript_summary.take() {
                    Some(existing) if !existing.is_empty() => {
                        format!("{existing}\n\n{summary}")
                    }
                    _ => summary,
                };
                *transcript_summary = Some(merged);
            }
        }
    }
    Ok(())
}

#[allow(clippy::too_many_arguments)]
fn daemon_snapshot_from_state(
    daemon_state: &str,
    visible_messages: &[serde_json::Value],
    recorded_messages: &[serde_json::Value],
    transcript_summary: &Option<String>,
    transcript_events: &[VmValue],
    total_text: &str,
    last_iteration_text: &str,
    all_tools_used: &[String],
    rejected_tools: &[String],
    deferred_user_messages: &[String],
    total_iterations: usize,
    idle_backoff_ms: u64,
    last_run_exit_code: Option<i32>,
    watch_state_map: &std::collections::BTreeMap<String, u64>,
) -> DaemonSnapshot {
    DaemonSnapshot {
        daemon_state: daemon_state.to_string(),
        visible_messages: visible_messages.to_vec(),
        recorded_messages: recorded_messages.to_vec(),
        transcript_summary: transcript_summary.clone(),
        transcript_events: transcript_events
            .iter()
            .map(crate::llm::helpers::vm_value_to_json)
            .collect(),
        total_text: total_text.to_string(),
        last_iteration_text: last_iteration_text.to_string(),
        all_tools_used: all_tools_used.to_vec(),
        rejected_tools: rejected_tools.to_vec(),
        deferred_user_messages: deferred_user_messages.to_vec(),
        total_iterations,
        idle_backoff_ms,
        last_run_exit_code,
        watch_state: watch_state_map.clone(),
        ..Default::default()
    }
}

fn maybe_persist_daemon_snapshot(
    config: &DaemonLoopConfig,
    snapshot: &DaemonSnapshot,
) -> Result<Option<String>, VmError> {
    let Some(path) = config.effective_persist_path() else {
        return Ok(None);
    };
    persist_snapshot(path, snapshot).map(Some)
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
    tool_format: Option<&str>,
    bridge: Option<&Rc<crate::bridge::HostBridge>>,
    retry_config: &LlmRetryConfig,
    iteration: Option<usize>,
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
    let daemon_config = config.daemon_config.clone();
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
    opts.native_tools = normalize_native_tools_for_format(&tool_format, opts.native_tools.clone());
    opts.tool_choice = normalize_tool_choice_for_format(
        &opts.provider,
        &tool_format,
        opts.native_tools.as_deref(),
        opts.tool_choice.clone(),
        config.turn_policy.as_ref(),
    );
    let native_tools_for_prompt = opts.native_tools.clone();
    let rendered_schemas =
        crate::llm::tools::collect_tool_schemas(tools_val, native_tools_for_prompt.as_deref());
    let has_tools = !rendered_schemas.is_empty();
    let base_system = opts.system.clone();
    let tool_examples =
        normalize_tool_examples_for_format(&tool_format, config.tool_examples.clone());
    let tool_contract_prompt = if has_tools {
        Some(build_tool_calling_contract_prompt(
            tools_val,
            native_tools_for_prompt.as_deref(),
            &tool_format,
            config
                .turn_policy
                .as_ref()
                .is_some_and(|policy| policy.require_action_or_yield),
            tool_examples.as_deref(),
        ))
    } else {
        None
    };

    let allow_done_sentinel = config
        .turn_policy
        .as_ref()
        .map(|policy| policy.allow_done_sentinel)
        .unwrap_or(true);
    let persistent_system_prompt = if persistent {
        if exit_when_verified {
            // When exit_when_verified is set, the harness enforces that the
            // done sentinel is only honoured after a passing run(). The
            // system prompt only needs a brief reminder, not a long rule.
            if allow_done_sentinel {
                Some(format!(
                    "\n\nKeep working until the task is complete. Take action with tools — \
                     do not stop to explain. Output {done_sentinel} when done."
                ))
            } else {
                Some(
                    "\n\nKeep working until the task is complete. Take action with tools — \
                     do not stop to explain."
                        .to_string(),
                )
            }
        } else {
            if allow_done_sentinel {
                Some(format!(
                    "\n\nIMPORTANT: You MUST keep working until the task is complete. \
                     Do NOT stop to explain or summarize — take action with tools. \
                     When the requested work is complete, output {done_sentinel} on its own line."
                ))
            } else {
                Some(
                    "\n\nIMPORTANT: You MUST keep working until the task is complete. \
                     Do NOT stop to explain or summarize — take action with tools."
                        .to_string(),
                )
            }
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
    let mut successful_tools_used: Vec<String> = Vec::new();
    let mut rejected_tools: Vec<String> = Vec::new();
    let mut deferred_user_messages: Vec<String> = Vec::new();
    let mut total_iterations = 0usize;
    let mut final_status = "done";
    let mut transcript_summary = opts.transcript_summary.clone();
    let loop_start = std::time::Instant::now();
    let mut transcript_events = Vec::new();
    let mut idle_backoff_ms = 100u64;
    let mut daemon_state = if daemon {
        "active".to_string()
    } else {
        "done".to_string()
    };
    let mut daemon_snapshot_path: Option<String> = None;
    let mut daemon_watch_state = watch_state(&daemon_config.watch_paths);
    let mut resumed_iterations = 0usize;

    if daemon {
        if let Some(path) = daemon_config.resume_path.as_deref() {
            let snapshot = load_snapshot(path)?;
            daemon_state = snapshot.daemon_state.clone();
            visible_messages = snapshot.visible_messages;
            recorded_messages = snapshot.recorded_messages;
            transcript_summary = snapshot.transcript_summary;
            transcript_events = snapshot
                .transcript_events
                .iter()
                .map(crate::stdlib::json_to_vm_value)
                .collect();
            total_text = snapshot.total_text;
            last_iteration_text = snapshot.last_iteration_text;
            all_tools_used = snapshot.all_tools_used;
            rejected_tools = snapshot.rejected_tools;
            deferred_user_messages = snapshot.deferred_user_messages;
            resumed_iterations = snapshot.total_iterations;
            total_iterations = resumed_iterations;
            idle_backoff_ms = snapshot.idle_backoff_ms.max(1);
            last_run_exit_code = snapshot.last_run_exit_code;
            daemon_watch_state = if snapshot.watch_state.is_empty() {
                watch_state(&daemon_config.watch_paths)
            } else {
                snapshot.watch_state
            };
            daemon_snapshot_path = Some(path.to_string());
        } else if let Some(path) = daemon_config.effective_persist_path() {
            daemon_snapshot_path = Some(path.to_string());
        }
    }

    for iteration in 0..max_iterations {
        total_iterations = resumed_iterations + iteration + 1;
        daemon_state = "active".to_string();
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
            Some(&tool_format),
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
            let parse_result = parse_text_tool_calls_with_tools(&text, tools_val);
            tool_parse_errors = parse_result.errors;
            text_prose = parse_result.prose;
            if tool_format == "native" {
                if !parse_result.calls.is_empty() || !tool_parse_errors.is_empty() {
                    let feedback = native_protocol_violation_nudge(
                        config.turn_policy.as_ref(),
                        !parse_result.calls.is_empty(),
                    );
                    append_message_to_contexts(
                        &mut visible_messages,
                        &mut recorded_messages,
                        serde_json::json!({"role": "user", "content": feedback}),
                    );
                }
                Vec::new()
            } else {
                let calls = parse_result.calls;

                // When the parser found tool-call-looking text but couldn't
                // parse it, inject the specific parse error into the conversation
                // so the model knows what to fix (e.g. unescaped backtick inside
                // a template literal). Without this, the generic nudge gives the
                // model no signal about what was wrong.
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
            }
        } else {
            Vec::new()
        };
        let prose_too_long = prose_exceeds_budget(&text_prose, config.turn_policy.as_ref());
        let shaped_text_prose = trim_prose_for_history(&text_prose, config.turn_policy.as_ref());
        let interpreted_call_id = format!("iteration-{iteration}");
        dump_llm_interpreted_response(
            iteration,
            &interpreted_call_id,
            &tool_format,
            &shaped_text_prose,
            &tool_calls,
            &tool_parse_errors,
        );
        // Surface the prose (not the raw text) to callers that read
        // `last_iteration_text` / `visible_text`. Tool call expressions are
        // structured data in `tool_calls`, not something the user should
        // see as the agent's "answer". This also means conversation history
        // will carry the prose, so future iterations don't see their own
        // prior call syntax as narration.
        last_iteration_text = shaped_text_prose.clone();

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
            let corrective =
                sentinel_without_action_nudge(&tool_format, config.turn_policy.as_ref());
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
                    shaped_text_prose.clone()
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
            let mut tool_results_this_iter: Vec<serde_json::Value> = Vec::new();
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
                    let result_text = render_tool_result(&denied_tool_result(
                        tool_name,
                        format!(
                            "{error}. Use one of the declared tools exactly as named and put extra fields inside that tool's arguments."
                        ),
                    ));
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
                        let result_text =
                            render_tool_result(&denied_tool_result(tool_name, reason));
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

                // on_tool_call closure hook (Harn-level pre-execution)
                if let Some(VmValue::Closure(ref closure)) = config.on_tool_call {
                    if let Some(mut vm) = crate::vm::clone_async_builtin_child_vm() {
                        let hook_arg = crate::stdlib::json_to_vm_value(&serde_json::json!({
                            "tool_name": tool_name,
                            "args": tool_args,
                        }));
                        if let Ok(result) = vm.call_closure_pub(closure, &[hook_arg], &[]).await {
                            if let Some(dict) = result.as_dict() {
                                if matches!(dict.get("allow"), Some(VmValue::Bool(false))) {
                                    let reason = dict
                                        .get("reason")
                                        .map(|v| v.display())
                                        .unwrap_or_else(|| "denied by on_tool_call hook".into());
                                    let result_text =
                                        render_tool_result(&denied_tool_result(tool_name, reason));
                                    if !rejected_tools.contains(&tool_name.to_string()) {
                                        rejected_tools.push(tool_name.to_string());
                                    }
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
                                if let Some(new_args) = dict.get("args") {
                                    tool_args = crate::llm::vm_value_to_json(new_args);
                                }
                            }
                        }
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
                                let result_text =
                                    render_tool_result(&denied_tool_result(tool_name, reason));
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

                    let rejected =
                        matches!(
                            &call_result,
                            Err(VmError::CategorizedError {
                                category: ErrorCategory::ToolRejected,
                                ..
                            })
                        ) || call_result.as_ref().ok().is_some_and(is_denied_tool_result);
                    let text = match &call_result {
                        Ok(val) => render_tool_result(val),
                        Err(VmError::CategorizedError {
                            message,
                            category: ErrorCategory::ToolRejected,
                        }) => render_tool_result(&denied_tool_result(
                            tool_name,
                            format!("{message} Do not retry this tool."),
                        )),
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

                // on_tool_result closure hook (Harn-level post-execution)
                let result_text = if let Some(VmValue::Closure(ref closure)) = config.on_tool_result
                {
                    if let Some(mut vm) = crate::vm::clone_async_builtin_child_vm() {
                        let hook_arg = crate::stdlib::json_to_vm_value(&serde_json::json!({
                            "tool_name": tool_name,
                            "result": result_text,
                        }));
                        match vm.call_closure_pub(closure, &[hook_arg], &[]).await {
                            Ok(VmValue::String(s)) if !s.is_empty() => s.to_string(),
                            _ => result_text,
                        }
                    } else {
                        result_text
                    }
                } else {
                    result_text
                };

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
                let tool_status = if is_rejected {
                    "rejected"
                } else if result_text.starts_with("Error:") || result_text.starts_with("ERROR:") {
                    "error"
                } else {
                    "ok"
                };
                tool_results_this_iter.push(serde_json::json!({
                    "tool_name": tool_name,
                    "status": tool_status,
                    "rejected": is_rejected,
                }));

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
            let successful_tool_names: Vec<&str> = tool_results_this_iter
                .iter()
                .filter(|result| result["status"].as_str() == Some("ok"))
                .filter_map(|result| result["tool_name"].as_str())
                .collect();
            for tool_name in &successful_tool_names {
                if !successful_tools_used
                    .iter()
                    .any(|existing| existing == tool_name)
                {
                    successful_tools_used.push((*tool_name).to_string());
                }
            }
            if let Some(VmValue::Closure(ref closure)) = config.post_turn_callback {
                let tool_names: Vec<&str> = tool_calls
                    .iter()
                    .filter_map(|tc| tc["name"].as_str())
                    .collect();
                let turn_info = serde_json::json!({
                    "tool_names": tool_names,
                    "tool_results": tool_results_this_iter,
                    "successful_tool_names": successful_tool_names,
                    "tool_count": tool_calls.len(),
                    "iteration": iteration,
                    "consecutive_single_tool_turns": consecutive_single_tool_turns,
                    "session_tools_used": all_tools_used,
                    "session_successful_tools": successful_tools_used,
                });
                let cb_arg = crate::stdlib::json_to_vm_value(&turn_info);
                if let Ok(mut vm) = crate::vm::clone_async_builtin_child_vm()
                    .ok_or_else(|| VmError::Runtime("no VM context".into()))
                {
                    if let Ok(val) = vm.call_closure_pub(closure, &[cb_arg], &[]).await {
                        let directive = parse_post_turn_directive(&val);
                        if let Some(msg) = directive.message {
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
                        if directive.stop {
                            crate::events::log_debug(
                                "agent.post_turn",
                                &format!("iter={iteration} requested stage stop"),
                            );
                            break;
                        }
                    }
                }
            }
            if let Some(stop_tools) = config.stop_after_successful_tools.as_ref() {
                if should_stop_after_successful_tools(&tool_results_this_iter, stop_tools) {
                    crate::events::log_debug(
                        "agent.stop_after_successful_tools",
                        &format!(
                            "iter={iteration} requested stage stop after successful tool turn"
                        ),
                    );
                    break;
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
            shaped_text_prose.clone()
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
            daemon_state = "idle".to_string();
            if daemon_config.consolidate_on_idle {
                maybe_auto_compact_agent_messages(
                    opts,
                    &auto_compact,
                    &mut visible_messages,
                    &mut transcript_summary,
                )
                .await?;
            }
            let idle_snapshot = daemon_snapshot_from_state(
                &daemon_state,
                &visible_messages,
                &recorded_messages,
                &transcript_summary,
                &transcript_events,
                &total_text,
                &last_iteration_text,
                &all_tools_used,
                &rejected_tools,
                &deferred_user_messages,
                total_iterations,
                idle_backoff_ms,
                last_run_exit_code,
                &daemon_watch_state,
            );
            daemon_snapshot_path = maybe_persist_daemon_snapshot(&daemon_config, &idle_snapshot)?
                .or(daemon_snapshot_path);
            if !daemon_config.has_wake_source(bridge.is_some()) {
                final_status = "idle";
                break;
            }
            loop {
                if let Some(bridge) = bridge.as_ref() {
                    bridge.notify(
                        "agent/idle",
                        serde_json::json!({
                            "iteration": total_iterations,
                            "backoff_ms": idle_backoff_ms,
                            "persist_path": daemon_snapshot_path,
                            "watch_paths": daemon_config.watch_paths,
                        }),
                    );
                }
                tokio::time::sleep(tokio::time::Duration::from_millis(
                    daemon_config.idle_wait_ms(idle_backoff_ms),
                ))
                .await;
                let resumed = bridge
                    .as_ref()
                    .is_some_and(|bridge| bridge.take_resume_signal());
                let idle_messages = inject_queued_user_messages(
                    bridge.as_ref(),
                    &mut visible_messages,
                    crate::bridge::DeliveryCheckpoint::InterruptImmediate,
                )
                .await?;
                append_host_messages_to_recorded(&mut recorded_messages, &idle_messages);
                let changed_paths = if daemon_config.watch_paths.is_empty() {
                    Vec::new()
                } else {
                    detect_watch_changes(&daemon_config.watch_paths, &mut daemon_watch_state)
                };
                for message in &idle_messages {
                    transcript_events.push(transcript_event(
                        "host_input",
                        "user",
                        "public",
                        &message.content,
                        Some(serde_json::json!({"delivery": format!("{:?}", message.mode)})),
                    ));
                }
                let wake_reason = if !idle_messages.is_empty() {
                    Some(("message", None))
                } else if resumed {
                    Some(("resume", None))
                } else if !changed_paths.is_empty() {
                    Some((
                        "watch",
                        Some(format!(
                            "Daemon wake: watched paths changed: {}. Re-check the task state and act only if something actually changed.",
                            changed_paths.join(", ")
                        )),
                    ))
                } else if daemon_config.wake_interval_ms.is_some() {
                    Some((
                        "timer",
                        Some(
                            "Daemon timer wake fired. Re-check for background work and only act when there is new information or a pending follow-up."
                                .to_string(),
                        ),
                    ))
                } else {
                    None
                };
                if let Some((reason, wake_message)) = wake_reason {
                    if let Some(message) = wake_message {
                        append_message_to_contexts(
                            &mut visible_messages,
                            &mut recorded_messages,
                            serde_json::json!({
                                "role": "user",
                                "content": message,
                            }),
                        );
                    }
                    transcript_events.push(transcript_event(
                        "daemon_wake",
                        "system",
                        "internal",
                        reason,
                        Some(serde_json::json!({
                            "reason": reason,
                            "watch_paths": changed_paths,
                            "resumed": resumed,
                        })),
                    ));
                    daemon_state = "active".to_string();
                    consecutive_text_only = 0;
                    idle_backoff_ms = 100;
                    break;
                }
                daemon_config.update_idle_backoff(&mut idle_backoff_ms);
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
        let nudge = action_turn_nudge(&tool_format, config.turn_policy.as_ref(), prose_too_long)
            .or_else(|| custom_nudge.clone())
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

    if daemon && final_status == "done" {
        final_status = "idle";
    }
    if final_status == "done" {
        if let Some(required_tools) = config.require_successful_tools.as_ref() {
            if !required_tools.is_empty()
                && !successful_tools_used
                    .iter()
                    .any(|tool_name| required_tools.iter().any(|wanted| wanted == tool_name))
            {
                final_status = "failed";
            }
        }
    }
    if daemon {
        daemon_state = final_status.to_string();
        let final_snapshot = daemon_snapshot_from_state(
            &daemon_state,
            &visible_messages,
            &recorded_messages,
            &transcript_summary,
            &transcript_events,
            &total_text,
            &last_iteration_text,
            &all_tools_used,
            &rejected_tools,
            &deferred_user_messages,
            total_iterations,
            idle_backoff_ms,
            last_run_exit_code,
            &daemon_watch_state,
        );
        daemon_snapshot_path = maybe_persist_daemon_snapshot(&daemon_config, &final_snapshot)?
            .or(daemon_snapshot_path);
    }

    Ok(serde_json::json!({
        "status": final_status,
        "daemon_state": daemon_state,
        "daemon_snapshot_path": daemon_snapshot_path,
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
        "successful_tools": successful_tools_used,
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
            let daemon_config = parse_daemon_loop_config(options.as_ref());
            let turn_policy = options
                .as_ref()
                .and_then(|o| o.get("turn_policy"))
                .map(|v| {
                    let json = crate::llm::helpers::vm_value_to_json(v);
                    serde_json::from_value::<crate::orchestration::TurnPolicy>(json)
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
                    daemon_config,
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
                    turn_policy,
                    stop_after_successful_tools: crate::llm::helpers::opt_str_list(
                        &options,
                        "stop_after_successful_tools",
                    ),
                    require_successful_tools: crate::llm::helpers::opt_str_list(
                        &options,
                        "require_successful_tools",
                    ),
                    on_tool_call: options
                        .as_ref()
                        .and_then(|o| o.get("on_tool_call"))
                        .cloned(),
                    on_tool_result: options
                        .as_ref()
                        .and_then(|o| o.get("on_tool_result"))
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
                opt_str(&options, "tool_format").as_deref(),
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
#[path = "agent_tests.rs"]
mod tests;
